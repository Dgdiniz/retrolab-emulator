/**
 * Framework-free helpers for the advanced "watchpoint" match conditions
 * (address range, value predicate, hit-skip, action, one-shot) and the
 * per-core event catalog, shared by the RetroLab platform
 * (its watchpoints panel) and the OSS web debugger
 * (`watchpoints-panel.ts`). Both UIs normalize, label, validate, and
 * serialize through here so they stay in lockstep with the Rust
 * `MatchSpec` / `EventClass` types that cross the WASM boundary.
 *
 * Two shapes exist on purpose:
 *
 *   - {@link MatchSpec}, the camelCase form the Vue/TS UIs work with.
 *   - {@link WireMatchSpec}, the snake_case form serde expects, produced
 *     by `list_breakpoints().match_spec` and consumed by
 *     `setBreakpointMatchSpec`.
 *
 * {@link parseMatchSpec} accepts either casing (so persisted JSON and the
 * raw WASM object both round-trip) and {@link matchSpecToWire} emits the
 * serde form. Enums cross as serde `rename_all = "snake_case"` strings
 * (`Eq -> "eq"`, `Break -> "break"`).
 */

import { shorthandToAccess, type AccessFlags } from './breakpoints-core'

/** Comparison operator for a value-matched watchpoint. */
export type ValueCompare = 'eq' | 'ne' | 'lt' | 'le' | 'gt' | 'ge'

/** What happens when a breakpoint's conditions are satisfied. */
export type BreakpointAction = 'break' | 'log'

/** Advanced match conditions (camelCase UI form). */
export interface MatchSpec {
  /** Inclusive end of an address range, or `null` for a single address. */
  addressEnd: number | null
  /** Target value for a value predicate, or `null` for no value match. */
  value: number | null
  /**
   * Width in bytes of the value predicate (1-4). A width > 1 makes the
   * watch multi-byte: the observed value is assembled from `valueSize`
   * consecutive bytes from the watched address, so a write to any of them
   * is a candidate. Defaults to 1 (single byte).
   */
  valueSize: number
  /** Mask AND-ed before comparing (`0` means compare all bits). */
  valueMask: number
  /** Comparison operator applied as `(observed & mask) <op> value`. */
  valueCompare: ValueCompare
  /**
   * Optional condition over live registers / memory, evaluated when the
   * base match holds (e.g. `"d0 == 5 && [0xFF0000].w > 0x100"`). `null` (or
   * blank) imposes no extra constraint. Parsed and evaluated by the Rust
   * expression engine; a malformed expression fails closed (the breakpoint
   * stays silent). See `retro_core::debug::expr`.
   */
  expr: string | null
  /** Ignore the first N qualifying matches before firing. */
  hitSkip: number
  /** Break (halt) vs. log (count and continue). */
  action: BreakpointAction
  /** Auto-disable after the first real fire. */
  oneShot: boolean
}

/** serde (snake_case) form of {@link MatchSpec}. */
export interface WireMatchSpec {
  address_end: number | null
  value: number | null
  value_size: number
  value_mask: number
  value_compare: ValueCompare
  expr: string | null
  hit_skip: number
  action: BreakpointAction
  one_shot: boolean
}

/** One selectable value for an event breakpoint's `param`. */
export interface EventParamOption {
  value: number
  label: string
}

/** One event class a core can break on (from `getAvailableEvents`). */
export interface EventClass {
  code: number
  label: string
  description: string | null
  /** Label for what `param` selects, or `null` for a param-less event. */
  paramLabel: string | null
  /** Enumerable param choices for a dropdown (empty = free-form/none). */
  paramOptions: EventParamOption[]
}

/** The bare default: reproduces plain address/access/event semantics. */
export const DEFAULT_MATCH_SPEC: MatchSpec = {
  addressEnd: null,
  value: null,
  valueSize: 1,
  valueMask: 0,
  valueCompare: 'eq',
  expr: null,
  hitSkip: 0,
  action: 'break',
  oneShot: false,
}

/** Supported value widths in bytes, with labels. Genesis (68000) allows
 * 1/2/4 (8/16/32-bit); SNES (65816) allows 1/2/3 (8/16/24-bit). Callers
 * filter this list per console via {@link valueSizeOptionsFor}. */
