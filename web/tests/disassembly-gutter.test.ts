import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// The real store writes to IndexedDB, which Node does not have. The mock
// records what the gutter path hands it.
vi.mock('../src/panels/breakpoint-persistence', () => ({
  saveBreakpoints: vi.fn(() => Promise.resolve()),
}))

import { saveBreakpoints } from '../src/panels/breakpoint-persistence'
import { breakpointsOnCpu, toggleExecBreakpoint } from '../src/panels/disassembly-panel'
import type { DebugBridge } from '../src/debug-bridge'

interface Flags {
  exec: boolean
  read: boolean
  write: boolean
}

type BpType = { Access: Flags } | { Event: { code: number; param: number } }

interface FakeBreakpoint {
  id: number
  breakpoint: { address: number; bp_type: BpType; cpu_id: number | null }
  enabled: boolean
}

interface Seed {
  address: number
  cpu?: number | null
  flags?: Flags
  event?: { code: number; param: number }
}

const EXEC: Flags = { exec: true, read: false, write: false }

// A WASM breakpoint list in memory, with the four calls the gutter toggle
// makes. It stores what both cores store: `bp_type` in the tagged form,
// and the CPU `addBreakpoint` was given, 0 when it was given none.
function harness(initial: Seed[] = []) {
  let nextId = 1
  const list: FakeBreakpoint[] = initial.map((seed) => ({
    id: nextId++,
    breakpoint: {
      address: seed.address,
      bp_type: seed.event ? { Event: seed.event } : { Access: seed.flags ?? EXEC },
      cpu_id: seed.cpu === undefined ? 0 : seed.cpu,
    },
    enabled: true,
  }))
  const snapshot = () => list.map((bp) => ({
    id: bp.id,
    address: bp.breakpoint.address,
    cpu: bp.breakpoint.cpu_id,
    type: structuredClone(bp.breakpoint.bp_type),
  }))
  const emulator = {
    listBreakpoints: vi.fn(() => list.map((bp) => structuredClone(bp))),
    addBreakpoint: vi.fn((address: number, type: string, cpu?: number) => {
      expect(type).toBe('execute')
      const id = nextId++
      list.push({
        id,
        breakpoint: { address, bp_type: { Access: { ...EXEC } }, cpu_id: cpu ?? 0 },
        enabled: true,
      })
      return id
    }),
    removeBreakpoint: vi.fn((id: number) => {
      const i = list.findIndex((bp) => bp.id === id)
      if (i >= 0) list.splice(i, 1)
    }),
    setBreakpointAccess: vi.fn((id: number, exec: boolean, read: boolean, write: boolean) => {
      const bp = list.find((b) => b.id === id)
      if (bp) bp.breakpoint.bp_type = { Access: { exec, read, write } }
    }),
  }
  const refreshPanel = vi.fn()
  const bridge = {
    getEmulator: () => emulator,
    refreshPanel,
  } as unknown as DebugBridge

  // What the store would write: the list as it stands when it is called.
  const saved: Array<ReturnType<typeof snapshot>> = []
  vi.mocked(saveBreakpoints).mockImplementation(async (b) => {
    expect(b).toBe(bridge)
    saved.push(snapshot())
  })

  return { emulator, bridge, refreshPanel, saved, snapshot }
}

