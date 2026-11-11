/**
 * Pure virtual-scroll + row-building helpers for the hex editor.
 *
 * Kept out of the editor component so the math can be unit-tested in Node
 * without mounting it. The component remains responsible for wiring
 * ResizeObserver, scroll events, keyboard focus, and the events it emits.
 */

import { hex } from './format-core'

export const ROW_HEIGHT = 20
export const VISIBLE_PADDING = 4

/**
 * Sensible default ladder for `computeBytesPerRow`. Multiples of 8 so
 * grouping every 4 / 8 bytes stays aligned, capped at 64 because past
 * that point the ASCII column becomes hard to scan.
 */
export const DEFAULT_BYTES_PER_ROW_CANDIDATES: readonly number[] = [8, 16, 24, 32, 48, 64]

export interface HexByteCell {
  offset: number
  value: number
  hex: string
  color: string | null
}

/**
 * Per-character cell for the ASCII column. Carries the same `offset`
 * as the matching `HexByteCell` so the shell can hang a `data-offset`
 * attribute on each `<span>`, enabling per-char click handling and
 * symmetric highlighting across the hex and ASCII columns.
 */
export interface HexAsciiCell {
  offset: number
  char: string
}

export interface HexRow {
  offset: number
  address: string
  bytes: HexByteCell[]
  /**
   * Joined ASCII representation, for call sites that just need a string
   * for innerText. Prefer `asciiCells` for per-char interactivity.
   */
  ascii: string
  /** Per-character cells with offsets aligned to `bytes`. */
  asciiCells: HexAsciiCell[]
}

export interface VisibleRange {
  start: number
  end: number
}

export type Colorizer = (offset: number, value: number) => string | null

// -- Virtual-scroll math ----------------------------------------------

/**
 * Compute the inclusive/exclusive row range that should be rendered
 * given the current scroll position and container height.
 *
 * Always clamps to `[0, totalRows]`. Negative scroll or zero height
 * collapses to an empty range at zero.
 */
export function computeVisibleRange(
  scrollTop: number,
  containerHeight: number,
  rowHeight: number,
  totalRows: number,
  padding: number = VISIBLE_PADDING,
): VisibleRange {
  if (!Number.isFinite(rowHeight) || rowHeight <= 0) return { start: 0, end: 0 }
  const safeRows = Math.max(0, totalRows | 0)
  const safePad = Math.max(0, padding | 0)

  const firstVisible = Math.max(0, Math.floor(Math.max(0, scrollTop) / rowHeight) - safePad)
  const visibleCount =
    Math.max(0, Math.ceil(Math.max(0, containerHeight) / rowHeight)) + safePad * 2
  const end = Math.min(safeRows, firstVisible + visibleCount)
  const start = Math.min(firstVisible, end)
  return { start, end }
}

/**
 * Number of virtual rows required to display every byte in `data`
 * using `bytesPerRow` columns. Zero bytes or an invalid column count
 * returns 0.
 */
export function totalRowCount(byteLength: number, bytesPerRow: number): number {
  if (!Number.isFinite(bytesPerRow) || bytesPerRow <= 0) return 0
  const safe = Math.max(0, byteLength | 0)
  if (safe === 0) return 0
  return Math.ceil(safe / bytesPerRow)
}

// -- Row rendering ----------------------------------------------------

/**
 * Build the slice of rows between `range.start` (inclusive) and
 * `range.end` (exclusive). The row objects carry enough information
 * for the template to render address, each byte (with optional color)
 * and an ASCII column. Bytes past the available data are omitted so
 * short trailing rows simply end early rather than rendering padding.
 *
 * `dataBaseOffset` lets the editor pass a *windowed* slice of a larger
 * region, when zero (default), `data` is treated as the full region
 * and offsets are direct indices into it; when non-zero, `data[0]`
 * lives at region-offset `dataBaseOffset`, so `bytes[].offset` and
 * the row `address` are emitted in absolute region coordinates and
 * rows that fall outside the window are silently skipped.
 */
