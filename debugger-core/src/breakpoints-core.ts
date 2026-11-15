/**
 * Framework-free helpers for the editable E/R/W breakpoint model shared
 * by the RetroLab platform's breakpoints panel and the OSS
 * web debugger panel (`breakpoints-panel.ts`). Neither UI re-implements
 * the parsing/labelling so they stay in lockstep.
 *
 * The Rust `BreakpointType` crosses the WASM boundary as an
 * externally-tagged serde enum:
 *
 *   - execute/read/write breakpoints  -> `{ Access: { exec, read, write } }`
 *   - event breakpoints               -> `{ Event: { code, param } }`
 *
 * These helpers fold the raw shape into a flat {@link AccessFlags}, map
 * to/from the four shorthand strings the `addBreakpoint(bp_type)` WASM
 * arg accepts, and format compact labels for the row UI. Everything is
 * tolerant of legacy/hand-written string inputs (`"execute"`, `"Read"`,
 * even the historical `"execution"`) so persisted files and lesson JSON
 * keep round-tripping.
 *
 * They also read what each CPU checks ({@link BreakpointChecks}, from
 * `getBreakpointChecks(cpuId)`), so both panels offer only the breakpoints
 * a CPU checks and say why they offer less.
 */

/** Independent execute / read / write checks on one breakpoint. */
export interface AccessFlags {
  exec: boolean
  read: boolean
  write: boolean
}

/** The four canonical strings the WASM `addBreakpoint(bp_type)` accepts. */
export type BpShorthand = 'execute' | 'read' | 'write' | 'readwrite'

/**
 * The `bp_type` value as it can appear before normalization: the
 * tagged-object form from WASM, a shorthand/legacy string, or nullish.
 */
export type RawBpType =
  | { Access: Partial<AccessFlags> }
  | { Event: { code: number; param: number } }
  | string
  | null
  | undefined

/** All-false flags (no checks). A breakpoint in this state is inert. */
export const EMPTY_ACCESS: AccessFlags = { exec: false, read: false, write: false }

function coerceBool(v: unknown): boolean {
  return v === true
}

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key)
}

/**
 * Map a shorthand/legacy string to flags. Tolerant of casing and the
 * historical `"execution"` spelling. Anything unrecognised (including
 * `null`) falls back to execute-only, matching the WASM `add_breakpoint`
 * default so the UI never silently produces an inert breakpoint.
 */
export function shorthandToAccess(shorthand: string | null | undefined): AccessFlags {
  switch ((shorthand ?? '').toLowerCase()) {
    case 'read':
      return { exec: false, read: true, write: false }
    case 'write':
      return { exec: false, read: false, write: true }
    case 'readwrite':
      return { exec: false, read: true, write: true }
    default:
      // 'execute', 'execution', '', or any unknown value.
      return { exec: true, read: false, write: false }
  }
}

/** Whether the raw `bp_type` is the event-hook variant (not access). */
export function isEventBpType(raw: RawBpType): boolean {
  return !!raw && typeof raw === 'object' && hasOwn(raw, 'Event')
}

/**
 * Fold a raw `bp_type` into {@link AccessFlags}. Event breakpoints carry
 * no read/write/execute semantics and normalize to {@link EMPTY_ACCESS}.
 */
export function parseAccessFlags(raw: RawBpType): AccessFlags {
  if (raw && typeof raw === 'object') {
    if (hasOwn(raw, 'Access')) {
      const a = (raw as { Access: Partial<AccessFlags> }).Access ?? {}
      return {
        exec: coerceBool(a.exec),
        read: coerceBool(a.read),
        write: coerceBool(a.write),
      }
    }
    // Event (or any other tagged object): no access semantics.
    return { ...EMPTY_ACCESS }
  }
  // Not a tagged object: a shorthand/legacy string or nullish. The
  // explicit string guard keeps the call well-typed even when the
  // consumer's tsconfig leaves `strictNullChecks` off.
  return shorthandToAccess(typeof raw === 'string' ? raw : null)
}

/**
 * Collapse flags into the single best shorthand the WASM add-arg can
 * express. Memory direction wins over execute so a write watch that also
 * happens to have exec set still reports as a write (the `access` field
 * remains the lossless source of truth for the UI).
 */
export function accessToShorthand(flags: AccessFlags): BpShorthand {
  if (flags.read && flags.write) return 'readwrite'
  if (flags.write) return 'write'
  if (flags.read) return 'read'
  return 'execute'
}

/** True when no check is enabled, caller should drop the breakpoint. */
export function isEmptyAccess(flags: AccessFlags): boolean {
  return !flags.exec && !flags.read && !flags.write
}

/** Structural equality over the three flags. */
export function accessEquals(a: AccessFlags, b: AccessFlags): boolean {
  return a.exec === b.exec && a.read === b.read && a.write === b.write
}

/** Return a copy of `flags` with one bit flipped. */
export function toggleFlag(flags: AccessFlags, which: keyof AccessFlags): AccessFlags {
  return { ...flags, [which]: !flags[which] }
}

/**
 * Compact uppercase label for a row, in `E R W` order: `"E"`, `"RW"`,
 * `"ERW"`, ... or `"-"` when inert. Stable enough to assert in tests and
 * short enough to sit next to the address column.
 */
export function accessLabel(flags: AccessFlags): string {
  let out = ''
  if (flags.exec) out += 'E'
  if (flags.read) out += 'R'
  if (flags.write) out += 'W'
  return out === '' ? '-' : out
}

