import { describe, expect, it } from 'vitest'
import {
  DEFAULT_BYTES_PER_ROW_CANDIDATES,
  ROW_HEIGHT,
  VISIBLE_PADDING,
  buildVisibleRows,
  clampCursor,
  clampWindowSize,
  computeBytesPerRow,
  computeVisibleRange,
  computeWindowAnchor,
  createModifiedColorizer,
  decideEditKey,
  isByteSelected,
  parseHexEdit,
  totalRowCount,
  windowCoversVisibleRange,
} from '../src/hex-editor-core'

describe('hex-editor-core > computeVisibleRange', () => {
  it('returns an empty range when rowHeight is non-positive', () => {
    expect(computeVisibleRange(0, 400, 0, 100, 4)).toEqual({ start: 0, end: 0 })
    expect(computeVisibleRange(0, 400, -10, 100, 4)).toEqual({ start: 0, end: 0 })
    expect(computeVisibleRange(0, 400, NaN, 100, 4)).toEqual({ start: 0, end: 0 })
  })

  it('clamps negative scrollTop to zero', () => {
    const r = computeVisibleRange(-500, 400, 20, 100, 0)
    expect(r.start).toBe(0)
    expect(r.end).toBe(20)
  })

  it('pads visible range at the top with the padding amount', () => {
    const r = computeVisibleRange(20 * 30, 400, 20, 100, 4)
    expect(r.start).toBe(26) // floor(600/20) - 4
    // ceil(400/20) + 4*2 visible rows after the padded start
    expect(r.end).toBe(54)
  })

  it('clamps end to totalRows', () => {
    const r = computeVisibleRange(20 * 95, 400, 20, 100, 4)
    expect(r.end).toBe(100)
    expect(r.start).toBeLessThanOrEqual(r.end)
  })

  it('returns an empty range when there are no rows', () => {
    expect(computeVisibleRange(0, 400, 20, 0, 4)).toEqual({ start: 0, end: 0 })
  })

  it('handles zero container height gracefully', () => {
    const r = computeVisibleRange(0, 0, 20, 100, 4)
    expect(r.start).toBe(0)
    // Only the padding window is visible
    expect(r.end).toBe(8)
  })

  it('treats padding of 0 as no padding', () => {
    const r = computeVisibleRange(100, 200, 20, 100, 0)
    expect(r.start).toBe(5)
    expect(r.end).toBe(15)
  })

  it('exports the default row constants', () => {
    expect(ROW_HEIGHT).toBe(20)
    expect(VISIBLE_PADDING).toBe(4)
  })
})

describe('hex-editor-core > totalRowCount', () => {
  it('returns 0 for empty buffers', () => {
    expect(totalRowCount(0, 16)).toBe(0)
  })

  it('ceils to include short trailing rows', () => {
    expect(totalRowCount(33, 16)).toBe(3)
  })

  it('returns 0 when bytesPerRow is invalid', () => {
    expect(totalRowCount(100, 0)).toBe(0)
    expect(totalRowCount(100, -1)).toBe(0)
    expect(totalRowCount(100, NaN)).toBe(0)
  })
})

