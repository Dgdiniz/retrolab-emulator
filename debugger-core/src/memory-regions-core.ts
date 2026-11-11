/**
 * Pure helpers that turn a core's `getMemoryRegions()` payload into
 * memory-panel region tabs, for cores without a hard-coded region map.
 *
 * The memory panel reads bytes through `RetroEmulator.readMemory()`, which
 * drives the *focused CPU's* address bus. So a region is only browsable
 * here when it lives at a real CPU-bus address. On the SNES that's WRAM
 * (`$7E:0000`), the PPU/APU memories (VRAM/OAM/CGRAM/ARAM) report
 * `start_address: 0` because they sit on separate buses reached through
 * I/O ports, and ROM is reached via mapper-dependent banks. Those get
 * dedicated viewers separately; here we keep only what `readMemory` can
 * faithfully show, so no tab ever renders garbage.
 *
 * (Genesis keeps its richer static `REGION_TABS` from `memory-core.ts`;
 * this module is for cores that derive their tabs at runtime.)
 */

import { isSnesFamilyProfile } from './debug-profile-core'

/** Loose shape of one `getMemoryRegions()` entry (serde snake_case). */
export interface RawMemoryRegion {
  name?: unknown
  start_address?: unknown
  size?: unknown
  writable?: unknown
}

export interface MemoryTabDescriptor {
  /** Slug used for the button id / dataset. */
  id: string
  /** Display label (the region's name). */
  label: string
  /** Absolute CPU-bus address the tab jumps to. */
  address: number
  /** Region size in bytes (informational). */
  size: number
}

function slugify(name: string, index: number): string {
  const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '')
  return slug || `region-${index}`
}

/**
 * The SPC700 audio RAM. It is NOT on the 65816 bus, so it can't be read
 * through `readMemory()` like WRAM, the panel slices it from a flat
 * `getAram()` snapshot instead. Surfaced as a read-only tab (ARAM
 * write-back is unsupported). The `address` is 0 because the slice is
 * offset-relative within the 64 KiB ARAM space, not a 65816 bus address.
 */
export const SNES_ARAM_TAB: MemoryTabDescriptor = {
  id: 'aram',
  label: 'ARAM',
  address: 0,
  size: 0x10000,
}

/**
 * Append the console-specific memory tabs that don't live on the focused
 * CPU bus (and therefore can't come out of `cpuAddressableMemoryTabs`).
 * Today only the SNES adds one: the SPC700's ARAM, read from a separate
 * `getAram()` snapshot. An SA-1 (`snes-sa1`), Super FX (`snes-superfx`) or Cx4
 * (`snes-cx4`) cartridge is the same console, so it gets the same ARAM tab.
 * Other profiles pass through unchanged.
 */
export function appendConsoleMemoryTabs(
  tabs: readonly MemoryTabDescriptor[],
  profileId: string,
): MemoryTabDescriptor[] {
  if (isSnesFamilyProfile(profileId)) return [...tabs, SNES_ARAM_TAB]
  return [...tabs]
}

/**
 * Read a window out of a flat memory snapshot (e.g. the `getAram()`
 * buffer) defensively: an out-of-range offset or non-positive length
 * yields an empty array, and the window is clamped to the buffer end so
 * the hex editor never sees more bytes than exist.
 */
export function sliceMemorySnapshot(
  buffer: Uint8Array,
  offset: number,
  length: number,
): Uint8Array {
  const off = offset | 0
  const len = length | 0
  if (off < 0 || len <= 0 || off >= buffer.length) return new Uint8Array(0)
  return buffer.slice(off, Math.min(off + len, buffer.length))
}

/**
 * Build memory-panel tabs from a `getMemoryRegions()` payload, keeping
 * only regions addressable on the focused CPU bus (a non-zero
 * `start_address`). Returns `[]` for a missing/empty payload so the caller
 * can fall back to the goto bar.
 */
export function cpuAddressableMemoryTabs(
  regions: readonly RawMemoryRegion[] | null | undefined,
): MemoryTabDescriptor[] {
  if (!Array.isArray(regions)) return []
  const tabs: MemoryTabDescriptor[] = []
  regions.forEach((r, i) => {
    const start = Number(r?.start_address)
    const name = typeof r?.name === 'string' && r.name.trim() ? r.name.trim() : `Region ${i}`
    // start_address 0 => not on the CPU bus (PPU/APU memory or a
    // mapper-banked ROM) => readMemory() at 0 would show WRAM bank 0.
    if (!Number.isFinite(start) || start <= 0) return
    const size = Number(r?.size)
    tabs.push({
      id: slugify(name, i),
      label: name,
      address: start >>> 0,
      size: Number.isFinite(size) && size > 0 ? size >>> 0 : 0,
    })
  })
  return tabs
}
