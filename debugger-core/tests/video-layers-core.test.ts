import { describe, it, expect } from 'vitest'
import {
  parseLayerRows,
  parseVideoRegisters,
  type LayerRow,
  type VideoRegisterRow,
} from '../src/video-layers-core'

describe('parseLayerRows', () => {
  it('parses a serialised LayerData[] into display rows', () => {
    const rows = parseLayerRows([
      { id: 0, name: 'Plane A', width: 512, height: 256, scroll_x: 16, scroll_y: -8, enabled: true },
      { id: 3, name: 'Sprites', width: 320, height: 224, scroll_x: 0, scroll_y: 0, enabled: false },
    ])
    expect(rows).toEqual<LayerRow[]>([
      { id: 0, name: 'Plane A', enabled: true, size: '512×256', scrollX: 16, scrollY: -8 },
      { id: 3, name: 'Sprites', enabled: false, size: '320×224', scrollX: 0, scrollY: 0 },
    ])
  })

  it('falls back to "Layer N" when the name is missing or empty', () => {
    const rows = parseLayerRows([{ id: 2, width: 8, height: 8 }, { id: 5, name: '' }])
    expect(rows[0].name).toBe('Layer 2')
    expect(rows[1].name).toBe('Layer 5')
  })

  it('coerces loose numeric fields and treats 1/true as enabled', () => {
    const rows = parseLayerRows([
      { id: '1', name: 'BG1', width: '64', height: '64', scroll_x: '4', scroll_y: '2', enabled: 1 },
    ])
    expect(rows[0]).toEqual<LayerRow>({
      id: 1, name: 'BG1', enabled: true, size: '64×64', scrollX: 4, scrollY: 2,
    })
  })

  it('uses 0 for non-finite numeric fields', () => {
    const rows = parseLayerRows([{ id: NaN, name: 'X', width: 'oops', height: undefined }])
    expect(rows[0]).toEqual<LayerRow>({
      id: 0, name: 'X', enabled: false, size: '0×0', scrollX: 0, scrollY: 0,
    })
  })

  it('skips non-object entries and returns [] for non-arrays', () => {
    expect(parseLayerRows([null, 5, 'x', { id: 1, name: 'Keep' }])).toHaveLength(1)
    expect(parseLayerRows(null)).toEqual([])
    expect(parseLayerRows(undefined)).toEqual([])
    expect(parseLayerRows({})).toEqual([])
  })
})

describe('parseVideoRegisters', () => {
  it('formats 8-bit values as $HH and wide values as $HHHH', () => {
    const rows = parseVideoRegisters([
      { index: 0, name: 'Mode Register 1', value: 0x04, description: '' },
      { index: 15, name: 'Auto Increment', value: 0x1234, description: 'inc' },
    ])
    expect(rows).toEqual<VideoRegisterRow[]>([
      { index: 0, name: 'Mode Register 1', value: 0x04, hex: '$04', description: '' },
      { index: 15, name: 'Auto Increment', value: 0x1234, hex: '$1234', description: 'inc' },
    ])
  })

  it('defaults missing name/description to empty strings', () => {
    const rows = parseVideoRegisters([{ index: 1, value: 0 }])
    expect(rows[0]).toEqual<VideoRegisterRow>({
      index: 1, name: '', value: 0, hex: '$00', description: '',
    })
  })

  it('skips non-object entries and returns [] for non-arrays', () => {
    expect(parseVideoRegisters([null, 'x', { index: 2, name: 'R', value: 1 }])).toHaveLength(1)
    expect(parseVideoRegisters(42)).toEqual([])
  })
})
