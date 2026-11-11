/**
 * Shared pure formatting helpers used across debug panels, the game hub,
 * the sync UI, and the tracing controls. Keeping them in one module means
 * the edge cases (NaN, undefined, zero, huge values) are unit-tested once
 * instead of in every caller.
 */

// -- Hex formatting ---------------------------------------------------

/**
 * Format an unsigned integer as an uppercased hex string zero-padded to
 * `digits` characters. Handles the sign-bit correctly via the `>>> 0`
 * coercion so that 0xFFFFFFFF renders as "FFFFFFFF" instead of "-1".
 *
 * @param value The number to format. Non-finite inputs return a run of '?'.
 * @param digits Target width (default 6, covering the 68000 24-bit space).
 */
export function hex(value: number, digits: number = 6): string {
  if (!Number.isFinite(value)) return '?'.repeat(digits)
  return (value >>> 0).toString(16).toUpperCase().padStart(digits, '0')
}

/**
 * Format a buffer/array of bytes as space-separated two-digit hex, e.g.
 * `[0x4e, 0x71]` -> "4E 71". Undefined/non-array inputs return "".
 */
export function hexBytes(bytes: number[] | Uint8Array | undefined | null): string {
  if (!bytes) return ''
  if (!Array.isArray(bytes) && !(bytes instanceof Uint8Array)) return ''
  const arr = bytes instanceof Uint8Array ? Array.from(bytes) : bytes
  return arr
    .map((b) => (b & 0xff).toString(16).toUpperCase().padStart(2, '0'))
    .join(' ')
}

/**
 * Parse a hex string (with or without `0x` prefix, spaces ignored). Returns
 * `null` if the string is empty or does not contain any hex digits so that
 * callers can distinguish "user typed nothing" from "user typed 0".
 */
export function parseHexAddress(input: string | null | undefined): number | null {
  if (input == null) return null
  const cleaned = input.trim().replace(/^0x/i, '').replace(/\s+/g, '')
  if (!cleaned) return null
  if (!/^[0-9a-f]+$/i.test(cleaned)) return null
  const n = parseInt(cleaned, 16)
  return Number.isFinite(n) ? n : null
}

/**
 * Parse a numeric address that may be written in any of the three notations
 * users actually type into RetroLab debug surfaces: bare hex (`200`), the
 * C-style `0x` prefix (`0x200`), or the assembler-style `$` prefix (`$200`).
 * Returns the numeric value or `null` if the input is empty / does not
 * resolve to a hex literal. Symbol-name resolution is the caller's job,
 * this helper only handles literals.
 *
 * Mirrors the numeric-literal rule of RetroLab's lesson symbol resolver so
 * the Breakpoints panel and the lesson validator agree on what counts as a
 * literal.
 */
export function parseAddressInput(input: string | null | undefined): number | null {
  if (input == null) return null
  const trimmed = input.trim().replace(/\s+/g, '')
  if (!trimmed) return null
  const cleaned = trimmed.replace(/^(0x|\$)/i, '')
  if (!cleaned) return null
  if (!/^[0-9a-f]+$/i.test(cleaned)) return null
  const n = parseInt(cleaned, 16)
  return Number.isFinite(n) ? n : null
}

// -- Byte / size formatting ------------------------------------------

/**
 * Human-friendly byte count: "512 B", "1.5 KB", "3.2 MB", "4.1 GB".
 * Negative values are formatted with a leading minus; non-finite inputs
 * return "0 B".
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes)) return '0 B'
  const sign = bytes < 0 ? '-' : ''
  const abs = Math.abs(bytes)
  if (abs < 1024) return `${sign}${abs} B`
  if (abs < 1024 * 1024) return `${sign}${(abs / 1024).toFixed(1)} KB`
  if (abs < 1024 * 1024 * 1024) return `${sign}${(abs / (1024 * 1024)).toFixed(1)} MB`
  return `${sign}${(abs / (1024 * 1024 * 1024)).toFixed(2)} GB`
}

// -- Date formatting -------------------------------------------------

/**
 * Format an ISO date into a short, locale-aware string. `null`/`undefined`
 * renders as the em-dash sentinel used by several UI surfaces.
 */
export function formatDate(iso: string | null | undefined, dash: string = '—'): string {
  if (!iso) return dash
  const d = new Date(iso)
  if (isNaN(d.getTime())) return dash
  return d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
}

/**
 * Parse an ISO date into epoch milliseconds. Invalid/missing dates sort
 * below real dates by returning 0, which is what every hub view wants.
 */
export function dateToEpochMs(iso: string | null | undefined): number {
  if (!iso) return 0
  const t = new Date(iso).getTime()
  return Number.isFinite(t) ? t : 0
}
