import { describe, expect, it } from 'vitest'
import {
  buildLeftColumn,
  buildRegistersLayout,
  buildRightColumn,
  cpuKindFromIndex,
} from '../src/registers-core'

describe('registers-core > cpuKindFromIndex', () => {
  it('maps index 0 to m68k', () => {
    expect(cpuKindFromIndex(0)).toBe('m68k')
  })

  it('maps anything else to z80', () => {
    expect(cpuKindFromIndex(1)).toBe('z80')
    expect(cpuKindFromIndex(42)).toBe('z80')
  })
})

describe('registers-core > buildLeftColumn (m68k)', () => {
  it('renders D0..D7 + PC with 8-hex data and 6-hex PC', () => {
    const registers = Array.from({ length: 8 }, (_, i) => ({ name: `D${i}`, value: i }))
    const rows = buildLeftColumn('m68k', { registers, pc: 0xabcdef })
    expect(rows.map((r) => r.name)).toEqual(['D0', 'D1', 'D2', 'D3', 'D4', 'D5', 'D6', 'D7', 'PC'])
    expect(rows[0].value).toBe('00000000')
    expect(rows[1].value).toBe('00000001')
    expect(rows[rows.length - 1].value).toBe('ABCDEF')
  })

  it('returns zeros for missing registers', () => {
    const rows = buildLeftColumn('m68k', { registers: [], pc: 0 })
    expect(rows[0].value).toBe('00000000')
    expect(rows[rows.length - 1].value).toBe('000000')
  })
})

describe('registers-core > buildRightColumn (m68k)', () => {
  it('renders A0..A7 + SR with correct widths', () => {
    const registers = [
      { name: 'A0', value: 0xff0000 },
      { name: 'A7', value: 0x12345678 },
      { name: 'SR', value: 0x2704 },
    ]
    const rows = buildRightColumn('m68k', { registers, pc: 0 })
    expect(rows.map((r) => r.name)).toEqual([
      'A0', 'A1', 'A2', 'A3', 'A4', 'A5', 'A6', 'A7', 'SR',
    ])
    expect(rows[0].value).toBe('00FF0000')
    expect(rows[7].value).toBe('12345678')
    expect(rows[8].value).toBe('2704')
  })
})

describe('registers-core > buildLeftColumn (z80)', () => {
  it('renders AF/BC/DE/HL + PC with 4-hex width', () => {
    const registers = [
      { name: 'AF', value: 0x0100 },
      { name: 'BC', value: 0x1234 },
      { name: 'DE', value: 0xabcd },
      { name: 'HL', value: 0xffff },
    ]
    const rows = buildLeftColumn('z80', { registers, pc: 0x0100 })
    expect(rows.map((r) => r.name)).toEqual(['AF', 'BC', 'DE', 'HL', 'PC'])
    expect(rows[0].value).toBe('0100')
    expect(rows[2].value).toBe('ABCD')
    expect(rows[3].value).toBe('FFFF')
    expect(rows[4].value).toBe('0100')
  })
})

describe('registers-core > buildRightColumn (z80)', () => {
  it('renders IX/IY/SP + I + R with the right widths', () => {
    const registers = [
      { name: 'IX', value: 0x1000 },
      { name: 'IY', value: 0x2000 },
      { name: 'SP', value: 0xffff },
      { name: 'I', value: 0x3f },
      { name: 'R', value: 0xae },
    ]
    const rows = buildRightColumn('z80', { registers, pc: 0 })
    expect(rows.map((r) => r.name)).toEqual(['IX', 'IY', 'SP', 'I', 'R'])
    expect(rows[0].value).toBe('1000')
    expect(rows[3].value).toBe('3F')
    expect(rows[4].value).toBe('AE')
  })
})

describe('registers-core > buildLeftColumn (65C816)', () => {
  it('renders A/X/Y/S/D (4-hex) + PC (6-hex)', () => {
    const registers = [
      { name: 'A', value: 0x1234 },
      { name: 'X', value: 0x00ab },
      { name: 'Y', value: 0xffff },
      { name: 'S', value: 0x01ff },
      { name: 'D', value: 0x0000 },
    ]
    const rows = buildLeftColumn('w65816', { registers, pc: 0x81abcd })
    expect(rows.map((r) => r.name)).toEqual(['A', 'X', 'Y', 'S', 'D', 'PC'])
    expect(rows[0].value).toBe('1234')
    expect(rows[1].value).toBe('00AB')
    expect(rows[2].value).toBe('FFFF')
    expect(rows[5].value).toBe('81ABCD')
  })
})