describe('hex-editor-core > buildVisibleRows', () => {
  const data = new Uint8Array(Array.from({ length: 40 }, (_, i) => i))

  it('renders rows with correct address, bytes and ASCII', () => {
    const rows = buildVisibleRows(data, { start: 0, end: 2 }, 16, 0)
    expect(rows).toHaveLength(2)
    expect(rows[0].offset).toBe(0)
    expect(rows[0].address).toBe('000000')
    expect(rows[0].bytes).toHaveLength(16)
    expect(rows[0].bytes[0]).toEqual({ offset: 0, value: 0, hex: '00', color: null })
    expect(rows[0].bytes[15]).toEqual({ offset: 15, value: 15, hex: '0F', color: null })
    expect(rows[1].offset).toBe(16)
    expect(rows[1].address).toBe('000010')
  })

  it('truncates the last row when data runs out', () => {
    const rows = buildVisibleRows(data, { start: 2, end: 3 }, 16, 0)
    expect(rows).toHaveLength(1)
    // Remaining 8 bytes
    expect(rows[0].bytes).toHaveLength(8)
    expect(rows[0].bytes.at(-1)?.offset).toBe(39)
  })

  it('uses baseAddress when computing addresses', () => {
    const rows = buildVisibleRows(data, { start: 0, end: 1 }, 16, 0xff0000)
    expect(rows[0].address).toBe('FF0000')
  })

  it('applies colorizer to each byte cell', () => {
    const rows = buildVisibleRows(
      data,
      { start: 0, end: 1 },
      4,
      0,
      (offset, value) => (value % 2 === 0 ? `#c${offset}` : null),
    )
    expect(rows[0].bytes[0].color).toBe('#c0')
    expect(rows[0].bytes[1].color).toBeNull()
    expect(rows[0].bytes[2].color).toBe('#c2')
  })

  it('builds ASCII column with printable bytes and dots', () => {
    const mixed = new Uint8Array([0x48, 0x69, 0x00, 0x7f, 0x41])
    const rows = buildVisibleRows(mixed, { start: 0, end: 1 }, 5, 0)
    expect(rows[0].ascii).toBe('Hi..A')
  })

  it('emits per-char asciiCells aligned with bytes', () => {
    const mixed = new Uint8Array([0x48, 0x69, 0x00, 0x7f, 0x41])
    const rows = buildVisibleRows(mixed, { start: 0, end: 1 }, 5, 0)
    expect(rows[0].asciiCells).toEqual([
      { offset: 0, char: 'H' },
      { offset: 1, char: 'i' },
      { offset: 2, char: '.' },
      { offset: 3, char: '.' },
      { offset: 4, char: 'A' },
    ])
    expect(rows[0].asciiCells).toHaveLength(rows[0].bytes.length)
  })

  it('returns [] when bytesPerRow is invalid', () => {
    expect(buildVisibleRows(data, { start: 0, end: 2 }, 0, 0)).toEqual([])
    expect(buildVisibleRows(data, { start: 0, end: 2 }, -1, 0)).toEqual([])
  })

  it('skips ranges past data length entirely', () => {
    const rows = buildVisibleRows(data, { start: 100, end: 200 }, 16, 0)
    expect(rows).toEqual([])
  })

  it('normalizes reversed ranges to empty output', () => {
    const rows = buildVisibleRows(data, { start: 5, end: 2 }, 16, 0)
    expect(rows).toEqual([])
  })

  it('treats missing colorizer as null colors', () => {
    const rows = buildVisibleRows(data, { start: 0, end: 1 }, 4, 0, null)
    expect(rows[0].bytes.every((b) => b.color === null)).toBe(true)
  })

  it('emits region-relative offsets when dataBaseOffset is given (windowed mode)', () => {
    // Pretend `data` is a window starting at region offset 0x100, so
    // row 16 (offset 0x100) should render with byte offset 0x100.
    const windowed = new Uint8Array(Array.from({ length: 32 }, (_, i) => 0x80 + i))
    const rows = buildVisibleRows(windowed, { start: 16, end: 18 }, 16, 0xff0000, null, 0x100)
    expect(rows).toHaveLength(2)
    expect(rows[0].offset).toBe(0x100)
    expect(rows[0].address).toBe('FF0100')
    expect(rows[0].bytes[0]).toEqual({ offset: 0x100, value: 0x80, hex: '80', color: null })
    expect(rows[0].bytes[15]).toEqual({ offset: 0x10f, value: 0x8f, hex: '8F', color: null })
    expect(rows[0].asciiCells[0].offset).toBe(0x100)
  })

  it('skips rows that sit before/after the windowed slice', () => {
    const windowed = new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd])
    // Window covers region offsets [16, 20). Iterating r=0..6 with
    // bpr=4 visits region offsets 0,4,8,12,16,20, only r=4 lands
    // inside the window.
    const rows = buildVisibleRows(windowed, { start: 0, end: 6 }, 4, 0, null, 16)
    expect(rows).toHaveLength(1)
    expect(rows[0].offset).toBe(16)
    expect(rows[0].bytes).toHaveLength(4)
    expect(rows[0].bytes[0]).toEqual({ offset: 16, value: 0xaa, hex: 'AA', color: null })
    expect(rows[0].bytes[3]).toEqual({ offset: 19, value: 0xdd, hex: 'DD', color: null })
  })

  it('returns no rows when the entire visible range is before the window', () => {
    const windowed = new Uint8Array([0xaa, 0xbb, 0xcc, 0xdd])
    const rows = buildVisibleRows(windowed, { start: 0, end: 2 }, 4, 0, null, 16)
    expect(rows).toEqual([])
  })
})

