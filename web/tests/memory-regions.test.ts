import { describe, it, expect } from 'vitest'
import { cpuAddressableMemoryTabs } from '../src/memory-regions'

describe('cpuAddressableMemoryTabs', () => {
  it('keeps only regions with a non-zero CPU-bus start address', () => {
    // Mirrors the SNES `getMemoryRegions()` payload: only WRAM sits on
    // the 65816 bus; VRAM/OAM/CGRAM/ARAM/ROM/SRAM report start 0.
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

  it('falls back to an index-based label/slug when name is missing', () => {
    const tabs = cpuAddressableMemoryTabs([{ start_address: 0x1000 }])
    expect(tabs[0].label).toBe('Region 0')
    expect(tabs[0].id).toBe('region-0')
    expect(tabs[0].size).toBe(0)
  })

  it('returns [] for empty / missing / non-array payloads', () => {
    expect(cpuAddressableMemoryTabs([])).toEqual([])
    expect(cpuAddressableMemoryTabs(null)).toEqual([])
    expect(cpuAddressableMemoryTabs(undefined)).toEqual([])
    // Defensive: a non-array (shouldn't happen) must not throw.
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
