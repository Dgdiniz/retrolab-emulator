import { describe, it, expect } from 'vitest'
import {
  DEFAULT_MATCH_SPEC,
  EVENT_PARAM_ANY,
  VALUE_COMPARE_OPTIONS,
  VALUE_SIZE_OPTIONS,
  BREAKPOINT_ACTION_OPTIONS,
  valueSizeOptionsFor,
  clampValueSize,
  maxValueForSize,
  compareSymbol,
  parseMatchSpec,
  matchSpecToWire,
  isDefaultMatchSpec,
  matchSpecSummary,
  validateMatchSpec,
  parseEventCatalog,
  isParamWildcard,
  parseEventTag,
  isWatchShorthand,
  addressLabel,
  eventLabel,
  buildWatchpointRows,
  parseHex,
  hexOrBlank,
  parseWatchAddForm,
  parseEventAddForm,
  specToDraft,
  parseMatchSpecEditor,
  validateExpr,
  type MatchSpec,
  type MatchSpecDraft,
  type ValueCompare,
  type EventClass,
  type AppliedWatchInput,
  type PendingWatchInput,
} from '../src/watchpoints-core'

function spec(over: Partial<MatchSpec> = {}): MatchSpec {
  return { ...DEFAULT_MATCH_SPEC, ...over }
}

describe('constants', () => {
  it('exposes a bare default spec', () => {
    expect(DEFAULT_MATCH_SPEC).toEqual({
      addressEnd: null,
      value: null,
      valueSize: 1,
      valueMask: 0,
      valueCompare: 'eq',
      expr: null,
      hitSkip: 0,
      action: 'break',
      oneShot: false,
    })
  })

  it('exposes dropdown option lists and the wildcard sentinel', () => {
    expect(VALUE_COMPARE_OPTIONS.map((o) => o.value)).toEqual(['eq', 'ne', 'lt', 'le', 'gt', 'ge'])
    expect(BREAKPOINT_ACTION_OPTIONS.map((o) => o.value)).toEqual(['break', 'log'])
    expect(EVENT_PARAM_ANY).toBe(0xffffffff)
  })
})

describe('compareSymbol', () => {
  it('maps every operator to its glyph', () => {
    const ops: ValueCompare[] = ['eq', 'ne', 'lt', 'le', 'gt', 'ge']
    expect(ops.map(compareSymbol)).toEqual(['=', '\u2260', '<', '\u2264', '>', '\u2265'])
  })
})

describe('value width helpers', () => {
  it('lists every supported width', () => {
    expect(VALUE_SIZE_OPTIONS.map((o) => o.value)).toEqual([1, 2, 3, 4])
    expect(VALUE_SIZE_OPTIONS.map((o) => o.bits)).toEqual([8, 16, 24, 32])
  })

  it('offers 8/16/32-bit for the 68000 and most CPUs', () => {
    expect(valueSizeOptionsFor('m68k').map((o) => o.value)).toEqual([1, 2, 4])
    expect(valueSizeOptionsFor('z80').map((o) => o.value)).toEqual([1, 2, 4])
    expect(valueSizeOptionsFor(null).map((o) => o.value)).toEqual([1, 2, 4])
    expect(valueSizeOptionsFor(undefined).map((o) => o.value)).toEqual([1, 2, 4])
  })

  it('offers 8/16/24-bit for the SNES CPUs + Super FX GSU + Cx4 (24-bit bus)', () => {
    expect(valueSizeOptionsFor('w65816').map((o) => o.value)).toEqual([1, 2, 3])
    expect(valueSizeOptionsFor('spc700').map((o) => o.value)).toEqual([1, 2, 3])
    expect(valueSizeOptionsFor('superfx').map((o) => o.value)).toEqual([1, 2, 3])
    expect(valueSizeOptionsFor('hg51b').map((o) => o.value)).toEqual([1, 2, 3])
  })

  it('clamps widths into the 1-4 range', () => {
    expect(clampValueSize(0)).toBe(1)
    expect(clampValueSize(-3)).toBe(1)
    expect(clampValueSize(2)).toBe(2)
    expect(clampValueSize(4)).toBe(4)
    expect(clampValueSize(99)).toBe(4)
    expect(clampValueSize(2.7)).toBe(2)
    expect(clampValueSize(NaN)).toBe(1)
  })

  it('computes the max unsigned value per width', () => {
    expect(maxValueForSize(1)).toBe(0xff)
    expect(maxValueForSize(2)).toBe(0xffff)
    expect(maxValueForSize(3)).toBe(0xffffff)
    expect(maxValueForSize(4)).toBe(0xffffffff)
  })
})