export const VALUE_SIZE_OPTIONS: { value: number; label: string; bits: number }[] = [
  { value: 1, label: '8-bit', bits: 8 },
  { value: 2, label: '16-bit', bits: 16 },
  { value: 3, label: '24-bit', bits: 24 },
  { value: 4, label: '32-bit', bits: 32 },
]

/**
 * The value widths a given CPU kind supports. The SNES CPUs (65816, SPC700),
 * the Super FX GSU and the Cx4's HG51B top out at a 24-bit bus, so they get
 * 1/2/3 bytes; everything else (68000, Z80, ...) gets the full 1/2/4 set.
 * Returns `[8-bit, 16-bit, 24-bit]` for those four and
 * `[8-bit, 16-bit, 32-bit]` otherwise.
 */
export function valueSizeOptionsFor(
  cpuKind: string | null | undefined,
): { value: number; label: string; bits: number }[] {
  if (
    cpuKind === 'w65816' ||
    cpuKind === 'spc700' ||
    cpuKind === 'superfx' ||
    cpuKind === 'hg51b'
  ) {
    return VALUE_SIZE_OPTIONS.filter((o) => o.value <= 3)
  }
  return VALUE_SIZE_OPTIONS.filter((o) => o.value !== 3)
}

/** Clamp a raw byte-width to the 1-4 range the core supports (`0` -> `1`). */
export function clampValueSize(n: number): number {
  if (!Number.isFinite(n)) return 1
  const t = Math.trunc(n)
  if (t < 1) return 1
  if (t > 4) return 4
  return t
}

/** Largest unsigned value representable in `size` bytes (e.g. 2 -> 0xFFFF). */
export function maxValueForSize(size: number): number {
  const s = clampValueSize(size)
  // 2 ** 32 - 1 stays exact in a JS number; avoid `<<` (32-bit signed).
  return 2 ** (8 * s) - 1
}

/** Wildcard `param` (matches the Rust `BREAKPOINT_PARAM_ANY`). */
export const EVENT_PARAM_ANY = 0xffffffff

/** Dropdown options for the value-comparison operator. */
export const VALUE_COMPARE_OPTIONS: { value: ValueCompare; label: string }[] = [
  { value: 'eq', label: '= equals' },
  { value: 'ne', label: '\u2260 not equal' },
  { value: 'lt', label: '< less than' },
  { value: 'le', label: '\u2264 at most' },
  { value: 'gt', label: '> greater than' },
  { value: 'ge', label: '\u2265 at least' },
]

/** Dropdown options for the breakpoint action. */
export const BREAKPOINT_ACTION_OPTIONS: { value: BreakpointAction; label: string }[] = [
  { value: 'break', label: 'Break' },
  { value: 'log', label: 'Log & continue' },
]

const COMPARE_SYMBOL: Record<ValueCompare, string> = {
  eq: '=',
  ne: '\u2260',
  lt: '<',
  le: '\u2264',
  gt: '>',
  ge: '\u2265',
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null
}

function toIntOrNull(v: unknown): number | null {
  return typeof v === 'number' && Number.isFinite(v) ? Math.trunc(v) : null
}

function toInt(v: unknown, fallback: number): number {
  return typeof v === 'number' && Number.isFinite(v) ? Math.trunc(v) : fallback
}

function toBool(v: unknown): boolean {
  return v === true
}

/**
 * Normalize a raw expression value: a non-blank string trimmed, else
 * `null`. Keeps the "blank means no condition" convention the Rust side
 * also applies.
 */
function toExpr(v: unknown): string | null {
  if (typeof v !== 'string') return null
  const t = v.trim()
  return t === '' ? null : t
}

function toValueCompare(v: unknown): ValueCompare {
  switch (v) {
    case 'ne':
    case 'lt':
    case 'le':
    case 'gt':
    case 'ge':
      return v
    default:
      return 'eq'
  }
}

function toAction(v: unknown): BreakpointAction {
  return v === 'log' ? 'log' : 'break'
}

/** `0x`-prefixed uppercase hex of `n` (unsigned). */
function hex(n: number): string {
  return '0x' + (n >>> 0).toString(16).toUpperCase()
}