export function buildVisibleRows(
  data: Uint8Array,
  range: VisibleRange,
  bytesPerRow: number,
  baseAddress: number = 0,
  colorizer?: Colorizer | null,
  dataBaseOffset: number = 0,
): HexRow[] {
  if (!Number.isFinite(bytesPerRow) || bytesPerRow <= 0) return []
  const rows: HexRow[] = []
  const base = Number.isFinite(baseAddress) ? baseAddress : 0
  const dataBase = Number.isFinite(dataBaseOffset) ? Math.max(0, dataBaseOffset | 0) : 0
  const safeStart = Math.max(0, range.start | 0)
  const safeEnd = Math.max(safeStart, range.end | 0)

  for (let r = safeStart; r < safeEnd; r++) {
    const offset = r * bytesPerRow
    const dataIndex = offset - dataBase
    if (dataIndex >= data.length) break
    if (dataIndex < -bytesPerRow) continue
    const address = hex(base + offset, 6)
    const bytes: HexByteCell[] = []
    const asciiCells: HexAsciiCell[] = []
    let ascii = ''
    for (let c = 0; c < bytesPerRow; c++) {
      const i = offset + c
      const di = dataIndex + c
      if (di >= data.length) break
      if (di < 0) continue
      const v = data[di]
      bytes.push({
        offset: i,
        value: v,
        hex: hex(v, 2),
        color: colorizer?.(i, v) ?? null,
      })
      const char = v >= 32 && v < 127 ? String.fromCharCode(v) : '.'
      ascii += char
      asciiCells.push({ offset: i, char })
    }
    if (bytes.length === 0) continue
    rows.push({ offset, address, bytes, ascii, asciiCells })
  }
  return rows
}

// -- Edit parsing -----------------------------------------------------

/**
 * Parse a 1- or 2-character hex-edit buffer into a byte value `0-255`.
 * A single-digit buffer is right-padded with `0` (so typing `A` yields
 * `0xA0`), matching the behaviour of the live component.
 *
 * Returns `null` when the input is empty, longer than two characters,
 * or contains any non-hex character.
 */
export function parseHexEdit(input: string): number | null {
  if (!input) return null
  if (input.length > 2) return null
  if (!/^[0-9a-f]{1,2}$/i.test(input)) return null
  const value = parseInt(input.padEnd(2, '0'), 16)
  return Number.isFinite(value) ? value & 0xff : null
}

/**
 * Decide what the editor should do for a given keyboard event while
 * actively editing a byte. Returns one of the following shapes:
 *
 * - `{ kind: 'cancel' }`: Escape pressed; caller should clear the
 *   editing state.
 * - `{ kind: 'commit-and-move', delta: -1 | 1 }`: Tab/Arrow navigation;
 *   caller should commit the buffer and move the cursor.
 * - `{ kind: 'append', char }`: Accept the hex char; caller pushes it
 *   onto the buffer.
 * - `{ kind: 'commit-and-advance' }`: The buffer is now full (length 2)
 *   after appending; caller commits and advances +1.
 * - `{ kind: 'ignore' }`: Key is irrelevant; swallow it.
 */
export type EditKeyAction =
  | { kind: 'cancel' }
  | { kind: 'commit-and-move'; delta: -1 | 1 }
  | { kind: 'append'; char: string }
  | { kind: 'commit-and-advance' }
  | { kind: 'ignore' }

export function decideEditKey(
  key: string,
  currentBuffer: string,
): EditKeyAction {
  if (key === 'Escape') return { kind: 'cancel' }
  if (key === 'Tab' || key === 'ArrowRight') return { kind: 'commit-and-move', delta: 1 }
  if (key === 'ArrowLeft') return { kind: 'commit-and-move', delta: -1 }

  const hexChar = key.toLowerCase()
  if (/^[0-9a-f]$/.test(hexChar)) {
    if (currentBuffer.length + 1 >= 2) return { kind: 'commit-and-advance' }
    return { kind: 'append', char: hexChar }
  }
  return { kind: 'ignore' }
}

// -- Selection --------------------------------------------------------

/**
 * Is `offset` within the inclusive `[start, end]` selection range,
 * handling reversed selections (start > end) and a single-byte
 * selection where only `start` is set.
 */
export function isByteSelected(
  offset: number,
  selectionStart: number | null | undefined,
  selectionEnd: number | null | undefined,
): boolean {
  if (selectionStart == null) return false
  const endCandidate = selectionEnd ?? selectionStart
  const lo = Math.min(selectionStart, endCandidate)
  const hi = Math.max(selectionStart, endCandidate)
  return offset >= lo && offset <= hi
}