describe('parseMatchSpec', () => {
  it('returns a fresh default for non-object input', () => {
    for (const raw of [null, undefined, 42, 'nope', true]) {
      expect(parseMatchSpec(raw)).toEqual(DEFAULT_MATCH_SPEC)
    }
    // Fresh copy, not the shared constant.
    expect(parseMatchSpec(null)).not.toBe(DEFAULT_MATCH_SPEC)
  })

  it('parses the serde snake_case wire shape', () => {
    expect(
      parseMatchSpec({
        address_end: 0xff000f,
        value: 0x42,
        value_mask: 0xf0,
        value_compare: 'ge',
        hit_skip: 3,
        action: 'log',
        one_shot: true,
      }),
    ).toEqual({
      addressEnd: 0xff000f,
      value: 0x42,
      valueSize: 1,
      valueMask: 0xf0,
      valueCompare: 'ge',
      expr: null,
      hitSkip: 3,
      action: 'log',
      oneShot: true,
    })
  })

  it('also accepts camelCase keys (persisted JSON)', () => {
    expect(
      parseMatchSpec({
        addressEnd: 0x10,
        valueMask: 0x0f,
        valueCompare: 'ne',
        hitSkip: 2,
        oneShot: true,
      }),
    ).toEqual(spec({ addressEnd: 0x10, valueMask: 0x0f, valueCompare: 'ne', hitSkip: 2, oneShot: true }))
  })

  it('preserves zero values without treating them as unset', () => {
    const out = parseMatchSpec({ address_end: 0, value: 0, value_mask: 0, hit_skip: 0 })
    expect(out.addressEnd).toBe(0)
    expect(out.value).toBe(0)
    expect(out.valueMask).toBe(0)
    expect(out.hitSkip).toBe(0)
  })

  it('truncates floats and nulls out non-numeric optionals', () => {
    const out = parseMatchSpec({ address_end: 5.9, value: 'x', value_mask: 7.4 })
    expect(out.addressEnd).toBe(5)
    expect(out.value).toBeNull()
    expect(out.valueMask).toBe(7)
  })

  it('clamps a negative hit-skip to zero', () => {
    expect(parseMatchSpec({ hit_skip: -4 }).hitSkip).toBe(0)
  })

  it('falls back to eq for an unknown comparator and break for an unknown action', () => {
    expect(parseMatchSpec({ value_compare: 'whatever' }).valueCompare).toBe('eq')
    expect(parseMatchSpec({ action: 'explode' }).action).toBe('break')
  })

  it('accepts each valid comparator', () => {
    for (const op of ['eq', 'ne', 'lt', 'le', 'gt', 'ge'] as ValueCompare[]) {
      expect(parseMatchSpec({ value_compare: op }).valueCompare).toBe(op)
    }
  })

  it('treats a missing value_mask as 0', () => {
    expect(parseMatchSpec({ value: 1 }).valueMask).toBe(0)
  })
})

describe('matchSpecToWire', () => {
  it('round-trips through parse', () => {
    const s = spec({ addressEnd: 0x20, value: 9, valueMask: 0xff, valueCompare: 'lt', hitSkip: 1, action: 'log', oneShot: true })
    expect(matchSpecToWire(s)).toEqual({
      address_end: 0x20,
      value: 9,
      value_size: 1,
      value_mask: 0xff,
      value_compare: 'lt',
      expr: null,
      hit_skip: 1,
      action: 'log',
      one_shot: true,
    })
    expect(parseMatchSpec(matchSpecToWire(s))).toEqual(s)
  })
})

