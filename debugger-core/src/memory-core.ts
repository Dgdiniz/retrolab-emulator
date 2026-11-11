/**
 * Pure helpers for the memory panel.
 *
 * The panel wires its state to the emulator bridge; this module owns the
 * lookup tables and deterministic helpers (region address map, address
 * parsing, save-ROM decision) so they can be unit tested without a UI
 * framework or a WASM bridge.
 */

import { parseHexAddress } from './format-core'

/**
 * The Mega Drive's memory tabs. A `bus` tab is read through the 68000's
 * bus (`readMemory`) at the CPU address of its base. The VDP's VRAM is
 * not on that bus: the 68000 reaches it only through the VDP's ports,
 * where every read moves the VDP's address. So the `snapshot` tab shows
 * the core's copy of it (`getVram()`), at VRAM addresses from 0, and does
 * not edit it.
 */
export const REGION_TABS = [
  { id: 'ram', label: 'RAM', source: 'bus' },
  { id: 'rom', label: 'ROM', source: 'bus' },
  { id: 'vram', label: 'VRAM', source: 'snapshot' },
  { id: 'z80', label: 'Z80', source: 'bus' },
] as const

export type MemoryRegionId = (typeof REGION_TABS)[number]['id']

export const REGION_ADDRESSES: Record<MemoryRegionId, number> = {
  ram: 0xff0000,
  rom: 0x000000,
  vram: 0x0000,
  z80: 0xa00000,
}

/**
 * True for a tab read from the core's snapshot of a memory rather than
 * through the CPU's bus (the VRAM tab). False for an unknown id.
 */
export function isSnapshotRegion(regionId: string): boolean {
  return REGION_TABS.find((t) => t.id === regionId)?.source === 'snapshot'
}

/**
 * Return the base address for a region id, falling back to 0 for
 * unknown ids. Keeping the map out of the component makes it easy to
 * add new regions without touching the template.
 */
export function regionBaseAddress(regionId: string): number {
  return (REGION_ADDRESSES as Record<string, number>)[regionId] ?? 0
}

/**
 * Parse the goto-bar input into an absolute address. Returns `null`
 * when the user typed garbage so the caller can no-op (the panel
 * intentionally doesn't surface parse errors to the user).
 */
export function parseGotoAddress(input: string): number | null {
  return parseHexAddress(input)
}

export interface EditAbsoluteOptions {
  baseAddress: number
  offset: number
}

/**
 * Translate an offset within the currently displayed memory window to
 * an absolute CPU address. Encapsulated so tests can pin the math and
 * so the component doesn't have to redo it in multiple places.
 */
export function editAbsoluteAddress(opts: EditAbsoluteOptions): number {
  return opts.baseAddress + opts.offset
}

export interface SaveRomDecisionInput {
  wasmReady: boolean
  romLoaded: boolean
  contextType: string | null | undefined
  saving: boolean
}

/**
 * Decide whether the "Save ROM" button should be enabled. Moves the
 * disable/enable logic out of the template expression to keep it
 * testable and consistent.
 */
export function canSaveRom(input: SaveRomDecisionInput): boolean {
  if (input.saving) return false
  if (!input.wasmReady) return false
  if (!input.romLoaded) return false
  return input.contextType === 'library'
}

/**
 * Short label for a region id (used in UI buttons / tooltips). Falls
 * back to the id itself for unknown regions so the helper is total.
 */
export function regionLabel(regionId: string): string {
  const entry = REGION_TABS.find((t) => t.id === regionId)
  return entry?.label ?? regionId
}

/**
 * Append `newHash` to `seen` without duplicating. Returns the original
 * array reference when the hash is already present so callers can
 * short-circuit persistence writes (saving ROM without really changing
 * the hash list would otherwise be indistinguishable from a no-op).
 */
export function mergeSeenHashes(
  seen: ReadonlyArray<string>,
  newHash: string,
): ReadonlyArray<string> {
  if (seen.includes(newHash)) return seen
  return [...seen, newHash]
}

export interface RomHashChange {
  oldHash: string | null | undefined
  newHash: string
}

/**
 * True when the newly-computed ROM hash differs from the one currently
 * tracked by the game context. `null`/`undefined`/empty-string "old"
 * values are treated as "missing" and therefore a change.
 */
export function hasRomHashChanged(input: RomHashChange): boolean {
  const prev = input.oldHash ?? ''
  if (!prev) return !!input.newHash
  return prev !== input.newHash
}