/**
 * Move the editing cursor by `delta` while keeping it in the valid
 * range `[0, byteLength - 1]`. Empty buffers return 0.
 */
export function clampCursor(offset: number, delta: number, byteLength: number): number {
  if (byteLength <= 0) return 0
  const next = offset + delta
  if (!Number.isFinite(next)) return offset
  return Math.max(0, Math.min(byteLength - 1, next))
}

// -- Auto-fit columns -------------------------------------------------

/**
 * Tuning knobs for `computeBytesPerRow`. All widths are in *character
 * cells* of the host's monospace font; the caller multiplies them by
 * the measured `charWidth` to get pixels.
 *
 * Defaults match the layout used by RetroLab's memory panel and the vanilla
 * `hex-editor.ts`:
 *   `[ADDR][:][SP][ HEX BYTES (2 chars each, sep) ][SP][ASCII]`
 * with a 6-char address ("FFFFFF"), 1 space separator between bytes,
 * and a 2-cell padding around the ASCII column.
 */
export interface ComputeBytesPerRowOptions {
  /** Allowed column counts, sorted ascending. Default: `DEFAULT_BYTES_PER_ROW_CANDIDATES`. */
  candidates?: readonly number[]
  /** Width of the address gutter (e.g. `"FFFFFF: "`). Default 8 cells. */
  addressColumnWidth?: number
  /** Padding cells reserved for the ASCII column gutter on the right (default 2). */
  asciiColumnPadding?: number
  /** Cells per byte in the hex column (default 2 hex digits + 1 separator). */
  cellsPerByte?: number
  /** Cells per byte in the ASCII column (default 1). */
  asciiCellsPerByte?: number
}

/**
 * Pick the largest `candidates` entry whose rendered row fits in
 * `containerWidth`. Returns the smallest candidate when nothing fits,
 * an editor stuck at 8 columns is still usable; a width of 0 is not.
 *
 * Pure function: the caller measures `containerWidth` (via
 * `clientWidth` / ResizeObserver) and `charWidth` (via a temporary
 * monospace span) and feeds them in.
 */
export function computeBytesPerRow(
  containerWidth: number,
  charWidth: number,
  options: ComputeBytesPerRowOptions = {},
): number {
  const candidates = options.candidates ?? DEFAULT_BYTES_PER_ROW_CANDIDATES
  const sorted = [...candidates].filter((n) => Number.isFinite(n) && n > 0).sort((a, b) => a - b)
  if (sorted.length === 0) return 8

  if (!Number.isFinite(containerWidth) || containerWidth <= 0) return sorted[0]
  if (!Number.isFinite(charWidth) || charWidth <= 0) return sorted[0]

  const addressW = options.addressColumnWidth ?? 8
  const asciiPad = options.asciiColumnPadding ?? 2
  const cellsPerByte = options.cellsPerByte ?? 3   // 2 hex digits + 1 separator
  const asciiCells = options.asciiCellsPerByte ?? 1

  let best = sorted[0]
  for (const cols of sorted) {
    const widthInCells = addressW + cols * cellsPerByte + asciiPad + cols * asciiCells
    const widthInPx = widthInCells * charWidth
    if (widthInPx <= containerWidth) {
      best = cols
    } else {
      break // sorted ascending, no point trying larger
    }
  }
  return best
}

// -- Modified-bytes colorizer -----------------------------------------

export interface ModifiedColorizerOptions {
  /** CSS colour for bytes whose offset is in the `modified` set. Default `'var(--accent-warn, #e89e64)'`. */
  color?: string
  /** Colorizer consulted for offsets *not* in the modified set. */
  fallbackColorizer?: Colorizer | null
}

/**
 * Build a `Colorizer` that paints modified bytes in a distinct colour
 * and falls through to `fallbackColorizer` for everything else.
 *
 * The set is captured by reference so the host can mutate it between
 * paints without rebuilding the colorizer; the editor just re-renders.
 */
export function createModifiedColorizer(
  modified: ReadonlySet<number>,
  options: ModifiedColorizerOptions = {},
): Colorizer {
  const color = options.color ?? 'var(--accent-warn, #e89e64)'
  const fallback = options.fallbackColorizer ?? null
  return (offset, value) => {
    if (modified.has(offset)) return color
    return fallback ? fallback(offset, value) : null
  }
}

