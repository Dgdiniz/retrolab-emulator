import { describe, expect, it } from 'vitest'
import {
  REGION_ADDRESSES,
  REGION_TABS,
  editAbsoluteAddress,
  isImageRegion,
  isReadonlyRegion,
  isSnapshotRegion,
  mergeSeenHashes,
  parseGotoAddress,
  regionBaseAddress,
  regionLabel,
} from '../src/memory-core'

describe('memory-core > REGION_TABS', () => {
  it('lists the four standard regions in order', () => {
    expect(REGION_TABS.map((t) => t.id)).toEqual(['ram', 'rom', 'vram', 'z80'])
  })

  it('reads ROM from the cartridge image, VRAM from a snapshot, RAM and Z80 through the bus', () => {
    expect(REGION_TABS.map((t) => [t.id, t.source])).toEqual([
      ['ram', 'bus'],
      ['rom', 'image'],
      ['vram', 'snapshot'],
      ['z80', 'bus'],
    ])
  })
})

describe('memory-core > isSnapshotRegion', () => {
  it('reads only the VRAM tab from a snapshot', () => {
    expect(isSnapshotRegion('vram')).toBe(true)
    expect(isSnapshotRegion('ram')).toBe(false)
    expect(isSnapshotRegion('rom')).toBe(false)
    expect(isSnapshotRegion('z80')).toBe(false)
  })

  it('is false for an id no tab has', () => {
    expect(isSnapshotRegion('unknown')).toBe(false)
  })
})

describe('memory-core > isImageRegion', () => {
  it('reads only the ROM tab from the cartridge image', () => {
    expect(isImageRegion('rom')).toBe(true)
    expect(isImageRegion('ram')).toBe(false)
    expect(isImageRegion('vram')).toBe(false)
    expect(isImageRegion('z80')).toBe(false)
  })

  it('is false for an id no tab has', () => {
    expect(isImageRegion('unknown')).toBe(false)
  })
})

describe('memory-core > isReadonlyRegion', () => {
  it('is true for the tabs read from a copy: the ROM image and the VRAM snapshot', () => {
    expect(isReadonlyRegion('rom')).toBe(true)
    expect(isReadonlyRegion('vram')).toBe(true)
  })

  it('is false for the tabs read through the bus', () => {
    expect(isReadonlyRegion('ram')).toBe(false)
    expect(isReadonlyRegion('z80')).toBe(false)
  })

  it('is false for an id no tab has, the goto window among them', () => {
    expect(isReadonlyRegion('custom')).toBe(false)
    expect(isReadonlyRegion('unknown')).toBe(false)
  })

  it('agrees with the table: read-only exactly where a tab is not read through the bus', () => {
    for (const tab of REGION_TABS) {
      expect(isReadonlyRegion(tab.id)).toBe(tab.source !== 'bus')
    }
  })
})

describe('memory-core > regionBaseAddress', () => {
  it('returns the canonical base address for known regions', () => {
    expect(regionBaseAddress('ram')).toBe(0xff0000)
    expect(regionBaseAddress('rom')).toBe(0x000000)
    expect(regionBaseAddress('vram')).toBe(0x0000)
    expect(regionBaseAddress('z80')).toBe(0xa00000)
  })

  it('falls back to 0 for unknown regions', () => {
    expect(regionBaseAddress('unknown')).toBe(0)
  })

  it('region map is exposed for direct lookup too', () => {
    expect(REGION_ADDRESSES.ram).toBe(0xff0000)
    expect(REGION_ADDRESSES.vram).toBe(0x0000)
  })
})

describe('memory-core > parseGotoAddress', () => {
  it('parses plain hex', () => {
    expect(parseGotoAddress('ff0000')).toBe(0xff0000)
    expect(parseGotoAddress('FF0000')).toBe(0xff0000)
  })

  it('handles 0x prefix and spaces', () => {
    expect(parseGotoAddress('0xdeadbe')).toBe(0xdeadbe)
    expect(parseGotoAddress('  a1 00 00 ')).toBe(0xa10000)
  })

  it('returns null for empty/garbage', () => {
    expect(parseGotoAddress('')).toBeNull()
    expect(parseGotoAddress('zzzz')).toBeNull()
  })
})

describe('memory-core > editAbsoluteAddress', () => {
  it('adds offset to base', () => {
    expect(editAbsoluteAddress({ baseAddress: 0xff0000, offset: 0x10 })).toBe(0xff0010)
  })

  it('returns base when offset is 0', () => {
    expect(editAbsoluteAddress({ baseAddress: 0x1234, offset: 0 })).toBe(0x1234)
  })
})

describe('memory-core > regionLabel', () => {
  it('returns the label for known regions', () => {
    expect(regionLabel('ram')).toBe('RAM')
    expect(regionLabel('rom')).toBe('ROM')
    expect(regionLabel('vram')).toBe('VRAM')
    expect(regionLabel('z80')).toBe('Z80')
  })

  it('falls back to the id for unknown regions', () => {
    expect(regionLabel('cram')).toBe('cram')
    expect(regionLabel('')).toBe('')
  })
})

describe('memory-core > mergeSeenHashes', () => {
  it('appends a new hash', () => {
    expect(mergeSeenHashes(['a'], 'b')).toEqual(['a', 'b'])
  })

  it('returns the same reference when the hash is already present', () => {
    const seen = ['a', 'b']
    expect(mergeSeenHashes(seen, 'a')).toBe(seen)
  })

  it('handles empty input lists', () => {
    expect(mergeSeenHashes([], 'a')).toEqual(['a'])
  })
})
