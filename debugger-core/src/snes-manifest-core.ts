/**
 * Pure helpers for viewing and editing a snes1 `cartridge-manifest-v1`
 * from a UI (the platform hub modal, the platform Debug tab, and the
 * standalone debugger all share this code).
 *
 * The schema mirrors the Rust `CartridgeManifest` in
 * `cores/snes1/core/src/cartridge/manifest.rs` 1:1. The authoritative
 * parser/validator is the Rust `parse_manifest` reached through the WASM
 * `setCartridgeManifest` binding; the helpers here exist for early,
 * Vue/DOM-free UI feedback (structured form <-> JSON, "modified" badge,
 * client-side shape checks) so the editor can react before a round-trip
 * to the core.
 *
 * The editable *form* is intentionally narrow (preset, SRAM size,
 * FastROM) because those are the knobs that fix real mapping bugs (e.g.
 * Donkey Kong Country's 2 KiB SRAM mirror). Power users edit custom
 * slots/chips through the raw-JSON view, which the form round-trip
 * preserves verbatim.
 */

/** Schema tag the Rust core stamps onto a detected manifest. */
export const SNES_MANIFEST_SCHEMA = 'cartridge-manifest-v1'
/** Manifest format version the Rust `parse_manifest` accepts. */
export const SNES_MANIFEST_VERSION = '1.0'
/** Upper bound for a sane SRAM size (1 MiB) used by form validation. */
export const MAX_SRAM_BYTES = 1 << 20

/** Stock mapper presets, plus `custom` for manifest-only layouts. */
export type SnesPreset = 'lorom' | 'hirom' | 'exhirom' | 'custom'

const KNOWN_PRESETS: readonly SnesPreset[] = ['lorom', 'hirom', 'exhirom', 'custom']

/** A `{ value, label }` choice for a `<select>`. */
export interface SnesOption<T> {
  value: T
  label: string
}

/** ROM-image metadata block (`rom`). */
export interface SnesManifestRom {
  sha256: string
  size: number
  title?: string | null
  region?: string | null
}

/** What lives behind a slot, tag-by-`kind`, matching the Rust enum. */
export type SnesManifestTarget =
  | { kind: 'Rom'; base_offset?: number }
  | { kind: 'Sram'; base_offset?: number }
  | { kind: 'ChipMmio'; chip_id: number; base?: number }
  | { kind: 'OpenBus' }

/** One mapping slot (hex addresses as strings, as on disk). */
export interface SnesManifestSlot {
  bank_range: [string, string]
  offset_range: [string, string]
  target: SnesManifestTarget
  speed: string
  mask?: string | null
  bank_stride?: string | null
}

/** Coprocessor chip declaration. */
export interface SnesManifestChip {
  id: number
  kind: string
  mapping: SnesManifestSlot
  /**
   * Bus address ranges the chip passively observes but does not own, each a
   * `"lo-hi"` 24-bit hex range like `"0x4300-0x437F"` (e.g. an S-DD1 spying
   * on the DMA registers). Optional; omitted for chips that observe nothing.
   */
  observes?: string[]
}

/** Mapping section: preset + per-mapper knobs + extra slots. */
export interface SnesManifestMapping {
  preset: string
  fastrom: boolean
  sram_size: number
  slots?: SnesManifestSlot[]
}

/** Top-level manifest, mirroring the on-disk JSON. */
export interface SnesManifest {
  $schema?: string | null
  core: string
  version: string
  rom: SnesManifestRom
  mapping: SnesManifestMapping
  chips?: SnesManifestChip[]
  controllers?: unknown
  metadata?: unknown
}

/** The narrow, structured form the editor binds its inputs to. */
export interface SnesManifestForm {
  preset: SnesPreset
  sramSize: number
  fastrom: boolean
}

/** Preset choices for the form `<select>`. */
export const PRESET_OPTIONS: ReadonlyArray<SnesOption<SnesPreset>> = [
  { value: 'lorom', label: 'LoROM' },
  { value: 'hirom', label: 'HiROM' },
  { value: 'exhirom', label: 'ExHiROM' },
  { value: 'custom', label: 'Custom (manifest slots only)' },
]