/** The single-character symbol for a comparison operator. */
export function compareSymbol(op: ValueCompare): string {
  return COMPARE_SYMBOL[op]
}

/**
 * Normalize a raw spec (from WASM `match_spec`, persisted JSON, or
 * partial UI state) into a clean {@link MatchSpec}. Accepts both
 * snake_case (serde) and camelCase keys; unset / nullish / malformed
 * fields fall back to the {@link DEFAULT_MATCH_SPEC} value, so a plain
 * breakpoint or an older serialized list round-trips unchanged.
 */
export function parseMatchSpec(raw: unknown): MatchSpec {
  if (!isObject(raw)) return { ...DEFAULT_MATCH_SPEC }
  return {
    addressEnd: toIntOrNull(raw.address_end ?? raw.addressEnd),
    value: toIntOrNull(raw.value),
    valueSize: clampValueSize(toInt(raw.value_size ?? raw.valueSize, 1)),
    valueMask: toInt(raw.value_mask ?? raw.valueMask, 0),
    valueCompare: toValueCompare(raw.value_compare ?? raw.valueCompare),
    expr: toExpr(raw.expr),
    hitSkip: Math.max(0, toInt(raw.hit_skip ?? raw.hitSkip, 0)),
    action: toAction(raw.action),
    oneShot: toBool(raw.one_shot ?? raw.oneShot),
  }
}

/** Produce the serde (snake_case) object for `setBreakpointMatchSpec`. */
export function matchSpecToWire(spec: MatchSpec): WireMatchSpec {
  return {
    address_end: spec.addressEnd,
    value: spec.value,
    value_size: clampValueSize(spec.valueSize),
    value_mask: spec.valueMask,
    value_compare: spec.valueCompare,
    expr: toExpr(spec.expr),
    hit_skip: spec.hitSkip,
    action: spec.action,
    one_shot: spec.oneShot,
  }
}

/** `true` when the spec carries no advanced conditions (bare breakpoint). */
export function isDefaultMatchSpec(spec: MatchSpec): boolean {
  return (
    spec.addressEnd === null &&
    spec.value === null &&
    spec.valueMask === 0 &&
    spec.valueCompare === 'eq' &&
    toExpr(spec.expr) === null &&
    spec.hitSkip === 0 &&
    spec.action === 'break' &&
    !spec.oneShot
  )
}

/**
 * Compact one-line summary of the active conditions, joined by a middle
 * dot (U+00B7), e.g. `..0xFF000F`, `=0x42 & 0xF0`, `skip 3`, `log`, `once`.
 * Returns `""` for a bare spec so callers can render nothing.
 */
export function matchSpecSummary(spec: MatchSpec): string {
  const parts: string[] = []
  if (spec.addressEnd !== null) parts.push(`..${hex(spec.addressEnd)}`)
  if (spec.value !== null) {
    const mask = spec.valueMask !== 0 ? ` & ${hex(spec.valueMask)}` : ''
    // For a multi-byte watch, tag the value with its bit-width (`:16`,
    // `:24`, `:32`) so the row makes the assembled span obvious.
    const width = spec.valueSize > 1 ? `:${spec.valueSize * 8}` : ''
    parts.push(`${compareSymbol(spec.valueCompare)}${hex(spec.value)}${width}${mask}`)
  }
  const expr = toExpr(spec.expr)
  if (expr !== null) parts.push(`if ${expr}`)
  if (spec.hitSkip > 0) parts.push(`skip ${spec.hitSkip}`)
  if (spec.action === 'log') parts.push('log')
  if (spec.oneShot) parts.push('once')
  return parts.join(' \u00b7 ')
}

/**
 * Lightweight client-side sanity check for an expression string. This is
 * *not* a full parser, the Rust engine is the source of truth and fails
 * closed on anything it can't parse, but it catches the obvious typos
 * (unbalanced `()` / `[]`, an empty `[]`, or a stray illegal character)
 * before the user commits, so the editor can show a hint. Returns the list
 * of problems (empty = looks ok). A blank string is always ok (no
 * condition).
 */