/**
 * Stable dedup key over the flags (`"erw"` subset, `"none"` when inert).
 * Used by the sync merge so two breakpoints at the same address/CPU but
 * different access combos don't collapse into one.
 */
export function accessKey(flags: AccessFlags): string {
  const key = `${flags.exec ? 'e' : ''}${flags.read ? 'r' : ''}${flags.write ? 'w' : ''}`
  return key === '' ? 'none' : key
}

// -- What each CPU checks ---------------------------------------------
//
// A console's CPUs do not all check breakpoints: a sound CPU may check
// none, or only some kinds. The core says what each CPU checks through
// `getBreakpointChecks(cpuId)`, and the panels offer a user only that,
// saying why where they offer less.

/** What a CPU's breakpoints are checked for, as `getBreakpointChecks(cpuId)` reports it. */
export interface BreakpointChecks {
  /** Execute breakpoints stop the CPU before the instruction at their address. */
  execute: boolean
  /** Read and write watchpoints stop it after an instruction that touched their bytes. */
  memory: boolean
  /** A breakpoint's condition is evaluated against the CPU's registers and memory. */
  conditions: boolean
}

/**
 * Every check. A core that cannot say what a CPU checks (a build older
 * than `getBreakpointChecks`) is taken to check everything, so a panel
 * offers what it offered before cores said.
 */
export const ALL_BREAKPOINT_CHECKS: Readonly<BreakpointChecks> = Object.freeze({
  execute: true,
  memory: true,
  conditions: true,
})

/**
 * Normalize a raw `getBreakpointChecks` result. Anything that is not an
 * object, and any field that is not a boolean, counts as checked: what a
 * core does not say is offered, as before cores said.
 */
export function parseBreakpointChecks(raw: unknown): BreakpointChecks {
  if (typeof raw !== 'object' || raw === null) return { ...ALL_BREAKPOINT_CHECKS }
  const fields = raw as Record<string, unknown>
  const read = (v: unknown): boolean => (typeof v === 'boolean' ? v : true)
  return { execute: read(fields.execute), memory: read(fields.memory), conditions: read(fields.conditions) }
}

/**
 * What CPU `cpuId` of `emulator` checks: its `getBreakpointChecks(cpuId)`,
 * or every check when the emulator has no such method or the call throws.
 */
export function breakpointChecksOf(emulator: unknown, cpuId: number): BreakpointChecks {
  const read =
    typeof emulator === 'object' && emulator !== null
      ? (emulator as { getBreakpointChecks?: unknown }).getBreakpointChecks
      : undefined
  if (typeof read !== 'function') return { ...ALL_BREAKPOINT_CHECKS }
  try {
    return parseBreakpointChecks(read.call(emulator, cpuId))
  } catch {
    return { ...ALL_BREAKPOINT_CHECKS }
  }
}

/** The access flags a CPU's checks let a user set: execute, and read and write when memory is checked. */
export function checkedAccess(checks: BreakpointChecks): AccessFlags {
  return { exec: checks.execute, read: checks.memory, write: checks.memory }
}

/**
 * Whether a breakpoint of these flags is one to offer on a CPU with these
 * checks: it watches something, and nothing its CPU does not check.
 */
export function isAccessOffered(flags: AccessFlags, checks: BreakpointChecks): boolean {
  if (isEmptyAccess(flags)) return false
  return (!flags.exec || checks.execute) && (!(flags.read || flags.write) || checks.memory)
}

/**
 * The access a click in a disassembly or code gutter leaves on the CPU's
 * breakpoint at an address: `current` (that breakpoint's access, or
 * {@link EMPTY_ACCESS} when there is none) with its exec bit flipped. An
 * access left empty means the breakpoint goes. Clearing the bit is always
 * allowed; setting it is refused, `null`, on a CPU that checks no execute
 * breakpoints, where one would never stop it.
 */
export function gutterExecToggle(current: AccessFlags, checks: BreakpointChecks): AccessFlags | null {
  if (!current.exec && !checks.execute) return null
  return toggleFlag(current, 'exec')
}

/**
 * Why a CPU is not offered every kind of breakpoint, as one sentence for a
 * panel, or `null` when it checks everything. `cpuName` names the CPU
 * (`"Z80"`); without one the sentence says "this CPU".
 */
export function breakpointChecksNote(checks: BreakpointChecks, cpuName?: string | null): string | null {
  const cpu = cpuName ? `the ${cpuName}` : 'this CPU'
  if (!checks.execute && !checks.memory) return `The emulator checks no breakpoints on ${cpu}.`
  const unchecked: string[] = []
  if (!checks.execute) unchecked.push('execute breakpoints')
  if (!checks.memory) unchecked.push('read and write watchpoints')
  if (!checks.conditions) unchecked.push('conditions')
  return unchecked.length ? `The emulator does not check ${unchecked.join(' or ')} on ${cpu}.` : null
}

/**
 * Why part of a breakpoint is never checked, or `null` when its CPU checks
 * all of it: an access its CPU does not check, or a condition its CPU never
 * evaluates. The sentence is {@link breakpointChecksNote}'s, about the CPU.
 */
export function breakpointUncheckedNote(
  flags: AccessFlags,
  hasCondition: boolean,
  checks: BreakpointChecks,
  cpuName?: string | null,
): string | null {
  const unchecked =
    (flags.exec && !checks.execute) ||
    ((flags.read || flags.write) && !checks.memory) ||
    (hasCondition && !checks.conditions)
  return unchecked ? breakpointChecksNote(checks, cpuName) : null
}
