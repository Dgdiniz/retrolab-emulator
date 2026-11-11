/**
 * Vanilla DOM hex editor. Mirrors the contract of RetroLab's hex editor,
 * consuming the same `@debugger-core/hex-editor-core`
 * helpers (`totalRowCount`, `computeVisibleRange`, `buildVisibleRows`,
 * `parseHexEdit`, `decideEditKey`, `computeBytesPerRow`,
 * `createModifiedColorizer`, `computeWindowAnchor`,
 * `windowCoversVisibleRange`) plus `@debugger-core/edit-history-core`
 * for the internal undo/redo stack. Both shells render the same
 * numbers and obey the same editing rules.
 *
 * Core surface:
 *   - `dataSource` (windowed) lets multi-MiB regions like the cartridge
 *     ROM stay performant: small regions (RAM/Z80/VRAM) load in one shot,
 *     larger ones keep only `windowSize` bytes in memory and re-anchor
 *     as the user scrolls or jumps with the address bar.
 *   - The cursor is independent of edit state, so clicks on the ASCII
 *     column select a byte without entering edit mode while the hex
 *     column keeps the existing click-to-edit behaviour. Both columns
 *     highlight the same offset.
 *   - When editing, the original byte is shown faintly behind the
 *     typed digits (toggleable via `showOldByteOnEdit`).
 *
 * The host registers an `onByteChange` callback that performs the
 * actual write (e.g. `getEmulator().writeMemoryDebug(addr, byte)`);
 * the editor itself never assumes a particular emulator surface.
 */

import {
  DEFAULT_BYTES_PER_ROW_CANDIDATES,
  ROW_HEIGHT,
  buildVisibleRows,
  clampCursor,
  clampWindowSize,
  computeBytesPerRow,
  computeVisibleRange,
  computeWindowAnchor,
  createModifiedColorizer,
  decideEditKey,
  parseHexEdit,
  totalRowCount,
  windowCoversVisibleRange,
  type Colorizer,
  type HexEditorDataSource,
  type HexRow,
} from '@debugger-core/hex-editor-core'
import { createEditHistory, type EditHistory } from '@debugger-core/edit-history-core'

export interface HexEditorOptions {
  /**
   * Lazy chunk reader. The editor renders a virtual scroll spanning
   * `dataSource.totalSize` bytes but only keeps `windowSize` (default
   * 1 MiB, clamped to `totalSize`) bytes in memory, re-anchoring on
   * scroll-edge or `setCursor`.
   */
  dataSource: HexEditorDataSource
  /** Address shown next to row 0; subsequent rows add `offset` to this. */
  baseAddress: number
  /**
   * Bytes to keep loaded at once. Defaults to 1 MiB. Smaller than the
   * region length means the editor re-reads when the user scrolls or
   * jumps near a window edge; larger is clamped down to `totalSize`.
   */
  windowSize?: number
  /**
   * Fixed column count used when auto-fit is off (or as the initial
   * fallback before measurement). Pass a number to lock columns, or
   * `'auto'` (equivalent to `autoFit: true`) to let the editor pick.
   */
  bytesPerRow?: number | 'auto'
  /**
   * When `true` (default) the editor measures its container and picks
   * the largest entry from `bytesPerRowCandidates` whose row fits
   * horizontally. Falls back to `bytesPerRow` for the very first paint.
   * `bytesPerRow: 'auto'` forces this on regardless.
   */
  autoFit?: boolean
  /** Allowed column counts for auto-fit. Defaults to `[8, 16, 24, 32, 48, 64]`. */
  bytesPerRowCandidates?: readonly number[]
  /** When `true`, all editing inputs are ignored (clicks still move the cursor). */
  readonly?: boolean
  /** Optional per-byte colour override (CSS colour or null). */
  colorizer?: Colorizer | null
  /**
   * Optional set of region-relative offsets to paint as "modified".
   * The editor layers an internal modified-colorizer on top of
   * `colorizer`. The host may mutate the set between paints; the
   * editor reads it fresh on every `paint()` call.
   */
  modifiedOffsets?: ReadonlySet<number> | null
  /**
   * When `true` (default), the editor maintains its own EditHistory
   * and binds Ctrl+Z / Ctrl+Y. Each undo / redo fires `onByteChange`
   * just like a regular edit so the host writes the previous value.
   */
  enableUndo?: boolean
  /**
   * When `true` (default), the original byte digits are rendered
   * faintly behind the typed digits while editing, so the user can
   * still see what they're overwriting.
   */
  showOldByteOnEdit?: boolean
  /**
   * Called when the user commits an edit (Tab, Arrow, full byte typed).
   * Host is responsible for writing the new byte to the underlying
   * buffer; the editor doesn't mutate `data` directly. `offset` is in
   * region coordinates (compatible with the host's base address).
   */
  onByteChange?(offset: number, value: number, prev: number): void
  /** Called on every cursor move (click + arrow + external `setCursor`). */
  onCursorChange?(offset: number): void
}

