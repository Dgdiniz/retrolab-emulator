// Pure helpers for the generic video-layer toggle UI shared by every
// console debugger (Genesis VDP, SNES PPU, ...). Both the platform Vue
// panels and the OSS vanilla debugger parse the WASM `getLayerInfo()`
// payload through `parseLayerRows`, so the two frontends render an
// identical layer list and stay in lock-step.
//
// The WASM side serialises `retro_core::video::LayerData` (snake_case:
// `{ id, name, width, height, scroll_x, scroll_y, enabled }`); this
// module is deliberately tolerant of missing/loose fields so a core that
// only fills a subset still renders sensible rows.

/** One toggleable rendering layer as shown in the debugger UI. */
export interface LayerRow {
  /** Stable layer id passed back to `toggleLayer(id, visible)`. */
  id: number
  /** Human-readable layer name (e.g. "Plane A", "BG1", "Sprites"). */
  name: string
  /** Whether the layer is currently composited (checkbox state). */
  enabled: boolean
  /** Pre-formatted "WxH" size string for display. */
  size: string
  /** Horizontal scroll in pixels (0 when not meaningful). */
  scrollX: number
  /** Vertical scroll in pixels (0 when not meaningful). */
  scrollY: number
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null
}

function toInt(v: unknown): number {
  const n = typeof v === 'number' ? v : Number(v)
  return Number.isFinite(n) ? Math.trunc(n) : 0
}

function toBool(v: unknown): boolean {
  return v === true || v === 1
}

/**
 * Parse the raw `getLayerInfo()` payload (a serialised `LayerData[]`)
 * into display-ready `LayerRow`s. Non-array input and non-object entries
 * are skipped, so a core without a video debugger (returns `null`/`[]`)
 * yields an empty list and the UI simply shows no layer controls.
 */
export function parseLayerRows(raw: unknown): LayerRow[] {
  if (!Array.isArray(raw)) return []
  const rows: LayerRow[] = []
  for (const entry of raw) {
    if (!isObject(entry)) continue
    const id = toInt(entry.id)
    rows.push({
      id,
      name: typeof entry.name === 'string' && entry.name ? entry.name : `Layer ${id}`,
      enabled: toBool(entry.enabled),
      size: `${toInt(entry.width)}×${toInt(entry.height)}`,
      scrollX: toInt(entry.scroll_x),
      scrollY: toInt(entry.scroll_y),
    })
  }
  return rows
}

/** One video control register as shown in the debugger State tab. */
export interface VideoRegisterRow {
  index: number
  name: string
  /** Raw register byte/word value. */
  value: number
  /** `$HH` formatted value for display. */
  hex: string
  /** Optional human description (may be empty). */
  description: string
}

/**
 * Parse the raw `getVdpRegisters()` / `getPpuRegisters()` payload (a
 * serialised `VideoRegister[]`) into display rows with a pre-formatted
 * hex string. Values are masked to the width implied by their
 * magnitude: <= 0xFF renders as two digits, otherwise four.
 */
export function parseVideoRegisters(raw: unknown): VideoRegisterRow[] {
  if (!Array.isArray(raw)) return []
  const rows: VideoRegisterRow[] = []
  for (const entry of raw) {
    if (!isObject(entry)) continue
    const value = toInt(entry.value)
    const digits = value > 0xff ? 4 : 2
    rows.push({
      index: toInt(entry.index),
      name: typeof entry.name === 'string' ? entry.name : '',
      value,
      hex: `$${(value >>> 0).toString(16).toUpperCase().padStart(digits, '0')}`,
      description: typeof entry.description === 'string' ? entry.description : '',
    })
  }
  return rows
}
