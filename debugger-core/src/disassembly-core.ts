/**
 * Pure helpers for the disassembly panel.
 *
 * The component itself mostly drives the emulator bridge (disassemble
 * around PC, disassemble at address, etc.). This module holds the
 * deterministic logic: deriving the breakpoint address set, classifying
 * rows by type (PC/breakpoint/both), and formatting address/byte
 * columns.
 */

import { hex, hexBytes } from './format-core'

export interface BreakpointLike {
  id?: number
  enabled: boolean
  breakpoint: { address: number }
}

/**
 * Collect only the addresses of *enabled* breakpoints. Disabled
 * breakpoints are ignored so the UI mirror matches the emulator's
 * behaviour (disabled ~ not set).
 */
export function enabledBreakpointAddresses(
  breakpoints: ReadonlyArray<BreakpointLike> | null | undefined,
): Set<number> {
  const set = new Set<number>()
  if (!breakpoints) return set
  for (const bp of breakpoints) {
    if (bp.enabled) set.add(bp.breakpoint.address)
  }
  return set
}

export type DisasmRowKind = 'normal' | 'pc' | 'bp' | 'pc-bp'

/**
 * Classify a row based on whether its address matches the PC and/or an
 * enabled breakpoint. The caller uses this to pick CSS classes.
 */
export function classifyRow(
  address: number,
  pc: number,
  breakpoints: ReadonlySet<number>,
): DisasmRowKind {
  const isPc = address === pc
  const isBp = breakpoints.has(address)
  if (isPc && isBp) return 'pc-bp'
  if (isPc) return 'pc'
  if (isBp) return 'bp'
  return 'normal'
}

export function formatDisasmAddress(addr: number): string {
  return hex(addr, 6)
}

export function formatDisasmBytes(bytes: number[] | Uint8Array | undefined): string {
  return hexBytes(bytes)
}

/**
 * Decide whether a given address toggle should add or remove a
 * breakpoint. Returns the existing breakpoint's id when the address is
 * already present, otherwise `null`.
 */
export function findExistingBreakpointId(
  address: number,
  breakpoints: ReadonlyArray<BreakpointLike>,
): number | null {
  const hit = breakpoints.find((bp) => bp.breakpoint.address === address)
  if (!hit) return null
  return typeof hit.id === 'number' ? hit.id : null
}

/**
 * Split a target row count into a before / after pair for the
 * "around-PC" disassembly window. Uses a 40/60 split so the user's
 * eye lands a little above center (the PC row sits ~40% from the
 * top of the visible area), and forces `before >= 1` / `after >= 1`
 * floors so a `total = 2` still produces a usable window.
 *
 * Non-positive totals collapse to `{ before: 0, after: 1 }` so the
 * caller can still render at least the PC row and a single spacer.
 */
export function splitWindowCounts(total: number): { before: number; after: number } {
  if (!Number.isFinite(total) || total <= 0) return { before: 0, after: 1 }
  if (total === 1) return { before: 0, after: 1 }
  const before = Math.max(1, Math.floor(total * 0.4))
  const after = Math.max(1, total - before)
  return { before, after }
}

/**
 * Pad / truncate a list of disassembly rows to exactly `target`
 * entries. Spacer rows fill the bottom when the source list is
 * short (typical right after a cold-start or after a far branch
 * when the coverage bitmap has few entries near the new PC).
 *
 * Pure / generic so both shells can reuse it with their own row shapes.
 */
export function padDisasmRows<T>(
  rows: ReadonlyArray<T>,
  target: number,
  makeSpacer: (index: number) => T,
): T[] {
  if (!Number.isFinite(target) || target <= 0) return []
  if (rows.length >= target) return rows.slice(0, target)
  const out: T[] = rows.slice()
  for (let i = rows.length; i < target; i++) out.push(makeSpacer(i))
  return out
}
