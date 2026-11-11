/**
 * Watchpoints panel, value/range memory watchpoints with advanced match
 * conditions, plus system-event breakpoints (on cores that advertise an
 * event catalog via `getAvailableEvents`). Mirrors RetroLab's watchpoints panel.
 *
 * Watchpoints and event breakpoints are entries in the same WASM
 * breakpoint list that the breakpoints panel owns; persistence is shared
 * through `breakpoint-persistence.ts`, and every mutation here refreshes
 * the breakpoints panel + disassembly gutter so the two stay in sync.
 *
 * Like the breakpoints panel, the row list is regenerated as `innerHTML`,
 * so the per-row controls call `window.*` globals (toggle/remove/access)
 * and the inline conditions editor keeps its draft in this closure so a
 * mid-edit re-render (e.g. after a step) doesn't lose typed values.
 */

import {
  accessToShorthand,
  parseAccessFlags,
  toggleFlag,
  type AccessFlags,
  type RawBpType,
} from '@debugger-core/breakpoints-core'
import {
  BREAKPOINT_ACTION_OPTIONS,
  DEFAULT_MATCH_SPEC,
  VALUE_COMPARE_OPTIONS,
  VALUE_SIZE_OPTIONS,
  buildWatchpointRows,
  clampValueSize,
  isDefaultMatchSpec,
  matchSpecToWire,
  parseEventAddForm,
  parseEventCatalog,
  parseEventTag,
  parseMatchSpec,
  parseMatchSpecEditor,
  parseWatchAddForm,
  specToDraft,
  type AppliedWatchInput,
  type EventClass,
  type MatchSpecDraft,
  type WatchpointRow,
} from '@debugger-core/watchpoints-core'
import type { DebugBridge, PanelHandle } from '../debug-bridge'
import { saveBreakpoints } from './breakpoint-persistence'

/** Escape a string for safe interpolation into an HTML attribute. */
function attr(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;')
}

