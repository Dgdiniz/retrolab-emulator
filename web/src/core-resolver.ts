// core-resolver: pure core-selection + ROM-identity policy for the
// standalone web debugger.
//
// The OSS debugger boots ONE native core per page load. Which core that
// is comes from (in priority order):
//
//   1. an explicit `?core=` query parameter, then
//   2. the dropped/opened ROM's file extension, then
//   3. a default of `sg1` (Genesis).
//
// Switching cores after boot is a full page reload with `?core=` set, so
// every decision here is a pure function of the URL + filename, no
// emulator state, no DOM. That keeps it unit-testable without WASM.
//
// This module also owns SNES ROM identity. `sg1-wasm` exposes
// `getCartridgeHash()` / `getCartridgeTitle()`, but `snes1-wasm` does
// not, it only offers `getRomInfo()` (`{ title, region, checksum, ... }`).
// `deriveRomIdentity()` papers over that difference with a JS byte hash
// so SRAM / breakpoint / save-state storage keys keep working for both.

export type WebCore = 'sg1' | 'snes1'

/** File extensions that select the SG1 (Genesis / Mega Drive) core. */
export const SG1_EXTENSIONS = ['.bin', '.gen', '.md', '.smd'] as const

/** File extensions that select the snes1 (Super Nintendo) core. */
export const SNES1_EXTENSIONS = ['.sfc', '.smc', '.fig', '.swc'] as const

/** Every ROM extension the file picker / drag-drop should accept. */
export const ACCEPTED_ROM_EXTENSIONS: readonly string[] = [
  ...SG1_EXTENSIONS,
  ...SNES1_EXTENSIONS,
]

/** The core used when neither the query string nor the ROM hints at one. */
export const DEFAULT_CORE: WebCore = 'sg1'

/**
 * Map a free-form `?core=` value to a {@link WebCore}.
 *
 * Accepts the canonical core ids (`sg1`, `snes1`) plus the obvious
 * system-name aliases (`genesis`/`megadrive`/`md` -> sg1,
 * `snes`/`sfc` -> snes1) so a shared URL stays readable. Unknown values
 * return `null` so the caller can fall back to the extension/default.
 */
export function normalizeCoreId(value: string | null | undefined): WebCore | null {
  if (!value) return null
  switch (value.trim().toLowerCase()) {
    case 'sg1':
    case 'genesis':
    case 'megadrive':
    case 'md':
      return 'sg1'
    case 'snes1':
    case 'snes':
    case 'sfc':
      return 'snes1'
    default:
      return null
  }
}

/**
 * Resolve the core from a URL query string (e.g. `location.search`).
 * Returns `null` when there is no `core` param or it is unrecognised.
 */
export function resolveCoreFromQuery(search: string | null | undefined): WebCore | null {
  if (!search) return null
  const params = new URLSearchParams(search)
  return normalizeCoreId(params.get('core'))
}

/**
 * Resolve the core from a ROM filename's extension. Case-insensitive.
 * Returns `null` for unknown extensions (the caller decides the default).
 */
export function resolveCoreFromExtension(filename: string | null | undefined): WebCore | null {
  if (!filename) return null
  const lower = filename.toLowerCase()
  if (SNES1_EXTENSIONS.some((ext) => lower.endsWith(ext))) return 'snes1'
  if (SG1_EXTENSIONS.some((ext) => lower.endsWith(ext))) return 'sg1'
  return null
}

/**
 * Decide the boot core: query param wins, then the (optional) ROM
 * filename, then {@link DEFAULT_CORE}. Pass the filename only when a ROM
 * is being auto-loaded at boot; the empty-boot case just uses the query.
 */
export function resolveBootCore(
  search: string | null | undefined,
  filename?: string | null,
): WebCore {
  return (
    resolveCoreFromQuery(search) ??
    resolveCoreFromExtension(filename) ??
    DEFAULT_CORE
  )
}

/** True when the filename carries any extension we know how to load. */
export function hasAcceptedRomExtension(filename: string | null | undefined): boolean {
  if (!filename) return false
  const lower = filename.toLowerCase()
  return ACCEPTED_ROM_EXTENSIONS.some((ext) => lower.endsWith(ext))
}