export function validateExpr(expr: string): string[] {
  const s = expr.trim()
  if (s === '') return []
  const errors: string[] = []
  // Only the character classes the grammar allows: identifiers, digits,
  // the number prefixes `$`/`%`, operators, brackets, dots, whitespace.
  if (/[^0-9a-zA-Z_\s+\-*/%&|^~!<>=()[\].$]/.test(s)) {
    errors.push('Expression has an unsupported character')
  }
  let round = 0
  let square = 0
  for (const ch of s) {
    if (ch === '(') round++
    else if (ch === ')') {
      round--
      if (round < 0) break
    } else if (ch === '[') square++
    else if (ch === ']') {
      square--
      if (square < 0) break
    }
  }
  if (round !== 0) errors.push('Unbalanced parentheses ()')
  if (square !== 0) errors.push('Unbalanced brackets []')
  if (/\[\s*\]/.test(s)) errors.push('Empty memory read []')
  return errors
}

/** Result of {@link validateMatchSpec}. */
export interface MatchSpecValidation {
  ok: boolean
  errors: string[]
}

/**
 * Validate a spec for the editor. When `baseAddress` is supplied, a range
 * end before the start is rejected. The value and mask are bounded by the
 * spec's `valueSize` (8/16/24/32-bit), and hit-skip must be a
 * non-negative integer.
 */
export function validateMatchSpec(spec: MatchSpec, baseAddress?: number): MatchSpecValidation {
  const errors: string[] = []
  if (spec.addressEnd !== null) {
    if (!Number.isInteger(spec.addressEnd) || spec.addressEnd < 0) {
      errors.push('Range end must be a non-negative integer')
    } else if (baseAddress !== undefined && spec.addressEnd < baseAddress) {
      errors.push('Range end must be \u2265 the start address')
    }
  }
  if (!Number.isInteger(spec.valueSize) || spec.valueSize < 1 || spec.valueSize > 4) {
    errors.push('Value size must be 1\u20134 bytes')
  }
  const max = maxValueForSize(spec.valueSize)
  const bits = clampValueSize(spec.valueSize) * 8
  if (spec.value !== null && (!Number.isInteger(spec.value) || spec.value < 0 || spec.value > max)) {
    errors.push(`Value must fit in ${bits} bits (0\u2013${hex(max)})`)
  }
  if (!Number.isInteger(spec.valueMask) || spec.valueMask < 0 || spec.valueMask > max) {
    errors.push(`Mask must fit in ${bits} bits (0\u2013${hex(max)})`)
  }
  if (!Number.isInteger(spec.hitSkip) || spec.hitSkip < 0) {
    errors.push('Hit-skip must be a non-negative integer')
  }
  if (spec.expr !== null) errors.push(...validateExpr(spec.expr))
  return { ok: errors.length === 0, errors }
}

function parseParamOptions(raw: unknown): EventParamOption[] {
  if (!Array.isArray(raw)) return []
  const out: EventParamOption[] = []
  for (const item of raw) {
    if (!isObject(item)) continue
    const value = toIntOrNull(item.value)
    if (value === null) continue
    out.push({
      value,
      label: typeof item.label === 'string' ? item.label : String(value),
    })
  }
  return out
}

/**
 * Normalize a raw `getAvailableEvents()` payload into typed
 * {@link EventClass} rows. Items without a numeric `code` are dropped;
 * missing labels/descriptions/param metadata get sensible defaults.
 */
export function parseEventCatalog(raw: unknown): EventClass[] {
  if (!Array.isArray(raw)) return []
  const out: EventClass[] = []
  for (const item of raw) {
    if (!isObject(item)) continue
    const code = toIntOrNull(item.code)
    if (code === null) continue
    const paramLabel =
      typeof item.param_label === 'string'
        ? item.param_label
        : typeof item.paramLabel === 'string'
          ? item.paramLabel
          : null
    out.push({
      code,
      label: typeof item.label === 'string' ? item.label : `Event ${code}`,
      description: typeof item.description === 'string' ? item.description : null,
      paramLabel,
      paramOptions: parseParamOptions(item.param_options ?? item.paramOptions),
    })
  }
  return out
}

/** `true` when `param` is the any-param wildcard. */
export function isParamWildcard(param: number): boolean {
  return (param >>> 0) === EVENT_PARAM_ANY
}