export function initWatchpointsPanel(bridge: DebugBridge): PanelHandle {
  const list = document.getElementById('watchpoints-list')
  const addrInput = document.getElementById('wp-address') as HTMLInputElement | null
  const readInput = document.getElementById('wp-read') as HTMLInputElement | null
  const writeInput = document.getElementById('wp-write') as HTMLInputElement | null
  const valueInput = document.getElementById('wp-value') as HTMLInputElement | null
  const sizeSelect = document.getElementById('wp-value-size') as HTMLSelectElement | null
  const addBtn = document.getElementById('add-wp-btn')
  const addError = document.getElementById('wp-add-error')

  const eventAdd = document.getElementById('wp-event-add')
  const eventSelect = document.getElementById('wp-event-select') as HTMLSelectElement | null
  const eventParam = document.getElementById('wp-event-param') as HTMLInputElement | null
  const addEventBtn = document.getElementById('add-wp-event-btn')
  const eventError = document.getElementById('wp-event-error')

  if (!list || !addrInput || !readInput || !writeInput || !valueInput || !addBtn) {
    throw new Error('watchpoints panel DOM elements missing (#watchpoints-list / #wp-* / #add-wp-btn)')
  }

  // Cached event catalog (refreshed each update) for labels + the add form.
  let catalog: EventClass[] = []
  // Inline-editor state, persisted across re-renders.
  let editingId: number | null = null
  let draft: MatchSpecDraft = specToDraft(DEFAULT_MATCH_SPEC)
  let editErrors: string[] = []

  function refreshCatalog(): void {
    const emulator = bridge.getEmulator()
    if (!emulator || typeof emulator.getAvailableEvents !== 'function') {
      catalog = []
      return
    }
    try {
      catalog = parseEventCatalog(emulator.getAvailableEvents())
    } catch {
      catalog = []
    }
  }

  /** Project the WASM list into the neutral applied-row inputs. */
  function appliedRows(emulator: any): AppliedWatchInput[] {
    const breakpoints: any[] = emulator.listBreakpoints() ?? []
    return breakpoints.map((bp: any) => {
      const rawType = bp.breakpoint.bp_type
      const event = parseEventTag(rawType)
      return {
        wasmId: bp.id,
        address: bp.breakpoint.address,
        access: parseAccess(rawType),
        cpuId: bp.breakpoint.cpu_id ?? 0,
        enabled: bp.enabled,
        hitCount: bp.hit_count ?? 0,
        matchSpec: parseMatchSpec(bp.match_spec),
        event,
      }
    })
  }

  function renderEditor(row: WatchpointRow): string {
    const compareOpts = VALUE_COMPARE_OPTIONS.map(
      (o) => `<option value="${o.value}"${o.value === draft.valueCompare ? ' selected' : ''}>${o.label}</option>`,
    ).join('')
    const actionOpts = BREAKPOINT_ACTION_OPTIONS.map(
      (o) => `<option value="${o.value}"${o.value === draft.action ? ' selected' : ''}>${o.label}</option>`,
    ).join('')
    const sizeOpts = VALUE_SIZE_OPTIONS.map(
      (o) => `<option value="${o.value}"${o.value === draft.valueSize ? ' selected' : ''}>${o.label}</option>`,
    ).join('')
    const errs = editErrors.length
      ? `<div class="wp-edit-errors">${attr(editErrors.join('. '))}</div>`
      : ''
    return `
      <div class="watchpoint-editor" data-wp-edit="${row.wasmId}">
        <div class="wp-edit-grid">
          <label class="wp-edit-field"><span>Range end</span><input class="wp-edit-input" value="${attr(draft.rangeEnd)}" placeholder="(single)" oninput="wpEditField('rangeEnd', this.value)"/></label>
          <label class="wp-edit-field"><span>Compare</span><select oninput="wpEditField('valueCompare', this.value)">${compareOpts}</select></label>
          <label class="wp-edit-field"><span>Value</span><input class="wp-edit-input" value="${attr(draft.value)}" placeholder="(any)" oninput="wpEditField('value', this.value)"/></label>
          <label class="wp-edit-field"><span>Size</span><select oninput="wpEditField('valueSize', this.value)">${sizeOpts}</select></label>
          <label class="wp-edit-field"><span>Mask</span><input class="wp-edit-input" value="${attr(draft.valueMask)}" placeholder="(all bits)" oninput="wpEditField('valueMask', this.value)"/></label>
          <label class="wp-edit-field"><span>Skip hits</span><input class="wp-edit-input" type="number" min="0" value="${draft.hitSkip}" oninput="wpEditField('hitSkip', this.value)"/></label>
          <label class="wp-edit-field"><span>Action</span><select oninput="wpEditField('action', this.value)">${actionOpts}</select></label>
          <label class="wp-edit-field wp-edit-check"><input type="checkbox"${draft.oneShot ? ' checked' : ''} onchange="wpEditField('oneShot', this.checked)"/><span>One-shot</span></label>
        </div>
        <label class="wp-edit-field wp-edit-field--full"><span>Condition</span><input class="wp-edit-input wp-edit-expr" value="${attr(draft.expr)}" placeholder="(none) e.g. d0 == 5 && [0xFF0000].w > 0x100" title="Optional expression over registers / memory. Fires only when non-zero. Operators: && || ! == != < <= > >= + - * / % &amp; | ^ ~ << >>. Memory: [addr] [addr].w [addr].l" oninput="wpEditField('expr', this.value)"/></label>
        ${errs}
        <div class="wp-edit-actions">
          <button class="btn-apply" onclick="wpEditApply(${row.wasmId})">Apply</button>
          <button class="btn-cancel" onclick="wpEditCancel()">Cancel</button>
        </div>
      </div>
    `
  }

  function renderRow(row: WatchpointRow): string {
    const enabledClass = row.enabled ? '' : ' disabled'
    const icon = row.enabled ? '●' : '○'
    const color = row.enabled ? '#e51400' : '#555'
    const title = row.enabled ? 'Click to disable' : 'Click to enable'
    const kindClass = row.kind === 'event' ? 'wp-kind--event' : 'wp-kind--memory'
    const kindLabel = row.kind === 'event' ? 'EVT' : 'MEM'
    const access =
      row.kind === 'memory'
        ? `<span class="watchpoint-access">` +
          `<label class="bp-flag" title="Break on read"><input type="checkbox" ${row.access.read ? 'checked' : ''} onchange="toggleWatchpointAccess(${row.wasmId}, 'read')"/>R</label>` +
          `<label class="bp-flag" title="Break on write"><input type="checkbox" ${row.access.write ? 'checked' : ''} onchange="toggleWatchpointAccess(${row.wasmId}, 'write')"/>W</label>` +
          `</span>`
        : ''
    const summary = row.summary
      ? `<span class="wp-summary" title="${attr(row.summary)}">${attr(row.summary)}</span>`
      : ''
    const editor = editingId === row.wasmId ? renderEditor(row) : ''
    return `
      <div class="watchpoint-item${enabledClass}" data-wp-id="${row.wasmId}">
        <div class="watchpoint-info">
          <span class="wp-toggle" onclick="toggleWatchpointEnabled(${row.wasmId}, ${row.enabled})" title="${title}" style="color: ${color}; cursor: pointer; font-size: 14px; min-width: 16px;">${icon}</span>
          <span class="wp-kind ${kindClass}">${kindLabel}</span>
          <span class="watchpoint-label">${attr(row.label)}</span>
          ${access}
          ${summary}
          <span style="color: #6a9955; font-size: 11px;">Hits: ${row.hitCount}</span>
        </div>
        <div class="watchpoint-actions">
          <button class="btn-edit" onclick="wpEditToggle(${row.wasmId})" title="Edit conditions">&#9881;</button>
          <button class="btn-remove" onclick="removeWatchpoint(${row.wasmId})">&#10005;</button>
        </div>
      </div>
      ${editor}
    `
  }

  function update(): void {
    const emulator = bridge.getEmulator()
    if (!emulator) return
    try {
      refreshCatalog()

      // Toggle the event add-form visibility on cores with a catalog.
      if (eventAdd) eventAdd.style.display = catalog.length ? '' : 'none'
      if (eventSelect && catalog.length) {
        const want =
          `<option value="" disabled selected>Add event\u2026</option>` +
          catalog.map((e) => `<option value="${e.code}">${attr(e.label)}</option>`).join('')
        if (eventSelect.dataset.built !== String(catalog.length)) {
          eventSelect.innerHTML = want
          eventSelect.dataset.built = String(catalog.length)
        }
      }

      const rows = buildWatchpointRows(appliedRows(emulator), [], catalog)
      if (rows.length === 0) {
        list!.innerHTML = '<div style="color: #858585;">No watchpoints</div>'
        return
      }
      list!.innerHTML = rows.map(renderRow).join('')
    } catch (err) {
      console.error('Failed to update watchpoints:', err)
    }
  }

  function addWatchpoint(): void {
    const r = parseWatchAddForm({
      address: addrInput!.value,
      read: readInput!.checked,
      write: writeInput!.checked,
      value: valueInput!.value,
      valueSize: sizeSelect ? clampValueSize(parseInt(sizeSelect.value, 10)) : 1,
    })
    if (!r.ok) {
      if (addError) addError.textContent = r.errors.join('. ')
      return
    }
    const emulator = bridge.getEmulator()
    if (!emulator) return
    try {
      const id = emulator.addBreakpoint(r.address, accessToShorthand(r.access), bridge.getFocusedCpu())
      if (!isDefaultMatchSpec(r.spec)) emulator.setBreakpointMatchSpec(id, matchSpecToWire(r.spec))
      addrInput!.value = ''
      valueInput!.value = ''
      if (addError) addError.textContent = ''
      afterMutate()
    } catch (err) {
      console.error('Failed to add watchpoint:', err)
    }
  }

  function addEvent(): void {
    const r = parseEventAddForm({ code: parseSelectCode(eventSelect), param: eventParam?.value ?? '' }, catalog)
    if (!r.ok) {
      if (eventError) eventError.textContent = r.errors.join('. ')
      return
    }
    const emulator = bridge.getEmulator()
    if (!emulator || typeof emulator.addEventBreakpoint !== 'function') return
    try {
      emulator.addEventBreakpoint(r.code, r.param)
      if (eventParam) eventParam.value = ''
      if (eventError) eventError.textContent = ''
      afterMutate()
    } catch (err) {
      console.error('Failed to add event breakpoint:', err)
    }
  }

  /** Re-render this panel + the breakpoints/disassembly views and persist. */
  function afterMutate(): void {
    update()
    bridge.refreshPanel('breakpoints')
    bridge.refreshPanel('disassembly')
    void saveBreakpoints(bridge)
  }

  addBtn.addEventListener('click', addWatchpoint)
  addrInput.addEventListener('keydown', (e) => {
    if ((e as KeyboardEvent).key === 'Enter') addWatchpoint()
  })
  valueInput.addEventListener('keydown', (e) => {
    if ((e as KeyboardEvent).key === 'Enter') addWatchpoint()
  })
  addEventBtn?.addEventListener('click', addEvent)
  eventParam?.addEventListener('keydown', (e) => {
    if ((e as KeyboardEvent).key === 'Enter') addEvent()
  })

  // -- Inline onclick globals (mirror the breakpoints panel) ----------
  const w = window as any

  w.toggleWatchpointEnabled = (id: number, currentlyEnabled: boolean) => {
    const emulator = bridge.getEmulator()
    if (!emulator) return
    try {
      emulator.enableBreakpoint(id, !currentlyEnabled)
      afterMutate()
    } catch (err) {
      console.error('Failed to toggle watchpoint:', err)
    }
  }

  w.removeWatchpoint = (id: number) => {
    const emulator = bridge.getEmulator()
    if (!emulator) return
    try {
      emulator.removeBreakpoint(id)
      if (editingId === id) editingId = null
      afterMutate()
    } catch (err) {
      console.error('Failed to remove watchpoint:', err)
    }
  }

  w.toggleWatchpointAccess = (id: number, which: keyof AccessFlags) => {
    const emulator = bridge.getEmulator()
    if (!emulator) return
    try {
      const bps: any[] = emulator.listBreakpoints() ?? []
      const bp = bps.find((b: any) => b.id === id)
      if (!bp) return
      const flags = toggleFlag(parseAccess(bp.breakpoint.bp_type), which)
      // An access-less watchpoint never fires, remove it instead.
      if (!flags.read && !flags.write && !flags.exec) {
        emulator.removeBreakpoint(id)
      } else {
        emulator.setBreakpointAccess(id, flags.exec, flags.read, flags.write)
      }
      afterMutate()
    } catch (err) {
      console.error('Failed to toggle watchpoint access:', err)
    }
  }

  w.wpEditToggle = (id: number) => {
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

  w.wpEditCancel = () => {
    editingId = null
    editErrors = []
    update()
  }

  w.wpEditField = (field: keyof MatchSpecDraft, value: string | boolean) => {
    if (field === 'hitSkip') {
      const n = parseInt(String(value), 10)
      draft.hitSkip = Number.isFinite(n) ? n : 0
    } else if (field === 'valueSize') {
      draft.valueSize = clampValueSize(parseInt(String(value), 10))
    } else if (field === 'oneShot') {
      draft.oneShot = value === true
    } else {
      // Remaining fields are string-typed (rangeEnd/value/valueMask/
      // valueCompare/action/expr).
      ;(draft as any)[field] = value
    }
  }

  w.wpEditApply = (id: number) => {
    const emulator = bridge.getEmulator()
    if (!emulator) return
    const bps: any[] = emulator.listBreakpoints() ?? []
    const bp = bps.find((b: any) => b.id === id)
    const base = bp && parseEventTag(bp.breakpoint.bp_type) === null ? bp.breakpoint.address : undefined
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
      afterMutate()
    } catch (err) {
      console.error('Failed to apply watchpoint conditions:', err)
    }
  }

  return {
    update,
    dispose() {
      delete w.toggleWatchpointEnabled
      delete w.removeWatchpoint
      delete w.toggleWatchpointAccess
      delete w.wpEditToggle
      delete w.wpEditCancel
      delete w.wpEditField
      delete w.wpEditApply
    },
  }
}

/** Parse the focused select's numeric `code`, or `null` when unset. */
function parseSelectCode(select: HTMLSelectElement | null): number | null {
  if (!select || select.value === '') return null
  const n = parseInt(select.value, 10)
  return Number.isFinite(n) ? n : null
}

/** `parseAccessFlags` over a raw `bp_type` value (loose WASM JSON). */
function parseAccess(rawType: unknown): AccessFlags {
  return parseAccessFlags(rawType as RawBpType)
}