/** Common SRAM sizes (bytes) for the form `<select>`. */
export const SRAM_SIZE_OPTIONS: ReadonlyArray<SnesOption<number>> = [
  { value: 0, label: 'None' },
  { value: 2048, label: '2 KiB' },
  { value: 8192, label: '8 KiB' },
  { value: 16384, label: '16 KiB' },
  { value: 32768, label: '32 KiB' },
  { value: 65536, label: '64 KiB' },
  { value: 131072, label: '128 KiB' },
  { value: 262144, label: '256 KiB' },
]

/** Region labels matching the Rust `region_label` mapping (display). */
export const REGION_OPTIONS: ReadonlyArray<SnesOption<string>> = [
  { value: 'Japan', label: 'Japan' },
  { value: 'USA', label: 'USA' },
  { value: 'Europe', label: 'Europe' },
  { value: 'Germany', label: 'Germany' },
  { value: 'France', label: 'France' },
  { value: 'Italy', label: 'Italy' },
  { value: 'Spain', label: 'Spain' },
  { value: 'Korea', label: 'Korea' },
  { value: 'Brazil', label: 'Brazil' },
  { value: 'Australia', label: 'Australia' },
]

/** Coerce any string to a known preset, defaulting unknowns to `custom`. */
export function normalizePreset(value: string): SnesPreset {
  const lower = value.toLowerCase() as SnesPreset
  return KNOWN_PRESETS.includes(lower) ? lower : 'custom'
}

/**
 * Parse + shape-check a manifest JSON string. Throws an `Error` with a
 * human-readable message on malformed JSON or a missing required field.
 * This is a *shape* check only, the Rust core remains the authority on
 * preset names, hex ranges, and the schema version.
 */
export function parseManifestJson(json: string): SnesManifest {
  let parsed: unknown
  try {
    parsed = JSON.parse(json)
  } catch (e) {
    throw new Error(`Manifest is not valid JSON: ${(e as Error).message}`)
  }
  return assertManifestShape(parsed)
}

/**
 * Parse the JSON returned by the WASM `getDetectedManifest()` binding,
 * which yields the JSON literal `null` (or an empty string) when no ROM
 * is loaded. Returns `null` in that case, otherwise a parsed manifest.
 */
export function parseDetectedManifest(json: string): SnesManifest | null {
  const trimmed = json.trim()
  if (trimmed === '' || trimmed === 'null') return null
  return parseManifestJson(trimmed)
}

function assertManifestShape(value: unknown): SnesManifest {
  if (typeof value !== 'object' || value === null) {
    throw new Error('Manifest must be a JSON object')
  }
  const m = value as Record<string, unknown>
  if (typeof m.core !== 'string') throw new Error('Manifest "core" must be a string')
  if (typeof m.version !== 'string') throw new Error('Manifest "version" must be a string')
  if (typeof m.rom !== 'object' || m.rom === null) {
    throw new Error('Manifest "rom" must be an object')
  }
  const rom = m.rom as Record<string, unknown>
  if (typeof rom.sha256 !== 'string') throw new Error('Manifest "rom.sha256" must be a string')
  if (typeof rom.size !== 'number') throw new Error('Manifest "rom.size" must be a number')
  if (typeof m.mapping !== 'object' || m.mapping === null) {
    throw new Error('Manifest "mapping" must be an object')
  }
  const mapping = m.mapping as Record<string, unknown>
  if (typeof mapping.preset !== 'string') {
    throw new Error('Manifest "mapping.preset" must be a string')
  }
  return value as SnesManifest
}

/** Extract the editable form fields from a parsed manifest. */
export function manifestFormFromJson(manifest: SnesManifest): SnesManifestForm {
  return {
    preset: normalizePreset(manifest.mapping.preset),
    sramSize: Math.max(0, Math.trunc(Number(manifest.mapping.sram_size) || 0)),
    fastrom: Boolean(manifest.mapping.fastrom),
  }
}

