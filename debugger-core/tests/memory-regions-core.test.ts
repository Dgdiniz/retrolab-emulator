import { describe, expect, it } from 'vitest'
import {
  SNES_ARAM_TAB,
  appendConsoleMemoryTabs,
  cpuAddressableMemoryTabs,
  sliceMemorySnapshot,
} from '../src/memory-regions-core'

describe('memory-regions-core > cpuAddressableMemoryTabs', () => {
  it('keeps only regions with a non-zero CPU-bus start address', () => {
    // Mirrors the SNES `getMemoryRegions()` payload: only WRAM sits on
    // the 65816 bus; VRAM/OAM/CGRAM/ARAM/ROM report start 0.
    const regions = [
      { region: 'MainRam', name: 'WRAM', start_address: 0x7e0000, size: 0x20000, writable: true },
      { region: 'VideoRam', name: 'VRAM', start_address: 0, size: 0x10000, writable: true },
      { region: { Custom: 0 }, name: 'OAM', start_address: 0, size: 0x220, writable: true },
      { region: 'AudioRam', name: 'ARAM', start_address: 0, size: 0x10000, writable: true },
      { region: 'Rom', name: 'ROM', start_address: 0, size: 0x80000, writable: false },
    ]
    const tabs = cpuAddressableMemoryTabs(regions)
    expect(tabs).toHaveLength(1)
    expect(tabs[0]).toEqual({ id: 'wram', label: 'WRAM', address: 0x7e0000, size: 0x20000 })
  })

  it('keeps every addressable region in order and slugifies names', () => {
    const tabs = cpuAddressableMemoryTabs([
      { name: 'Work RAM', start_address: 0xff0000, size: 0x10000 },
      { name: 'Extra Bank #2', start_address: 0x200000, size: 0x10000 },
    ])
    expect(tabs.map((t) => t.id)).toEqual(['work-ram', 'extra-bank-2'])
    expect(tabs.map((t) => t.address)).toEqual([0xff0000, 0x200000])
  })

  it('falls back to an index-based label when name is missing/non-string/blank', () => {
    expect(cpuAddressableMemoryTabs([{ start_address: 0x1000 }])[0].label).toBe('Region 0')
    expect(cpuAddressableMemoryTabs([{ name: 42, start_address: 0x1000 }])[0].label).toBe('Region 0')
    expect(cpuAddressableMemoryTabs([{ name: '   ', start_address: 0x1000 }])[0].label).toBe('Region 0')
  })

  it('uses an index-based slug when the name slugifies to empty', () => {
    const tabs = cpuAddressableMemoryTabs([{ name: '###', start_address: 0x10 }])
    expect(tabs[0].id).toBe('region-0')
    expect(tabs[0].label).toBe('###')
  })

  it('coerces size: keeps a valid size, zeroes a non-positive / missing one', () => {
    const tabs = cpuAddressableMemoryTabs([
      { name: 'A', start_address: 0x10, size: 0x40 },
      { name: 'B', start_address: 0x20, size: 0 },
      { name: 'C', start_address: 0x30 },
    ])
    expect(tabs.map((t) => t.size)).toEqual([0x40, 0, 0])
  })

  it('returns [] for empty / missing / non-array payloads', () => {
    expect(cpuAddressableMemoryTabs([])).toEqual([])
    expect(cpuAddressableMemoryTabs(null)).toEqual([])
    expect(cpuAddressableMemoryTabs(undefined)).toEqual([])
    expect(cpuAddressableMemoryTabs({} as never)).toEqual([])
  })

  it('ignores regions with a non-finite or negative start address', () => {
    const tabs = cpuAddressableMemoryTabs([
      { name: 'Bad', start_address: Number.NaN, size: 4 },
      { name: 'AlsoBad', start_address: -1, size: 4 },
      { name: 'Good', start_address: 0x10, size: 4 },
    ])
    expect(tabs.map((t) => t.label)).toEqual(['Good'])
  })
})

describe('memory-regions-core > appendConsoleMemoryTabs', () => {
  const wram = { id: 'wram', label: 'WRAM', address: 0x7e0000, size: 0x20000 }

  it('appends the ARAM tab for the SNES profile', () => {
    const tabs = appendConsoleMemoryTabs([wram], 'snes')
    expect(tabs).toEqual([wram, SNES_ARAM_TAB])
    expect(SNES_ARAM_TAB).toEqual({ id: 'aram', label: 'ARAM', address: 0, size: 0x10000 })
  })

  it('appends the ARAM tab for the SNES SA-1 profile too (same console)', () => {
    const tabs = appendConsoleMemoryTabs([wram], 'snes-sa1')
    expect(tabs).toEqual([wram, SNES_ARAM_TAB])
  })

  it('appends the ARAM tab for the SNES Super FX profile too (same console)', () => {
    const tabs = appendConsoleMemoryTabs([wram], 'snes-superfx')
    expect(tabs).toEqual([wram, SNES_ARAM_TAB])
  })

  it('appends the ARAM tab for the SNES Cx4 profile too (same console)', () => {
    const tabs = appendConsoleMemoryTabs([wram], 'snes-cx4')
    expect(tabs).toEqual([wram, SNES_ARAM_TAB])
  })

  it('passes other profiles through unchanged (fresh copy, not the input)', () => {
    const input = [wram]
    const tabs = appendConsoleMemoryTabs(input, 'genesis')
    expect(tabs).toEqual([wram])
    expect(tabs).not.toBe(input)
  })

  it('still appends ARAM when there are no CPU-bus tabs', () => {
    expect(appendConsoleMemoryTabs([], 'snes')).toEqual([SNES_ARAM_TAB])
  })
})

describe('memory-regions-core > sliceMemorySnapshot', () => {
  const buf = new Uint8Array([0, 1, 2, 3, 4, 5, 6, 7])

  it('returns the requested window', () => {
    expect(Array.from(sliceMemorySnapshot(buf, 2, 3))).toEqual([2, 3, 4])
  })

  it('clamps the window to the buffer end', () => {
    expect(Array.from(sliceMemorySnapshot(buf, 6, 10))).toEqual([6, 7])
  })

  it('returns empty for an out-of-range offset, non-positive length, or empty buffer', () => {
    expect(sliceMemorySnapshot(buf, 8, 4)).toHaveLength(0)
    expect(sliceMemorySnapshot(buf, -1, 4)).toHaveLength(0)
    expect(sliceMemorySnapshot(buf, 0, 0)).toHaveLength(0)
    expect(sliceMemorySnapshot(new Uint8Array(0), 0, 4)).toHaveLength(0)
  })
})
