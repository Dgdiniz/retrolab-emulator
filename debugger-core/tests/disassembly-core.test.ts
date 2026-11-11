import { describe, expect, it } from 'vitest'
import {
  classifyRow,
  enabledBreakpointAddresses,
  findExistingBreakpointId,
  formatDisasmAddress,
  formatDisasmBytes,
  padDisasmRows,
  splitWindowCounts,
} from '../src/disassembly-core'

describe('disassembly-core > enabledBreakpointAddresses', () => {
  it('returns empty set for null/undefined', () => {
    expect(enabledBreakpointAddresses(null).size).toBe(0)
    expect(enabledBreakpointAddresses(undefined).size).toBe(0)
  })

  it('collects only enabled breakpoint addresses', () => {
    const set = enabledBreakpointAddresses([
      { enabled: true, breakpoint: { address: 0x100 } },
      { enabled: false, breakpoint: { address: 0x200 } },
      { enabled: true, breakpoint: { address: 0x300 } },
    ])
    expect([...set]).toEqual([0x100, 0x300])
  })

  it('deduplicates identical addresses', () => {
    const set = enabledBreakpointAddresses([
      { enabled: true, breakpoint: { address: 0x100 } },
      { enabled: true, breakpoint: { address: 0x100 } },
    ])
    expect(set.size).toBe(1)
  })
})

describe('disassembly-core > classifyRow', () => {
  const pc = 0x200
  const bps = new Set<number>([0x100, 0x200])

  it('returns pc-bp when address is both PC and breakpoint', () => {
    expect(classifyRow(0x200, pc, bps)).toBe('pc-bp')
  })

  it('returns pc when only PC', () => {
    expect(classifyRow(0x200, pc, new Set())).toBe('pc')
  })

  it('returns bp when only breakpoint', () => {
    expect(classifyRow(0x100, pc, bps)).toBe('bp')
  })

  it('returns normal when neither', () => {
    expect(classifyRow(0x300, pc, bps)).toBe('normal')
  })
})

describe('disassembly-core > formatDisasmAddress', () => {
  it('formats as 6-digit uppercase hex', () => {
    expect(formatDisasmAddress(0)).toBe('000000')
    expect(formatDisasmAddress(0xabcdef)).toBe('ABCDEF')
  })
})

describe('disassembly-core > formatDisasmBytes', () => {
  it('joins bytes with spaces', () => {
    expect(formatDisasmBytes([0x4e, 0x71])).toBe('4E 71')
  })

  it('accepts Uint8Array', () => {
    expect(formatDisasmBytes(new Uint8Array([0x60, 0x04]))).toBe('60 04')
  })

  it('returns empty string for undefined', () => {
    expect(formatDisasmBytes(undefined)).toBe('')
  })
})

describe('disassembly-core > findExistingBreakpointId', () => {
  const bps = [
    { id: 7, enabled: true, breakpoint: { address: 0x100 } },
    { id: 8, enabled: true, breakpoint: { address: 0x200 } },
  ]

  it('returns id when address exists', () => {
    expect(findExistingBreakpointId(0x100, bps)).toBe(7)
    expect(findExistingBreakpointId(0x200, bps)).toBe(8)
  })

  it('returns null when address not present', () => {
    expect(findExistingBreakpointId(0x300, bps)).toBeNull()
  })

  it('returns null when id is missing on the hit', () => {
    expect(
      findExistingBreakpointId(0x100, [
        { enabled: true, breakpoint: { address: 0x100 } },
      ]),
    ).toBeNull()
  })
})

describe('disassembly-core > splitWindowCounts', () => {
  it('splits 50 into roughly 20/30 (40/60)', () => {
    const { before, after } = splitWindowCounts(50)
    expect(before).toBe(20)
    expect(after).toBe(30)
    expect(before + after).toBe(50)
  })

  it('splits 25 into 10/15 with totals preserved', () => {
    const { before, after } = splitWindowCounts(25)
    expect(before).toBe(10)
    expect(after).toBe(15)
    expect(before + after).toBe(25)
  })

  it('always preserves the total exactly (no rounding gap)', () => {
    for (const t of [10, 11, 23, 47, 99, 200]) {
      const { before, after } = splitWindowCounts(t)
      expect(before + after).toBe(t)
    }
  })

  it('enforces a before >= 1 floor (so the PC line is never the top row)', () => {
    expect(splitWindowCounts(2).before).toBe(1)
    expect(splitWindowCounts(2).after).toBe(1)
    expect(splitWindowCounts(3).before).toBe(1)
    expect(splitWindowCounts(3).after).toBe(2)
  })

  it('collapses sub-2 totals to a single forward row', () => {
    expect(splitWindowCounts(1)).toEqual({ before: 0, after: 1 })
    expect(splitWindowCounts(0)).toEqual({ before: 0, after: 1 })
    expect(splitWindowCounts(-5)).toEqual({ before: 0, after: 1 })
    expect(splitWindowCounts(Number.NaN)).toEqual({ before: 0, after: 1 })
    expect(splitWindowCounts(Number.POSITIVE_INFINITY)).toEqual({ before: 0, after: 1 })
  })
})

describe('disassembly-core > padDisasmRows', () => {
  const spacer = (i: number) => ({ spacer: true, index: i })

  it('pads with spacer rows to reach exactly the target', () => {
    type Row = { a: number } | { spacer: boolean; index: number }
    const rows: Row[] = [{ a: 1 }, { a: 2 }]
    const padded = padDisasmRows<Row>(rows, 5, spacer)
    expect(padded).toHaveLength(5)
    expect(padded[0]).toEqual({ a: 1 })
    expect(padded[1]).toEqual({ a: 2 })
    expect(padded[2]).toEqual({ spacer: true, index: 2 })
    expect(padded[3]).toEqual({ spacer: true, index: 3 })
    expect(padded[4]).toEqual({ spacer: true, index: 4 })
  })

  it('truncates when the source list is longer than the target', () => {
    const padded = padDisasmRows([1, 2, 3, 4, 5], 3, () => 0)
    expect(padded).toEqual([1, 2, 3])
  })

  it('returns the input verbatim when sizes match', () => {
    const rows = [1, 2, 3]
    const padded = padDisasmRows(rows, 3, () => 0)
    expect(padded).toEqual([1, 2, 3])
  })

  it('returns an empty array for non-positive targets', () => {
    expect(padDisasmRows([1, 2, 3], 0, () => 0)).toEqual([])
    expect(padDisasmRows([1, 2, 3], -1, () => 0)).toEqual([])
    expect(padDisasmRows([1, 2, 3], Number.NaN, () => 0)).toEqual([])
  })

  it('produces a fresh array (does not mutate input)', () => {
    const rows = [1, 2]
    const padded = padDisasmRows(rows, 4, () => 0)
    expect(padded).not.toBe(rows)
    expect(rows).toHaveLength(2)
  })

  it('threads the index into makeSpacer so callers can key spacer rows distinctly', () => {
    const seen: number[] = []
    padDisasmRows([1, 2], 5, (i) => {
      seen.push(i)
      return 0
    })
    expect(seen).toEqual([2, 3, 4])
  })
})