describe('registers-core > buildRightColumn (65C816)', () => {
  it('renders DB/PB/P (2-hex) + E (1-hex)', () => {
    const registers = [
      { name: 'DB', value: 0x7e },
      { name: 'PB', value: 0x81 },
      { name: 'P', value: 0x34 },
      { name: 'E', value: 0x01 },
    ]
    const rows = buildRightColumn('w65816', { registers, pc: 0 })
    expect(rows.map((r) => r.name)).toEqual(['DB', 'PB', 'P', 'E'])
    expect(rows[0].value).toBe('7E')
    expect(rows[1].value).toBe('81')
    expect(rows[2].value).toBe('34')
    expect(rows[3].value).toBe('1')
  })
})

describe('registers-core > buildLeftColumn (SPC700)', () => {
  it('renders A/X/Y (2-hex) + PC (4-hex)', () => {
    const registers = [
      { name: 'A', value: 0xab },
      { name: 'X', value: 0x00 },
      { name: 'Y', value: 0xff },
    ]
    const rows = buildLeftColumn('spc700', { registers, pc: 0xffc0 })
    expect(rows.map((r) => r.name)).toEqual(['A', 'X', 'Y', 'PC'])
    expect(rows[0].value).toBe('AB')
    expect(rows[2].value).toBe('FF')
    expect(rows[3].value).toBe('FFC0')
  })
})

describe('registers-core > buildRightColumn (SPC700)', () => {
  it('renders SP (2-hex), YA (4-hex), PSW (2-hex)', () => {
    const registers = [
      { name: 'SP', value: 0xef },
      { name: 'YA', value: 0xff00 },
      { name: 'PSW', value: 0x02 },
    ]
    const rows = buildRightColumn('spc700', { registers, pc: 0 })
    expect(rows.map((r) => r.name)).toEqual(['SP', 'YA', 'PSW'])
    expect(rows[0].value).toBe('EF')
    expect(rows[1].value).toBe('FF00')
    expect(rows[2].value).toBe('02')
  })
})

describe('registers-core > buildLeftColumn (Super FX GSU)', () => {
  it('renders R0..R7 (4-hex) + PC (6-hex)', () => {
    const registers = Array.from({ length: 16 }, (_, i) => ({ name: `R${i}`, value: i * 0x1111 }))
    const rows = buildLeftColumn('superfx', { registers, pc: 0x0100ff })
    expect(rows.map((r) => r.name)).toEqual(['R0', 'R1', 'R2', 'R3', 'R4', 'R5', 'R6', 'R7', 'PC'])
    expect(rows[0].value).toBe('0000')
    expect(rows[1].value).toBe('1111')
    expect(rows[7].value).toBe('7777')
    // PC carries the 24-bit PBR:R15.
    expect(rows[8].value).toBe('0100FF')
  })
})

describe('registers-core > buildRightColumn (Super FX GSU)', () => {
  it('renders R8..R15 (4-hex) + banks/plot bytes (2-hex) + CBR (4-hex)', () => {
    const registers = [
      { name: 'R8', value: 0x8888 },
      { name: 'R15', value: 0x0104 },
      { name: 'PBR', value: 0x01 },
      { name: 'ROMBR', value: 0x00 },
      { name: 'RAMBR', value: 0x01 },
      { name: 'CBR', value: 0xfff0 },
      { name: 'SCBR', value: 0x20 },
      { name: 'SCMR', value: 0x2c },
      { name: 'COLR', value: 0xff },
      { name: 'POR', value: 0x10 },
    ]
    const rows = buildRightColumn('superfx', { registers, pc: 0 })
    expect(rows.map((r) => r.name)).toEqual([
      'R8', 'R9', 'R10', 'R11', 'R12', 'R13', 'R14', 'R15',
      'PBR', 'ROMBR', 'RAMBR', 'CBR', 'SCBR', 'SCMR', 'COLR', 'POR',
    ])
    expect(rows[0].value).toBe('8888')
    // R15 also appears as a raw 16-bit register (PC is the PBR:R15 pair).
    expect(rows[7].value).toBe('0104')
    expect(rows[8].value).toBe('01') // PBR (2-hex)
    expect(rows[10].value).toBe('01') // RAMBR (2-hex)
    expect(rows[11].value).toBe('FFF0') // CBR (4-hex)
    expect(rows[13].value).toBe('2C') // SCMR
  })
})

