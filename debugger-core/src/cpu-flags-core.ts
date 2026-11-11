/**
 * Pure CPU flag-bit decoders for the debug UI.
 *
 * The 68000 Status Register and the Z80 F register pack different sets of
 * flags at different bit positions. Keeping the tables in one pure module
 * lets us table-test every bit and guarantees the UI and the trace viewer
 * always agree on interpretation.
 */

export interface DecodedFlag {
  name: string
  set: boolean
}

/**
 * Register/flag layout kind for a debugger CPU. One per CPU family the
 * platform can inspect, not per console, a console's {@link CpuKind}s
 * come from its debug profile (see `debug-profile-core.ts`).
 *
 * Adding a console is purely additive: append its CPU kind here (e.g.
 * `'mos6502'` for the NES 2A03), add a FLAGS table below + a layout in
 * `registers-core.ts`, and reference it from a new debug profile. No
 * call site needs editing.
 */
export type CpuKind = 'm68k' | 'z80' | 'w65816' | 'spc700' | 'superfx' | 'hg51b'

// -- 68000 SR bits -----------------------------------------------------
// T S . . I2 I1 I0 . . . X N Z V C       (simplified, we expose the UI-visible subset)
const M68K_FLAGS: Array<{ name: string; mask: number }> = [
  { name: 'T', mask: 0x8000 },
  { name: 'S', mask: 0x2000 },
  { name: 'X', mask: 0x0010 },
  { name: 'N', mask: 0x0008 },
  { name: 'Z', mask: 0x0004 },
  { name: 'V', mask: 0x0002 },
  { name: 'C', mask: 0x0001 },
]

// -- Z80 F register bits -----------------------------------------------
// S Z . H . P N C
const Z80_FLAGS: Array<{ name: string; mask: number }> = [
  { name: 'S', mask: 0x80 },
  { name: 'Z', mask: 0x40 },
  { name: 'H', mask: 0x10 },
  { name: 'P', mask: 0x04 },
  { name: 'N', mask: 0x02 },
  { name: 'C', mask: 0x01 },
]

// -- 65C816 P register bits --------------------------------------------
// N V M X D I Z C  (native mode; in emulation mode bit 4 is B/break and
// bit 5 is unused, we always show the native names, matching bsnes-plus).
const W65816_FLAGS: Array<{ name: string; mask: number }> = [
  { name: 'N', mask: 0x80 },
  { name: 'V', mask: 0x40 },
  { name: 'M', mask: 0x20 },
  { name: 'X', mask: 0x10 },
  { name: 'D', mask: 0x08 },
  { name: 'I', mask: 0x04 },
  { name: 'Z', mask: 0x02 },
  { name: 'C', mask: 0x01 },
]

// -- SPC700 PSW bits ---------------------------------------------------
// N V P B H I Z C  (P = direct-page select, B = break, H = half-carry)
const SPC700_FLAGS: Array<{ name: string; mask: number }> = [
  { name: 'N', mask: 0x80 },
  { name: 'V', mask: 0x40 },
  { name: 'P', mask: 0x20 },
  { name: 'B', mask: 0x10 },
  { name: 'H', mask: 0x08 },
  { name: 'I', mask: 0x04 },
  { name: 'Z', mask: 0x02 },
  { name: 'C', mask: 0x01 },
]

// -- Super FX GSU SFR bits ---------------------------------------------
// The status flag register (`SFR`) is 16-bit. High->low over the flags a GSU
// trace cares about: IRQ (STOP interrupt latched to the S-CPU), B (immediate
// byte), IH/IL (immediate high/low prefetch), ALT2/ALT1 (opcode prefixes),
// R (R14 ROM prefetch busy), G (GO, running), OV, S (sign), CY, Z. Bit 0 and
// bits 13-14 are unused.
const SUPERFX_FLAGS: Array<{ name: string; mask: number }> = [
  { name: 'IRQ', mask: 0x8000 },
  { name: 'B', mask: 0x1000 },
  { name: 'IH', mask: 0x0800 },
  { name: 'IL', mask: 0x0400 },
  { name: 'ALT2', mask: 0x0200 },
  { name: 'ALT1', mask: 0x0100 },
  { name: 'R', mask: 0x0040 },
  { name: 'G', mask: 0x0020 },
  { name: 'OV', mask: 0x0010 },
  { name: 'S', mask: 0x0008 },
  { name: 'CY', mask: 0x0004 },
  { name: 'Z', mask: 0x0002 },
]

// -- Cx4 HG51B169 status flags -----------------------------------------
// The DSP exposes a 4-bit ALU condition set. High->low: N (negative), Z
// (zero), V (overflow), C (carry), matching the Rust `cpu_state_hg51b`
// packing (`N<<3 | Z<<2 | V<<1 | C`).
const HG51B_FLAGS: Array<{ name: string; mask: number }> = [
  { name: 'N', mask: 0x8 },
  { name: 'Z', mask: 0x4 },
  { name: 'V', mask: 0x2 },
  { name: 'C', mask: 0x1 },
]

const FLAG_TABLES: Record<CpuKind, Array<{ name: string; mask: number }>> = {
  m68k: M68K_FLAGS,
  z80: Z80_FLAGS,
  w65816: W65816_FLAGS,
  spc700: SPC700_FLAGS,
  superfx: SUPERFX_FLAGS,
  hg51b: HG51B_FLAGS,
}

export function decodeCpuFlags(kind: CpuKind, rawValue: number): DecodedFlag[] {
  if (!Number.isFinite(rawValue)) return []
  const table = FLAG_TABLES[kind]
  return table.map(({ name, mask }) => ({ name, set: (rawValue & mask) !== 0 }))
}

/**
 * Shape-agnostic lookup of a register entry by name. The debug API returns
 * `{ registers: Array<{ name, value }> }`; this helper encapsulates the
 * "maybe missing / maybe not an array" pattern.
 */
export function lookupRegister(
  registers: ReadonlyArray<{ name: string; value: number | bigint }> | undefined | null,
  name: string,
): number {
  if (!Array.isArray(registers)) return 0
  const entry = registers.find((r) => r.name === name)
  return entry ? Number(entry.value) : 0
}
