import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { traceCpuSpecForKind } from '@debugger-core/trace-spec-core'
import type { CpuKind } from '@debugger-core/cpu-flags-core'
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
}

function harness(focusedCpu: number) {
  const emulator = {
    setTracingEnabled: vi.fn(),
    setZ80TracingEnabled: vi.fn(),
    setSpc700TracingEnabled: vi.fn(),
  }
  const bridge = {
    getEmulator: () => emulator,
    isRomLoaded: () => true,
    getFocusedCpu: () => focusedCpu,
  } as unknown as DebugBridge
  return { emulator, bridge }
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

  const cases: Array<[WebCore, number, CpuKind, string]> = [
    ['sg1', 0, 'm68k', 'setTracingEnabled'],
    ['sg1', 1, 'z80', 'setZ80TracingEnabled'],
    ['snes1', 0, 'w65816', 'setTracingEnabled'],
    ['snes1', 1, 'spc700', 'setSpc700TracingEnabled'],
  ]

  for (const [core, cpu, kind, binding] of cases) {
    it(`${core} cpu${cpu}: hands the worker the ${kind} entry size`, () => {
      const { emulator, bridge } = harness(cpu)
      setupTracing(bridge, core).startTracing()

      const worker = FakeWorker.instances[0]
      const start = worker.posted.find((m) => m.type === 'start')
      expect(start).toBeDefined()
      expect(start.entrySize).toBe(traceCpuSpecForKind(kind).entrySize)
      expect((emulator as Record<string, any>)[binding]).toHaveBeenCalledWith(true)
    })
  }

  it('sends 44 bytes for the 65816 and 24 for the SPC700, the SNES core entry sizes', () => {
    const sizes = [0, 1].map((cpu) => {
      FakeWorker.instances = []
      setupTracing(harness(cpu).bridge, 'snes1').startTracing()
      return FakeWorker.instances[0].posted.find((m) => m.type === 'start').entrySize
    })
    expect(sizes).toEqual([44, 24])
  })
})