describe('registers-core > buildLeftColumn (Cx4 HG51B169)', () => {
  it('renders A/ACCH/ACCL (6-hex) + R0..R7 (6-hex) + PC (6-hex)', () => {
    const registers = [
      { name: 'A', value: 0xabcdef },
      { name: 'ACCH', value: 0x001234 },
      { name: 'ACCL', value: 0x005678 },
      ...Array.from({ length: 16 }, (_, i) => ({ name: `R${i}`, value: i * 0x111111 })),
    ]
    const rows = buildLeftColumn('hg51b', { registers, pc: 0x0100ff })
    expect(rows.map((r) => r.name)).toEqual([
      'A', 'ACCH', 'ACCL', 'R0', 'R1', 'R2', 'R3', 'R4', 'R5', 'R6', 'R7', 'PC',
    ])
    // 24-bit registers render as 6 hex digits.
    expect(rows[0].value).toBe('ABCDEF')
    expect(rows[1].value).toBe('001234')
    expect(rows[3].value).toBe('000000') // R0
    expect(rows[10].value).toBe('777777') // R7
    // PC is the 23-bit program counter (6-hex).
    expect(rows[11].value).toBe('0100FF')
  })
})

describe('registers-core > buildRightColumn (Cx4 HG51B169)', () => {
  it('renders R8..R15 (6-hex) + P (4-hex)', () => {
    const registers = [
      ...Array.from({ length: 16 }, (_, i) => ({ name: `R${i}`, value: i * 0x111111 })),
      { name: 'P', value: 0x1234 },
    ]
    const rows = buildRightColumn('hg51b', { registers, pc: 0 })
    expect(rows.map((r) => r.name)).toEqual([
      'R8', 'R9', 'R10', 'R11', 'R12', 'R13', 'R14', 'R15', 'P',
    ])
    expect(rows[0].value).toBe('888888') // R8 (6-hex)
    expect(rows[7].value).toBe('FFFFFF') // R15 (6-hex)
    expect(rows[8].value).toBe('1234') // P (15-bit -> 4-hex)
  })
})

describe('registers-core > buildRegistersLayout', () => {
  it('returns headers matching the CPU kind', () => {
    const mk = buildRegistersLayout('m68k', { registers: [], pc: 0 })
    expect(mk.leftHeader).toBe('Data')
    expect(mk.rightHeader).toBe('Address')
    const z = buildRegistersLayout('z80', { registers: [], pc: 0 })
    expect(z.leftHeader).toBe('Main')
    expect(z.rightHeader).toBe('Index/Other')
    const s = buildRegistersLayout('w65816', { registers: [], pc: 0 })
    expect(s.leftHeader).toBe('Registers')
    expect(s.rightHeader).toBe('Bank/Status')
    const a = buildRegistersLayout('spc700', { registers: [], pc: 0 })
    expect(a.leftHeader).toBe('Registers')
    expect(a.rightHeader).toBe('Pointer/Status')
    const fx = buildRegistersLayout('superfx', { registers: [], pc: 0 })
    expect(fx.leftHeader).toBe('Registers')
    expect(fx.rightHeader).toBe('Banks/Plot')
    const cx4 = buildRegistersLayout('hg51b', { registers: [], pc: 0 })
    expect(cx4.leftHeader).toBe('Registers')
    expect(cx4.rightHeader).toBe('GPR/Status')
  })

  it('produces 9-row columns for 68k', () => {
    const layout = buildRegistersLayout('m68k', { registers: [], pc: 0 })
    expect(layout.left).toHaveLength(9)
    expect(layout.right).toHaveLength(9)
  })

  it('produces 5-row columns for z80', () => {
    const layout = buildRegistersLayout('z80', { registers: [], pc: 0 })
    expect(layout.left).toHaveLength(5)
    expect(layout.right).toHaveLength(5)
  })

  it('produces 6-left/4-right columns for 65C816', () => {
    const layout = buildRegistersLayout('w65816', { registers: [], pc: 0 })
    expect(layout.left).toHaveLength(6)
    expect(layout.right).toHaveLength(4)
  })

  it('produces 4-left/3-right columns for SPC700', () => {
    const layout = buildRegistersLayout('spc700', { registers: [], pc: 0 })
    expect(layout.left).toHaveLength(4)
    expect(layout.right).toHaveLength(3)
  })

  it('produces 9-left/16-right columns for Super FX', () => {
    const layout = buildRegistersLayout('superfx', { registers: [], pc: 0 })
    // Left: R0..R7 + PC (9). Right: R8..R15 (8) + 8 banks/plot regs (16).
    expect(layout.left).toHaveLength(9)
    expect(layout.right).toHaveLength(16)
  })

  it('produces 12-left/9-right columns for Cx4', () => {
    const layout = buildRegistersLayout('hg51b', { registers: [], pc: 0 })
    // Left: A/ACCH/ACCL + R0..R7 + PC (12). Right: R8..R15 (8) + P (9).
    expect(layout.left).toHaveLength(12)
    expect(layout.right).toHaveLength(9)
  })
})
