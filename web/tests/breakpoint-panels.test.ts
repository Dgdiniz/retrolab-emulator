import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// The panels save their breakpoints to IndexedDB, which Node lacks.
vi.mock('../src/panels/breakpoint-persistence', () => ({
  loadBreakpoints: vi.fn(async () => 0),
  saveBreakpoints: vi.fn(async () => {}),
}))

import { initHistoryPanel } from '../src/panels/history-panel'
import { initBreakpointsPanel } from '../src/panels/breakpoints-panel'
import { initWatchpointsPanel } from '../src/panels/watchpoints-panel'
import type { DebugBridge } from '../src/debug-bridge'

const ALL = { execute: true, memory: true, conditions: true }
const NONE = { execute: false, memory: false, conditions: false }

/** A page element as the panels use one: a value, a disabled flag, its listeners. */
interface FakeElement {
  value: string
  checked: boolean
  disabled: boolean
  innerHTML: string
  textContent: string
  style: { display: string }
  dataset: Record<string, string>
  options: Array<{ value: string; disabled: boolean }>
  listeners: Record<string, () => void>
  addEventListener(type: string, fn: () => void): void
}

function element(over: Partial<FakeElement> = {}): FakeElement {
  return {
    value: '',
    checked: false,
    disabled: false,
    innerHTML: '',
    textContent: '',
    style: { display: '' },
    dataset: {},
    options: [],
    listeners: {},
    addEventListener(type, fn) {
      this.listeners[type] = fn
    },
    ...over,
  }
}

/** The elements of the page the three panels look up, by id. */
function page(): Record<string, FakeElement> {
  const elements: Record<string, FakeElement> = {
    'history-list': element(),
    'breakpoints-list': element(),
    'bp-address': element(),
    'bp-type': element({
      value: 'execute',
      options: ['execute', 'read', 'write', 'readwrite'].map((value) => ({ value, disabled: false })),
    }),
    'add-bp-btn': element(),
    'watchpoints-list': element(),
    'wp-address': element(),
    'wp-read': element({ checked: true }),
    'wp-write': element({ checked: true }),
    'wp-value': element(),
    'wp-value-size': element({ value: '1' }),
    'add-wp-btn': element(),
    'wp-add-error': element(),
    'wp-event-add': element(),
    'wp-event-select': element(),
    'wp-event-param': element(),
    'add-wp-event-btn': element(),
    'wp-event-error': element(),
  }
  vi.stubGlobal('document', { getElementById: (id: string) => elements[id] ?? null })
  return elements
}

/** A Genesis-like core: the 68000 checks everything, the Z80 nothing. */
function emulator(checks: Array<typeof ALL> | null = [ALL, NONE]) {
  const emu: Record<string, any> = {
    breakpoints: [] as unknown[],
    listBreakpoints() {
      return this.breakpoints
    },
    addBreakpoint: vi.fn(() => 1),
    setBreakpointMatchSpec: vi.fn(),
    getAvailableEvents: () => [{ code: 1, kind: 'vertical_blank', label: 'VBlank' }],
    getSystemInfo: () => ({ cpus: [{ name: 'Motorola 68000' }, { name: 'Zilog Z80' }] }),
    getInstructionHistory: vi.fn(() => []),
  }
  if (checks) emu.getBreakpointChecks = vi.fn((cpu: number) => checks[cpu] ?? NONE)
  return emu
}

function bridge(emu: unknown, focusedCpu: number): DebugBridge {
  return {
    getEmulator: () => emu,
    getFocusedCpu: () => focusedCpu,
    refreshPanel: vi.fn(),
  } as unknown as DebugBridge
}

function execBp(id: number, address: number, cpu: number) {
  return { id, enabled: true, hit_count: 0, breakpoint: { address, bp_type: { Access: { exec: true, read: false, write: false } }, cpu_id: cpu } }
}

function writeBp(id: number, address: number, cpu: number) {
  return { id, enabled: true, hit_count: 0, breakpoint: { address, bp_type: { Access: { exec: false, read: false, write: true } }, cpu_id: cpu } }
}