/**
 * Produce a new manifest from `base` with the form's mapping knobs
 * applied. Everything else (`rom` digest/size, custom slots, chips,
 * metadata, controllers, schema) is preserved verbatim so a round-trip
 * through the form never drops hand-authored detail.
 */
export function applyFormToManifest(form: SnesManifestForm, base: SnesManifest): SnesManifest {
  return {
    ...base,
    mapping: {
      ...base.mapping,
      preset: form.preset,
      fastrom: form.fastrom,
      sram_size: form.sramSize,
    },
  }
}

/** Serialize a manifest to pretty JSON for the raw-edit view. */
export function stringifyManifest(manifest: SnesManifest): string {
  return JSON.stringify(manifest, null, 2)
}

/** Human label for an SRAM size in bytes (`0` -> "None"). */
export function formatSramSize(bytes: number): string {
  if (bytes <= 0) return 'None'
  if (bytes >= 1024) return `${bytes / 1024} KiB`
  return `${bytes} B`
}

function isPowerOfTwo(n: number): boolean {
  return (n & (n - 1)) === 0
}

/**
 * Validate the structured form. Returns a list of human-readable error
 * messages (empty when valid). The core still has the final say, but
 * catching obvious mistakes here keeps the editor responsive.
 */
export function validateManifestForm(form: SnesManifestForm): string[] {
  const errors: string[] = []
  if (!KNOWN_PRESETS.includes(form.preset)) {
    errors.push(`Unknown preset "${form.preset}"`)
  }
  if (!Number.isInteger(form.sramSize) || form.sramSize < 0) {
    errors.push('SRAM size must be a non-negative whole number of bytes')
  } else if (form.sramSize > MAX_SRAM_BYTES) {
    errors.push(`SRAM size must not exceed ${formatSramSize(MAX_SRAM_BYTES)}`)
  } else if (form.sramSize > 0 && !isPowerOfTwo(form.sramSize)) {
    errors.push('SRAM size should be a power of two (e.g. 2 KiB, 8 KiB, 32 KiB)')
  }
  return errors
}

function slotCount(mapping: SnesManifestMapping): number {
  return Array.isArray(mapping.slots) ? mapping.slots.length : 0
}

function chipCount(manifest: SnesManifest): number {
  return Array.isArray(manifest.chips) ? manifest.chips.length : 0
}

/**
 * `true` when `override` represents a change from the detected baseline;
 * it drives the "Modified" badge in the editors. A `null` override is
 * never a change; a non-null override against a `null` baseline always
 * is. Compares the mapping knobs plus custom-slot and chip counts.
 */
export function manifestDiffersFromDetected(
  detected: SnesManifest | null,
  override: SnesManifest | null,
): boolean {
  if (!override) return false
  if (!detected) return true
  const a = detected.mapping
  const b = override.mapping
  if (normalizePreset(a.preset) !== normalizePreset(b.preset)) return true
  if ((Number(a.sram_size) || 0) !== (Number(b.sram_size) || 0)) return true
  if (Boolean(a.fastrom) !== Boolean(b.fastrom)) return true
  if (slotCount(a) !== slotCount(b)) return true
  if (chipCount(detected) !== chipCount(override)) return true
  return false
}

/**
 * Human label for the detected coprocessor recorded under
 * `metadata.coprocessor` (e.g. `"DSP1 (HiRom)"`), or `null` when the
 * manifest carries none. Lets the editors surface the chip without
 * re-running detection.
 */
export function coprocessorLabel(manifest: SnesManifest | null): string | null {
  const meta = manifest?.metadata
  if (!meta || typeof meta !== 'object') return null
  const cop = (meta as Record<string, unknown>).coprocessor
  if (!cop || typeof cop !== 'object') return null
  const kind = (cop as Record<string, unknown>).kind
  if (typeof kind !== 'string') return null
  const layout = (cop as Record<string, unknown>).layout
  const upper = kind.toUpperCase()
  return typeof layout === 'string' ? `${upper} (${layout})` : upper
}