describe('hex-editor-core > parseHexEdit', () => {
  it('returns null for empty input', () => {
    expect(parseHexEdit('')).toBeNull()
  })

  it('pads single hex digit with trailing zero', () => {
    expect(parseHexEdit('a')).toBe(0xa0)
    expect(parseHexEdit('F')).toBe(0xf0)
    expect(parseHexEdit('1')).toBe(0x10)
  })

  it('parses two hex digits case-insensitively', () => {
    expect(parseHexEdit('ff')).toBe(0xff)
    expect(parseHexEdit('FF')).toBe(0xff)
    expect(parseHexEdit('1a')).toBe(0x1a)
  })

  it('returns null when the input is too long', () => {
    expect(parseHexEdit('abc')).toBeNull()
  })

  it('returns null when the input contains non-hex chars', () => {
    expect(parseHexEdit('gz')).toBeNull()
    expect(parseHexEdit(' a')).toBeNull()
    expect(parseHexEdit('a-')).toBeNull()
  })
})

describe('hex-editor-core > decideEditKey', () => {
  it('cancels on Escape', () => {
    expect(decideEditKey('Escape', '')).toEqual({ kind: 'cancel' })
  })

  it('moves right on Tab and ArrowRight', () => {
    expect(decideEditKey('Tab', 'a')).toEqual({ kind: 'commit-and-move', delta: 1 })
    expect(decideEditKey('ArrowRight', 'a')).toEqual({ kind: 'commit-and-move', delta: 1 })
  })

  it('moves left on ArrowLeft', () => {
    expect(decideEditKey('ArrowLeft', '')).toEqual({ kind: 'commit-and-move', delta: -1 })
  })

  it('appends hex chars while the buffer has room', () => {
    expect(decideEditKey('a', '')).toEqual({ kind: 'append', char: 'a' })
    expect(decideEditKey('F', '')).toEqual({ kind: 'append', char: 'f' })
    expect(decideEditKey('0', '')).toEqual({ kind: 'append', char: '0' })
  })

  it('commits-and-advances once the buffer would be full', () => {
    expect(decideEditKey('a', '5')).toEqual({ kind: 'commit-and-advance' })
  })

  it('ignores unrelated keys', () => {
    expect(decideEditKey('Shift', '')).toEqual({ kind: 'ignore' })
    expect(decideEditKey('g', '')).toEqual({ kind: 'ignore' })
    expect(decideEditKey('@', '')).toEqual({ kind: 'ignore' })
  })
})

describe('hex-editor-core > isByteSelected', () => {
  it('returns false when no selection is active', () => {
    expect(isByteSelected(5, null, null)).toBe(false)
    expect(isByteSelected(5, undefined, undefined)).toBe(false)
  })

  it('treats a single anchor as an inclusive single-byte selection', () => {
    expect(isByteSelected(5, 5, null)).toBe(true)
    expect(isByteSelected(4, 5, null)).toBe(false)
  })

  it('handles forward ranges inclusively', () => {
    expect(isByteSelected(4, 2, 6)).toBe(true)
    expect(isByteSelected(2, 2, 6)).toBe(true)
    expect(isByteSelected(6, 2, 6)).toBe(true)
    expect(isByteSelected(7, 2, 6)).toBe(false)
  })

  it('handles reversed ranges identically', () => {
    expect(isByteSelected(4, 6, 2)).toBe(true)
    expect(isByteSelected(1, 6, 2)).toBe(false)
  })
})

describe('hex-editor-core > clampCursor', () => {
  it('clamps to [0, length-1]', () => {
    expect(clampCursor(5, 10, 8)).toBe(7)
    expect(clampCursor(0, -5, 8)).toBe(0)
    expect(clampCursor(4, 1, 8)).toBe(5)
  })

  it('returns 0 when byteLength is zero', () => {
    expect(clampCursor(4, 1, 0)).toBe(0)
  })

  it('returns the original offset when delta is non-finite', () => {
    expect(clampCursor(4, NaN, 8)).toBe(4)
  })
})

