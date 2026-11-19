/**
 * The trace of each CPU, keyed by {@link CpuKind}.
 *
 * Every core traces each of its CPUs through the same `RetroEmulator`
 * methods, which take the CPU's id first (the id `getCpus()` gives, the
 * debugger's CPU slot): `setCpuTracingEnabled(cpuId, enabled)`,
 * `isCpuTracingEnabled(cpuId)`, `needsCpuTraceFlush(cpuId)`,
 * `getCpuTraceFlushData(cpuId)`, `clearCpuTraceFlushed(cpuId)`,
 * `getCpuTraceEntrySize(cpuId)` and `convertCpuTraceChunkToText(cpuId, data)`.
 * What differs from one CPU to another is only what the tracing UI calls
 * it: a label for the record button and the stats, and a token for the
 * exported file's name. The tracing UI resolves a {@link TraceCpuSpec} for
 * the CPU it records and calls those methods with the spec's `cpuId`, so it
 * never branches on core or CPU inline; keeping the table here (pure data)
 * gives both debuggers' tracing one source of truth and lets the wiring be
 * unit-tested without WASM. The size of an entry is not in the table: the
 * core reports it ({@link traceEntrySize}), so a reader cuts and counts
 * entries by the size the core writes. {@link takeTrace} takes what a CPU's
 * trace holds, which a host does whenever the core asks and once more when
 * a recording stops.
 *
 *   m68k    (sg1 cpu 0)                      68K, 68k
 *   z80     (sg1 cpu 1)                      Z80, z80
 *   w65816  (snes1 cpu 0; an SA-1's, cpu 2)  65816, 65816
 *   spc700  (snes1 cpu 1)                    SPC700, spc700
 *   superfx (snes1 cpu 2, a Super FX cartridge) GSU, gsu
 *   hg51b   (snes1 cpu 2, a Cx4 cartridge)   Cx4, cx4
 */

import type { CpuKind } from './cpu-flags-core'
import { cpuForSlot, type DebugProfile } from './debug-profile-core'

export interface TraceCpuSpec {
  /** The CPU's id, the first argument of every by-CPU trace method. */
  cpuId: number
  /** UI label for the record button / stats (`68K`, `Z80`, `65816`, `SPC700`, `GSU`, `Cx4`). */
  uiLabel: string
  /** Filename token for the exported trace (`68k`, `z80`, `65816`, `spc700`, `gsu`, `cx4`). */
  fileLabel: string
}

/** What the tracing UI calls the trace of each kind of CPU. */
const TRACE_LABELS: Record<CpuKind, Pick<TraceCpuSpec, 'uiLabel' | 'fileLabel'>> = {
  m68k: { uiLabel: '68K', fileLabel: '68k' },
  z80: { uiLabel: 'Z80', fileLabel: 'z80' },
  w65816: { uiLabel: '65816', fileLabel: '65816' },
  spc700: { uiLabel: 'SPC700', fileLabel: 'spc700' },
  superfx: { uiLabel: 'GSU', fileLabel: 'gsu' },
  hg51b: { uiLabel: 'Cx4', fileLabel: 'cx4' },
}

/** The trace spec of CPU `cpuId`, a CPU of kind `kind`. */
export function traceCpuSpecForKind(kind: CpuKind, cpuId: number): TraceCpuSpec {
  return { cpuId, ...TRACE_LABELS[kind] }
}

/**
 * The trace spec of the CPU at slot `slot` of `profile`: its kind's labels
 * and its id. A slot the profile lacks resolves to the primary CPU, as
 * {@link cpuForSlot} does.
 */
export function traceCpuSpecForSlot(profile: DebugProfile, slot: number): TraceCpuSpec {
  const cpu = cpuForSlot(profile, slot)
  return traceCpuSpecForKind(cpu.kind, cpu.id)
}

/**
 * The size in bytes of one entry of a CPU's trace, as the core reports it
 * through `getCpuTraceEntrySize(spec.cpuId)`; 0 when the emulator has no
 * such method or reports no positive whole number (a CPU that keeps no
 * trace).
 */
export function traceEntrySize(emulator: unknown, spec: TraceCpuSpec): number {
  if (typeof emulator !== 'object' || emulator === null) return 0
  const read = (emulator as Record<string, unknown>).getCpuTraceEntrySize
  if (typeof read !== 'function') return 0
  const size: unknown = read.call(emulator, spec.cpuId)
  return typeof size === 'number' && Number.isInteger(size) && size > 0 ? size : 0
}

/**
 * Take what the trace of the spec's CPU holds and clear it: a copy of the
 * entries recorded since the trace was last taken, empty when there are
 * none.
 *
 * The core asks for a trace to be taken (`needsCpuTraceFlush`) only when
 * the buffer that holds it fills. A host takes it whenever the core asks
 * while a recording runs, and once more when the recording stops, asked or
 * not: what the recording's last stretch left in the buffer is there and
 * nowhere else, and a recording too short to fill the buffer is all there.
 */
export function takeTrace(
  emulator: {
    getCpuTraceFlushData(cpuId: number): Uint8Array
    clearCpuTraceFlushed(cpuId: number): void
  },
  spec: TraceCpuSpec,
): Uint8Array {
  const bytes = emulator.getCpuTraceFlushData(spec.cpuId)
  emulator.clearCpuTraceFlushed(spec.cpuId)
  return bytes
}
