/**
 * Per-console debugger profiles, the multi-console seam.
 *
 * A `DebugProfile` holds, as pure data, the per-console decisions of the
 * debugger UI: the CPUs in the CPU selector, the register/flag/memory
 * layouts for them, and the viewers in the panel tab bar. The UI reads them
 * from the active core instead of branching on `'sg1'`.
 *
 * Each native core references a profile by id (its core definition's `debugProfile`),
 * and the host resolves it with {@link resolveDebugProfile}. Once a core runs,
 * its `getCpus()` refines that: the CPU a cartridge adds selects the
 * cartridge's profile ({@link debugProfileIdForCpus}), and each CPU steps as
 * the core says ({@link withCoreSteppable}). Adding a new
 * console's debugger is then purely additive: register one profile here,
 * add its CPU kind(s) + flag/register tables (`cpu-flags-core.ts` /
 * `registers-core.ts`) and the labels of their traces
 * (`trace-spec-core.ts`). No component needs editing.
 */

import type { CpuKind } from './cpu-flags-core'

export interface CpuDescriptor {
  /**
   * Debugger CPU slot index. Matches the order of `getCpuStates()` and the
   * argument to `setFocusedCpu()` / `disassembleCpu()` etc. (0 = primary).
   */
  id: number
  /** Short label for the CPU selector (e.g. `68000`, `SPC700`). */
  label: string
  /** Register/flag/trace layout kind for this CPU. */
  kind: CpuKind
  /**
   * Whether this CPU can be single-stepped from the debugger. Omitted
   * means `true`. The SNES SPC700 sets it `false`: the APU advances via
   * lazy mailbox catch-up, not as a debugger-driven single-step target,
   * so the core's `stepCpu(1)` is a no-op. The toolbar disables Step
   * (and the F11 shortcut no-ops) while a non-steppable CPU is focused.
   * A running core reports it for each CPU in `getCpus()`, which
   * {@link withCoreSteppable} reads; the profile's value stands until then.
   */
  steppable?: boolean
}

/**
 * A CPU as a running core describes it in `getCpus()`: the fields the
 * profiles read.
 */
export interface CoreCpu {
  /** Its id, the debugger's CPU slot. */
  id: number
  /** Its architecture as the core names it (`M68000`, `Mos65816`, `SuperFx`, ...). */
  architecture: string
  /** Whether the core's `stepCpu(id)` steps it. */
  steppable: boolean
}

/**
 * Stable ids for the debugger's bottom info tabs. Generic tabs
 * (`breakpoints`/`history`/`callstack`/`input`) work for every core; the
 * rest are console-specific viewers a profile opts into.
 */
export type DebugInfoTabId =
  | 'breakpoints'
  | 'watchpoints'
  | 'history'
  | 'callstack'
  | 'input'
  | 'vdp'
  | 'audio'
  | 'cartridge'
  // SNES (snes1) viewers. Kept distinct from the Genesis `vdp`/`audio`/
  // `cartridge` ids because they read different WASM surfaces (PPU
  // registers/Mode 7/CGRAM/OAM vs. VDP; S-DSP voices vs. YM2612+PSG;
  // LoROM/HiROM map vs. Sega mappers) and map to different components.
  | 'ppu'
  | 'apu'
  | 'snesCartridge'

export interface DebugProfile {
  /** Stable id, referenced from a core definition's `debugProfile`. */
  id: string
  /** CPUs the debugger exposes, in slot order. Never empty. */
  cpus: readonly CpuDescriptor[]
  /** Bottom info tabs to show, in display order. */
  infoTabs: readonly DebugInfoTabId[]
}

/** Human label for an info tab id, used to build the tab bar. */
export const INFO_TAB_LABELS: Record<DebugInfoTabId, string> = {
  breakpoints: 'Breakpoints',
  watchpoints: 'Watchpoints',
  history: 'History',
  callstack: 'Call Stack',
  input: 'Input',
  vdp: 'VDP',
  audio: 'Audio',
  cartridge: 'Cartridge',
  ppu: 'PPU',
  apu: 'APU',
  snesCartridge: 'Cartridge',
}

export function infoTabLabel(id: DebugInfoTabId): string {
  return INFO_TAB_LABELS[id]
}

// Genesis (sg1): 68000 + Z80, with the Genesis VDP/Audio/Cartridge viewers wired in.
const GENESIS_PROFILE: DebugProfile = {
  id: 'genesis',
  cpus: [
    { id: 0, label: '68000', kind: 'm68k' },
    { id: 1, label: 'Z80', kind: 'z80' },
  ],
  infoTabs: [
    'breakpoints',
    'watchpoints',
    'history',
    'callstack',
    'vdp',
    'audio',
    'input',
    'cartridge',
  ],
}