// -- Windowed data source ---------------------------------------------

/**
 * Lazy chunk reader for the hex editor. Lets the editor render an
 * arbitrarily large logical region (think: 4 MiB ROM) while only
 * holding a sliding window of bytes in memory at a time.
 *
 * - `totalSize` is the logical region length used to compute the
 *   virtual-scroll spacer; the editor scrolls across all of it even
 *   though it never loads more than the window.
 * - `read(offset, length)` returns up to `length` bytes starting at
 *   `offset` within the region. Implementations are free to clamp
 *   the result if the region ends earlier (the editor uses the
 *   returned length, not the requested one).
 */
export interface HexEditorDataSource {
  readonly totalSize: number
  read(offset: number, length: number): Uint8Array
}

/**
 * The currently-loaded slice of a `HexEditorDataSource`. `base` is
 * the region offset where `data` (passed separately) begins, and
 * `length` is the number of valid bytes (`data.length` once read).
 */
export interface WindowState {
  base: number
  length: number
}

export interface ComputeWindowOptions {
  /** Bytes to attempt to load per window. Clamped to `totalSize`. */
  windowSize: number
  /** Logical region length. */
  totalSize: number
  /** Row width, `base` is snapped down to a multiple of this. Default 1. */
  bytesPerRow?: number
}

export interface WindowCoverageOptions {
  /**
   * Re-anchor before the visible range comes within this many bytes
   * of the window edge; tuning balances re-read cost vs jitter.
   */
  edgeMarginBytes: number
  /** Logical region length; required so end-of-region windows are accepted. */
  totalSize: number
}

/**
 * Clamp a requested window size against the region length so callers
 * can ask for "1 MiB" without worrying that the region is only 8 KiB.
 * Returns `0` for empty/invalid regions; non-positive requests are
 * treated as "give me the whole region".
 */
export function clampWindowSize(requestedSize: number, totalSize: number): number {
  const total = Math.max(0, totalSize | 0)
  if (total <= 0) return 0
  const req = requestedSize | 0
  if (req <= 0) return total
  return Math.min(total, req)
}

/**
 * Pick the window base that roughly centres `desiredOffset` inside a
 * window of size `windowSize`, clamped to `[0, totalSize - windowSize]`
 * and snapped down to the nearest `bytesPerRow` boundary so the first
 * visible row aligns cleanly with the address gutter.
 *
 * Snap-down is safe because the pre-snap `base` is already non-negative
 * and bounded by `maxBase`; `Math.floor` only decreases it, keeping it
 * in range without further clamps.
 */
export function computeWindowAnchor(
  desiredOffset: number,
  options: ComputeWindowOptions,
): number {
  const totalSize = Math.max(0, options.totalSize | 0)
  if (totalSize <= 0) return 0
  const windowSize = Math.max(1, Math.min(totalSize, options.windowSize | 0))
  const desired = Math.max(0, Math.min(totalSize - 1, desiredOffset | 0))
  const bpr = Math.max(1, options.bytesPerRow ?? 1)
  const maxBase = Math.max(0, totalSize - windowSize)
  const centred = desired - Math.floor(windowSize / 2)
  const clamped = Math.max(0, Math.min(maxBase, centred))
  return Math.floor(clamped / bpr) * bpr
}

/**
 * True when the loaded `window` covers `[visibleStartOffset, visibleEndOffset]`
 * with enough margin on each edge that scrolling won't immediately
 * fall off the buffer. Returns `true` when the visible range bumps
 * against region start/end (no re-anchor possible).
 */
export function windowCoversVisibleRange(
  window: WindowState,
  visibleStartOffset: number,
  visibleEndOffset: number,
  options: WindowCoverageOptions,
): boolean {
  if (window.length <= 0) return false
  const margin = Math.max(0, options.edgeMarginBytes | 0)
  const totalSize = Math.max(0, options.totalSize | 0)
  const windowStart = window.base
  const windowEnd = window.base + window.length
  // Need left margin unless we're already at region start.
  if (visibleStartOffset < windowStart + margin && windowStart > 0) return false
  // Need right margin unless the window already reaches region end.
  if (visibleEndOffset > windowEnd - margin && windowEnd < totalSize) return false
  return true
}
