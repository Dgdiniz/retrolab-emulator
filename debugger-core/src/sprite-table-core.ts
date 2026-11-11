/**
 * Pure-core helpers for the VDP sprite-table panel that ships in both
 * UI shells (RetroLab's sprite-table panel and the web debugger's
 * `panels/vdp/sprite-table-viewer.ts`).
 *
 * The shells own reactivity / DOM refs / guide-target wiring; this
 * module owns the deterministic decoding from a SAT entry into display
 * string columns + canonical guide-target IDs. Tests can drive every
 * function without mounting Vue or touching WASM.
 */

/**
 * Input contract: a single Sprite Attribute Table entry as returned by
 * the WASM emulator's `getSpriteAttrTable()` walker. The shape mirrors
 * what `Sprite::from_sat` in `cores/sg1/core/src/vdp/sprites.rs` produces; we
 * redeclare it here (rather than import from a WASM types file) so this
 * module stays in the pure-core layer.
 *
 * `x` / `y` are screen-space (raw SAT word minus 128, matching the
 * `Sprite::from_sat` decoder).
 */
export interface SpriteAttrInput {
  /** SAT slot index (0..79). */
  index: number
  x: number
  y: number
  h_tiles: number
  v_tiles: number
  tile: number
  palette: number
  priority: boolean
  h_flip: boolean
  v_flip: boolean
  link: number
}

export interface SpriteTableRow {
  /** SAT slot index (0..79). Stable across calls. */
  index: number
  /** `vdp.sprites.entry.{index}`: used by the engine to highlight this row. */
  guideTargetId: string
  /** Pre-formatted columns ready for `<td>` rendering. */
  columns: SpriteTableColumns
  /** Raw entry, kept for tooltips / copy-to-clipboard / future drill-downs. */
  raw: SpriteAttrInput
}

export interface SpriteTableColumns {
  index: string
  pos: string
  size: string
  tile: string
  palette: string
  priority: string
  flips: string
  link: string
}

/**
 * Build the canonical guide-target id for a given SAT index.
 *
 * IMPORTANT: this id is part of the engine <-> content contract; RetroLab's
 * lessons reference it from `focus[]` blocks. Do not change it without
 * updating the lessons that name it.
 */
export function spriteGuideTargetId(index: number): string {
  return `vdp.sprites.entry.${index}`
}

/**
 * Format a signed 16-bit screen coordinate as a fixed-width decimal.
 * The SAT decoder already subtracts 128 so values can be negative.
 */
export function formatSpritePos(x: number, y: number): string {
  return `(${x}, ${y})`
}

/**
 * Format the tile-multiple size as the classic `WxH` notation Genesis
 * docs use (e.g. `1x1`, `4x4`).
 */
export function formatSpriteSize(hTiles: number, vTiles: number): string {
  return `${hTiles}x${vTiles}`
}

/**
 * Format the pattern base as a 4-digit hex with a `$` prefix to match
 * the rest of the debugger UI (which uses `$XXXX` for VRAM addresses).
 */
export function formatSpriteTile(tile: number): string {
  return `$${(tile & 0x07ff).toString(16).toUpperCase().padStart(4, '0')}`
}

/**
 * Single-character flip flag for compactness (`-` when neither flip
 * applies). Keeps the table readable at small panel widths.
 */
export function formatSpriteFlips(hFlip: boolean, vFlip: boolean): string {
  if (!hFlip && !vFlip) return '-'
  let out = ''
  if (hFlip) out += 'H'
  if (vFlip) out += 'V'
  return out
}

/**
 * Format the `link` field as decimal, link is a SAT slot index, not
 * an address, so decimal is the convention used by Sega and MAME.
 */
export function formatSpriteLink(link: number): string {
  return link.toString(10).padStart(2, '0')
}

/**
 * Build the per-sprite display row from a raw SAT entry. Pure /
 * memo-friendly: caller can `.map(buildSpriteTableRow)` and re-use
 * results across renders.
 */
export function buildSpriteTableRow(entry: SpriteAttrInput): SpriteTableRow {
  return {
    index: entry.index,
    guideTargetId: spriteGuideTargetId(entry.index),
    columns: {
      index: entry.index.toString(10).padStart(2, '0'),
      pos: formatSpritePos(entry.x, entry.y),
      size: formatSpriteSize(entry.h_tiles, entry.v_tiles),
      tile: formatSpriteTile(entry.tile),
      palette: `P${entry.palette}`,
      priority: entry.priority ? 'high' : 'low',
      flips: formatSpriteFlips(entry.h_flip, entry.v_flip),
      link: formatSpriteLink(entry.link),
    },
    raw: entry,
  }
}

/**
 * Build the full table from a SAT walk. Empty array -> "no sprites in
 * the chain" (panel renders an empty-state message).
 */
export function buildSpriteTable(entries: ReadonlyArray<SpriteAttrInput>): SpriteTableRow[] {
  return entries.map(buildSpriteTableRow)
}
