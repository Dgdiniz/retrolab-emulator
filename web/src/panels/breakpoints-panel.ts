/**
 * Breakpoints panel, add/list/enable/disable/remove breakpoints with
 * IndexedDB persistence keyed by ROM hash. Mirrors
 * RetroLab's breakpoints panel.
 *
 * Event breakpoints and value/range watchpoints share the same WASM
 * breakpoint list but render in the watchpoints panel; this panel hides
 * event breakpoints (they have no address to show here) and keeps the
 * plain execute/read/write rows. Persistence is shared with the
 * watchpoints panel via `breakpoint-persistence.ts` so the two panels
 * never clobber each other's entries on load.
 *
 * `window.toggleBreakpointEnabled` and `window.removeBreakpoint` are
 * exposed because the breakpoint list uses inline `onclick=` for the
 * toggle/remove buttons: the list's HTML is rebuilt on every update, and
 * inline handlers need no rebinding afterwards.
 */

import { hex, parseHexAddress } from '@debugger-core/format-core'
import {
  isEmptyAccess,
  parseAccessFlags,
  toggleFlag,
  type AccessFlags,
  type RawBpType,
} from '@debugger-core/breakpoints-core'
import {
  BREAKPOINT_ACTION_OPTIONS,
  DEFAULT_MATCH_SPEC,
  matchSpecSummary,
  matchSpecToWire,
  parseEventTag,
  parseMatchSpec,
  parseMatchSpecEditor,
  specToDraft,
  type MatchSpecDraft,
} from '@debugger-core/watchpoints-core'
import type { DebugBridge, PanelHandle } from '../debug-bridge'
import { loadBreakpoints, saveBreakpoints } from './breakpoint-persistence'

/** Escape a string for safe interpolation into an HTML attribute. */
function attr(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;')
}

export interface BreakpointsPanelHandle extends PanelHandle {
  /** Force a reload of breakpoints from IndexedDB for the currently-loaded ROM. Used after ROM load + force reset. */
  loadFromStorage(): Promise<void>
}

