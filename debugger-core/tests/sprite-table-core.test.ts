import { describe, expect, it } from 'vitest'
import {
  buildSpriteTable,
  buildSpriteTableRow,
  formatSpriteFlips,
  formatSpriteLink,
  formatSpritePos,
  formatSpriteSize,
  formatSpriteTile,
  spriteAttrFromTableEntry,
  spriteGuideTargetId,
  type SpriteAttrInput,
  type SpriteTableEntryInput,
} from '../src/sprite-table-core'

function attr(overrides: Partial<SpriteAttrInput> = {}): SpriteAttrInput {
  return {
    index: 0,
    x: 0,
    y: 0,
    h_tiles: 1,
    v_tiles: 1,
    tile: 0,
    palette: 0,
    priority: false,
    h_flip: false,
    v_flip: false,
    link: 0,
    ...overrides,
  }
}

describe('sprite-table-core > spriteGuideTargetId', () => {
  it('produces the canonical id for the engine <-> content contract', () => {
    expect(spriteGuideTargetId(0)).toBe('vdp.sprites.entry.0')
    expect(spriteGuideTargetId(7)).toBe('vdp.sprites.entry.7')
    expect(spriteGuideTargetId(79)).toBe('vdp.sprites.entry.79')
  })
})

describe('sprite-table-core > formatters', () => {
  it('formatSpritePos renders signed coordinates', () => {
    expect(formatSpritePos(0, 0)).toBe('(0, 0)')
    expect(formatSpritePos(160, 22)).toBe('(160, 22)')
    expect(formatSpritePos(-128, -128)).toBe('(-128, -128)')
  })

  it('formatSpriteSize uses Sega-style WxH', () => {
    expect(formatSpriteSize(1, 1)).toBe('1x1')
    expect(formatSpriteSize(4, 4)).toBe('4x4')
    expect(formatSpriteSize(2, 3)).toBe('2x3')
  })

  it('formatSpriteTile pads to 4 hex digits with $ prefix', () => {
    expect(formatSpriteTile(0)).toBe('$0000')
    expect(formatSpriteTile(0x0234)).toBe('$0234')
    expect(formatSpriteTile(0xff)).toBe('$00FF')
  })

  it('formatSpriteTile masks tile to 11 bits (Genesis SAT pattern field)', () => {
    expect(formatSpriteTile(0xffff)).toBe('$07FF')
    expect(formatSpriteTile(0x1234)).toBe('$0234')
  })

  it('formatSpriteFlips uses dash for none, H/V/HV otherwise', () => {
    expect(formatSpriteFlips(false, false)).toBe('-')
    expect(formatSpriteFlips(true, false)).toBe('H')
    expect(formatSpriteFlips(false, true)).toBe('V')
    expect(formatSpriteFlips(true, true)).toBe('HV')
  })

  it('formatSpriteLink renders zero-padded decimal slot indices', () => {
    expect(formatSpriteLink(0)).toBe('00')
    expect(formatSpriteLink(7)).toBe('07')
    expect(formatSpriteLink(79)).toBe('79')
  })
})

describe('sprite-table-core > buildSpriteTableRow', () => {
  it('builds the column set for a vanilla 1x1 sprite at (32,22)', () => {
    const row = buildSpriteTableRow(
      attr({ index: 0, x: 32, y: 22, tile: 0, palette: 0, link: 1 }),
    )
    expect(row.index).toBe(0)
    expect(row.guideTargetId).toBe('vdp.sprites.entry.0')
    expect(row.columns).toEqual({
      index: '00',
      pos: '(32, 22)',
      size: '1x1',
      tile: '$0000',
      palette: 'P0',
      priority: 'low',
      flips: '-',
      link: '01',
    })
  })

  it('flags high priority sprites and renders flip flags', () => {
    const row = buildSpriteTableRow(
      attr({
        index: 5,
        x: 100,
        y: 50,
        h_tiles: 4,
        v_tiles: 2,
        tile: 0x10,
        palette: 3,
        priority: true,
        h_flip: true,
        v_flip: true,
        link: 0,
      }),
    )
    expect(row.guideTargetId).toBe('vdp.sprites.entry.5')
    expect(row.columns.size).toBe('4x2')
    expect(row.columns.tile).toBe('$0010')
    expect(row.columns.palette).toBe('P3')
    expect(row.columns.priority).toBe('high')
    expect(row.columns.flips).toBe('HV')
    expect(row.columns.link).toBe('00')
  })

  it('preserves the raw entry for tooltips / drill-downs', () => {
    const entry = attr({ index: 9, tile: 0x42, link: 11 })
    const row = buildSpriteTableRow(entry)
    expect(row.raw).toBe(entry)
  })
})

describe('sprite-table-core > buildSpriteTable', () => {
  it('returns an empty array for an empty SAT walk', () => {
    expect(buildSpriteTable([])).toEqual([])
  })

  it('preserves walk order and gives every row a unique guide target id', () => {
    const rows = buildSpriteTable([
      attr({ index: 0, link: 1 }),
      attr({ index: 1, link: 5 }),
      attr({ index: 5, link: 0 }),
    ])
    expect(rows.map((r) => r.index)).toEqual([0, 1, 5])
    expect(rows.map((r) => r.guideTargetId)).toEqual([
      'vdp.sprites.entry.0',
      'vdp.sprites.entry.1',
      'vdp.sprites.entry.5',
    ])
  })
})

describe('sprite-table-core > spriteAttrFromTableEntry', () => {
  // The first two entries of the Mega Drive's `getSpriteTable()` on the
  // capture test program, the second the last of a short chain.
  const linked: SpriteTableEntryInput = {
    index: 0, x: 37, y: 24, width: 8, height: 8, tile_index: 1, palette: 0, priority: 0,
    flip_h: false, flip_v: false, next: 1, tile_address: 32, large: false,
  }
  const last: SpriteTableEntryInput = {
    index: 1, x: -3, y: 100, width: 32, height: 16, tile_index: 0x7ff, palette: 3, priority: 1,
    flip_h: true, flip_v: false, tile_address: 0xffe0, large: false,
  }

  it('reads each field as the SAT entry holds it', () => {
    expect(spriteAttrFromTableEntry(linked)).toEqual(
      attr({ index: 0, x: 37, y: 24, h_tiles: 1, v_tiles: 1, tile: 1, link: 1 }),
    )
    expect(spriteAttrFromTableEntry(last)).toEqual(
      attr({
        index: 1, x: -3, y: 100, h_tiles: 4, v_tiles: 2, tile: 0x7ff, palette: 3,
        priority: true, h_flip: true,
      }),
    )
  })

  it('links the last entry with the null link that ends a chain', () => {
    expect(spriteAttrFromTableEntry(last).link).toBe(0)
    expect(spriteAttrFromTableEntry({ ...last, next: undefined }).link).toBe(0)
  })

  it('builds the rows the SAT entries build', () => {
    const rows = buildSpriteTable([linked, last].map(spriteAttrFromTableEntry))
    expect(rows.map((row) => row.columns)).toEqual([
      {
        index: '00', pos: '(37, 24)', size: '1x1', tile: '$0001', palette: 'P0',
        priority: 'low', flips: '-', link: '01',
      },
      {
        index: '01', pos: '(-3, 100)', size: '4x2', tile: '$07FF', palette: 'P3',
        priority: 'high', flips: 'H', link: '00',
      },
    ])
  })
})
