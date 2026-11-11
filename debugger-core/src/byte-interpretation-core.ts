/**
 * Byte interpretation decoder for the InterpretationPane that ships
 * next to the HexEditor in both UI shells.
 *
 * Given a buffer and a starting offset, returns the same byte sequence
 * interpreted as the half-dozen primitive types a debugger user
 * actually cares about: unsigned/signed 8/16/32-bit ints in both
 * endiannesses, the ASCII byte, and the binary representation of the
 * single byte. The pure layer means tests pin the endianness +
 * sign-extension math once, and both shells display the same numbers.
 */

import { hex } from './format-core'

export type Endianness = 'le' | 'be'

export interface Interpretation {
  /** Two-digit hex of the byte at `offset` (uppercase, no prefix). `'??'` when out of range. */
  hex: string
  u8: number
  i8: number
  u16le: number
  u16be: number
  i16le: number
  i16be: number
  u32le: number
  u32be: number
  i32le: number
  i32be: number
  /** Printable ASCII char or `'.'` for non-printable / out of range. */
  charAscii: string
  /** 8-character binary of the byte at `offset` (e.g. "10110100"). `'????????'` when out of range. */
  binary: string
}

const OUT_OF_RANGE_NUMBER = Number.NaN

/**
 * Decode the byte sequence starting at `offset` for display in the
 * interpretation pane. Reads of widths that go past `buffer.length`
 * report `NaN` for that field; the UI shows an em dash for those.
 *
 * Endianness is in the field name (`u16le` vs `u16be`) rather than
 * being a parameter so the pane can show both side-by-side without
 * calling the helper twice.
 */
export function decodeBytes(buffer: Uint8Array, offset: number): Interpretation {
  const len = buffer.length
  const inRange = offset >= 0 && offset < len

  const out: Interpretation = {
    hex: inRange ? hex(buffer[offset], 2) : '??',
    u8: inRange ? buffer[offset] : OUT_OF_RANGE_NUMBER,
    i8: inRange ? signExtend(buffer[offset], 8) : OUT_OF_RANGE_NUMBER,
    u16le: OUT_OF_RANGE_NUMBER,
    u16be: OUT_OF_RANGE_NUMBER,
    i16le: OUT_OF_RANGE_NUMBER,
    i16be: OUT_OF_RANGE_NUMBER,
    u32le: OUT_OF_RANGE_NUMBER,
    u32be: OUT_OF_RANGE_NUMBER,
    i32le: OUT_OF_RANGE_NUMBER,
    i32be: OUT_OF_RANGE_NUMBER,
    charAscii: inRange ? toCharAscii(buffer[offset]) : '.',
    binary: inRange ? toBinary(buffer[offset]) : '????????',
  }

  if (offset >= 0 && offset + 2 <= len) {
    const lo = buffer[offset]
    const hi = buffer[offset + 1]
    out.u16le = (lo | (hi << 8)) >>> 0
    out.u16be = (hi | (lo << 8)) >>> 0
    out.i16le = signExtend(out.u16le, 16)
    out.i16be = signExtend(out.u16be, 16)
  }

  if (offset >= 0 && offset + 4 <= len) {
    const b0 = buffer[offset]
    const b1 = buffer[offset + 1]
    const b2 = buffer[offset + 2]
    const b3 = buffer[offset + 3]
    out.u32le = (b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)) >>> 0
    out.u32be = (b3 | (b2 << 8) | (b1 << 16) | (b0 << 24)) >>> 0
    out.i32le = signExtend(out.u32le, 32)
    out.i32be = signExtend(out.u32be, 32)
  }

  return out
}

/**
 * Format a decoded unsigned value as decimal + hex. Used by the pane
 * to render rows like `52 (0x34)`. Out-of-range (NaN) renders as an em dash.
 */
export function formatDecimalHex(value: number, hexDigits: number): string {
  if (!Number.isFinite(value)) return '—'
  return `${value} (0x${hex(value, hexDigits)})`
}

/**
 * Format a decoded signed value. Negative numbers omit the parenthesised
 * hex because two's-complement-in-hex is rarely what a user actually
 * wants to read.
 */
export function formatSigned(value: number): string {
  if (!Number.isFinite(value)) return '—'
  return value.toString(10)
}

function signExtend(value: number, bits: number): number {
  // Caller (`decodeBytes`) guards against non-finite values upstream,
  // so we only handle the math here. `value` is always a small
  // non-negative integer when we get here.
  const mask = bits === 32 ? 0xffffffff : (1 << bits) - 1
  const signBit = bits === 32 ? 0x80000000 : 1 << (bits - 1)
  const v = value & mask
  if (v & signBit) {
    // For 32-bit, `v | 0` coerces to int32 (giving -2147483648 when
    // v === 0x80000000); for narrower widths we subtract 2^bits.
    if (bits === 32) return v | 0
    return v - (1 << bits)
  }
  return v
}

function toCharAscii(byte: number): string {
  if (byte >= 32 && byte < 127) return String.fromCharCode(byte)
  return '.'
}

function toBinary(byte: number): string {
  return (byte & 0xff).toString(2).padStart(8, '0')
}
