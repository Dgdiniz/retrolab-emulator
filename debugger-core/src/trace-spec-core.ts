/**
 * Per-CPU trace binding specs, keyed by {@link CpuKind}.
 *
 * Each CPU's trace ring is driven by parallel-but-differently-named
 * `RetroEmulator` methods. The tracing UI
 * resolves a {@link TraceCpuSpec} for the focused CPU and then calls
 * `emulator[spec.<fn>](...)` dynamically, so it never branches on core/CPU
 * inline. Keeping the name + entry-size table here (pure data) gives both
 * debuggers' tracing one source of truth and lets the wiring be unit-tested
 * without WASM.
 *
 *   m68k   (sg1 cpu0)  setTracingEnabled / getTraceFlushData / ...
 *   z80    (sg1 cpu1)  setZ80TracingEnabled / getZ80TraceFlushData / ...
 *   w65816 (snes1 cpu0) setTracingEnabled / getTraceFlushData / ... (same names as the primary sg1 channel)
 *   spc700 (snes1 cpu1) setSpc700TracingEnabled / getSpc700TraceFlushData / ...
 *   superfx (snes1 cpu2, a Super FX cartridge) setSuperfxTracingEnabled / getSuperfxTraceFlushData / ...
 *   hg51b  (snes1 cpu2, a Cx4 cartridge) setHg51bTracingEnabled / getHg51bTraceFlushData / ...
 */

import type { CpuKind } from './cpu-flags-core'

export interface TraceCpuSpec {
  /** `RetroEmulator` method: enable/disable this CPU's trace ring. */
  setEnabled: string
  /** Method: does a trace ring need flushing to the worker? */
  needsFlush: string
  /** Method: drain the trace ring as a `Uint8Array`. */
  getFlushData: string
  /** Method: mark the drained bytes as consumed. */
  clearFlushed: string
  /** Method: turn a binary chunk into human-readable trace text. */
  convertChunkToText: string
  /** Bytes per on-disk trace entry (export chunk alignment). */
  entrySize: number
  /** Bytes per progress-stats entry (the live "N entries" estimate). */
  statsEntrySize: number
  /** UI label for the record button / stats (`68K`, `Z80`, `65816`, `SPC700`, `GSU`, `Cx4`). */
  uiLabel: string
  /** Filename token for the exported trace (`68k`, `z80`, `65816`, `spc700`, `gsu`, `cx4`). */
  fileLabel: string
}

const M68K: TraceCpuSpec = {
  setEnabled: 'setTracingEnabled',
  needsFlush: 'needsTraceFlush',
  getFlushData: 'getTraceFlushData',
  clearFlushed: 'clearTraceFlushed',
  convertChunkToText: 'convertTraceChunkToText',
  entrySize: 84,
  statsEntrySize: 74,
  uiLabel: '68K',
  fileLabel: '68k',
}

const Z80: TraceCpuSpec = {
  setEnabled: 'setZ80TracingEnabled',
  needsFlush: 'needsZ80TraceFlush',
  getFlushData: 'getZ80TraceFlushData',
  clearFlushed: 'clearZ80TraceFlushed',
  convertChunkToText: 'convertZ80TraceChunkToText',
  entrySize: 38,
  statsEntrySize: 34,
  uiLabel: 'Z80',
  fileLabel: 'z80',
}

const W65816: TraceCpuSpec = {
  // snes1's primary CPU shares the generic trace binding names with sg1.
  setEnabled: 'setTracingEnabled',
  needsFlush: 'needsTraceFlush',
  getFlushData: 'getTraceFlushData',
  clearFlushed: 'clearTraceFlushed',
  convertChunkToText: 'convertTraceChunkToText',
  entrySize: 44,
  statsEntrySize: 44,
  uiLabel: '65816',
  fileLabel: '65816',
}

const SPC700: TraceCpuSpec = {
  setEnabled: 'setSpc700TracingEnabled',
  // snes1 exposes a single shared `needsTraceFlush()` (any-CPU), not a
  // per-CPU variant, reuse it for the SPC700.
  needsFlush: 'needsTraceFlush',
  getFlushData: 'getSpc700TraceFlushData',
  clearFlushed: 'clearSpc700TraceFlushed',
  convertChunkToText: 'convertSpc700TraceChunkToText',
  entrySize: 24,
  statsEntrySize: 24,
  uiLabel: 'SPC700',
  fileLabel: 'spc700',
}

const SUPERFX: TraceCpuSpec = {
  setEnabled: 'setSuperfxTracingEnabled',
  // Same shared any-CPU `needsTraceFlush()` as the SPC700 channel.
  needsFlush: 'needsTraceFlush',
  getFlushData: 'getSuperfxTraceFlushData',
  clearFlushed: 'clearSuperfxTraceFlushed',
  convertChunkToText: 'convertSuperfxTraceChunkToText',
  // Must equal the Rust `TRACE_ENTRY_SIZE_GSU` (52 bytes: master_cycle(8)
  // pbr_pc(4) sfr(2) rombr(1) rambr(1) bytes_len(1) bytes(3) r[16](32)).
  entrySize: 52,
  statsEntrySize: 52,
  uiLabel: 'GSU',
  fileLabel: 'gsu',
}

const HG51B: TraceCpuSpec = {
  setEnabled: 'setHg51bTracingEnabled',
  // Same shared any-CPU `needsTraceFlush()` as the SPC700/GSU channels.
  needsFlush: 'needsTraceFlush',
  getFlushData: 'getHg51bTraceFlushData',
  clearFlushed: 'clearHg51bTraceFlushed',
  convertChunkToText: 'convertHg51bTraceChunkToText',
  // Must equal the Rust `TRACE_ENTRY_SIZE_CX4` (93 bytes: cycles(8) pc(4)
  // opcode(2) p(2) flags(1) a(4) acch(4) accl(4) gpr[16](64)).
  entrySize: 93,
  statsEntrySize: 93,
  uiLabel: 'Cx4',
  fileLabel: 'cx4',
}

const TRACE_SPECS: Record<CpuKind, TraceCpuSpec> = {
  m68k: M68K,
  z80: Z80,
  w65816: W65816,
  spc700: SPC700,
  superfx: SUPERFX,
  hg51b: HG51B,
}

/** Trace binding spec for a CPU kind. */
export function traceCpuSpecForKind(kind: CpuKind): TraceCpuSpec {
  return TRACE_SPECS[kind]
}