export interface HexEditorHandle {
  /** Re-read the current window and repaint. Safe to call on every panel tick. */
  refresh(): void
  /** Move the cursor and scroll it into view, re-anchoring the window if needed. */
  setCursor(offset: number): void
  /**
   * Read the current scroll position (CSS pixels from top). The memory
   * panel uses this together with `setScrollTop` to keep each region
   * tab on the line the user left it on.
   */
  getScrollTop(): number
  /** Restore a previously captured scroll position. */
  setScrollTop(value: number): void
  /** Undo the most recent committed edit if `enableUndo` is true. */
  undo(): void
  /** Redo the most recently undone edit if `enableUndo` is true. */
  redo(): void
  canUndo(): boolean
  canRedo(): boolean
  /** Drop the undo / redo stacks (e.g. after a region change). */
  clearHistory(): void
  /** Override the column count at runtime (mirrors the constructor option). */
  setBytesPerRow(value: number | 'auto'): void
  /** Override "show old byte while editing" at runtime. */
  setShowOldByteOnEdit(value: boolean): void
  /** Force the auto-fit math to re-run (e.g. after a tab becomes visible). */
  resize(): void
  dispose(): void
}

const DEFAULT_BYTES_PER_ROW = 16
const DEFAULT_WINDOW_SIZE = 0x100000 // 1 MiB
const REANCHOR_MARGIN_ROWS = 16

function escapeAscii(s: string): string {
  return s.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
}

interface RenderState {
  cursorOffset: number | null
  editingOffset: number | null
  editBuffer: string
  modified: ReadonlySet<number> | null
  showOldByteOnEdit: boolean
  readonly: boolean
}

function renderEditingLabel(oldHex: string, buffer: string): string {
  // Both digits start faint; typed digits replace them one by one and
  // render bold so the user always sees the previous byte underneath
  // the overwrite. Buffer is at most 2 chars (see decideEditKey).
  //
  // The slot the next keystroke will land in gets the `caret` class so
  // the blinking-underline CSS animation pins itself to the right
  // digit. Once both slots are typed the byte commits on the next
  // keystroke, so the caret naturally drops off (`buffer.length === 2`
  // matches no slot).
  const caretSlot = buffer.length
  const slotClass = (slot: 0 | 1, base: 'faint' | 'typed'): string =>
    slot === caretSlot ? `${base} caret` : base
  const d0 = buffer[0]
    ? `<span class="${slotClass(0, 'typed')}">${buffer[0]}</span>`
    : `<span class="${slotClass(0, 'faint')}">${oldHex[0]}</span>`
  const d1 = buffer[1]
    ? `<span class="${slotClass(1, 'typed')}">${buffer[1]}</span>`
    : `<span class="${slotClass(1, 'faint')}">${oldHex[1]}</span>`
  return d0 + d1
}

