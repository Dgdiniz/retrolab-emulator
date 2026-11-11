// memory-regions: pure helpers that turn a core's `getMemoryRegions()`
// payload into the memory-panel's region tabs.
//
// The web memory panel reads bytes through `RetroEmulator.readMemory()`,
// which drives the *focused CPU's* address bus. So a region is only
// browsable here when it lives at a real CPU-bus address. On the SNES
// that's WRAM (`$7E:0000`); the PPU/APU memories (VRAM/OAM/CGRAM/ARAM)
// report `start_address: 0` because they sit on separate buses reached
// through I/O ports, and ROM is reached via mapper-dependent banks. Those
// get dedicated viewers separately; here we keep only what `readMemory`
// can faithfully show, so no tab ever renders garbage.

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
 * Build memory-panel tabs from a `getMemoryRegions()` payload, keeping
 * only regions addressable on the focused CPU bus (a non-zero
 * `start_address`). Returns `[]` for a missing/empty payload so the
 * caller can fall back to the goto bar.
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
