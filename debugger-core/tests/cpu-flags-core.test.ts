import { describe, expect, it } from 'vitest'
import { decodeCpuFlags, lookupRegister } from '../src/cpu-flags-core'

describe('decodeCpuFlags (68000)', () => {
  it('decodes an all-zero SR', () => {
    const out = decodeCpuFlags('m68k', 0)
    expect(out.map((f) => f.name)).toEqual(['T', 'S', 'X', 'N', 'Z', 'V', 'C'])
    expect(out.every((f) => !f.set)).toBe(true)
  })

  it.each([
    [0x8000, 'T'],
    [0x2000, 'S'],
    [0x0010, 'X'],
    [0x0008, 'N'],
    [0x0004, 'Z'],
    [0x0002, 'V'],
    [0x0001, 'C'],
  ])('mask %i isolates flag %s', (mask, expected) => {
    const out = decodeCpuFlags('m68k', mask)
    const set = out.filter((f) => f.set).map((f) => f.name)
    expect(set).toEqual([expected])
  })

  it('decodes a superposition of flags', () => {
    const sr = 0x2004 // S + Z
    const set = decodeCpuFlags('m68k', sr).filter((f) => f.set).map((f) => f.name)
    expect(set).toEqual(['S', 'Z'])
  })
})

describe('decodeCpuFlags (Z80)', () => {
  it('decodes an all-zero F register', () => {
    const out = decodeCpuFlags('z80', 0)
    expect(out.map((f) => f.name)).toEqual(['S', 'Z', 'H', 'P', 'N', 'C'])
    expect(out.every((f) => !f.set)).toBe(true)
  })

  it.each([
    [0x80, 'S'],
    [0x40, 'Z'],
    [0x10, 'H'],
    [0x04, 'P'],
    [0x02, 'N'],
    [0x01, 'C'],
  ])('mask %i isolates flag %s', (mask, expected) => {
    const out = decodeCpuFlags('z80', mask)
    const set = out.filter((f) => f.set).map((f) => f.name)
    expect(set).toEqual([expected])
  })

  it('decodes all flags set', () => {
    const set = decodeCpuFlags('z80', 0xd7) // 1101_0111 -> S Z H P N C
      .filter((f) => f.set).map((f) => f.name)
    expect(set).toEqual(['S', 'Z', 'H', 'P', 'N', 'C'])
  })
})

describe('decodeCpuFlags (65C816 P)', () => {
  it('decodes an all-zero P register', () => {
    const out = decodeCpuFlags('w65816', 0)
    expect(out.map((f) => f.name)).toEqual(['N', 'V', 'M', 'X', 'D', 'I', 'Z', 'C'])
    expect(out.every((f) => !f.set)).toBe(true)
  })

  it.each([
    [0x80, 'N'],
    [0x40, 'V'],
    [0x20, 'M'],
    [0x10, 'X'],
    [0x08, 'D'],
    [0x04, 'I'],
    [0x02, 'Z'],
    [0x01, 'C'],
  ])('mask %i isolates flag %s', (mask, expected) => {
    const set = decodeCpuFlags('w65816', mask).filter((f) => f.set).map((f) => f.name)
    expect(set).toEqual([expected])
  })

  it('decodes native 8-bit-accumulator state (M+X)', () => {
    const set = decodeCpuFlags('w65816', 0x30).filter((f) => f.set).map((f) => f.name)
    expect(set).toEqual(['M', 'X'])
  })
})

describe('decodeCpuFlags (SPC700 PSW)', () => {
  it('decodes an all-zero PSW', () => {
    const out = decodeCpuFlags('spc700', 0)
    expect(out.map((f) => f.name)).toEqual(['N', 'V', 'P', 'B', 'H', 'I', 'Z', 'C'])
    expect(out.every((f) => !f.set)).toBe(true)
  })

  it.each([
    [0x80, 'N'],
    [0x40, 'V'],
    [0x20, 'P'],
    [0x10, 'B'],
    [0x08, 'H'],
    [0x04, 'I'],
    [0x02, 'Z'],
    [0x01, 'C'],
  ])('mask %i isolates flag %s', (mask, expected) => {
    const set = decodeCpuFlags('spc700', mask).filter((f) => f.set).map((f) => f.name)
    expect(set).toEqual([expected])
  })

  it('decodes all flags set', () => {
    const set = decodeCpuFlags('spc700', 0xff).filter((f) => f.set).map((f) => f.name)
    expect(set).toEqual(['N', 'V', 'P', 'B', 'H', 'I', 'Z', 'C'])
  })
})