describe('isDefaultMatchSpec', () => {
  it('is true only for the bare spec', () => {
    expect(isDefaultMatchSpec(spec())).toBe(true)
  })

  it('is false when any single field deviates', () => {
    expect(isDefaultMatchSpec(spec({ addressEnd: 1 }))).toBe(false)
    expect(isDefaultMatchSpec(spec({ value: 0 }))).toBe(false)
    expect(isDefaultMatchSpec(spec({ valueMask: 1 }))).toBe(false)
    expect(isDefaultMatchSpec(spec({ valueCompare: 'ne' }))).toBe(false)
    expect(isDefaultMatchSpec(spec({ hitSkip: 1 }))).toBe(false)
    expect(isDefaultMatchSpec(spec({ action: 'log' }))).toBe(false)
    expect(isDefaultMatchSpec(spec({ oneShot: true }))).toBe(false)
  })
})

describe('matchSpecSummary', () => {
  it('is empty for a bare spec', () => {
    expect(matchSpecSummary(spec())).toBe('')
  })

  it('renders a range end', () => {
    expect(matchSpecSummary(spec({ addressEnd: 0xff000f }))).toBe('..0xFF000F')
  })

  it('renders a value with and without a mask', () => {
    expect(matchSpecSummary(spec({ value: 0x42 }))).toBe('=0x42')
    expect(matchSpecSummary(spec({ value: 0x42, valueMask: 0xf0, valueCompare: 'ge' }))).toBe('\u22650x42 & 0xF0')
  })

  it('tags a multi-byte value with its bit-width', () => {
    expect(matchSpecSummary(spec({ value: 0x1234, valueSize: 2 }))).toBe('=0x1234:16')
    expect(matchSpecSummary(spec({ value: 0x123456, valueSize: 3 }))).toBe('=0x123456:24')
    expect(matchSpecSummary(spec({ value: 0x12345678, valueSize: 4 }))).toBe('=0x12345678:32')
    // Width 1 carries no tag.
    expect(matchSpecSummary(spec({ value: 0x42, valueSize: 1 }))).toBe('=0x42')
  })

  it('renders hit-skip, log, and one-shot flags', () => {
    expect(matchSpecSummary(spec({ hitSkip: 3 }))).toBe('skip 3')
    expect(matchSpecSummary(spec({ action: 'log' }))).toBe('log')
    expect(matchSpecSummary(spec({ oneShot: true }))).toBe('once')
  })

  it('joins multiple conditions with a middot', () => {
    expect(
      matchSpecSummary(spec({ addressEnd: 0x1f, value: 1, hitSkip: 2, action: 'log', oneShot: true })),
    ).toBe('..0x1F \u00b7 =0x1 \u00b7 skip 2 \u00b7 log \u00b7 once')
  })
})

describe('validateMatchSpec', () => {
  it('accepts a bare spec', () => {
    expect(validateMatchSpec(spec())).toEqual({ ok: true, errors: [] })
  })

  it('rejects a non-integer or negative range end', () => {
    expect(validateMatchSpec(spec({ addressEnd: 1.5 })).ok).toBe(false)
    expect(validateMatchSpec(spec({ addressEnd: -1 })).errors[0]).toMatch(/non-negative integer/)
  })

  it('rejects a range end before the start address', () => {
    expect(validateMatchSpec(spec({ addressEnd: 0x10 }), 0x20).errors[0]).toMatch(/start address/)
  })

  it('accepts a range end at or after the start address', () => {
    expect(validateMatchSpec(spec({ addressEnd: 0x20 }), 0x20).ok).toBe(true)
  })

  it('accepts a valid range end without a base address', () => {
    expect(validateMatchSpec(spec({ addressEnd: 0x20 })).ok).toBe(true)
  })

  it('rejects an out-of-byte-range value', () => {
    expect(validateMatchSpec(spec({ value: 256 })).ok).toBe(false)
    expect(validateMatchSpec(spec({ value: -1 })).ok).toBe(false)
    expect(validateMatchSpec(spec({ value: 1.5 })).ok).toBe(false)
    expect(validateMatchSpec(spec({ value: 0xff })).ok).toBe(true)
  })

  it('rejects an out-of-byte-range mask', () => {
    expect(validateMatchSpec(spec({ valueMask: 256 })).ok).toBe(false)
    expect(validateMatchSpec(spec({ valueMask: -1 })).ok).toBe(false)
  })

  it('widens the value/mask bounds for a multi-byte watch', () => {
    // 0x1234 is rejected at width 1 but accepted at width 2.
    expect(validateMatchSpec(spec({ value: 0x1234 })).ok).toBe(false)
    expect(validateMatchSpec(spec({ value: 0x1234, valueSize: 2 })).ok).toBe(true)
    expect(validateMatchSpec(spec({ value: 0x12345678, valueSize: 4 })).ok).toBe(true)
    // A 32-bit value still overflows a 24-bit watch.
    expect(validateMatchSpec(spec({ value: 0x1000000, valueSize: 3 })).ok).toBe(false)
    expect(validateMatchSpec(spec({ valueMask: 0xffff, valueSize: 2 })).ok).toBe(true)
  })

  it('rejects an out-of-range value size', () => {
    expect(validateMatchSpec(spec({ valueSize: 0 })).ok).toBe(false)
    expect(validateMatchSpec(spec({ valueSize: 5 })).ok).toBe(false)
    expect(validateMatchSpec(spec({ valueSize: 2.5 })).ok).toBe(false)
  })

  it('rejects a negative or non-integer hit-skip', () => {
    expect(validateMatchSpec(spec({ hitSkip: -1 })).ok).toBe(false)
    expect(validateMatchSpec(spec({ hitSkip: 2.5 })).ok).toBe(false)
  })

  it('accumulates multiple errors', () => {
    const v = validateMatchSpec(spec({ addressEnd: -1, value: 999, valueMask: 999, hitSkip: -3 }))
    expect(v.ok).toBe(false)
    expect(v.errors.length).toBe(4)
  })
})