beforeEach(() => {
  vi.stubGlobal('window', {})
  vi.stubGlobal('alert', vi.fn())
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('history panel', () => {
  it('shows the history in the order the core returns it, newest first', () => {
    const elements = page()
    const emu = emulator()
    emu.getInstructionHistory = vi.fn(() => [
      { address: 0x3000, instruction: 'newest', cycle_count: 4 },
      { address: 0x2000, instruction: 'older', cycle_count: 4 },
      { address: 0x1000, instruction: 'oldest', cycle_count: 4 },
    ])
    // The Z80's history keeps the text it was captured with.
    initHistoryPanel(bridge(emu, 1)).update()
    const html = elements['history-list'].innerHTML
    expect(html.indexOf('newest')).toBeLessThan(html.indexOf('older'))
    expect(html.indexOf('older')).toBeLessThan(html.indexOf('oldest'))
  })
})

describe('breakpoints panel', () => {
  it('offers everything on a CPU that checks everything, and says nothing', () => {
    const elements = page()
    initBreakpointsPanel(bridge(emulator(), 0)).update()
    expect(elements['bp-address'].disabled).toBe(false)
    expect(elements['add-bp-btn'].disabled).toBe(false)
    expect(elements['bp-type'].options.every((o) => !o.disabled)).toBe(true)
    expect(elements['breakpoints-list'].innerHTML).not.toContain('bp-checks-note')
  })

  it('offers nothing on a CPU that checks nothing, and says why', () => {
    const elements = page()
    initBreakpointsPanel(bridge(emulator(), 1)).update()
    expect(elements['bp-address'].disabled).toBe(true)
    expect(elements['bp-type'].disabled).toBe(true)
    expect(elements['add-bp-btn'].disabled).toBe(true)
    expect(elements['breakpoints-list'].innerHTML).toContain('The emulator checks no breakpoints on the Zilog Z80.')
  })

  it('offers only the kinds a CPU checks', () => {
    const elements = page()
    elements['bp-type'].value = 'write'
    const emu = emulator([{ execute: true, memory: false, conditions: true }])
    initBreakpointsPanel(bridge(emu, 0)).update()
    expect(elements['bp-type'].options.map((o) => o.disabled)).toEqual([false, true, true, true])
    expect(elements['bp-type'].value).toBe('execute')
    expect(elements['add-bp-btn'].disabled).toBe(false)
    expect(elements['breakpoints-list'].innerHTML).toContain(
      'The emulator does not check read and write watchpoints on the Motorola 68000.',
    )
  })

  it('adds the breakpoint on the focused CPU, and refuses a kind it does not check', () => {
    const elements = page()
    const emu = emulator([ALL, ALL])
    const panel = initBreakpointsPanel(bridge(emu, 1))
    panel.update()
    elements['bp-address'].value = '38'
    elements['add-bp-btn'].listeners.click()
    expect(emu.addBreakpoint).toHaveBeenCalledWith(0x38, 'execute', 1)

    const refusing = emulator()
    const offList = page()
    initBreakpointsPanel(bridge(refusing, 1))
    offList['bp-address'].value = '38'
    offList['add-bp-btn'].listeners.click()
    expect(refusing.addBreakpoint).not.toHaveBeenCalled()
  })

  it('tags a breakpoint its CPU does not check, and lets its unset flags stay unset', () => {
    const elements = page()
    const emu = emulator()
    emu.breakpoints = [execBp(1, 0x100, 0), execBp(2, 0x38, 1)]
    initBreakpointsPanel(bridge(emu, 0)).update()
    const [main, sound] = elements['breakpoints-list'].innerHTML.split('data-bp-id=').slice(1)
    expect(main).not.toContain('not checked')
    expect(sound).toContain('title="The emulator checks no breakpoints on the Zilog Z80."')
    expect(sound).toContain('not checked')
    // E is set and can be cleared; R and W cannot be set on the Z80.
    expect(sound.match(/<input type="checkbox" checked onchange/g)).toHaveLength(1)
    expect(sound.match(/<input type="checkbox"  disabled onchange/g)).toHaveLength(2)
  })

  it('offers everything when the core cannot say what a CPU checks', () => {
    const elements = page()
    initBreakpointsPanel(bridge(emulator(null), 1)).update()
    expect(elements['add-bp-btn'].disabled).toBe(false)
    expect(elements['breakpoints-list'].innerHTML).not.toContain('bp-checks-note')
  })
})

describe('watchpoints panel', () => {
  it('offers memory watchpoints on a CPU that checks them', () => {
    const elements = page()
    initWatchpointsPanel(bridge(emulator(), 0)).update()
    expect(elements['wp-address'].disabled).toBe(false)
    expect(elements['add-wp-btn'].disabled).toBe(false)
    expect(elements['watchpoints-list'].innerHTML).not.toContain('wp-checks-note')
  })

  it('offers none on a CPU that checks none, says why, and keeps the events', () => {
    const elements = page()
    const emu = emulator()
    initWatchpointsPanel(bridge(emu, 1)).update()
    for (const id of ['wp-address', 'wp-read', 'wp-write', 'wp-value', 'wp-value-size', 'add-wp-btn']) {
      expect(elements[id].disabled).toBe(true)
    }
    expect(elements['watchpoints-list'].innerHTML).toContain('The emulator checks no breakpoints on the Zilog Z80.')
    // An event breakpoint watches the console, not a CPU.
    expect(elements['wp-event-add'].style.display).toBe('')
    expect(elements['add-wp-event-btn'].disabled).toBe(false)

    elements['wp-address'].value = '1800'
    elements['add-wp-btn'].listeners.click()
    expect(emu.addBreakpoint).not.toHaveBeenCalled()
  })

  it('tags a memory watchpoint its CPU does not check, never an event', () => {
    const elements = page()
    const emu = emulator()
    emu.breakpoints = [
      writeBp(1, 0xff0000, 0),
      writeBp(2, 0x1800, 1),
      { id: 3, enabled: true, hit_count: 0, breakpoint: { address: 0, bp_type: { Event: { code: 1, param: 0xffffffff } }, cpu_id: 0 } },
    ]
    initWatchpointsPanel(bridge(emu, 0)).update()
    const [main, sound, event] = elements['watchpoints-list'].innerHTML.split('data-wp-id=').slice(1)
    expect(main).not.toContain('not checked')
    expect(sound).toContain('not checked')
    expect(sound).toContain('title="The emulator checks no breakpoints on the Zilog Z80."')
    // W is set and can be cleared; R cannot be set on the Z80.
    expect(sound).toMatch(/<input type="checkbox"  disabled onchange="toggleWatchpointAccess\(2, 'read'\)"/)
    expect(event).not.toContain('not checked')
  })

  describe('an event breakpoint', () => {
    const NOTE = 'An event breakpoint checks no value or condition: with either set, it never fires.'

    function eventBp(matchSpec?: unknown) {
      return {
        id: 3,
        enabled: true,
        hit_count: 0,
        breakpoint: { address: 0, bp_type: { Event: { code: 1, param: 0xffffffff } }, cpu_id: 0 },
        match_spec: matchSpec,
      }
    }

    it('offers no value test and no condition, and keeps hit-skip, the action and one-shot', () => {
      const elements = page()
      const emu = emulator()
      emu.breakpoints = [eventBp()]
      initWatchpointsPanel(bridge(emu, 0)).update()
      ;(window as any).wpEditToggle(3)
      const html = elements['watchpoints-list'].innerHTML
      for (const offered of ['Skip hits', 'Action', 'One-shot']) expect(html).toContain(`<span>${offered}</span>`)
      for (const absent of ['Range end', 'Compare', 'Value', 'Size', 'Mask', 'Condition']) {
        expect(html).not.toContain(`<span>${absent}</span>`)
      }
      expect(html).not.toContain('not checked')
      expect(html).not.toContain('wp-edit-note')
    })

    it('shows a saved value test and condition locked, says why, and keeps them on Apply', () => {
      const elements = page()
      const emu = emulator()
      emu.breakpoints = [eventBp({ value: 5, expr: 'd0 == 5', hit_skip: 2 })]
      initWatchpointsPanel(bridge(emu, 0)).update()
      expect(elements['watchpoints-list'].innerHTML).toContain(`<span class="wp-unchecked" title="${NOTE}"`)
      ;(window as any).wpEditToggle(3)
      const html = elements['watchpoints-list'].innerHTML
      expect(html).toMatch(/<span>Value<\/span><input class="wp-edit-input" value="5" placeholder="\(any\)" disabled title="An event/)
      expect(html).toContain('<span>Condition</span><input class="wp-edit-input wp-edit-expr" value="d0 == 5"')
      expect(html).toContain(`title="${NOTE}" disabled oninput="wpEditField('expr', this.value)"`)
      expect(html).not.toContain('<span>Range end</span>')
      expect(html).toContain(`<div class="wp-edit-note" style="color: #d7ba7d; font-size: 11px; padding: 2px 0;">${NOTE}</div>`)

      ;(window as any).wpEditField('hitSkip', '4')
      ;(window as any).wpEditApply(3)
      expect(emu.setBreakpointMatchSpec).toHaveBeenCalledWith(
        3,
        expect.objectContaining({ value: 5, expr: 'd0 == 5', hit_skip: 4 }),
      )
    })
  })
})

