import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { PerformanceOverlay, processorPcs } from '../src/perf-overlay.js'
import { describeShape, recordedIn } from './reader-shapes'

const registers = (pairs: Array<[string, number]>) => pairs.map(([name, value]) => ({ name, value }))

// The CPU states `getCpuStates()` gives at the stop of the Mega Drive
// binding's own test (`what_the_cpu_states_hold`): the 68000 at $500, the
// Z80 at $1A.
const states = [
  {
    cpu_id: 0,
    flags: 9984,
    pc: 0x500,
    registers: registers([
      ['D0', 0], ['D1', 0], ['D2', 0], ['D3', 0], ['D4', 0], ['D5', 0], ['D6', 0], ['D7', 0],
      ['A0', 12582916], ['A1', 12582912], ['A2', 0], ['A3', 0], ['A4', 0], ['A5', 0], ['A6', 0],
      ['A7', 4294967292], ['PC', 1280], ['SR', 9984],
    ]),
  },
  {
    cpu_id: 1,
    flags: 0,
    pc: 0x1a,
    registers: registers([
      ['A', 0], ['F', 0], ['B', 0], ['C', 0], ['D', 0], ['E', 0], ['H', 0], ['L', 0],
      ['PC', 26], ['SP', 65535], ['IX', 0], ['IY', 0],
    ]),
  },
]

/** A page element as the overlay uses one. */
function element() {
  return { innerHTML: '', textContent: '', className: '', addEventListener() {}, querySelector: () => element() }
}

describe("the performance overlay's program counters", () => {
  it('read the CPU states as the binding records them', () => {
    expect(recordedIn('cores/sg1/wasm/src/tests/debugger_shape_tests.rs')).toContain(
      describeShape(states),
    )
  })

  it("are each CPU's pc, found by its id, 0 for a CPU the states lack", () => {
    expect(processorPcs(states)).toEqual([0x500, 0x1a])
    expect(processorPcs([...states].reverse())).toEqual([0x500, 0x1a])
    expect(processorPcs([states[0]])).toEqual([0x500, 0])
    expect(processorPcs(undefined)).toEqual([0, 0])
  })

  describe('on the page', () => {
    let elements: Record<string, ReturnType<typeof element>>
    let overlay: ReturnType<typeof element>

    beforeEach(() => {
      elements = { 'perf-pc-0': element(), 'perf-pc-1': element() }
      overlay = element()
      vi.stubGlobal('document', {
        createElement: () => overlay,
        body: { appendChild() {} },
        getElementById: (id: string) => elements[id] ?? null,
      })
      vi.stubGlobal('localStorage', { getItem: () => null, setItem() {}, removeItem() {} })
    })

    afterEach(() => {
      vi.unstubAllGlobals()
    })

    it("are named after the core's CPUs, and say when they were read", () => {
      new PerformanceOverlay(null, ['65816', 'SPC700'])
      expect(overlay.innerHTML).toContain('65816 PC:')
      expect(overlay.innerHTML).toContain('SPC700 PC:')
      expect(overlay.innerHTML).toContain('a running game does not update it')
    })

    it('show the states the emulator gives, in six and four digits', () => {
      const perf = new PerformanceOverlay({ getCpuStates: () => states })
      perf.refreshProcessors()
      expect(elements['perf-pc-0'].textContent).toBe('000500')
      expect(elements['perf-pc-1'].textContent).toBe('001A')
    })
  })
})
