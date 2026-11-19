// trace-ops: pure mapping from (core, focused CPU) to the trace the tracing
// UI records.
//
// The standalone debugger records ONE CPU at a time (the focused one),
// and its CPU selector offers two CPUs per core:
//
//   sg1   cpu0 = 68000,  cpu1 = Z80
//   snes1 cpu0 = 65C816, cpu1 = SPC700
//
// Every CPU's trace is read through the same methods, which take the CPU's
// id first (`setCpuTracingEnabled(cpuId, enabled)` and the rest); what the
// UI calls each CPU's trace comes from `@debugger-core/trace-spec-core`,
// keyed by CPU kind, the table RetroLab's debugger reads too; this module
// only maps a selector slot to its kind and id. `tracing.ts` resolves a
// {@link TraceCpuSpec} once at record-start and calls the by-CPU methods
// with its `cpuId`, so it never branches on core/cpu inline; it also asks
// the core for the size of an entry (`traceEntrySize`) and hands it to the
// trace worker, which cuts the export into chunks of whole entries. Keeping
// the mapping pure lets us unit-test the wiring without WASM.

import type { CpuKind } from '@debugger-core/cpu-flags-core'
import { traceCpuSpecForKind, type TraceCpuSpec } from '@debugger-core/trace-spec-core'
import type { WebCore } from './core-resolver'

export type { TraceCpuSpec }

/** The CPU kinds behind each core's two selector slots, primary first. */
const SELECTOR_CPU_KINDS: Record<WebCore, readonly [CpuKind, CpuKind]> = {
  sg1: ['m68k', 'z80'],
  snes1: ['w65816', 'spc700'],
}

/**
 * Resolve the trace of a core's focused CPU. `cpuId === 1` is each core's
 * secondary CPU (Z80 / SPC700); anything else is treated as the primary CPU
 * (68000 / 65C816), CPU 0.
 */
export function traceCpuSpec(core: WebCore, cpuId: number): TraceCpuSpec {
  const [primary, secondary] = SELECTOR_CPU_KINDS[core]
  return cpuId === 1 ? traceCpuSpecForKind(secondary, 1) : traceCpuSpecForKind(primary, 0)
}

/** The two CPU labels a core's focus selector should offer, in id order. */
export function cpuSelectorLabels(core: WebCore): [string, string] {
  return core === 'snes1' ? ['65816', 'SPC700'] : ['68000', 'Z80']
}