describe('parseEventCatalog', () => {
  it('returns [] for a non-array', () => {
    expect(parseEventCatalog(null)).toEqual([])
    expect(parseEventCatalog({})).toEqual([])
  })

  it('skips non-object items and items without a numeric code', () => {
    expect(parseEventCatalog([42, null, 'x', {}, { code: 'nope' }])).toEqual([])
  })

  it('parses the snake_case event shape with defaults', () => {
    const out = parseEventCatalog([
      {
        code: 7,
        label: 'PPU register write',
        description: 'Write to a PPU register',
        param_label: 'Register',
        param_options: [{ value: 0, label: 'INIDISP' }, { value: 1 }],
      },
      { code: 2 },
    ])
    expect(out).toEqual([
      {
        code: 7,
        label: 'PPU register write',
        description: 'Write to a PPU register',
        paramLabel: 'Register',
        paramOptions: [
          { value: 0, label: 'INIDISP' },
          { value: 1, label: '1' },
        ],
      },
      {
        code: 2,
        label: 'Event 2',
        description: null,
        paramLabel: null,
        paramOptions: [],
      },
    ])
  })

  it('accepts camelCase param metadata', () => {
    const out = parseEventCatalog([{ code: 8, paramLabel: 'Port', paramOptions: [{ value: 0, label: 'Port 0' }] }])
    expect(out[0].paramLabel).toBe('Port')
    expect(out[0].paramOptions).toEqual([{ value: 0, label: 'Port 0' }])
  })

  it('drops malformed param options', () => {
    const out = parseEventCatalog([{ code: 1, param_options: ['x', null, {}, { value: 'no' }, { value: 5 }] }])
    expect(out[0].paramOptions).toEqual([{ value: 5, label: '5' }])
  })

  it('treats a non-array param_options as empty', () => {
    const out = parseEventCatalog([{ code: 1, param_options: 'nope' }])
    expect(out[0].paramOptions).toEqual([])
  })
})

describe('isParamWildcard', () => {
  it('detects the any-param sentinel', () => {
    expect(isParamWildcard(0xffffffff)).toBe(true)
    expect(isParamWildcard(-1)).toBe(true) // >>> 0 normalizes
    expect(isParamWildcard(0)).toBe(false)
    expect(isParamWildcard(7)).toBe(false)
  })
})