describe('decodeCpuFlags (Super FX SFR)', () => {
  it('decodes an all-zero SFR, high->low over the 12 GSU flags', () => {
    const out = decodeCpuFlags('superfx', 0)
    expect(out.map((f) => f.name)).toEqual([
      'IRQ', 'B', 'IH', 'IL', 'ALT2', 'ALT1', 'R', 'G', 'OV', 'S', 'CY', 'Z',
    ])
    expect(out.every((f) => !f.set)).toBe(true)
  })

  it.each([
    [0x8000, 'IRQ'],
    [0x1000, 'B'],
    [0x0800, 'IH'],
    [0x0400, 'IL'],
    [0x0200, 'ALT2'],
    [0x0100, 'ALT1'],
    [0x0040, 'R'],
    [0x0020, 'G'],
    [0x0010, 'OV'],
    [0x0008, 'S'],
    [0x0004, 'CY'],
    [0x0002, 'Z'],
  ])('mask %i isolates flag %s', (mask, expected) => {
    const set = decodeCpuFlags('superfx', mask).filter((f) => f.set).map((f) => f.name)
    expect(set).toEqual([expected])
  })

  it('ignores the unused SFR bits (0x0001, 0x2000, 0x4000)', () => {
    const set = decodeCpuFlags('superfx', 0x6001).filter((f) => f.set).map((f) => f.name)
    expect(set).toEqual([])
  })

  it('decodes a running GSU mid-ALT1 opcode (G + ALT1 + Z)', () => {
    const set = decodeCpuFlags('superfx', 0x0122).filter((f) => f.set).map((f) => f.name)
    expect(set).toEqual(['ALT1', 'G', 'Z'])
  })
})

describe('decodeCpuFlags (Cx4 HG51B169)', () => {
  it('decodes an all-zero status, high->low over N/Z/V/C', () => {
    const out = decodeCpuFlags('hg51b', 0)
    expect(out.map((f) => f.name)).toEqual(['N', 'Z', 'V', 'C'])
    expect(out.every((f) => !f.set)).toBe(true)
  })

  // The Rust `cpu_state_hg51b` packs `N<<3 | Z<<2 | V<<1 | C`.
  it.each([
    [0x8, 'N'],
    [0x4, 'Z'],
    [0x2, 'V'],
    [0x1, 'C'],
  ])('mask %i isolates flag %s', (mask, expected) => {
    const set = decodeCpuFlags('hg51b', mask).filter((f) => f.set).map((f) => f.name)
    expect(set).toEqual([expected])
  })

  it('decodes all flags set', () => {
    const set = decodeCpuFlags('hg51b', 0xf).filter((f) => f.set).map((f) => f.name)
    expect(set).toEqual(['N', 'Z', 'V', 'C'])
  })
})

describe('decodeCpuFlags (invalid input)', () => {
  it('returns [] for non-finite values', () => {
    expect(decodeCpuFlags('m68k', NaN)).toEqual([])
    expect(decodeCpuFlags('z80', Infinity)).toEqual([])
    expect(decodeCpuFlags('w65816', NaN)).toEqual([])
    expect(decodeCpuFlags('spc700', -Infinity)).toEqual([])
    expect(decodeCpuFlags('superfx', NaN)).toEqual([])
    expect(decodeCpuFlags('hg51b', NaN)).toEqual([])
  })
})

describe('lookupRegister', () => {
  it('returns the matching entry as number', () => {
    const regs = [{ name: 'D0', value: 0x42 }, { name: 'D1', value: 0x100 }]
    expect(lookupRegister(regs, 'D0')).toBe(0x42)
    expect(lookupRegister(regs, 'D1')).toBe(0x100)
  })

  it('coerces bigint values into number', () => {
    const regs = [{ name: 'PC', value: 0x123456n as unknown as bigint }]
    expect(lookupRegister(regs, 'PC')).toBe(0x123456)
  })

  it('returns 0 when missing / not an array / nullish', () => {
    expect(lookupRegister(null, 'D0')).toBe(0)
    expect(lookupRegister(undefined, 'D0')).toBe(0)
    expect(lookupRegister([], 'D0')).toBe(0)
    expect(lookupRegister([{ name: 'D1', value: 1 }], 'D0')).toBe(0)
  })
})
