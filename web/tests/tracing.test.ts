import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { setupTracing } from '../src/tracing'
import type { DebugBridge } from '../src/debug-bridge'
import type { WebCore } from '../src/core-resolver'

// Stands in for the OPFS trace worker: records what the page posts to it.
class FakeWorker {
  static instances: FakeWorker[] = []
  posted: any[] = []
  onmessage: ((e: MessageEvent) => void) | null = null
  onerror: ((e: unknown) => void) | null = null
  constructor() {
    FakeWorker.instances.push(this)
  }
  postMessage(message: unknown): void {
    this.posted.push(message)
  }
  terminate(): void {}
  /** A message from the worker, as the page receives it. */
  reply(data: unknown): void {
    this.onmessage?.({ data } as MessageEvent)
  }
}

/** Each core's trace entry sizes by CPU, as its bindings report them. */
const SIZES: Record<WebCore, number[]> = {
  sg1: [84, 38],
  snes1: [44, 24],
}

function harness(core: WebCore, focusedCpu: number) {
  const emulator: Record<string, any> = {
    setCpuTracingEnabled: vi.fn(),
    getCpuTraceEntrySize: vi.fn((cpu: number) => SIZES[core][cpu] ?? 0),
    getTraceStats: vi.fn(() => ({ total_entries: 1234 })),
  }
  const bridge = {
    getEmulator: () => emulator,
    isRomLoaded: () => true,
    getFocusedCpu: () => focusedCpu,
  } as unknown as DebugBridge
  return { emulator, bridge }
}

/** The toolbar the tracing module wires, as plain objects. */
function toolbar() {
  const element = () => ({
    disabled: false,
    textContent: '',
    classList: { add: vi.fn(), remove: vi.fn() },
    addEventListener: vi.fn(),
  })
  const elements: Record<string, ReturnType<typeof element>> = {
    'trace-start-btn': element(),
    'trace-stop-btn': element(),
    'trace-export-btn': element(),
    'trace-clear-btn': element(),
    'trace-stats': element(),
  }
  vi.stubGlobal('document', { getElementById: (id: string) => elements[id] ?? null })
  return elements
}

describe('setupTracing - the start message', () => {
  beforeEach(() => {
    FakeWorker.instances = []
    // No toolbar in Node: every element lookup misses, which the module
    // already allows for.
    vi.stubGlobal('document', { getElementById: () => null })
    vi.stubGlobal('Worker', FakeWorker)
    vi.spyOn(console, 'log').mockImplementation(() => {})
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  const cases: Array<[WebCore, number, number]> = [
    ['sg1', 0, 84],
    ['sg1', 1, 38],
    ['snes1', 0, 44],
    ['snes1', 1, 24],
  ]

  for (const [core, cpu, size] of cases) {
    it(`${core} cpu${cpu}: hands the worker the entry size the core reports (${size})`, () => {
      const { emulator, bridge } = harness(core, cpu)
      setupTracing(bridge, core).startTracing()

      const worker = FakeWorker.instances[0]
      const start = worker.posted.find((m) => m.type === 'start')
      expect(start).toBeDefined()
      expect(start.entrySize).toBe(size)
      expect(emulator.getCpuTraceEntrySize).toHaveBeenCalledWith(cpu)
      expect(emulator.setCpuTracingEnabled).toHaveBeenCalledWith(cpu, true)
    })
  }

  it('does not start a trace whose entry size the core does not report', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const { emulator, bridge } = harness('sg1', 1)
    emulator.getCpuTraceEntrySize = vi.fn(() => 0)
    setupTracing(bridge, 'sg1').startTracing()

    expect(FakeWorker.instances[0].posted.find((m) => m.type === 'start')).toBeUndefined()
    expect(emulator.setCpuTracingEnabled).not.toHaveBeenCalled()
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('Z80'))
  })
})