describe('parseEventTag', () => {
  it('extracts the serde { Event: { code, param } } tag', () => {
    expect(parseEventTag({ Event: { code: 3, param: 5 } })).toEqual({ code: 3, param: 5 })
  })

  it('accepts a pre-extracted { code, param } object (persisted form)', () => {
    expect(parseEventTag({ code: 9, param: 0 })).toEqual({ code: 9, param: 0 })
  })

  it('accepts a camelCase event wrapper', () => {
    expect(parseEventTag({ event: { code: 1, param: 2 } })).toEqual({ code: 1, param: 2 })
  })

  it('defaults a missing param to the any-param wildcard', () => {
    expect(parseEventTag({ Event: { code: 4 } })).toEqual({ code: 4, param: EVENT_PARAM_ANY })
  })

  it('normalizes a negative param through >>> 0', () => {
    expect(parseEventTag({ code: 2, param: -1 })).toEqual({ code: 2, param: 0xffffffff })
  })

  it('returns null for access tags, non-objects, and missing code', () => {
    expect(parseEventTag({ Access: { exec: true, read: false, write: false } })).toBeNull()
    expect(parseEventTag('execute')).toBeNull()
    expect(parseEventTag(null)).toBeNull()
    expect(parseEventTag(42)).toBeNull()
    expect(parseEventTag({ Event: { param: 1 } })).toBeNull()
  })
})

const CATALOG: EventClass[] = [
  {
    code: 1,
    label: 'VBlank',
    description: 'Vertical blank',
    paramLabel: null,
    paramOptions: [],
  },
  {
    code: 2,
    label: 'DMA start',
    description: null,
    paramLabel: 'Channel',
    paramOptions: [
      { value: 0, label: 'Channel 0' },
      { value: 3, label: 'Channel 3' },
    ],
  },
]

function applied(over: Partial<AppliedWatchInput> = {}): AppliedWatchInput {
  return {
    wasmId: 1,
    address: 0xff0000,
    access: { exec: false, read: false, write: true },
    cpuId: 0,
    enabled: true,
    hitCount: 0,
    matchSpec: { ...DEFAULT_MATCH_SPEC },
    event: null,
    ...over,
  }
}

describe('isWatchShorthand', () => {
  it('accepts read/write/readwrite (any casing), rejects execute', () => {
    expect(isWatchShorthand('read')).toBe(true)
    expect(isWatchShorthand('Write')).toBe(true)
    expect(isWatchShorthand('READWRITE')).toBe(true)
    expect(isWatchShorthand('execute')).toBe(false)
    expect(isWatchShorthand('')).toBe(false)
  })
})

describe('addressLabel', () => {
  it('formats an unsigned hex address', () => {
    expect(addressLabel(0xff0000)).toBe('$FF0000')
    expect(addressLabel(0)).toBe('$0')
    expect(addressLabel(-1)).toBe('$FFFFFFFF')
  })
})

describe('eventLabel', () => {
  it('uses the catalog label for a param-less / wildcard event', () => {
    expect(eventLabel(1, EVENT_PARAM_ANY, CATALOG)).toBe('VBlank')
  })

  it('suffixes the matching param option label', () => {
    expect(eventLabel(2, 3, CATALOG)).toBe('DMA start: Channel 3')
  })

  it('falls back to a hex param when no option matches', () => {
    expect(eventLabel(2, 9, CATALOG)).toBe('DMA start: 0x9')
  })

  it('falls back to "Event N" for an unknown code', () => {
    expect(eventLabel(99, EVENT_PARAM_ANY, CATALOG)).toBe('Event 99')
    expect(eventLabel(99, 5, CATALOG)).toBe('Event 99: 0x5')
  })
})

describe('buildWatchpointRows', () => {
  it('keeps read/write + event rows and drops execute-only', () => {
    const rows = buildWatchpointRows(
      [
        applied({ wasmId: 1, address: 0x10, access: { exec: false, read: true, write: false } }),
        applied({ wasmId: 2, address: 0x20, access: { exec: true, read: false, write: false } }),
        applied({ wasmId: 3, address: 0, access: { exec: false, read: false, write: false }, event: { code: 1, param: EVENT_PARAM_ANY } }),
      ],
      [],
      CATALOG,
    )
    expect(rows.map((r) => r.wasmId)).toEqual([1, 3])
    expect(rows[0].kind).toBe('memory')
    expect(rows[0].label).toBe('$10')
    expect(rows[1].kind).toBe('event')
    expect(rows[1].address).toBeNull()
    expect(rows[1].label).toBe('VBlank')
  })

  it('renders the conditions summary on applied memory rows', () => {
    const rows = buildWatchpointRows(
      [applied({ matchSpec: spec({ value: 0x42, oneShot: true }) })],
      [],
      CATALOG,
    )
    expect(rows[0].summary).toBe('=0x42 \u00b7 once')
  })

  it('prepends pending watchpoints (read/write only) with a default spec', () => {
    const pend: PendingWatchInput[] = [
      { localId: 'bp-1', bpType: 'write', cpuId: 0, enabled: true, address: 0x30, label: '$30' },
      { localId: 'bp-2', bpType: 'execute', cpuId: 0, enabled: true, address: 0x40, label: '$40' },
    ]
    const rows = buildWatchpointRows([applied({ wasmId: 7 })], pend, CATALOG)
    expect(rows.map((r) => r.rowId)).toEqual(['pending:bp-1', 'applied:7'])
    const p = rows[0]
    expect(p.status).toBe('pending')
    expect(p.localId).toBe('bp-1')
    expect(p.wasmId).toBeNull()
    expect(p.access).toEqual({ exec: false, read: false, write: true })
    expect(p.matchSpec).toEqual(DEFAULT_MATCH_SPEC)
    expect(p.summary).toBe('')
  })
})