/** The `code` + `param` carried by an event breakpoint. */
export interface EventRef {
  code: number
  param: number
}

/**
 * Extract `{ code, param }` from a raw `bp_type` when it is the serde
 * `{ Event: { code, param } }` tag (as emitted by `listBreakpoints()`),
 * else `null`. Also accepts a pre-extracted `{ code, param }` object so
 * persisted entries round-trip. A missing `param` defaults to the
 * any-param wildcard.
 */
export function parseEventTag(raw: unknown): EventRef | null {
  if (!isObject(raw)) return null
  const inner = isObject(raw.Event) ? raw.Event : isObject(raw.event) ? raw.event : raw
  const code = toIntOrNull(inner.code)
  if (code === null) return null
  return { code, param: toInt(inner.param, EVENT_PARAM_ANY) >>> 0 }
}

// -- Panel view-model: rows + add forms -------------------------------
//
// The watchpoints panel (memory watchpoints with advanced conditions +
// event breakpoints) renders the same way in both UIs. The host maps its
// native breakpoint list into the neutral {@link AppliedWatchInput} /
// {@link PendingWatchInput} projections and gets back ready-to-render
// {@link WatchpointRow}s; the add forms parse + validate here too, so the
// Vue and vanilla shells stay thin.

/** Whether a row is live in WASM (`applied`) or still queued (`pending`). */
export type WatchRowStatus = 'applied' | 'pending'

/** A memory watchpoint vs. a system-event breakpoint. */
export type WatchRowKind = 'memory' | 'event'

/** One ready-to-render row in the watchpoints panel. */
export interface WatchpointRow {
  rowId: string
  status: WatchRowStatus
  kind: WatchRowKind
  /** Watched address (memory rows); `null` for event rows. */
  address: number | null
  access: AccessFlags
  cpuId: number
  enabled: boolean
  hitCount: number
  /** Set on applied rows for `setBreakpointMatchSpec` / remove. */
  wasmId: number | null
  /** Set on pending rows for `removeUserBreakpoint`. */
  localId: string | null
  matchSpec: MatchSpec
  /** Set on event rows; `null` for memory rows. */
  event: EventRef | null
  /** Display label: `$ADDR` for memory rows, event class label otherwise. */
  label: string
  /** Compact conditions summary (empty for a bare spec). */
  summary: string
}

/** Neutral projection of an applied (WASM) breakpoint for row building. */
export interface AppliedWatchInput {
  wasmId: number
  address: number
  access: AccessFlags
  cpuId: number
  enabled: boolean
  hitCount: number
  matchSpec: MatchSpec
  event: EventRef | null
}

/** Neutral projection of a pending (not-yet-applied) breakpoint. */
export interface PendingWatchInput {
  localId: string
  bpType: string
  cpuId: number
  enabled: boolean
  address: number | null
  label: string
}

/** Whether a shorthand `bp_type` is a memory watchpoint (read/write). */
export function isWatchShorthand(bpType: string): boolean {
  const t = bpType.toLowerCase()
  return t === 'read' || t === 'write' || t === 'readwrite'
}

/** `$`-prefixed uppercase hex address label. */
export function addressLabel(addr: number): string {
  return '$' + (addr >>> 0).toString(16).toUpperCase()
}

/**
 * Human label for an event row: the class label from the catalog (or a
 * `Event N` fallback), suffixed with the selected param option (or its
 * hex value) unless the param is the any-param wildcard.
 */
export function eventLabel(code: number, param: number, catalog: EventClass[]): string {
  const cls = catalog.find((c) => c.code === code)
  const base = cls ? cls.label : `Event ${code}`
  if (isParamWildcard(param)) return base
  const opt = cls ? cls.paramOptions.find((o) => o.value === param) : undefined
  return `${base}: ${opt ? opt.label : hex(param)}`
}

/**
 * Build the watchpoints-panel rows from the host's applied + pending
 * breakpoint projections. Keeps only memory watchpoints (read/write) and
 * event breakpoints, plain execute breakpoints belong to the breakpoints
 * panel. Pending rows precede applied rows (mirroring the breakpoints
 * panel's ordering).
 */
