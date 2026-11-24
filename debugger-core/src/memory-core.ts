/**
 * Pure helpers for the memory panel.
 *
 * The panel wires its state to the emulator bridge; this module owns the
 * Mega Drive's memory tabs (where each one reads its bytes, and which ones
 * are read-only), the address helpers (a tab's base and label, the goto
 * input, an edit's absolute address) and a list of hashes merged without
 * repeats, so they can be unit tested without a UI framework or a WASM
 * bridge.
 */

import { parseHexAddress } from './format-core'

/**
 * The Mega Drive's memory tabs, each with the source it reads its bytes
 * from:
 *
 * - `bus`: the 68000's bus (`readMemory`) at the CPU address of the tab's
 *   base, edited through it (`writeMemoryDebug`);
 * - `snapshot`: the core's copy of a memory the 68000 cannot read without
 *   changing something, which the game changes as it runs. The 68000
 *   reaches the VDP's VRAM only through the VDP's ports, where every read
 *   moves the VDP's address, so its tab shows `getVram()`;
 * - `image`: the cartridge's ROM image (`getRomBytes()`), which does not
 *   change until another ROM is loaded. The bus does not show it whole:
 *   the cartridge's window on it is 4 MiB, a larger cartridge pages its
 *   ROM into that window through its mapper's banks, and a save RAM
 *   switched in over the ROM hides the bytes under it.
 *
 * A `snapshot` or `image` tab is addressed from its first byte and is
 * read-only: an edit of the copy would never reach the machine.
 */
export const REGION_TABS = [
  { id: 'ram', label: 'RAM', source: 'bus' },
  { id: 'rom', label: 'ROM', source: 'image' },
  { id: 'vram', label: 'VRAM', source: 'snapshot' },
  { id: 'z80', label: 'Z80', source: 'bus' },
] as const

export type MemoryRegionId = (typeof REGION_TABS)[number]['id']

/** Where a tab reads its bytes from (`REGION_TABS`). */
export type MemoryRegionSource = (typeof REGION_TABS)[number]['source']

/**
 * Each tab's base address: the CPU address a `bus` tab starts at, 0 for a
 * `snapshot` or `image` tab.
 */
export const REGION_ADDRESSES: Record<MemoryRegionId, number> = {
  ram: 0xff0000,
  rom: 0x000000,
  vram: 0x0000,
  z80: 0xa00000,
}

/** The source of a tab; undefined for an id no tab has. */
function regionSource(regionId: string): MemoryRegionSource | undefined {
  return REGION_TABS.find((t) => t.id === regionId)?.source
}

/**
 * True for the tab read from the core's snapshot of a memory (the VRAM
 * tab). False for an unknown id.
 */
export function isSnapshotRegion(regionId: string): boolean {
  return regionSource(regionId) === 'snapshot'
}

/**
 * True for the tab read from the cartridge's ROM image (the ROM tab). The
 * image is the same until another ROM is loaded, and `getRomBytes()`
 * copies all of it at each call, so a host fetches it once for each ROM
 * rather than at each read. False for an unknown id.
 */
export function isImageRegion(regionId: string): boolean {
  return regionSource(regionId) === 'image'
}

/**
 * True for a tab that is not edited, one read from a copy (the ROM and
 * VRAM tabs): a write to the copy would never reach the machine. False for
 * a tab read through the bus and for an id no tab has.
 */
export function isReadonlyRegion(regionId: string): boolean {
  const source = regionSource(regionId)
  return source === 'snapshot' || source === 'image'
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
 * array reference when the hash is already present, so a caller can skip
 * a write that would change nothing.
 */
export function mergeSeenHashes(
  seen: ReadonlyArray<string>,
  newHash: string,
): ReadonlyArray<string> {
  if (seen.includes(newHash)) return seen
  return [...seen, newHash]
}