describe('parseHex', () => {
  it('parses $, 0x, and bare hex', () => {
    expect(parseHex('$FF0000')).toBe(0xff0000)
    expect(parseHex('0x1f')).toBe(0x1f)
    expect(parseHex('  20 ')).toBe(0x20)
  })

  it('returns null for empty / non-hex input', () => {
    expect(parseHex('')).toBeNull()
    expect(parseHex('   ')).toBeNull()
    expect(parseHex('xyz')).toBeNull()
    expect(parseHex('$')).toBeNull()
  })
})

describe('hexOrBlank', () => {
  it('formats a number or returns empty for null', () => {
    expect(hexOrBlank(0x42)).toBe('42')
    expect(hexOrBlank(0)).toBe('0')
    expect(hexOrBlank(null)).toBe('')
  })
})

describe('parseWatchAddForm', () => {
  it('parses a read/write watchpoint with an optional value', () => {
    const r = parseWatchAddForm({ address: '$FF0000', read: true, write: true, value: '42' })
    expect(r.ok).toBe(true)
    expect(r.address).toBe(0xff0000)
    expect(r.access).toEqual({ exec: false, read: true, write: true })
    expect(r.spec.value).toBe(0x42)
  })

  it('omits the value predicate when the value field is blank', () => {
    const r = parseWatchAddForm({ address: '10', read: false, write: true, value: '   ' })
    expect(r.ok).toBe(true)
    expect(r.spec.value).toBeNull()
  })

  it('reports a missing/invalid address', () => {
    const r = parseWatchAddForm({ address: 'zzz', read: true, write: false, value: '' })
    expect(r.ok).toBe(false)
    expect(r.errors).toContain('Enter a valid address (hex, e.g. $FF0000)')
    expect(r.address).toBe(0)
  })

  it('requires at least one of read/write', () => {
    const r = parseWatchAddForm({ address: '10', read: false, write: false, value: '' })
    expect(r.ok).toBe(false)
    expect(r.errors).toContain('Select read, write, or both')
  })

  it('rejects a non-byte / non-hex value', () => {
    expect(parseWatchAddForm({ address: '10', read: true, write: false, value: '100' }).ok).toBe(false)
    const r = parseWatchAddForm({ address: '10', read: true, write: false, value: 'zz' })
    expect(r.ok).toBe(false)
    expect(r.errors[0]).toMatch(/Value must fit in 8 bits/)
    expect(r.spec.value).toBeNull()
  })

  it('widens the value range for a multi-byte watch', () => {
    // 0x1234 overflows a byte but fits a 16-bit watch.
    const narrow = parseWatchAddForm({ address: '10', read: false, write: true, value: '1234' })
    expect(narrow.ok).toBe(false)
    const wide = parseWatchAddForm({ address: '10', read: false, write: true, value: '1234', valueSize: 2 })
    expect(wide.ok).toBe(true)
    expect(wide.spec.value).toBe(0x1234)
    expect(wide.spec.valueSize).toBe(2)
  })

  it('clamps an out-of-range valueSize', () => {
    const r = parseWatchAddForm({ address: '10', read: false, write: true, value: 'FF', valueSize: 9 })
    expect(r.spec.valueSize).toBe(4)
  })
})