// SNES (snes1): 65816 + SPC700, with the SNES-specific viewers wired in.
// `ppu`/`apu`/`snesCartridge` map to the SNES PPU, APU and cartridge panels
// (the SNES analogues of the Genesis VDP/Audio/Cartridge viewers).
const SNES_PROFILE: DebugProfile = {
  id: 'snes',
  cpus: [
    { id: 0, label: '65816', kind: 'w65816' },
    { id: 1, label: 'SPC700', kind: 'spc700', steppable: false },
  ],
  infoTabs: ['breakpoints', 'watchpoints', 'history', 'callstack', 'input', 'ppu', 'apu', 'snesCartridge'],
}

// SNES with an SA-1 cartridge: the same console as `snes` plus the SA-1's
// second 65816 as a steppable slot-2 CPU (same register/flag/trace layout as
// the 5A22, so `kind: 'w65816'`). The core's `getCpus()` selects this profile
// per cartridge (`debugProfileIdForCpus`); the info tabs are identical to
// `snes`.
const SNES_SA1_PROFILE: DebugProfile = {
  id: 'snes-sa1',
  cpus: [
    { id: 0, label: '65816', kind: 'w65816' },
    { id: 1, label: 'SPC700', kind: 'spc700', steppable: false },
    { id: 2, label: 'SA-1', kind: 'w65816' },
  ],
  infoTabs: SNES_PROFILE.infoTabs,
}

// SNES with a Super FX cartridge: the same console as `snes` plus the GSU as a
// steppable slot-2 CPU (its own `superfx` register/flag/trace layout). Unlike
// the SPC700 the GSU *is* single-steppable (`stepCpu(2)` -> the chip's
// `step_secondary_cpu`). The core's `getCpus()` selects this profile per
// cartridge (`debugProfileIdForCpus`); the info tabs are identical to `snes`.
const SNES_SUPERFX_PROFILE: DebugProfile = {
  id: 'snes-superfx',
  cpus: [
    { id: 0, label: '65816', kind: 'w65816' },
    { id: 1, label: 'SPC700', kind: 'spc700', steppable: false },
    { id: 2, label: 'GSU', kind: 'superfx' },
  ],
  infoTabs: SNES_PROFILE.infoTabs,
}

// SNES with a Cx4 cartridge (Mega Man X2/X3): the same console as `snes` plus
// the Hitachi HG51B169 DSP as a steppable slot-2 CPU (its own `hg51b`
// register/flag/trace layout). Like the GSU it *is* single-steppable
// (`stepCpu(2)` -> the chip's `step_secondary_cpu`). The core's `getCpus()`
// selects this profile per cartridge (`debugProfileIdForCpus`); the info tabs
// match `snes`.
const SNES_CX4_PROFILE: DebugProfile = {
  id: 'snes-cx4',
  cpus: [
    { id: 0, label: '65816', kind: 'w65816' },
    { id: 1, label: 'SPC700', kind: 'spc700', steppable: false },
    { id: 2, label: 'Cx4', kind: 'hg51b' },
  ],
  infoTabs: SNES_PROFILE.infoTabs,
}

const PROFILES: Record<string, DebugProfile> = {
  genesis: GENESIS_PROFILE,
  snes: SNES_PROFILE,
  'snes-sa1': SNES_SA1_PROFILE,
  'snes-superfx': SNES_SUPERFX_PROFILE,
  'snes-cx4': SNES_CX4_PROFILE,
}

/** Profile used when a core declares no (or an unknown) `debugProfile`. */
export const DEFAULT_DEBUG_PROFILE_ID = 'genesis'

/**
 * The SNES-family profile ids, the base SNES roster and every
 * cartridge-varied superset of it (`snes-sa1` adds the SA-1's second 65816;
 * `snes-superfx` adds the Super FX GSU; `snes-cx4` adds the Cx4 HG51B169).
 * Hosts gate SNES-specific behaviour (PPU/APU readback, the ARAM memory
 * tab, the SNES controller diagram) on membership here rather than
 * `=== 'snes'`, so an SA-1 / Super FX / Cx4 cart isn't misclassified as
 * Genesis.
 */
export const SNES_FAMILY_PROFILE_IDS: readonly string[] = [
  'snes',
  'snes-sa1',
  'snes-superfx',
  'snes-cx4',
]

