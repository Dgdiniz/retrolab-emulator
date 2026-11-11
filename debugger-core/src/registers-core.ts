/**
 * Pure register-table row builders for the registers panel.
 *
 * The raw `cpu.registers` array coming from the debugger is noisy: names
 * depend on CPU kind (68K vs Z80), widths vary (byte / word / long), and
 * the UI wants two columns split differently per kind. This module owns
 * those layout decisions so the component can just render the rows.
 */

import { hex } from './format-core'
import { lookupRegister, type CpuKind } from './cpu-flags-core'

export type CpuRegisterLookup = Parameters<typeof lookupRegister>[0]

export interface RegisterRow {
  name: string
  value: string
}

export interface BuildColumnsInput {
  registers: CpuRegisterLookup
  pc: number
}

/**
 * One column entry: `[registerName, widthInHexDigits]`. The special name
 * `PC` reads {@link BuildColumnsInput.pc}; every other name is looked up
 * by value from the debugger's register array.
 */
type ColumnEntry = readonly [name: string, width: number]

interface ColumnSpec {
  left: readonly ColumnEntry[]
  right: readonly ColumnEntry[]
  leftHeader: string
  rightHeader: string
}

// Per-kind two-column layouts. Widths are hex-digit counts (= register
// byte width x 2): m68k data/address are 32-bit (8), z80 is 16-bit (4),
// 65C816 registers are 16-bit (4) with a 24-bit PC (6) and 8-bit
// bank/status bytes (2), SPC700 is an 8-bit core (2) with a 16-bit PC/YA
// (4).
const COLUMN_SPECS: Record<CpuKind, ColumnSpec> = {
  m68k: {
    leftHeader: 'Data',
    rightHeader: 'Address',
    left: [
      ['D0', 8], ['D1', 8], ['D2', 8], ['D3', 8],
      ['D4', 8], ['D5', 8], ['D6', 8], ['D7', 8], ['PC', 6],
    ],
    right: [
      ['A0', 8], ['A1', 8], ['A2', 8], ['A3', 8],
      ['A4', 8], ['A5', 8], ['A6', 8], ['A7', 8], ['SR', 4],
    ],
  },
  z80: {
    leftHeader: 'Main',
    rightHeader: 'Index/Other',
    left: [['AF', 4], ['BC', 4], ['DE', 4], ['HL', 4], ['PC', 4]],
    right: [['IX', 4], ['IY', 4], ['SP', 4], ['I', 2], ['R', 2]],
  },
  // 65C816 register names match snes1's `cpu_state_65816` (`A` is the
  // 16-bit C accumulator; `S` stack, `D` direct page, `DB`/`PB` data/program
  // bank, `P` status, `E` emulation bit). PC carries the 24-bit PBR:PC.
  w65816: {
    leftHeader: 'Registers',
    rightHeader: 'Bank/Status',
    left: [['A', 4], ['X', 4], ['Y', 4], ['S', 4], ['D', 4], ['PC', 6]],
    right: [['DB', 2], ['PB', 2], ['P', 2], ['E', 1]],
  },
  // SPC700 register names match snes1's `cpu_state_spc700` (8-bit A/X/Y/SP,
  // the `YA` 16-bit pair, and the 8-bit `PSW`). PC is 16-bit.
  spc700: {
    leftHeader: 'Registers',
    rightHeader: 'Pointer/Status',
    left: [['A', 2], ['X', 2], ['Y', 2], ['PC', 4]],
    right: [['SP', 2], ['YA', 4], ['PSW', 2]],
  },
  // Super FX GSU register names match snes1's `cpu_state_gsu` (16-bit
  // `R0..R15`, `R15` = PC; then the 8-bit banks/plot bytes `PBR`/`ROMBR`/
  // `RAMBR`/`SCBR`/`SCMR`/`COLR`/`POR` and the 16-bit `CBR`). PC carries the
  // 24-bit `PBR:R15`. This name set is a hard contract with the Rust helper,
  // a mismatch silently blanks the panel, so the seam test asserts it.
  superfx: {
    leftHeader: 'Registers',
    rightHeader: 'Banks/Plot',
    left: [
      ['R0', 4], ['R1', 4], ['R2', 4], ['R3', 4],
      ['R4', 4], ['R5', 4], ['R6', 4], ['R7', 4], ['PC', 6],
    ],
    right: [
      ['R8', 4], ['R9', 4], ['R10', 4], ['R11', 4],
      ['R12', 4], ['R13', 4], ['R14', 4], ['R15', 4],
      ['PBR', 2], ['ROMBR', 2], ['RAMBR', 2], ['CBR', 4],
      ['SCBR', 2], ['SCMR', 2], ['COLR', 2], ['POR', 2],
    ],
  },
  // Cx4 HG51B169 register names match snes1's `cpu_state_hg51b` (24-bit `A`
  // accumulator + `ACCH`/`ACCL` multiply halves, 24-bit `R0..R15`, and the
  // 15-bit `P` page register). PC is the 23-bit program counter. All data
  // registers are 24-bit -> 6 hex digits; `P` is 15-bit -> 4. This name set is a
  // hard contract with the Rust helper, a mismatch silently blanks the panel,
  // so the seam test asserts it.
  hg51b: {
    leftHeader: 'Registers',
    rightHeader: 'GPR/Status',
    left: [
      ['A', 6], ['ACCH', 6], ['ACCL', 6],
      ['R0', 6], ['R1', 6], ['R2', 6], ['R3', 6],
      ['R4', 6], ['R5', 6], ['R6', 6], ['R7', 6], ['PC', 6],
    ],
    right: [
      ['R8', 6], ['R9', 6], ['R10', 6], ['R11', 6],
      ['R12', 6], ['R13', 6], ['R14', 6], ['R15', 6], ['P', 4],
    ],
  },
}

function buildColumn(entries: readonly ColumnEntry[], input: BuildColumnsInput): RegisterRow[] {
  return entries.map(([name, width]) => ({
    name,
    value: hex(name === 'PC' ? input.pc : lookupRegister(input.registers, name), width),
  }))
}

/**
 * Build the left column rows for a given CPU kind.
 */
export function buildLeftColumn(kind: CpuKind, input: BuildColumnsInput): RegisterRow[] {
  return buildColumn(COLUMN_SPECS[kind].left, input)
}

/**
 * Build the right column rows for a given CPU kind.
 */
export function buildRightColumn(kind: CpuKind, input: BuildColumnsInput): RegisterRow[] {
  return buildColumn(COLUMN_SPECS[kind].right, input)
}

/**
 * Convenience helper that returns both columns along with the header
 * labels used by the UI.
 */
export interface RegistersLayout {
  kind: CpuKind
  leftHeader: string
  rightHeader: string
  left: RegisterRow[]
  right: RegisterRow[]
}

export function buildRegistersLayout(
  kind: CpuKind,
  input: BuildColumnsInput,
): RegistersLayout {
  const spec = COLUMN_SPECS[kind]
  return {
    kind,
    leftHeader: spec.leftHeader,
    rightHeader: spec.rightHeader,
    left: buildColumn(spec.left, input),
    right: buildColumn(spec.right, input),
  }
}

/**
 * Pick the CPU kind from the debugger's `focusedCpu` index for the
 * Genesis layout. Slot 0 is the 68000 main CPU, everything else is the
 * Z80. Multi-console callers should prefer `cpuKindForSlot(profile, ...)`
 * from `debug-profile-core.ts`, which is not hard-wired to Genesis.
 */
export function cpuKindFromIndex(index: number): CpuKind {
  return index === 0 ? 'm68k' : 'z80'
}
