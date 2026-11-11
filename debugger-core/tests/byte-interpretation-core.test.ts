import { describe, expect, it } from 'vitest'
import {
  decodeBytes,
  formatDecimalHex,
  formatSigned,
} from '../src/byte-interpretation-core'

describe('byte-interpretation-core > decodeBytes', () => {
  it('decodes u8/i8 from a single byte', () => {
    const buf = new Uint8Array([0x80])
    const r = decodeBytes(buf, 0)
    expect(r.hex).toBe('80')
    expect(r.u8).toBe(128)
    expect(r.i8).toBe(-128)
    expect(r.binary).toBe('10000000')
    expect(r.charAscii).toBe('.')
  })

  it('decodes printable ASCII chars', () => {
    const buf = new Uint8Array([0x41])
    expect(decodeBytes(buf, 0).charAscii).toBe('A')
  })

  it('decodes u16/i16 LE and BE', () => {
    const buf = new Uint8Array([0x34, 0x12])
    const r = decodeBytes(buf, 0)
    expect(r.u16le).toBe(0x1234)
    expect(r.u16be).toBe(0x3412)
    expect(r.i16le).toBe(0x1234)
    expect(r.i16be).toBe(0x3412)
  })

  it('sign-extends negative i16 values', () => {
    const buf = new Uint8Array([0xff, 0xff])
    const r = decodeBytes(buf, 0)
    expect(r.u16le).toBe(0xffff)
    expect(r.i16le).toBe(-1)
    expect(r.i16be).toBe(-1)
  })

  it('decodes u32/i32 LE and BE', () => {
    const buf = new Uint8Array([0x78, 0x56, 0x34, 0x12])
    const r = decodeBytes(buf, 0)
    expect(r.u32le).toBe(0x12345678)
    expect(r.u32be).toBe(0x78563412)
    expect(r.i32le).toBe(0x12345678)
    expect(r.i32be).toBe(0x78563412)
  })

  it('sign-extends negative i32 values', () => {
    const buf = new Uint8Array([0x00, 0x00, 0x00, 0x80])
    const r = decodeBytes(buf, 0)
    expect(r.u32le).toBe(0x80000000)
    expect(r.i32le).toBe(-0x80000000)
  })

  it('reports NaN for fields whose width overflows the buffer', () => {
    const buf = new Uint8Array([0xaa])
    const r = decodeBytes(buf, 0)
    expect(r.u8).toBe(0xaa)
    expect(Number.isNaN(r.u16le)).toBe(true)
    expect(Number.isNaN(r.u32le)).toBe(true)
  })

  it('handles offsets near the end of the buffer correctly', () => {
    const buf = new Uint8Array([0x01, 0x02, 0x03, 0x04, 0x05])
    const r = decodeBytes(buf, 3)
    expect(r.u8).toBe(0x04)
    expect(r.u16le).toBe(0x0504)
    // Only 2 bytes left, can't read 4
    expect(Number.isNaN(r.u32le)).toBe(true)
  })

  it('returns out-of-range markers when offset is invalid', () => {
    const buf = new Uint8Array([0x01])
    const r = decodeBytes(buf, -1)
    expect(r.hex).toBe('??')
    expect(r.binary).toBe('????????')
    expect(r.charAscii).toBe('.')
    expect(Number.isNaN(r.u8)).toBe(true)
    expect(Number.isNaN(r.i8)).toBe(true)
  })

  it('returns out-of-range markers when buffer is empty', () => {
    const r = decodeBytes(new Uint8Array(), 0)
    expect(r.hex).toBe('??')
    expect(Number.isNaN(r.u8)).toBe(true)
  })

  it('encodes the binary field as a padded 8-char string', () => {
    expect(decodeBytes(new Uint8Array([0]), 0).binary).toBe('00000000')
    expect(decodeBytes(new Uint8Array([1]), 0).binary).toBe('00000001')
    expect(decodeBytes(new Uint8Array([0xff]), 0).binary).toBe('11111111')
  })
})

describe('byte-interpretation-core > formatDecimalHex', () => {
  it('formats finite values with both representations', () => {
    expect(formatDecimalHex(52, 2)).toBe('52 (0x34)')
    expect(formatDecimalHex(0xff, 2)).toBe('255 (0xFF)')
    expect(formatDecimalHex(0x1234, 4)).toBe('4660 (0x1234)')
  })

  it('renders NaN as an em-dash', () => {
    expect(formatDecimalHex(Number.NaN, 2)).toBe('—')
  })
})

describe('byte-interpretation-core > formatSigned', () => {
  it('formats negative values', () => {
    expect(formatSigned(-1)).toBe('-1')
    expect(formatSigned(-0x80000000)).toBe('-2147483648')
  })

  it('renders NaN as an em-dash', () => {
    expect(formatSigned(Number.NaN)).toBe('—')
  })
})
