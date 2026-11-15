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

/** Each core's trace entry sizes, as its bindings report them. */
const SIZES: Record<WebCore, Record<string, number>> = {
  sg1: { getTraceEntrySize: 84, getZ80TraceEntrySize: 38 },
  snes1: { getTraceEntrySize: 44, getSpc700TraceEntrySize: 24 },
}

function harness(core: WebCore, focusedCpu: number) {
  const emulator: Record<string, any> = {
    setTracingEnabled: vi.fn(),
    setZ80TracingEnabled: vi.fn(),
    setSpc700TracingEnabled: vi.fn(),
    getTraceStats: vi.fn(() => ({ total_entries: 1234 })),
  }
  for (const [method, size] of Object.entries(SIZES[core])) emulator[method] = vi.fn(() => size)
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

  const cases: Array<[WebCore, number, number, string]> = [
    ['sg1', 0, 84, 'setTracingEnabled'],
    ['sg1', 1, 38, 'setZ80TracingEnabled'],
    ['snes1', 0, 44, 'setTracingEnabled'],
    ['snes1', 1, 24, 'setSpc700TracingEnabled'],
  ]

  for (const [core, cpu, size, binding] of cases) {
    it(`${core} cpu${cpu}: hands the worker the entry size the core reports (${size})`, () => {
      const { emulator, bridge } = harness(core, cpu)
      setupTracing(bridge, core).startTracing()

      const worker = FakeWorker.instances[0]
      const start = worker.posted.find((m) => m.type === 'start')
      expect(start).toBeDefined()
      expect(start.entrySize).toBe(size)
      expect(emulator[binding]).toHaveBeenCalledWith(true)
    })
  }

  it('does not start a trace whose entry size the core does not report', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const { emulator, bridge } = harness('sg1', 1)
    delete emulator.getZ80TraceEntrySize
    setupTracing(bridge, 'sg1').startTracing()

    expect(FakeWorker.instances[0].posted.find((m) => m.type === 'start')).toBeUndefined()
    expect(emulator.setZ80TracingEnabled).not.toHaveBeenCalled()
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
