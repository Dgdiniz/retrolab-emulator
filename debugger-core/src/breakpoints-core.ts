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