/**
 * The wasm-pack glue filename for a core, relative to the served
 * `pkg/` directory. `build-wasm.sh` emits `sg1_wasm.js` and
 * `snes1_wasm.js` side by side under `public/pkg/`.
 */
export function wasmModulePath(core: WebCore): string {
  return core === 'snes1' ? 'pkg/snes1_wasm.js' : 'pkg/sg1_wasm.js'
}

/**
 * The `RetroEmulator` constructor `system` string for a core. Both
 * cores name their primary system the same way the platform does.
 */
export function systemString(core: WebCore): string {
  return core === 'snes1' ? 'snes' : 'genesis'
}

/** Human-facing core label for status lines / the page title. */
export function coreDisplayName(core: WebCore): string {
  return core === 'snes1' ? 'SNES' : 'Genesis'
}

// -- ROM identity ----------------------------------------------------

/**
 * Deterministic, dependency-free byte hash (FNV-1a, widened to 16 hex
 * chars by running two offset bases). Used as the SNES SRAM /
 * breakpoint / save-state key because `snes1-wasm` has no native
 * cartridge-hash export. Folding the length in keeps same-prefix ROMs
 * of different sizes from colliding.
 */
export function computeByteHash(data: Uint8Array): string {
  // Two independent FNV-1a passes (different offset basis) give us a
  // 64-bit-ish key from a 32-bit primitive without pulling in a crypto
  // dependency. Collisions are astronomically unlikely for ROM-sized
  // inputs and a local-only storage key.
  let h1 = 0x811c9dc5
  let h2 = 0x01000193
  for (let i = 0; i < data.length; i++) {
    const b = data[i]
    h1 ^= b
    h1 = Math.imul(h1, 0x01000193)
    h2 ^= b
    h2 = Math.imul(h2, 0x85ebca77)
  }
  // `>>> 0` coerces to unsigned before hex; pad each half to 8 chars.
  const lenTag = (data.length >>> 0).toString(16).padStart(8, '0')
  const a = (h1 >>> 0).toString(16).padStart(8, '0')
  const b = (h2 >>> 0).toString(16).padStart(8, '0')
  // Length-salted so e.g. a ROM and its truncated header don't collide.
  return `${a}${b}${lenTag}`.slice(0, 16)
}

/** Minimal projection of the bits of `RetroEmulator` we read for identity. */
export interface RomIdentitySource {
  getCartridgeHash?: () => string | null | undefined
  getCartridgeTitle?: () => string | null | undefined
  getRomInfo?: () => { title?: string | null } | null | undefined
}

export interface RomIdentity {
  /** Stable storage key for SRAM / breakpoints / save states. */
  hash: string | null
  /** Display title for the ROM (falls back to the filename). */
  title: string
}

/**
 * Resolve a ROM's `{ hash, title }` for storage keys + UI.
 *
 *   - SG1 has native `getCartridgeHash()` / `getCartridgeTitle()`, use
 *     them when present (older builds may lack them, hence the guards).
 *   - snes1 has neither, so we hash the ROM bytes ourselves and read
 *     the title from `getRomInfo()`.
 *
 * The filename is the last-resort title so the UI never shows an empty
 * label. A `null` hash means "don't persist SRAM/breakpoints"; it is
 * what SG1 gets when `getCartridgeHash()` is missing or returns nothing.
 */
export function deriveRomIdentity(
  emulator: RomIdentitySource,
  data: Uint8Array,
  filename: string,
  core: WebCore,
): RomIdentity {
  if (core === 'snes1') {
    const info = emulator.getRomInfo?.()
    const title = (info?.title && info.title.trim()) || filename
    return { hash: computeByteHash(data), title }
  }
  const hash = emulator.getCartridgeHash ? emulator.getCartridgeHash() ?? null : null
  const title = (emulator.getCartridgeTitle && emulator.getCartridgeTitle()) || filename
  return { hash, title }
}