describe('hex-editor-core > computeBytesPerRow', () => {
  // Default cost per row: 8 (addr) + cols * 3 (hex) + 2 (pad) + cols * 1 (ascii)
  // = 10 + 4 * cols. With charWidth 10px we need (10 + 4 * cols) * 10 <= width.

  it('picks the smallest candidate when container width is too small', () => {
    expect(computeBytesPerRow(50, 10)).toBe(8)
  })

  it('picks the largest candidate that fits', () => {
    // 16 cols -> (10 + 64) * 10 = 740px
    // 24 cols -> (10 + 96) * 10 = 1060px
    expect(computeBytesPerRow(800, 10)).toBe(16)
    expect(computeBytesPerRow(1100, 10)).toBe(24)
  })

  it('honours a custom candidates list', () => {
    // 4 cols -> (10 + 16) * 10 = 260px
    // 12 cols -> (10 + 48) * 10 = 580px
    expect(computeBytesPerRow(300, 10, { candidates: [4, 12] })).toBe(4)
    expect(computeBytesPerRow(600, 10, { candidates: [4, 12] })).toBe(12)
  })

  it('falls back to 8 when candidates list is empty', () => {
    expect(computeBytesPerRow(1000, 10, { candidates: [] })).toBe(8)
  })

  it('handles non-finite or non-positive inputs', () => {
    expect(computeBytesPerRow(0, 10)).toBe(8)
    expect(computeBytesPerRow(-100, 10)).toBe(8)
    expect(computeBytesPerRow(NaN, 10)).toBe(8)
    expect(computeBytesPerRow(800, 0)).toBe(8)
    expect(computeBytesPerRow(800, NaN)).toBe(8)
  })

  it('exports a sensible default candidate ladder', () => {
    expect(DEFAULT_BYTES_PER_ROW_CANDIDATES).toEqual([8, 16, 24, 32, 48, 64])
  })

  it('respects custom layout overrides', () => {
    // No ASCII at all: pad=0, asciiCellsPerByte=0
    // 16 cols -> (8 + 48) * 10 = 560px
    const cols = computeBytesPerRow(600, 10, {
      asciiColumnPadding: 0,
      asciiCellsPerByte: 0,
    })
    expect(cols).toBe(16)
  })
})

describe('hex-editor-core > createModifiedColorizer', () => {
  it('paints modified offsets with the default colour', () => {
    const modified = new Set([2, 5])
    const colorizer = createModifiedColorizer(modified)
    expect(colorizer(2, 0)).toBe('var(--accent-warn, #e89e64)')
    expect(colorizer(5, 0)).toBe('var(--accent-warn, #e89e64)')
    expect(colorizer(0, 0)).toBeNull()
  })

  it('uses a custom colour when provided', () => {
    const colorizer = createModifiedColorizer(new Set([1]), { color: '#ff00ff' })
    expect(colorizer(1, 0)).toBe('#ff00ff')
  })

  it('falls through to fallback for unmodified offsets', () => {
    const fallback = (offset: number) => (offset === 0 ? '#abc' : null)
    const colorizer = createModifiedColorizer(new Set([1]), { fallbackColorizer: fallback })
    expect(colorizer(0, 0)).toBe('#abc')
    expect(colorizer(1, 0)).toBe('var(--accent-warn, #e89e64)')
    expect(colorizer(2, 0)).toBeNull()
  })

  it('reflects mutations to the captured set without rebuilding', () => {
    const modified = new Set<number>()
    const colorizer = createModifiedColorizer(modified)
    expect(colorizer(3, 0)).toBeNull()
    modified.add(3)
    expect(colorizer(3, 0)).toBe('var(--accent-warn, #e89e64)')
    modified.delete(3)
    expect(colorizer(3, 0)).toBeNull()
  })
})

describe('hex-editor-core > clampWindowSize', () => {
  it('returns 0 for empty/invalid regions', () => {
    expect(clampWindowSize(1024, 0)).toBe(0)
    expect(clampWindowSize(1024, -10)).toBe(0)
    expect(clampWindowSize(1024, NaN)).toBe(0)
  })

  it('returns totalSize when the request is non-positive', () => {
    expect(clampWindowSize(0, 4096)).toBe(4096)
    expect(clampWindowSize(-1, 4096)).toBe(4096)
  })

  it('clamps to totalSize when the request is larger', () => {
    expect(clampWindowSize(1024 * 1024, 4096)).toBe(4096)
  })

  it('returns the request when it fits', () => {
    expect(clampWindowSize(1024, 64 * 1024)).toBe(1024)
  })
})