describe('toggleExecBreakpoint (disassembly gutter click)', () => {
  beforeEach(() => {
    vi.mocked(saveBreakpoints).mockReset()
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('adds an execute breakpoint at an empty address and saves the list with it', () => {
    const h = harness()
    toggleExecBreakpoint(h.bridge, 0x000200, 0)
    expect(h.emulator.addBreakpoint).toHaveBeenCalledWith(0x000200, 'execute', 0)
    expect(saveBreakpoints).toHaveBeenCalledTimes(1)
    expect(h.saved).toEqual([[{ id: 1, address: 0x000200, cpu: 0, type: { Access: EXEC } }]])
  })

  it('removes an execute breakpoint toggled off and saves the list without it', () => {
    const h = harness([{ address: 0x000200 }, { address: 0x000300 }])
    toggleExecBreakpoint(h.bridge, 0x000200, 0)
    expect(h.emulator.removeBreakpoint).toHaveBeenCalledWith(1)
    expect(saveBreakpoints).toHaveBeenCalledTimes(1)
    expect(h.saved).toEqual([[{ id: 2, address: 0x000300, cpu: 0, type: { Access: EXEC } }]])
  })

  it('adds the exec bit to a read/write watchpoint, keeping its flags, and saves it', () => {
    const h = harness([{ address: 0xff0000, flags: { exec: false, read: true, write: true } }])
    toggleExecBreakpoint(h.bridge, 0xff0000, 0)
    expect(h.emulator.setBreakpointAccess).toHaveBeenCalledWith(1, true, true, true)
    expect(h.emulator.addBreakpoint).not.toHaveBeenCalled()
    expect(h.saved).toEqual([
      [{ id: 1, address: 0xff0000, cpu: 0, type: { Access: { exec: true, read: true, write: true } } }],
    ])
  })

  it('takes the exec bit off a watchpoint that has one, keeping it as a watchpoint', () => {
    const h = harness([{ address: 0xff0000, flags: { exec: true, read: false, write: true } }])
    toggleExecBreakpoint(h.bridge, 0xff0000, 0)
    expect(h.emulator.setBreakpointAccess).toHaveBeenCalledWith(1, false, false, true)
    expect(h.emulator.removeBreakpoint).not.toHaveBeenCalled()
    expect(h.saved).toEqual([
      [{ id: 1, address: 0xff0000, cpu: 0, type: { Access: { exec: false, read: false, write: true } } }],
    ])
  })

  it('redraws the breakpoints list, the watchpoints list and the gutter', () => {
    const h = harness()
    toggleExecBreakpoint(h.bridge, 0x000200, 0)
    const ids = h.refreshPanel.mock.calls.map((call) => call[0]).sort()
    expect(ids).toEqual(['breakpoints', 'disassembly', 'watchpoints'])
  })

  it('saves nothing when no emulator is loaded', () => {
    const bridge = { getEmulator: () => null, refreshPanel: vi.fn() } as unknown as DebugBridge
    toggleExecBreakpoint(bridge, 0x000200, 0)
    expect(saveBreakpoints).not.toHaveBeenCalled()
  })

  it('saves nothing when the emulator refuses the change', () => {
    const h = harness()
    h.emulator.addBreakpoint.mockImplementation(() => {
      throw new Error('refused')
    })
    const error = vi.spyOn(console, 'error').mockImplementation(() => {})
    toggleExecBreakpoint(h.bridge, 0x000200, 0)
    expect(saveBreakpoints).not.toHaveBeenCalled()
    expect(h.refreshPanel).not.toHaveBeenCalled()
    expect(error).toHaveBeenCalled()
  })
})

// The second CPU of each core shares numeric addresses with the first:
// the Z80's $0100 is not the 68000's $000100, and the SPC700's $0400 is
// not the 65816's $000400.
describe('toggleExecBreakpoint on the second CPU', () => {
  beforeEach(() => {
    vi.mocked(saveBreakpoints).mockReset()
  })

  const secondCpus: Array<[string, number]> = [
    ['Z80 (sg1)', 0x0100],
    ['SPC700 (snes1)', 0x0400],
  ]

  for (const [label, address] of secondCpus) {
    it(`${label}: a click adds the breakpoint on CPU 1 and leaves the primary CPU's alone`, () => {
      const h = harness([{ address, cpu: 0 }])
      toggleExecBreakpoint(h.bridge, address, 1)
      expect(h.emulator.addBreakpoint).toHaveBeenCalledWith(address, 'execute', 1)
      expect(h.emulator.removeBreakpoint).not.toHaveBeenCalled()
      expect(h.emulator.setBreakpointAccess).not.toHaveBeenCalled()
      expect(h.snapshot()).toEqual([
        { id: 1, address, cpu: 0, type: { Access: EXEC } },
        { id: 2, address, cpu: 1, type: { Access: EXEC } },
      ])
      expect(saveBreakpoints).toHaveBeenCalledTimes(1)
    })

    it(`${label}: a second click removes the CPU 1 breakpoint only`, () => {
      const h = harness([{ address, cpu: 0 }, { address, cpu: 1 }])
      toggleExecBreakpoint(h.bridge, address, 1)
      expect(h.emulator.removeBreakpoint).toHaveBeenCalledWith(2)
      expect(h.snapshot()).toEqual([{ id: 1, address, cpu: 0, type: { Access: EXEC } }])
      expect(saveBreakpoints).toHaveBeenCalledTimes(1)
    })

    it(`${label}: a click on the primary CPU leaves the CPU 1 breakpoint alone`, () => {
      const h = harness([{ address, cpu: 1 }])
      toggleExecBreakpoint(h.bridge, address, 0)
      expect(h.emulator.addBreakpoint).toHaveBeenCalledWith(address, 'execute', 0)
      expect(h.emulator.removeBreakpoint).not.toHaveBeenCalled()
      expect(h.snapshot().map((bp) => bp.cpu)).toEqual([1, 0])
    })
  }

  it('never takes an event breakpoint for an execute breakpoint at address 0', () => {
    const vblank = { code: 1, param: 0xffffffff }
    const h = harness([{ address: 0, cpu: null, event: vblank }])
    toggleExecBreakpoint(h.bridge, 0x0000, 1)
    expect(h.emulator.setBreakpointAccess).not.toHaveBeenCalled()
    expect(h.emulator.removeBreakpoint).not.toHaveBeenCalled()
    expect(h.emulator.addBreakpoint).toHaveBeenCalledWith(0x0000, 'execute', 1)
    expect(h.snapshot()[0]).toEqual({ id: 1, address: 0, cpu: null, type: { Event: vblank } })
  })
})

describe('breakpointsOnCpu (what the gutter marks)', () => {
  const list = [
    { id: 1, enabled: true, breakpoint: { address: 0x0100, bp_type: { Access: EXEC }, cpu_id: 0 } },
    { id: 2, enabled: true, breakpoint: { address: 0x0100, bp_type: { Access: EXEC }, cpu_id: 1 } },
    { id: 3, enabled: true, breakpoint: { address: 0x0000, bp_type: { Event: { code: 1, param: 0 } }, cpu_id: null } },
    { id: 4, enabled: false, breakpoint: { address: 0x0200, bp_type: { Access: EXEC }, cpu_id: null } },
  ]

  it('keeps the address breakpoints of the CPU asked for', () => {
    expect(breakpointsOnCpu(list, 1).map((bp) => bp.id)).toEqual([2])
  })

  it('counts a breakpoint listed with no CPU as the primary CPU one, and leaves events out', () => {
    expect(breakpointsOnCpu(list, 0).map((bp) => bp.id)).toEqual([1, 4])
  })
})