function renderRow(row: HexRow, state: RenderState): string {
  const bytesHtml = row.bytes
    .map((cell) => {
      const isEditing = state.editingOffset === cell.offset
      const isSelected = state.cursorOffset === cell.offset
      const isModified = state.modified?.has(cell.offset)
      const classes = ['hex-byte']
      if (isEditing) classes.push('editing')
      if (isSelected && !isEditing) classes.push('selected')
      if (isModified) classes.push('modified')
      const style = cell.color ? ` style="color:${cell.color}"` : ''
      let label: string
      if (isEditing) {
        // Always render through `renderEditingLabel` so the caret
        // animation lands on the right slot. When the user opted
        // out of "show old byte", pass `__` so faint slots render
        // as an underscore placeholder while keeping the
        // span/caret structure intact.
        const oldHex = state.showOldByteOnEdit ? cell.hex : '__'
        label = renderEditingLabel(oldHex, state.editBuffer)
      } else {
        label = cell.hex
      }
      return `<span class="${classes.join(' ')}" data-area="hex" data-offset="${cell.offset}"${style}>${label}</span>`
    })
    // Cells are flush against each other in the DOM; the visible gap
    // between bytes is `.hex-byte`'s internal padding. A literal space
    // here would create a dead hit-target the user keeps missing
    // through.
    .join('')

  const asciiHtml = row.asciiCells
    .map((c) => {
      const isSelected = state.cursorOffset === c.offset
      const isModified = state.modified?.has(c.offset)
      const classes = ['hex-char']
      if (isSelected) classes.push('selected')
      if (isModified) classes.push('modified')
      return `<span class="${classes.join(' ')}" data-area="ascii" data-offset="${c.offset}">${escapeAscii(c.char)}</span>`
    })
    .join('')

  return `
    <div class="hex-row" style="height:${ROW_HEIGHT}px;line-height:${ROW_HEIGHT}px">
      <span class="hex-address">${row.address}:</span>
      <span class="hex-bytes">${bytesHtml}</span>
      <span class="hex-ascii">${asciiHtml}</span>
    </div>
  `
}