/**
 * Whether a profile id belongs to the SNES family (see
 * {@link SNES_FAMILY_PROFILE_IDS}). Single source of truth for the
 * `=== 'snes'` gates that must also accept `snes-sa1`.
 */
export function isSnesFamilyProfile(id: string | null | undefined): boolean {
  return id != null && SNES_FAMILY_PROFILE_IDS.includes(id)
}

/**
 * The profile each architecture of a cartridge's own CPU selects, keyed by
 * the profile the core names: the SNES with an SA-1's second 65816, a
 * Super FX GSU or a Cx4 HG51B169 at slot 2.
 */
const CARTRIDGE_CPU_PROFILES: ReadonlyMap<string, ReadonlyMap<string, string>> = new Map([
  [
    'snes',
    new Map([
      ['Mos65816', 'snes-sa1'],
      ['SuperFx', 'snes-superfx'],
      ['Hg51b', 'snes-cx4'],
    ]),
  ],
])

/** The slot of the CPU a cartridge adds to its console's. */
const CARTRIDGE_CPU_SLOT = 2

/**
 * The profile id for a running core: `baseId` is the profile its core
 * definition names, `cpus` the CPUs the core reports (`getCpus()`). The
 * cartridge's CPU at slot 2 selects the profile `baseId` has for its
 * architecture; with none (no such CPU, or one `baseId` has no profile
 * for) the id is `baseId`.
 */
export function debugProfileIdForCpus(
  baseId: string,
  cpus: readonly CoreCpu[] | null | undefined,
): string {
  const cartridge = cpus?.find((cpu) => cpu.id === CARTRIDGE_CPU_SLOT)
  if (!cartridge) return baseId
  return CARTRIDGE_CPU_PROFILES.get(baseId)?.get(cartridge.architecture) ?? baseId
}

/**
 * `profile` with each CPU's `steppable` as the running core reports it in
 * `getCpus()`, matched by id; a CPU the core does not describe keeps the
 * profile's. The profile given is left as it is.
 */
export function withCoreSteppable(
  profile: DebugProfile,
  cpus: readonly CoreCpu[] | null | undefined,
): DebugProfile {
  if (!cpus || cpus.length === 0) return profile
  return {
    ...profile,
    cpus: profile.cpus.map((cpu) => {
      const described = cpus.find((core) => core.id === cpu.id)
      return described ? { ...cpu, steppable: described.steppable } : cpu
    }),
  }
}

/**
 * Resolve a profile by id, falling back to the Genesis profile for an
 * unknown / missing id so the debugger is always renderable.
 */
export function resolveDebugProfile(id: string | null | undefined): DebugProfile {
  return (id != null && PROFILES[id]) || GENESIS_PROFILE
}

/** The CPU descriptor for a slot index, or the primary CPU as fallback. */
export function cpuForSlot(profile: DebugProfile, index: number): CpuDescriptor {
  return profile.cpus.find((c) => c.id === index) ?? profile.cpus[0]
}

/**
 * Register/flag/trace `CpuKind` for a focused-CPU slot index: the
 * multi-console counterpart of the Genesis-only `cpuKindFromIndex` in
 * `registers-core.ts`.
 */
export function cpuKindForSlot(profile: DebugProfile, index: number): CpuKind {
  return cpuForSlot(profile, index).kind
}

/**
 * Inverse of {@link cpuKindForSlot}: the debugger slot index for a CPU
 * kind, or `null` when the profile has no CPU of that kind. Lets hosts
 * that key off a `CpuKind` (e.g. the learning engine, which names CPUs
 * by kind rather than slot) resolve the slot the core's `setFocusedCpu`
 * / breakpoint `cpu_id` use, without hard-coding `m68k -> 0` / `z80 -> 1`.
 */
export function slotForCpuKind(
  profile: DebugProfile,
  kind: CpuKind,
): number | null {
  const cpu = profile.cpus.find((c) => c.kind === kind)
  return cpu ? cpu.id : null
}

/** CPU selector label for a focused-CPU slot index. */
export function cpuLabelForSlot(profile: DebugProfile, index: number): string {
  return cpuForSlot(profile, index).label
}

/**
 * Whether the CPU at a focused-CPU slot index can be single-stepped.
 * Defaults to `true` (only the SNES SPC700 opts out today). The toolbar
 * uses this to disable the Step control for CPUs the core can't advance
 * one instruction at a time.
 */
export function cpuSteppableForSlot(profile: DebugProfile, index: number): boolean {
  return cpuForSlot(profile, index).steppable !== false
}