describe('parseEventAddForm', () => {
  it('parses a chosen event with the wildcard param by default', () => {
    expect(parseEventAddForm({ code: 1, param: '' }, CATALOG)).toEqual({
      ok: true,
      errors: [],
      code: 1,
      param: EVENT_PARAM_ANY,
    })
    expect(parseEventAddForm({ code: 1, param: 'any' }, CATALOG).param).toBe(EVENT_PARAM_ANY)
  })

  it('parses a hex param', () => {
    const r = parseEventAddForm({ code: 2, param: '0x3' }, CATALOG)
    expect(r).toEqual({ ok: true, errors: [], code: 2, param: 3 })
  })

  it('rejects no selection and unknown codes', () => {
    expect(parseEventAddForm({ code: null, param: '' }, CATALOG).ok).toBe(false)
    const r = parseEventAddForm({ code: 99, param: '' }, CATALOG)
    expect(r.ok).toBe(false)
    expect(r.errors).toContain('Choose an event')
    expect(r.code).toBe(0)
  })

  it('rejects a non-hex param', () => {
    const r = parseEventAddForm({ code: 1, param: 'zz' }, CATALOG)
    expect(r.ok).toBe(false)
    expect(r.errors).toContain('Param must be hex or "any"')
  })
})

function draft(over: Partial<MatchSpecDraft> = {}): MatchSpecDraft {
  return {
    rangeEnd: '',
    value: '',
    valueSize: 1,
    valueMask: '',
    valueCompare: 'eq',
    expr: '',
    hitSkip: 0,
    action: 'break',
    oneShot: false,
    ...over,
  }
}

describe('specToDraft', () => {
  it('seeds string fields from a spec, blanking unset numerics', () => {
    expect(specToDraft(DEFAULT_MATCH_SPEC)).toEqual({
      rangeEnd: '',
      value: '',
      valueSize: 1,
      valueMask: '',
      valueCompare: 'eq',
      expr: '',
      hitSkip: 0,
      action: 'break',
      oneShot: false,
    })
  })

  it('formats set fields as hex (mask 0 stays blank)', () => {
    expect(specToDraft(spec({ addressEnd: 0xff00, value: 0x42, valueSize: 4, valueMask: 0xf0, expr: 'd0 == 1', hitSkip: 3, action: 'log', oneShot: true }))).toEqual({
      rangeEnd: 'FF00',
      value: '42',
      valueSize: 4,
      valueMask: 'F0',
      valueCompare: 'eq',
      expr: 'd0 == 1',
      hitSkip: 3,
      action: 'log',
      oneShot: true,
    })
  })
})

describe('parseMatchSpecEditor', () => {
  it('parses a full valid draft', () => {
    const r = parseMatchSpecEditor(
      draft({ rangeEnd: 'FF0010', value: '42', valueMask: 'F0', valueCompare: 'ge', hitSkip: 2, action: 'log', oneShot: true }),
      0xff0000,
    )
    expect(r.ok).toBe(true)
    expect(r.errors).toEqual([])
    expect(r.spec).toEqual({
      addressEnd: 0xff0010,
      value: 0x42,
      valueSize: 1,
      valueMask: 0xf0,
      valueCompare: 'ge',
      expr: null,
      hitSkip: 2,
      action: 'log',
      oneShot: true,
    })
  })

  it('treats blank hex fields as unset and mask as 0', () => {
    const r = parseMatchSpecEditor(draft())
    expect(r.ok).toBe(true)
    expect(r.spec).toEqual(DEFAULT_MATCH_SPEC)
  })

  it('flags non-hex fields', () => {
    const r = parseMatchSpecEditor(draft({ rangeEnd: 'zz', value: 'qq', valueMask: 'gg' }))
    expect(r.ok).toBe(false)
    expect(r.errors).toEqual(
      expect.arrayContaining(['Range end must be hex', 'Value must be hex', 'Mask must be hex']),
    )
  })

  it('clamps a negative hit-skip to zero', () => {
    const r = parseMatchSpecEditor(draft({ hitSkip: -5 }))
    expect(r.spec.hitSkip).toBe(0)
  })

  it('surfaces validateMatchSpec range / byte errors', () => {
    const tooBig = parseMatchSpecEditor(draft({ value: '100' }))
    expect(tooBig.ok).toBe(false)
    expect(tooBig.errors.some((e) => /Value must fit in 8 bits/.test(e))).toBe(true)

    const badRange = parseMatchSpecEditor(draft({ rangeEnd: '10' }), 0x20)
    expect(badRange.ok).toBe(false)
    expect(badRange.errors).toContain('Range end must be \u2265 the start address')
  })

  it('carries a multi-byte width through into the spec', () => {
    const r = parseMatchSpecEditor(draft({ value: '12345678', valueSize: 4 }))
    expect(r.ok).toBe(true)
    expect(r.spec.value).toBe(0x12345678)
    expect(r.spec.valueSize).toBe(4)
  })

  it('carries a condition expression into the spec (trimmed)', () => {
    const r = parseMatchSpecEditor(draft({ expr: '  d0 == 5  ' }))
    expect(r.ok).toBe(true)
    expect(r.spec.expr).toBe('d0 == 5')
  })

  it('rejects a malformed condition expression', () => {
    const r = parseMatchSpecEditor(draft({ expr: 'd0 == (5' }))
    expect(r.ok).toBe(false)
    expect(r.errors).toContain('Unbalanced parentheses ()')
  })
})