export function initBreakpointsPanel(bridge: DebugBridge): BreakpointsPanelHandle {
  const list = document.getElementById('breakpoints-list')
  const addrInput = document.getElementById('bp-address') as HTMLInputElement | null
  const typeInput = document.getElementById('bp-type') as HTMLSelectElement | null
  const addBtn = document.getElementById('add-bp-btn')

  if (!list || !addrInput || !typeInput || !addBtn) {
    throw new Error('breakpoints panel DOM elements missing (#breakpoints-list / #bp-* / #add-bp-btn)')
  }

  // Inline condition-editor state, persisted across re-renders (a step
  // re-renders the list; a mid-edit draft must survive).
  let editingId: number | null = null
  let draft: MatchSpecDraft = specToDraft(DEFAULT_MATCH_SPEC)
  let editErrors: string[] = []

  /**
   * Render the inline condition editor for one execute/access breakpoint.
   * Surfaces the headline subset useful for code breakpoints, a condition
   * expression plus hit-skip / action / one-shot. Range/value/mask carried
   * by the spec are preserved across an edit (the draft is seeded from the
   * full spec) even though they aren't shown here.
   */
  function renderEditor(id: number): string {
    const actionOpts = BREAKPOINT_ACTION_OPTIONS.map(
      (o) => `<option value="${o.value}"${o.value === draft.action ? ' selected' : ''}>${o.label}</option>`,
    ).join('')
    const errs = editErrors.length
      ? `<div class="wp-edit-errors">${attr(editErrors.join('. '))}</div>`
      : ''
    return `
      <div class="watchpoint-editor" data-bp-edit="${id}">
        <label class="wp-edit-field wp-edit-field--full"><span>Condition</span><input class="wp-edit-input wp-edit-expr" value="${attr(draft.expr)}" placeholder="(none) e.g. d0 == 5 && [0xFF0000].w > 0x100" title="Optional expression over registers / memory. The breakpoint fires only when it evaluates non-zero. Operators: && || ! == != < <= > >= + - * / % &amp; | ^ ~ << >>. Memory: [addr] [addr].w [addr].l" oninput="bpEditField('expr', this.value)"/></label>
        <div class="wp-edit-grid">
          <label class="wp-edit-field"><span>Skip hits</span><input class="wp-edit-input" type="number" min="0" value="${draft.hitSkip}" oninput="bpEditField('hitSkip', this.value)"/></label>
          <label class="wp-edit-field"><span>Action</span><select oninput="bpEditField('action', this.value)">${actionOpts}</select></label>
          <label class="wp-edit-field wp-edit-check"><input type="checkbox"${draft.oneShot ? ' checked' : ''} onchange="bpEditField('oneShot', this.checked)"/><span>One-shot</span></label>
        </div>
        ${errs}
        <div class="wp-edit-actions">
          <button class="btn-apply" onclick="bpEditApply(${id})">Apply</button>
          <button class="btn-cancel" onclick="bpEditCancel()">Cancel</button>
        </div>
      </div>
    `
  }

  addBtn.addEventListener('click', () => {
    const addr = parseHexAddress(addrInput.value)
    if (addr === null) {
      alert('Invalid address')
      return
    }
    const emulator = bridge.getEmulator()
    if (!emulator) return
    try {
      emulator.addBreakpoint(addr, typeInput.value)
      addrInput.value = ''
      update()
      bridge.refreshPanel('disassembly')
      bridge.refreshPanel('watchpoints')
      void saveBreakpoints(bridge)
    } catch (err) {
      console.error('Failed to add breakpoint:', err)
    }
  })

  function update(): void {
    const emulator = bridge.getEmulator()
    if (!emulator) return
    try {
      // Event breakpoints live in the watchpoints panel, they have no
      // address to render in this list.
      const breakpoints: any[] = (emulator.listBreakpoints() ?? []).filter(
        (bp: any) => parseEventTag(bp.breakpoint.bp_type as RawBpType) === null,
      )

      if (breakpoints.length === 0) {
        list!.innerHTML = '<div style="color: #858585;">No breakpoints</div>'
        return
      }

      list!.innerHTML = breakpoints.map((bp: any) => {
        const addrStr = hex(bp.breakpoint.address, 6)
        const enabledClass = bp.enabled ? '' : ' disabled'
        const enabledIcon = bp.enabled ? '●' : '○'
        const enabledTitle = bp.enabled ? 'Click to disable' : 'Click to enable'
        const enabledColor = bp.enabled ? '#e51400' : '#555'
        const flags = parseAccessFlags(bp.breakpoint.bp_type as RawBpType)
        const flag = (on: boolean, which: 'exec' | 'read' | 'write', letter: string, title: string) =>
          `<label class="bp-flag" title="${title}"><input type="checkbox" ${on ? 'checked' : ''} onchange="toggleBreakpointAccess(${bp.id}, '${which}')"/>${letter}</label>`
        const summaryText = matchSpecSummary(parseMatchSpec(bp.match_spec))
        const summary = summaryText
          ? `<span class="wp-summary" title="${attr(summaryText)}">${attr(summaryText)}</span>`
          : ''
        const editor = editingId === bp.id ? renderEditor(bp.id) : ''

        return `
          <div class="breakpoint-item${enabledClass}" data-bp-id="${bp.id}">
            <div class="breakpoint-info">
              <span class="bp-toggle" onclick="toggleBreakpointEnabled(${bp.id}, ${bp.enabled})" title="${enabledTitle}" style="color: ${enabledColor}; cursor: pointer; font-size: 14px; min-width: 16px;">${enabledIcon}</span>
              <span class="breakpoint-address">${addrStr}</span>
              <span class="breakpoint-access">${flag(flags.exec, 'exec', 'E', 'Break on execute')}${flag(flags.read, 'read', 'R', 'Break on read')}${flag(flags.write, 'write', 'W', 'Break on write')}</span>
              ${summary}
              <span style="color: #6a9955; font-size: 11px;">Hits: ${bp.hit_count}</span>
            </div>
            <div class="breakpoint-actions">
              <button class="btn-edit" onclick="bpEditToggle(${bp.id})" title="Edit condition">&#9881;</button>
              <button class="btn-remove" onclick="removeBreakpoint(${bp.id})">✕</button>
            </div>
          </div>
          ${editor}
        `
      }).join('')
    } catch (err) {
      console.error('Failed to update breakpoints:', err)
    }
  }

  async function loadFromStorage(): Promise<void> {
    const count = await loadBreakpoints(bridge)
    if (count > 0) console.log(`Loaded ${count} breakpoints for ROM`)
    update()
    bridge.refreshPanel('disassembly')
    bridge.refreshPanel('watchpoints')
  }

  // Inline onclicks in the breakpoint rows expect global helpers; we
  // attach them once and they delegate back into this panel's update +
  // persist flows.
  ;(window as any).toggleBreakpointEnabled = (id: number, currentlyEnabled: boolean) => {
    const emulator = bridge.getEmulator()
    if (!emulator) return
    try {
      emulator.enableBreakpoint(id, !currentlyEnabled)
      update()
      bridge.refreshPanel('disassembly')
      bridge.refreshPanel('watchpoints')
      void saveBreakpoints(bridge)
    } catch (err) {
      console.error('Failed to toggle breakpoint enabled:', err)
    }
  }

  ;(window as any).removeBreakpoint = (id: number) => {
    const emulator = bridge.getEmulator()
    if (!emulator) return
    try {
      emulator.removeBreakpoint(id)
      if (editingId === id) editingId = null
      update()
      bridge.refreshPanel('disassembly')
      bridge.refreshPanel('watchpoints')
      void saveBreakpoints(bridge)
    } catch (err) {
      console.error('Failed to remove breakpoint:', err)
    }
  }

  // -- Inline condition-editor globals --------------------------------
  ;(window as any).bpEditToggle = (id: number) => {
    if (editingId === id) {
      editingId = null
    } else {
      const emulator = bridge.getEmulator()
      const bps: any[] = emulator?.listBreakpoints() ?? []
      const bp = bps.find((b: any) => b.id === id)
      editingId = id
      editErrors = []
      draft = specToDraft(parseMatchSpec(bp?.match_spec))
    }
    update()
  }

  ;(window as any).bpEditCancel = () => {
    editingId = null
    editErrors = []
    update()
  }

  ;(window as any).bpEditField = (field: keyof MatchSpecDraft, value: string | boolean) => {
    if (field === 'hitSkip') {
      const n = parseInt(String(value), 10)
      draft.hitSkip = Number.isFinite(n) ? n : 0
    } else if (field === 'oneShot') {
      draft.oneShot = value === true
    } else {
      // Remaining edited fields are string-typed (action/expr).
      ;(draft as any)[field] = value
    }
  }

  ;(window as any).bpEditApply = (id: number) => {
    const emulator = bridge.getEmulator()
    if (!emulator) return
    const bps: any[] = emulator.listBreakpoints() ?? []
    const bp = bps.find((b: any) => b.id === id)
    const base = bp ? bp.breakpoint.address : undefined
    const r = parseMatchSpecEditor(draft, base)
    if (!r.ok) {
      editErrors = r.errors
      update()
      return
    }
    try {
      emulator.setBreakpointMatchSpec(id, matchSpecToWire(r.spec))
      editingId = null
      editErrors = []
      update()
      bridge.refreshPanel('disassembly')
      bridge.refreshPanel('watchpoints')
      void saveBreakpoints(bridge)
    } catch (err) {
      console.error('Failed to apply breakpoint condition:', err)
    }
  }

  // Flip one E/R/W bit on a breakpoint. Clearing the last bit removes
  // the breakpoint (an access-less breakpoint never fires).
  ;(window as any).toggleBreakpointAccess = (id: number, which: keyof AccessFlags) => {
    const emulator = bridge.getEmulator()
    if (!emulator) return
    try {
      const list: any[] = emulator.listBreakpoints() ?? []
      const bp = list.find((b: any) => b.id === id)
      if (!bp) return
      const flags = toggleFlag(parseAccessFlags(bp.breakpoint.bp_type as RawBpType), which)
      if (isEmptyAccess(flags)) {
        emulator.removeBreakpoint(id)
      } else {
        emulator.setBreakpointAccess(id, flags.exec, flags.read, flags.write)
      }
      update()
      bridge.refreshPanel('disassembly')
      bridge.refreshPanel('watchpoints')
      void saveBreakpoints(bridge)
    } catch (err) {
      console.error('Failed to toggle breakpoint access:', err)
    }
  }

  return {
    update,
    dispose() {
      delete (window as any).toggleBreakpointEnabled
      delete (window as any).removeBreakpoint
      delete (window as any).toggleBreakpointAccess
      delete (window as any).bpEditToggle
      delete (window as any).bpEditCancel
      delete (window as any).bpEditField
      delete (window as any).bpEditApply
    },
    loadFromStorage,
  }
}
