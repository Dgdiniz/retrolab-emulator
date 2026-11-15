import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// The real store writes to IndexedDB, which Node does not have. The mock
// records what the gutter path hands it.
vi.mock('../src/panels/breakpoint-persistence', () => ({
  saveBreakpoints: vi.fn(() => Promise.resolve()),
}))

import { saveBreakpoints } from '../src/panels/breakpoint-persistence'
import { initDisassemblyPanel, toggleExecBreakpoint } from '../src/panels/disassembly-panel'
import type { DebugBridge } from '../src/debug-bridge'

interface Checks {
  execute: boolean
  memory: boolean
  conditions: boolean
}

const ALL: Checks = { execute: true, memory: true, conditions: true }
const NONE: Checks = { execute: false, memory: false, conditions: false }

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
// and the CPU `addBreakpoint` was given, 0 when it was given none. With
// `checks`, the core says what each CPU checks, as both cores do; without,
// it is a core that cannot say, and everything is checked.
function harness(initial: Seed[] = [], checks: Checks[] | null = null) {
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
    ...(checks
      ? {
          getBreakpointChecks: vi.fn((cpu: number) => checks[cpu] ?? NONE),
          getSystemInfo: () => ({ cpus: [{ name: 'Motorola 68000' }, { name: 'Zilog Z80' }] }),
        }
      : {}),
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
    expect(toggleExecBreakpoint(h.bridge, 0x000200, 0)).toBeNull()
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

// Both cores say what each CPU checks; the second CPUs check nothing.
describe('toggleExecBreakpoint on a CPU that checks no execute breakpoints', () => {
  beforeEach(() => {
    vi.mocked(saveBreakpoints).mockReset()
  })

  it('adds nothing, saves nothing and says why', () => {
    const h = harness([], [ALL, NONE])
    expect(toggleExecBreakpoint(h.bridge, 0x0100, 1)).toBe(
      'The emulator checks no breakpoints on the Zilog Z80.',
    )
    expect(h.emulator.getBreakpointChecks).toHaveBeenCalledWith(1)
    expect(h.emulator.addBreakpoint).not.toHaveBeenCalled()
    expect(h.refreshPanel).not.toHaveBeenCalled()
    expect(saveBreakpoints).not.toHaveBeenCalled()
  })

  it('does not set the exec bit of a watchpoint there', () => {
    const h = harness([{ address: 0x0100, cpu: 1, flags: { exec: false, read: false, write: true } }], [ALL, NONE])
    expect(toggleExecBreakpoint(h.bridge, 0x0100, 1)).toBe(
      'The emulator checks no breakpoints on the Zilog Z80.',
    )
    expect(h.emulator.setBreakpointAccess).not.toHaveBeenCalled()
    expect(saveBreakpoints).not.toHaveBeenCalled()
  })

  it('still takes away an execute breakpoint set there before', () => {
    const h = harness([{ address: 0x0100, cpu: 1 }], [ALL, NONE])
    expect(toggleExecBreakpoint(h.bridge, 0x0100, 1)).toBeNull()
    expect(h.emulator.removeBreakpoint).toHaveBeenCalledWith(1)
    expect(saveBreakpoints).toHaveBeenCalledTimes(1)
  })

  it('adds as before on the CPU that checks everything', () => {
    const h = harness([], [ALL, NONE])
    expect(toggleExecBreakpoint(h.bridge, 0x000200, 0)).toBeNull()
    expect(h.emulator.addBreakpoint).toHaveBeenCalledWith(0x000200, 'execute', 0)
  })

  it('says "this CPU" when the core names no CPU', () => {
    const h = harness([], [ALL, NONE])
    const emulator = { ...h.emulator, getSystemInfo: undefined }
    const bridge = { getEmulator: () => emulator, refreshPanel: vi.fn() } as unknown as DebugBridge
    expect(toggleExecBreakpoint(bridge, 0x0100, 1)).toBe('The emulator checks no breakpoints on this CPU.')
  })
})

/** The disassembly view, holding the HTML the panel last drew. */
function disassemblyPage() {
  const view = { innerHTML: '', querySelector: () => null, querySelectorAll: () => [] }
  const goto = { value: '', addEventListener: vi.fn() }
  const ids: Record<string, unknown> = { 'disassembly-view': view, 'goto-address': goto }
  vi.stubGlobal('document', { getElementById: (id: string) => ids[id] ?? null })
  return view
}

/** One code row and one never-executed row on each CPU, and the breakpoints given. */
function panelEmulator(breakpoints: unknown[] = []) {
  return {
    getCpuStates: () => [{ pc: 0x000200 }, { pc: 0x0100 }],
    disassembleWindow: (_cpu: number, pc: number) => [
      { address: pc, kind: 'code', bytes: [0x4e, 0x71], mnemonic: 'nop', operands: '' },
      { address: pc + 2, kind: 'unknown', bytes: [], mnemonic: '', operands: '' },
    ],
    listBreakpoints: () => breakpoints,
    getBreakpointChecks: (cpu: number) => (cpu === 0 ? ALL : NONE),
    getSystemInfo: () => ({ cpus: [{ name: 'Motorola 68000' }, { name: 'Zilog Z80' }] }),
  }
}

function drawn(emulator: unknown, focusedCpu: number): string {
  const view = disassemblyPage()
  const bridge = {
    getEmulator: () => emulator,
    getFocusedCpu: () => focusedCpu,
    refreshPanel: vi.fn(),
  } as unknown as DebugBridge
  initDisassemblyPanel(bridge).update()
  return view.innerHTML
}

describe('the disassembly gutter, as drawn', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it("titles each code row of a CPU that checks no execute breakpoints with the reason", () => {
    const html = drawn(panelEmulator(), 1)
    expect(html).toContain('class="disasm-line current no-exec" data-address="0100" title="The emulator checks no breakpoints on the Zilog Z80."')
    // A never-executed row takes no click, so it says nothing.
    expect(html).toMatch(/class="disasm-line unknown" data-address="0102">/)
  })

  it('says nothing on a CPU that checks execute breakpoints', () => {
    const html = drawn(panelEmulator(), 0)
    expect(html).not.toContain('no-exec')
    expect(html).not.toContain('title=')
  })

  it("marks only the focused CPU's address breakpoints, and lets a click take one away", () => {
    const list = [
      { id: 1, enabled: true, breakpoint: { address: 0x0100, bp_type: { Access: EXEC }, cpu_id: 0 } },
      { id: 2, enabled: true, breakpoint: { address: 0x0000, bp_type: { Event: { code: 1, param: 0 } }, cpu_id: null } },
    ]
    // The 68000's breakpoint at $0100 is not the Z80's.
    expect(drawn(panelEmulator(list), 1)).not.toMatch(/class="disasm-line[^"]* breakpoint/)
    const onZ80 = [{ id: 3, enabled: true, breakpoint: { address: 0x0100, bp_type: { Access: EXEC }, cpu_id: 1 } }]
    const html = drawn(panelEmulator(onZ80), 1)
    // A breakpoint there can still be clicked away, so the row keeps its pointer.
    expect(html).toContain('class="disasm-line current breakpoint" data-address="0100" title=')
  })
})