export function createHexEditor(root: HTMLElement, opts: HexEditorOptions): HexEditorHandle {
  const candidates = opts.bytesPerRowCandidates ?? DEFAULT_BYTES_PER_ROW_CANDIDATES
  const enableUndo = opts.enableUndo ?? true
  const initialBpr =
    opts.bytesPerRow === 'auto' || opts.bytesPerRow == null
      ? DEFAULT_BYTES_PER_ROW
      : Math.max(1, opts.bytesPerRow | 0)
  let autoFit = opts.bytesPerRow === 'auto' ? true : (opts.autoFit ?? true)
  let initialColumns = initialBpr
  let bytesPerRow = initialColumns
  let charWidth = 8
  let showOldByteOnEdit = opts.showOldByteOnEdit ?? true

  const totalSize = Math.max(0, opts.dataSource.totalSize | 0)
  const requestedWindow = opts.windowSize ?? DEFAULT_WINDOW_SIZE
  const windowSize = clampWindowSize(requestedWindow, totalSize)

  let windowBase = 0
  let data: Uint8Array = totalSize === 0 ? new Uint8Array(0) : opts.dataSource.read(0, windowSize)

  let cursorOffset: number | null = null
  let editingOffset: number | null = null
  let editBuffer = ''

  const history: EditHistory | null = enableUndo ? createEditHistory() : null

  root.classList.add('hex-editor')
  root.style.position = 'relative'
  root.style.overflow = 'auto'
  root.tabIndex = 0
  root.style.outline = 'none'

  const spacer = document.createElement('div')
  spacer.style.position = 'relative'
  spacer.style.width = '100%'

  const viewport = document.createElement('div')
  viewport.style.position = 'absolute'
  viewport.style.left = '0'
  viewport.style.right = '0'
  viewport.style.top = '0'

  spacer.appendChild(viewport)
  root.appendChild(spacer)

  function measureCharWidth(): void {
    // Reuse a hidden span styled like a byte cell to get the rendered
    // advance of a single monospace character. Refreshed on resize so
    // DPR / zoom changes don't throw the auto-fit math off.
    const probe = document.createElement('span')
    probe.className = 'hex-byte hex-char-probe'
    probe.textContent = '0'
    probe.style.position = 'absolute'
    probe.style.visibility = 'hidden'
    probe.style.pointerEvents = 'none'
    probe.style.whiteSpace = 'pre'
    root.appendChild(probe)
    const w = probe.getBoundingClientRect().width
    root.removeChild(probe)
    if (Number.isFinite(w) && w > 0) charWidth = w
  }

  function recomputeColumns(): void {
    if (!autoFit) {
      bytesPerRow = initialColumns
      return
    }
    const widthPx = root.clientWidth
    if (widthPx <= 0 || charWidth <= 0) {
      bytesPerRow = initialColumns
      return
    }
    bytesPerRow = computeBytesPerRow(widthPx, charWidth, { candidates })
  }

  function effectiveColorizer(): Colorizer | null {
    const modified = opts.modifiedOffsets ?? null
    if (!modified || modified.size === 0) return opts.colorizer ?? null
    return createModifiedColorizer(modified, { fallbackColorizer: opts.colorizer ?? null })
  }

  /**
   * Ensure the loaded window covers `[regionOffset, regionOffset]` (or a
   * range derived from the visible viewport). Re-reads from the data
   * source if the offset falls outside the current window with the
   * configured edge margin.
   */
  function ensureWindowCoversOffset(regionOffset: number): void {
    if (totalSize === 0 || windowSize === 0) return
    const target = Math.max(0, Math.min(totalSize - 1, regionOffset | 0))
    const margin = REANCHOR_MARGIN_ROWS * bytesPerRow
    if (
      windowCoversVisibleRange(
        { base: windowBase, length: data.length },
        target,
        target,
        { edgeMarginBytes: margin, totalSize },
      )
    ) {
      return
    }
    const newBase = computeWindowAnchor(target, { windowSize, totalSize, bytesPerRow })
    windowBase = newBase
    data = opts.dataSource.read(newBase, windowSize)
  }

  function ensureWindowCoversVisible(): void {
    if (totalSize === 0 || windowSize === 0) return
    const range = computeVisibleRange(
      root.scrollTop,
      root.clientHeight,
      ROW_HEIGHT,
      totalRowCount(totalSize, bytesPerRow),
    )
    const startByte = range.start * bytesPerRow
    const endByte = Math.min(totalSize, range.end * bytesPerRow)
    const margin = REANCHOR_MARGIN_ROWS * bytesPerRow
    if (
      windowCoversVisibleRange(
        { base: windowBase, length: data.length },
        startByte,
        endByte,
        { edgeMarginBytes: margin, totalSize },
      )
    ) {
      return
    }
    const centre = Math.floor((startByte + endByte) / 2)
    const newBase = computeWindowAnchor(centre, { windowSize, totalSize, bytesPerRow })
    windowBase = newBase
    data = opts.dataSource.read(newBase, windowSize)
  }

  function paint(): void {
    ensureWindowCoversVisible()
    const totalRows = totalRowCount(totalSize, bytesPerRow)
    spacer.style.height = `${totalRows * ROW_HEIGHT}px`
    const range = computeVisibleRange(root.scrollTop, root.clientHeight, ROW_HEIGHT, totalRows)
    const rows = buildVisibleRows(
      data,
      range,
      bytesPerRow,
      opts.baseAddress,
      effectiveColorizer(),
      windowBase,
    )
    viewport.style.transform = `translateY(${range.start * ROW_HEIGHT}px)`
    const state: RenderState = {
      cursorOffset,
      editingOffset,
      editBuffer,
      modified: opts.modifiedOffsets ?? null,
      showOldByteOnEdit,
      readonly: !!opts.readonly,
    }
    viewport.innerHTML = rows.map((row) => renderRow(row, state)).join('')
  }

  /** Byte at a region-relative offset; returns 0 when outside the window. */
  function byteAt(regionOffset: number): number {
    const i = regionOffset - windowBase
    if (i < 0 || i >= data.length) return 0
    return data[i] ?? 0
  }

  function commitEdit(): void {
    if (editingOffset === null) return
    const val = parseHexEdit(editBuffer)
    if (val === null) return
    const offset = editingOffset
    const prev = byteAt(offset)
    if (history) history.push(offset, prev, val)
    opts.onByteChange?.(offset, val, prev)
  }

  function applyUndoRedoEntry(offset: number, value: number, isUndo: boolean): void {
    ensureWindowCoversOffset(offset)
    const prev = byteAt(offset)
    // Host writes the byte; we deliberately don't push back onto the
    // history (undo/redo already consumed the entry).
    opts.onByteChange?.(offset, value, prev)
    cursorOffset = offset
    editingOffset = offset
    editBuffer = ''
    opts.onCursorChange?.(offset)
    paint()
    // `isUndo` is informational for future hooks (e.g. status-bar
    // "Undo: byte $XXXX restored"); kept on the signature so callers
    // can react without us changing the contract later.
    void isUndo
  }

  function onScroll(): void {
    paint()
  }

  function moveCursorTo(offset: number, opts2: { enterEdit: boolean }): void {
    if (totalSize === 0) return
    const clamped = clampCursor(offset, 0, totalSize)
    ensureWindowCoversOffset(clamped)
    cursorOffset = clamped
    if (opts2.enterEdit && !opts.readonly) {
      editingOffset = clamped
      editBuffer = ''
    } else if (!opts2.enterEdit) {
      // ASCII clicks deliberately leave any in-flight edit alone;
      // they only move the cursor for inspection.
      if (editingOffset !== null && editingOffset !== clamped) {
        editingOffset = null
        editBuffer = ''
      }
    }
    opts.onCursorChange?.(clamped)
    paint()
  }

  function onClick(e: MouseEvent): void {
    // Walk up to the nearest element carrying a `data-area` attribute.
    // This makes clicks land correctly even when the user hits the
    // cell's padding OR an inner span like the faint old-byte preview
    // / typed digit / caret marker rendered during edit mode, a strict
    // `e.target.dataset.area` check would silently drop those.
    const target = e.target as HTMLElement | null
    if (!target) return
    const hit = target.closest<HTMLElement>('[data-area="hex"], [data-area="ascii"]')
    if (!hit) return
    const area = hit.dataset.area
    if (area !== 'hex' && area !== 'ascii') return
    const attr = hit.getAttribute('data-offset')
    if (attr === null) return
    const offset = Number.parseInt(attr, 10)
    if (!Number.isFinite(offset)) return
    moveCursorTo(offset, { enterEdit: area === 'hex' })
  }

  function onKeyDown(e: KeyboardEvent): void {
    if (enableUndo && (e.ctrlKey || e.metaKey)) {
      const key = e.key.toLowerCase()
      if (key === 'z' && !e.shiftKey) {
        e.preventDefault()
        undo()
        return
      }
      if (key === 'y' || (key === 'z' && e.shiftKey)) {
        e.preventDefault()
        redo()
        return
      }
    }
    if (opts.readonly) return
    if (editingOffset === null) return
    const action = decideEditKey(e.key, editBuffer)
    switch (action.kind) {
      case 'cancel':
        editingOffset = null
        editBuffer = ''
        paint()
        return
      case 'commit-and-move':
        e.preventDefault()
        commitEdit()
        editingOffset = clampCursor(editingOffset, action.delta, totalSize)
        cursorOffset = editingOffset
        editBuffer = ''
        ensureWindowCoversOffset(editingOffset)
        opts.onCursorChange?.(editingOffset)
        paint()
        return
      case 'append':
        editBuffer += action.char
        paint()
        return
      case 'commit-and-advance':
        editBuffer += e.key.toLowerCase()
        commitEdit()
        editingOffset = clampCursor(editingOffset, 1, totalSize)
        cursorOffset = editingOffset
        editBuffer = ''
        ensureWindowCoversOffset(editingOffset)
        opts.onCursorChange?.(editingOffset)
        paint()
        return
      case 'ignore':
    }
  }

  function undo(): void {
    if (!history) return
    const entry = history.undo()
    if (!entry) return
    applyUndoRedoEntry(entry.offset, entry.oldValue, true)
  }

  function redo(): void {
    if (!history) return
    const entry = history.redo()
    if (!entry) return
    applyUndoRedoEntry(entry.offset, entry.newValue, false)
  }

  root.addEventListener('scroll', onScroll, { passive: true })
  root.addEventListener('click', onClick)
  root.addEventListener('keydown', onKeyDown)

  // ResizeObserver keeps `bytesPerRow` in sync with the container.
  // Falls back gracefully in environments that lack it (older test
  // runners), where `resize()` can be called manually.
  let ro: ResizeObserver | null = null
  if (typeof ResizeObserver !== 'undefined') {
    ro = new ResizeObserver(() => {
      measureCharWidth()
      recomputeColumns()
      paint()
    })
    ro.observe(root)
  }

  measureCharWidth()
  recomputeColumns()
  paint()

  return {
    refresh() {
      // Re-read the current window in place so edits applied via the
      // host (`writeMemoryDebug` then host calls refresh) propagate
      // back without resetting `windowBase`.
      if (totalSize > 0 && windowSize > 0) {
        data = opts.dataSource.read(windowBase, windowSize)
      }
      paint()
    },
    setCursor(offset) {
      if (totalSize === 0) return
      const clamped = clampCursor(offset, 0, totalSize)
      ensureWindowCoversOffset(clamped)
      cursorOffset = clamped
      editingOffset = null
      editBuffer = ''
      // Scroll the row containing the cursor into view, anchoring
      // roughly mid-viewport for the "goto address" UX.
      const rowIndex = Math.floor(clamped / Math.max(1, bytesPerRow))
      const targetTop = Math.max(
        0,
        rowIndex * ROW_HEIGHT - Math.floor(root.clientHeight / 2),
      )
      root.scrollTop = targetTop
      opts.onCursorChange?.(clamped)
      paint()
    },
    getScrollTop() {
      return root.scrollTop
    },
    setScrollTop(value) {
      // Assigning to scrollTop fires the native `scroll` event so
      // `onScroll` re-runs `ensureWindowCoversVisible` + `paint()`
      // naturally; no manual re-paint needed here.
      root.scrollTop = Math.max(0, value | 0)
    },
    undo,
    redo,
    canUndo: () => history?.canUndo() ?? false,
    canRedo: () => history?.canRedo() ?? false,
    clearHistory: () => history?.clear(),
    setBytesPerRow(value) {
      if (value === 'auto') {
        autoFit = true
      } else {
        autoFit = false
        initialColumns = Math.max(1, value | 0)
      }
      measureCharWidth()
      recomputeColumns()
      paint()
    },
    setShowOldByteOnEdit(value) {
      showOldByteOnEdit = !!value
      paint()
    },
    resize() {
      measureCharWidth()
      recomputeColumns()
      paint()
    },
    dispose() {
      root.removeEventListener('scroll', onScroll)
      root.removeEventListener('click', onClick)
      root.removeEventListener('keydown', onKeyDown)
      ro?.disconnect()
      root.innerHTML = ''
      root.classList.remove('hex-editor')
    },
  }
}
