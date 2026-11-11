import { describe, expect, it } from 'vitest'
import fc from 'fast-check'
import {
  dateToEpochMs,
  formatBytes,
  formatDate,
  hex,
  hexBytes,
  parseAddressInput,
  parseHexAddress,
} from '../src/format-core'

describe('hex', () => {
  it('pads to the requested width', () => {
    expect(hex(0, 4)).toBe('0000')
    expect(hex(1, 4)).toBe('0001')
    expect(hex(0xffff, 4)).toBe('FFFF')
    expect(hex(0xdead, 6)).toBe('00DEAD')
  })

  it('handles unsigned conversion of the sign bit', () => {
    expect(hex(0xffffffff, 8)).toBe('FFFFFFFF')
    expect(hex(-1, 8)).toBe('FFFFFFFF')
  })

  it('returns "?" placeholders for non-finite input', () => {
    expect(hex(NaN, 6)).toBe('??????')
    expect(hex(Infinity, 4)).toBe('????')
    expect(hex(-Infinity, 4)).toBe('????')
  })

  it('defaults to 6 digits (68K address space)', () => {
    expect(hex(0xabcd)).toBe('00ABCD')
  })

  it('property: roundtrip hex -> parseHexAddress returns the same unsigned value', () => {
    fc.assert(
      fc.property(fc.integer({ min: 0, max: 0x7fffffff }), (n) => {
        const s = hex(n, 8)
        expect(parseHexAddress(s)).toBe(n)
      }),
      { numRuns: 100 },
    )
  })
})

describe('hexBytes', () => {
  it('formats a plain array', () => {
    expect(hexBytes([0x4e, 0x71])).toBe('4E 71')
    expect(hexBytes([0])).toBe('00')
    expect(hexBytes([])).toBe('')
  })

  it('accepts Uint8Array and masks >8-bit values', () => {
    expect(hexBytes(new Uint8Array([0x01, 0xff, 0xab]))).toBe('01 FF AB')
    expect(hexBytes([0x1234, 0x5678])).toBe('34 78')
  })

  it('returns "" for nullish / non-array inputs', () => {
    expect(hexBytes(undefined)).toBe('')
    expect(hexBytes(null)).toBe('')
    expect(hexBytes('00 01' as unknown as number[])).toBe('')
  })
})

describe('parseHexAddress', () => {
  it.each([
    ['0', 0],
    ['1', 1],
    ['FF', 0xff],
    ['0xFF', 0xff],
    ['0XFF', 0xff],
    ['ff ff', 0xffff],
    ['  DEAD ', 0xdead],
    ['ff00ff00', 0xff00ff00],
  ])('parses "%s" -> %i', (input, expected) => {
    expect(parseHexAddress(input)).toBe(expected)
  })

  it.each([
    null,
    undefined,
    '',
    '   ',
    'xyz',
    '0xGGG',
    '0x',
    '0x-1',
  ])('rejects %j', (bad) => {
    expect(parseHexAddress(bad as string | null | undefined)).toBeNull()
  })
})

describe('parseAddressInput', () => {
  it.each([
    ['0', 0],
    ['200', 0x200],
    ['$200', 0x200],
    ['$ff', 0xff],
    ['0x200', 0x200],
    ['0XFF', 0xff],
    ['  $DEAD  ', 0xdead],
    ['ff00ff00', 0xff00ff00],
  ])('parses "%s" -> %i (accepts $/0x prefixes)', (input, expected) => {
    expect(parseAddressInput(input)).toBe(expected)
  })

  it.each([
    null,
    undefined,
    '',
    '   ',
    'EntryPoint',     // symbol resolution is the caller's job
    '$',              // prefix without digits
    '0x',
    '$xyz',
    '$-1',
  ])('rejects %j', (bad) => {
    expect(parseAddressInput(bad as string | null | undefined)).toBeNull()
  })
})

describe('formatBytes', () => {
  it('formats bytes, KB, MB, GB', () => {
    expect(formatBytes(0)).toBe('0 B')
    expect(formatBytes(512)).toBe('512 B')
    expect(formatBytes(1024)).toBe('1.0 KB')
    expect(formatBytes(1024 * 1024)).toBe('1.0 MB')
    expect(formatBytes(1024 * 1024 * 1024)).toBe('1.00 GB')
    expect(formatBytes(2.5 * 1024 * 1024 * 1024)).toBe('2.50 GB')
  })

  it('handles negatives with a sign prefix', () => {
    expect(formatBytes(-1024)).toBe('-1.0 KB')
  })

  it('returns "0 B" for non-finite values', () => {
    expect(formatBytes(NaN)).toBe('0 B')
    expect(formatBytes(Infinity)).toBe('0 B')
  })
})

describe('formatDate', () => {
  it('formats a valid ISO date', () => {
    const out = formatDate('2024-01-15T00:00:00Z')
    expect(out).toMatch(/\d{4}/) // contains the year
  })

  it('returns the dash sentinel for null/undefined', () => {
    expect(formatDate(null)).toBe('—')
    expect(formatDate(undefined)).toBe('—')
    expect(formatDate('')).toBe('—')
  })

  it('returns the dash for invalid dates', () => {
    expect(formatDate('not-a-date')).toBe('—')
  })

  it('honors a custom dash sentinel', () => {
    expect(formatDate(null, 'Never')).toBe('Never')
  })
})

describe('dateToEpochMs', () => {
  it('converts a valid date', () => {
    expect(dateToEpochMs('1970-01-01T00:00:00Z')).toBe(0)
    expect(dateToEpochMs('1970-01-01T00:00:01Z')).toBe(1000)
  })

  it('returns 0 for invalid/missing dates', () => {
    expect(dateToEpochMs(null)).toBe(0)
    expect(dateToEpochMs(undefined)).toBe(0)
    expect(dateToEpochMs('not-a-date')).toBe(0)
    expect(dateToEpochMs('')).toBe(0)
  })
})