export function buildWatchpointRows(
  applied: AppliedWatchInput[],
  pending: PendingWatchInput[],
  catalog: EventClass[],
): WatchpointRow[] {
  const rows: WatchpointRow[] = []
  for (const p of pending) {
    if (!isWatchShorthand(p.bpType)) continue
    rows.push({
      rowId: `pending:${p.localId}`,
      status: 'pending',
      kind: 'memory',
      address: p.address,
      access: shorthandToAccess(p.bpType),
      cpuId: p.cpuId,
      enabled: p.enabled,
      hitCount: 0,
      wasmId: null,
      localId: p.localId,
      matchSpec: { ...DEFAULT_MATCH_SPEC },
      event: null,
      label: p.label,
      summary: '',
    })
  }
  for (const b of applied) {
    const ev = b.event
    const isEvent = ev !== null
    if (!isEvent && !b.access.read && !b.access.write) continue
    rows.push({
      rowId: `applied:${b.wasmId}`,
      status: 'applied',
      kind: isEvent ? 'event' : 'memory',
      address: isEvent ? null : b.address,
      access: b.access,
      cpuId: b.cpuId,
      enabled: b.enabled,
      hitCount: b.hitCount,
      wasmId: b.wasmId,
      localId: null,
      matchSpec: b.matchSpec,
      event: ev,
      label: isEvent ? eventLabel(ev.code, ev.param, catalog) : addressLabel(b.address),
      summary: matchSpecSummary(b.matchSpec),
    })
  }
  return rows
}

/**
 * Parse a hex input that may carry a `$` or `0x` prefix (bare digits are
 * treated as hex too, the watchpoints domain is hex-first). Returns
 * `null` for empty or non-hex input.
 */
export function parseHex(input: string): number | null {
  const s = input.trim().replace(/^\$/, '').replace(/^0x/i, '')
  if (s === '' || !/^[0-9a-fA-F]+$/.test(s)) return null
  // The regex guarantees a non-empty hex string, so parseInt is finite.
  return parseInt(s, 16)
}

/** Seed an editor's hex input from a numeric field (`''` for `null`). */
export function hexOrBlank(n: number | null): string {
  return n === null ? '' : (n >>> 0).toString(16).toUpperCase()
}

/** Raw "add memory watchpoint" form state. */
export interface WatchAddDraft {
  address: string
  read: boolean
  write: boolean
  /** Optional value to match (hex); `''` means no value predicate. */
  value: string
  /**
   * Width in bytes of the value predicate (1-4). Optional; defaults to 1
   * (single byte) when omitted so existing callers keep working.
   */
  valueSize?: number
  /**
   * Optional condition expression (registers / memory). `''` or omitted
   * means no condition. Validated leniently here ({@link validateExpr});
   * the Rust engine is the source of truth.
   */
  expr?: string
}

/** Result of {@link parseWatchAddForm}. */
export interface WatchAddResult {
  ok: boolean
  errors: string[]
  address: number
  access: AccessFlags
  spec: MatchSpec
}

/**
 * Parse + validate the "add memory watchpoint" form. Requires a valid
 * address and at least one of read/write; an optional value must fit in
 * the value size. On error the numeric fields fall back to safe defaults so a
 * caller that ignores `ok` can't push a malformed breakpoint.
 */
export function parseWatchAddForm(draft: WatchAddDraft): WatchAddResult {
  const errors: string[] = []
  const address = parseHex(draft.address)
  if (address === null) errors.push('Enter a valid address (hex, e.g. $FF0000)')
  if (!draft.read && !draft.write) errors.push('Select read, write, or both')
  const valueSize = clampValueSize(draft.valueSize ?? 1)
  const max = maxValueForSize(valueSize)
  let value: number | null = null
  if (draft.value.trim() !== '') {
    // parseHex returns a non-negative number or null, so a range check
    // only needs the null + upper-bound cases.
    const v = parseHex(draft.value)
    if (v === null || v > max) {
      errors.push(`Value must fit in ${valueSize * 8} bits (\u2264 ${hex(max)})`)
    } else {
      value = v
    }
  }
  const expr = toExpr(draft.expr)
  if (expr !== null) errors.push(...validateExpr(expr))
  return {
    ok: errors.length === 0,
    errors,
    address: address ?? 0,
    access: { exec: false, read: draft.read, write: draft.write },
    spec: { ...DEFAULT_MATCH_SPEC, value, valueSize, expr },
  }
}