describe('validateExpr', () => {
  it('accepts a blank expression (no condition)', () => {
    expect(validateExpr('')).toEqual([])
    expect(validateExpr('   ')).toEqual([])
  })

  it('accepts a well-formed expression', () => {
    expect(validateExpr('d0 == 5 && [0xFF0000].w > 0x100')).toEqual([])
    expect(validateExpr('!(d1 & 1) || a7 < 0xFFF000')).toEqual([])
  })

  it('flags unbalanced parentheses and brackets', () => {
    expect(validateExpr('(d0 == 5')).toContain('Unbalanced parentheses ()')
    expect(validateExpr('[0xFF0000.w == 1')).toContain('Unbalanced brackets []')
  })

  it('flags a closing delimiter with no opener (negative balance)', () => {
    expect(validateExpr('d0 == 5)')).toContain('Unbalanced parentheses ()')
    expect(validateExpr('d0 == 5]')).toContain('Unbalanced brackets []')
  })

  it('flags an empty memory read', () => {
    expect(validateExpr('[] == 0')).toContain('Empty memory read []')
  })

  it('flags an unsupported character', () => {
    expect(validateExpr('d0 == 5 ; drop table')).toContain('Expression has an unsupported character')
  })
})

describe('expr field in match specs', () => {
  it('parses an expr string and blanks empty / non-string values', () => {
    expect(parseMatchSpec({ expr: '  d0 == 1 ' }).expr).toBe('d0 == 1')
    expect(parseMatchSpec({ expr: '   ' }).expr).toBeNull()
    expect(parseMatchSpec({ expr: 42 }).expr).toBeNull()
    expect(parseMatchSpec({}).expr).toBeNull()
  })

  it('round-trips through the wire form', () => {
    const s = spec({ expr: 'a1 > 0xFF0000' })
    expect(matchSpecToWire(s).expr).toBe('a1 > 0xFF0000')
    expect(parseMatchSpec(matchSpecToWire(s)).expr).toBe('a1 > 0xFF0000')
  })

  it('is part of the non-default check and the summary', () => {
    expect(isDefaultMatchSpec(spec({ expr: 'd0 == 1' }))).toBe(false)
    expect(isDefaultMatchSpec(spec({ expr: '   ' }))).toBe(true)
    expect(matchSpecSummary(spec({ expr: 'd0 == 1' }))).toBe('if d0 == 1')
  })

  it('is validated by validateMatchSpec', () => {
    expect(validateMatchSpec(spec({ expr: 'd0 == 1' })).ok).toBe(true)
    const bad = validateMatchSpec(spec({ expr: 'd0 == (1' }))
    expect(bad.ok).toBe(false)
    expect(bad.errors).toContain('Unbalanced parentheses ()')
  })

  it('flows through the quick-add form', () => {
    const ok = parseWatchAddForm({ address: 'FF0000', read: false, write: true, value: '', expr: 'd0 == 5' })
    expect(ok.ok).toBe(true)
    expect(ok.spec.expr).toBe('d0 == 5')

    const bad = parseWatchAddForm({ address: 'FF0000', read: false, write: true, value: '', expr: 'd0 == [' })
    expect(bad.ok).toBe(false)
    expect(bad.errors).toContain('Unbalanced brackets []')
  })
})