describe('setupTracing - the entry count', () => {
  beforeEach(() => {
    FakeWorker.instances = []
    vi.stubGlobal('Worker', FakeWorker)
    vi.spyOn(console, 'log').mockImplementation(() => {})
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  it("counts the worker's bytes in entries of the size the core reports", () => {
    const elements = toolbar()
    const { bridge } = harness('sg1', 1)
    const tracing = setupTracing(bridge, 'sg1')
    tracing.startTracing()

    // 38 bytes an entry: 3 800 bytes are 100 entries.
    FakeWorker.instances[0].reply({ type: 'progress', bytes: 3800 })
    expect(elements['trace-stats'].textContent).toBe(`${(100).toLocaleString()} Z80 entries`)
  })

  it("reads the core's count only for the main CPU, whose ring it describes", () => {
    const elements = toolbar()

    const main = harness('snes1', 0)
    setupTracing(main.bridge, 'snes1').startTracing()
    expect(main.emulator.getTraceStats).toHaveBeenCalled()
    expect(elements['trace-stats'].textContent).toBe(`${(1234).toLocaleString()} entries`)

    elements['trace-stats'].textContent = ''
    const sound = harness('snes1', 1)
    setupTracing(sound.bridge, 'snes1').startTracing()
    expect(sound.emulator.getTraceStats).not.toHaveBeenCalled()
    expect(elements['trace-stats'].textContent).toBe('')
  })
})

describe('setupTracing - taking the trace', () => {
  beforeEach(() => {
    FakeWorker.instances = []
    vi.stubGlobal('document', { getElementById: () => null })
    vi.stubGlobal('Worker', FakeWorker)
    vi.spyOn(console, 'log').mockImplementation(() => {})
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  /**
   * A core whose CPU `cpu` holds the trace bytes `held`, and asks for them
   * to be taken when `asks`, as a core asks only once its buffer fills.
   */
  function holding(core: WebCore, cpu: number, held: number[], asks: boolean) {
    const { emulator, bridge } = harness(core, cpu)
    let bytes = Uint8Array.from(held)
    emulator.needsCpuTraceFlush = vi.fn(() => asks)
    emulator.getCpuTraceFlushData = vi.fn(() => bytes.slice())
    emulator.clearCpuTraceFlushed = vi.fn(() => {
      bytes = new Uint8Array(0)
    })
    return { emulator, bridge }
  }

  const types = (worker: FakeWorker) => worker.posted.map((m) => m.type)

  it('hands the file what a recording left when it stops, though the core did not ask', () => {
    const { emulator, bridge } = holding('sg1', 1, [1, 2, 3], false)
    const tracing = setupTracing(bridge, 'sg1')
    tracing.startTracing()
    tracing.stopTracing()

    const worker = FakeWorker.instances[0]
    expect(types(worker)).toEqual(['start', 'chunk', 'stop'])
    expect(Array.from(worker.posted[1].data)).toEqual([1, 2, 3])
    expect(emulator.clearCpuTraceFlushed).toHaveBeenCalledWith(1)
    // The trace is off before it is taken, so nothing is added after.
    expect(emulator.setCpuTracingEnabled).toHaveBeenLastCalledWith(1, false)
    const switches = emulator.setCpuTracingEnabled.mock.invocationCallOrder
    const taken = emulator.getCpuTraceFlushData.mock.invocationCallOrder[0]
    expect(switches[switches.length - 1]).toBeLessThan(taken)
  })

  it('hands the file nothing more when the recording left nothing', () => {
    const { bridge } = holding('snes1', 0, [], false)
    const tracing = setupTracing(bridge, 'snes1')
    tracing.startTracing()
    tracing.stopTracing()

    expect(types(FakeWorker.instances[0])).toEqual(['start', 'stop'])
  })

  it('takes the trace while recording only when the core asks', () => {
    const quiet = holding('sg1', 0, [9, 9], false)
    const tracing = setupTracing(quiet.bridge, 'sg1')
    tracing.startTracing()
    tracing.checkFlush()

    expect(quiet.emulator.getCpuTraceFlushData).not.toHaveBeenCalled()
    expect(types(FakeWorker.instances[0])).toEqual(['start'])
  })

  it('hands the file the trace while recording when the core asks, and clears it', () => {
    const full = holding('sg1', 0, [7, 8], true)
    const tracing = setupTracing(full.bridge, 'sg1')
    tracing.startTracing()
    tracing.checkFlush()

    const worker = FakeWorker.instances[0]
    expect(types(worker)).toEqual(['start', 'chunk'])
    expect(Array.from(worker.posted[1].data)).toEqual([7, 8])
    expect(full.emulator.clearCpuTraceFlushed).toHaveBeenCalledWith(0)
  })
})