/** Raw (string-input) state of the advanced match-conditions editor. */
export interface MatchSpecDraft {
  /** Range end (hex); `''` = single address. */
  rangeEnd: string
  /** Target value (hex); `''` = no value predicate. */
  value: string
  /** Value width in bytes (1-4) for a multi-byte watch. */
  valueSize: number
  /** Mask (hex); `''` = compare all bits. */
  valueMask: string
  valueCompare: ValueCompare
  /** Condition expression; `''` = no condition. */
  expr: string
  hitSkip: number
  action: BreakpointAction
  oneShot: boolean
}

/** Seed an editor draft from an existing {@link MatchSpec}. */
export function specToDraft(spec: MatchSpec): MatchSpecDraft {
  return {
    rangeEnd: hexOrBlank(spec.addressEnd),
    value: hexOrBlank(spec.value),
    valueSize: clampValueSize(spec.valueSize),
    valueMask: spec.valueMask === 0 ? '' : hexOrBlank(spec.valueMask),
    valueCompare: spec.valueCompare,
    expr: spec.expr ?? '',
    hitSkip: spec.hitSkip,
    action: spec.action,
    oneShot: spec.oneShot,
  }
}

/** Result of {@link parseMatchSpecEditor}. */
export interface MatchSpecEditorResult {
  ok: boolean
  errors: string[]
  spec: MatchSpec
}

/**
 * Parse + validate the advanced match-conditions editor draft into a
 * {@link MatchSpec}. Hex fields that fail to parse add an error and fall
 * back to "unset"; byte/range rules are then applied via
 * {@link validateMatchSpec} (passing `baseAddress` rejects a range end
 * before the watched address).
 */
export function parseMatchSpecEditor(draft: MatchSpecDraft, baseAddress?: number): MatchSpecEditorResult {
  const errors: string[] = []
  const field = (raw: string, name: string): number | null => {
    if (raw.trim() === '') return null
    const v = parseHex(raw)
    if (v === null) {
      errors.push(`${name} must be hex`)
      return null
    }
    return v
  }
  const addressEnd = field(draft.rangeEnd, 'Range end')
  const value = field(draft.value, 'Value')
  const valueMask = field(draft.valueMask, 'Mask') ?? 0
  const spec: MatchSpec = {
    addressEnd,
    value,
    valueSize: clampValueSize(draft.valueSize),
    valueMask,
    valueCompare: draft.valueCompare,
    expr: toExpr(draft.expr),
    hitSkip: Math.max(0, Math.trunc(draft.hitSkip)),
    action: draft.action,
    oneShot: draft.oneShot,
  }
  const validation = validateMatchSpec(spec, baseAddress)
  return { ok: errors.length === 0 && validation.ok, errors: [...errors, ...validation.errors], spec }
}

/** Raw "add event breakpoint" form state. */
export interface EventAddDraft {
  /** Selected event class `code`, or `null` when nothing is chosen. */
  code: number | null
  /** Param input: `''` (or `'any'`) = wildcard, else a hex value. */
  param: string
}

/** Result of {@link parseEventAddForm}. */
export interface EventAddResult {
  ok: boolean
  errors: string[]
  code: number
  param: number
}

/**
 * Parse + validate the "add event breakpoint" form against the catalog.
 * The chosen `code` must exist in the catalog; an empty/`any` param maps
 * to the wildcard, otherwise it must parse as hex.
 */
export function parseEventAddForm(draft: EventAddDraft, catalog: EventClass[]): EventAddResult {
  const errors: string[] = []
  const cls = draft.code === null ? undefined : catalog.find((c) => c.code === draft.code)
  if (!cls) errors.push('Choose an event')
  let param = EVENT_PARAM_ANY
  const raw = draft.param.trim().toLowerCase()
  if (raw !== '' && raw !== 'any') {
    const v = parseHex(draft.param)
    if (v === null) {
      errors.push('Param must be hex or "any"')
    } else {
      param = v >>> 0
    }
  }
  return {
    ok: errors.length === 0,
    errors,
    code: cls ? cls.code : 0,
    param,
  }
}