describe('hex-editor-core > computeWindowAnchor', () => {
  it('returns 0 for empty/invalid regions', () => {
    expect(computeWindowAnchor(100, { windowSize: 1024, totalSize: 0 })).toBe(0)
  })

  it('centres the window on the desired offset', () => {
    // windowSize 1000 -> half = 500; desired 5000 -> base = 4500
    expect(computeWindowAnchor(5000, { windowSize: 1000, totalSize: 10000 })).toBe(4500)
  })

  it('clamps base to [0, totalSize - windowSize]', () => {
    expect(computeWindowAnchor(0, { windowSize: 1000, totalSize: 10000 })).toBe(0)
    expect(computeWindowAnchor(10000, { windowSize: 1000, totalSize: 10000 })).toBe(9000)
  })

  it('snaps base down to a bytesPerRow boundary', () => {
    // desired 1000, windowSize 200 -> centre would give base = 900,
    // snap down to nearest multiple of 16 = 896.
    expect(computeWindowAnchor(1000, { windowSize: 200, totalSize: 4096, bytesPerRow: 16 })).toBe(896)
    // desired 503, windowSize 100 -> centre would give base = 453,
    // snap down to nearest multiple of 16 = 448.
    expect(computeWindowAnchor(503, { windowSize: 100, totalSize: 4096, bytesPerRow: 16 })).toBe(448)
  })

  it('handles regions smaller than the requested window', () => {
    // windowSize > totalSize -> effective windowSize = totalSize, base must be 0
    expect(computeWindowAnchor(100, { windowSize: 10000, totalSize: 1000 })).toBe(0)
  })

  it('keeps the snapped base valid when clamped at the right edge', () => {
    // totalSize 1000, windowSize 100 -> maxBase 900 (already multiple of 16? 900/16=56.25 -> no)
    // snap-down of 900 = 896
    expect(computeWindowAnchor(1000, { windowSize: 100, totalSize: 1000, bytesPerRow: 16 })).toBe(896)
  })
})

describe('hex-editor-core > windowCoversVisibleRange', () => {
  it('returns false when the window is empty', () => {
    const ok = windowCoversVisibleRange(
      { base: 0, length: 0 },
      0,
      100,
      { edgeMarginBytes: 64, totalSize: 4096 },
    )
    expect(ok).toBe(false)
  })

  it('returns true when the visible range fits inside the window with margin', () => {
    const ok = windowCoversVisibleRange(
      { base: 0x1000, length: 0x4000 },
      0x2000,
      0x2200,
      { edgeMarginBytes: 0x100, totalSize: 0x10000 },
    )
    expect(ok).toBe(true)
  })

  it('rejects when the visible range is too close to the left edge', () => {
    const ok = windowCoversVisibleRange(
      { base: 0x1000, length: 0x4000 },
      0x1010, // inside window, but less than 0x100 margin from base
      0x1200,
      { edgeMarginBytes: 0x100, totalSize: 0x10000 },
    )
    expect(ok).toBe(false)
  })

  it('rejects when the visible range is too close to the right edge', () => {
    const ok = windowCoversVisibleRange(
      { base: 0x1000, length: 0x1000 },
      0x1800,
      0x1ff0, // close to windowEnd (0x2000), only 0x10 of margin
      { edgeMarginBytes: 0x100, totalSize: 0x10000 },
    )
    expect(ok).toBe(false)
  })

  it('accepts being at the region start (no left re-anchor possible)', () => {
    const ok = windowCoversVisibleRange(
      { base: 0, length: 0x1000 },
      0x10, // close to start but base is already 0
      0x200,
      { edgeMarginBytes: 0x100, totalSize: 0x10000 },
    )
    expect(ok).toBe(true)
  })

  it('accepts being at the region end (no right re-anchor possible)', () => {
    const ok = windowCoversVisibleRange(
      { base: 0xf000, length: 0x1000 }, // windowEnd = 0x10000 = totalSize
      0xfff0,
      0xfffe,
      { edgeMarginBytes: 0x100, totalSize: 0x10000 },
    )
    expect(ok).toBe(true)
  })
})
