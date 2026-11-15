import { describe, expect, it } from 'vitest'
import {
  ALL_BREAKPOINT_CHECKS,
  EMPTY_ACCESS,
  accessEquals,
  accessKey,
  accessLabel,
  accessToShorthand,
  breakpointChecksNote,
  breakpointChecksOf,
  breakpointUncheckedNote,
  checkedAccess,
  gutterExecToggle,
  isAccessOffered,
  isEmptyAccess,
  isEventBpType,
  parseAccessFlags,
  parseBreakpointChecks,
  shorthandToAccess,
  toggleFlag,
  type AccessFlags,
  type BreakpointChecks,
} from '../src/breakpoints-core'

const X: AccessFlags = { exec: true, read: false, write: false }
const R: AccessFlags = { exec: false, read: true, write: false }
const W: AccessFlags = { exec: false, read: false, write: true }
const RW: AccessFlags = { exec: false, read: true, write: true }
const XRW: AccessFlags = { exec: true, read: true, write: true }

describe('breakpoints-core > shorthandToAccess', () => {
  it('maps the four canonical strings', () => {
    expect(shorthandToAccess('execute')).toEqual(X)
    expect(shorthandToAccess('read')).toEqual(R)
    expect(shorthandToAccess('write')).toEqual(W)
    expect(shorthandToAccess('readwrite')).toEqual(RW)
  })
  it('is case-insensitive and tolerates legacy spellings', () => {
    expect(shorthandToAccess('READ')).toEqual(R)
    expect(shorthandToAccess('Execution')).toEqual(X)
  })
  it('falls back to execute for null/empty/unknown', () => {
    expect(shorthandToAccess(null)).toEqual(X)
    expect(shorthandToAccess(undefined)).toEqual(X)
    expect(shorthandToAccess('')).toEqual(X)
    expect(shorthandToAccess('nonsense')).toEqual(X)
  })
})

describe('breakpoints-core > parseAccessFlags', () => {
  it('reads the tagged Access object, coercing missing bits to false', () => {
    expect(parseAccessFlags({ Access: { exec: true, read: false, write: false } })).toEqual(X)
    expect(parseAccessFlags({ Access: { read: true, write: true } })).toEqual(RW)
    expect(parseAccessFlags({ Access: {} })).toEqual(EMPTY_ACCESS)
  })
  it('tolerates a null/missing Access payload', () => {
    expect(parseAccessFlags({ Access: null as unknown as Partial<AccessFlags> })).toEqual(
      EMPTY_ACCESS,
    )
  })
  it('treats non-true values as false (no truthy coercion)', () => {
    // serde never emits this, but the parser must not trust truthy junk.
    expect(parseAccessFlags({ Access: { exec: 1 as unknown as boolean } })).toEqual(EMPTY_ACCESS)
  })
  it('maps Event breakpoints to empty access', () => {
    expect(parseAccessFlags({ Event: { code: 7, param: 3 } })).toEqual(EMPTY_ACCESS)
  })
  it('accepts shorthand/legacy strings and nullish', () => {
    expect(parseAccessFlags('write')).toEqual(W)
    expect(parseAccessFlags('Execute')).toEqual(X)
    expect(parseAccessFlags(null)).toEqual(X)
    expect(parseAccessFlags(undefined)).toEqual(X)
  })
})

describe('breakpoints-core > isEventBpType', () => {
  it('detects the Event variant only', () => {
    expect(isEventBpType({ Event: { code: 1, param: 0 } })).toBe(true)
    expect(isEventBpType({ Access: { exec: true } })).toBe(false)
    expect(isEventBpType('execute')).toBe(false)
    expect(isEventBpType(null)).toBe(false)
    expect(isEventBpType(undefined)).toBe(false)
  })
})

describe('breakpoints-core > accessToShorthand', () => {
  it('prefers memory direction, then execute', () => {
    expect(accessToShorthand(RW)).toBe('readwrite')
    expect(accessToShorthand(XRW)).toBe('readwrite')
    expect(accessToShorthand(W)).toBe('write')
    expect(accessToShorthand({ exec: true, read: false, write: true })).toBe('write')
    expect(accessToShorthand(R)).toBe('read')
    expect(accessToShorthand(X)).toBe('execute')
    expect(accessToShorthand(EMPTY_ACCESS)).toBe('execute')
  })
})

describe('breakpoints-core > isEmptyAccess', () => {
  it('is true only when all flags are off', () => {
    expect(isEmptyAccess(EMPTY_ACCESS)).toBe(true)
    expect(isEmptyAccess(X)).toBe(false)
    expect(isEmptyAccess(R)).toBe(false)
    expect(isEmptyAccess(W)).toBe(false)
  })
})

describe('breakpoints-core > accessEquals', () => {
  it('compares all three flags', () => {
    expect(accessEquals(X, { exec: true, read: false, write: false })).toBe(true)
    expect(accessEquals(X, R)).toBe(false)
    expect(accessEquals(R, W)).toBe(false)
    expect(accessEquals(RW, XRW)).toBe(false)
  })
})

describe('breakpoints-core > toggleFlag', () => {
  it('flips one bit without mutating the input', () => {
    const flipped = toggleFlag(X, 'read')
    expect(flipped).toEqual({ exec: true, read: true, write: false })
    expect(X).toEqual({ exec: true, read: false, write: false })
    expect(toggleFlag(X, 'exec')).toEqual(EMPTY_ACCESS)
    expect(toggleFlag(EMPTY_ACCESS, 'write')).toEqual(W)
  })
})

describe('breakpoints-core > accessLabel', () => {
  it('renders E/R/W order, dash when inert', () => {
    expect(accessLabel(X)).toBe('E')
    expect(accessLabel(R)).toBe('R')
    expect(accessLabel(W)).toBe('W')
    expect(accessLabel(RW)).toBe('RW')
    expect(accessLabel(XRW)).toBe('ERW')
    expect(accessLabel(EMPTY_ACCESS)).toBe('-')
  })
})

describe('breakpoints-core > accessKey', () => {
  it('is a stable subset string, "none" when inert', () => {
    expect(accessKey(X)).toBe('e')
    expect(accessKey(R)).toBe('r')
    expect(accessKey(W)).toBe('w')
    expect(accessKey(RW)).toBe('rw')
    expect(accessKey(XRW)).toBe('erw')
    expect(accessKey(EMPTY_ACCESS)).toBe('none')
  })
})

// What a CPU checks, as the cores declare it: everything on a main CPU,
// nothing on a CPU whose breakpoints no loop checks, and the partial sets
// the type allows.
const ALL: BreakpointChecks = { execute: true, memory: true, conditions: true }
const NONE: BreakpointChecks = { execute: false, memory: false, conditions: false }
const EXEC_ONLY: BreakpointChecks = { execute: true, memory: false, conditions: true }
const MEMORY_ONLY: BreakpointChecks = { execute: false, memory: true, conditions: true }
const NO_CONDITIONS: BreakpointChecks = { execute: true, memory: true, conditions: false }

describe('breakpoints-core > parseBreakpointChecks', () => {
  it('reads the three booleans a core reports', () => {
    expect(parseBreakpointChecks({ execute: true, memory: true, conditions: true })).toEqual(ALL)
    expect(parseBreakpointChecks({ execute: false, memory: false, conditions: false })).toEqual(NONE)
    expect(parseBreakpointChecks({ execute: true, memory: false, conditions: true })).toEqual(EXEC_ONLY)
  })

  it('counts what a core does not say as checked', () => {
    expect(parseBreakpointChecks(undefined)).toEqual(ALL)
    expect(parseBreakpointChecks(null)).toEqual(ALL)
    expect(parseBreakpointChecks('none')).toEqual(ALL)
    expect(parseBreakpointChecks({})).toEqual(ALL)
    expect(parseBreakpointChecks({ execute: 0, memory: 'no', conditions: false })).toEqual(NO_CONDITIONS)
  })

  it('never hands out the shared constant', () => {
    const parsed = parseBreakpointChecks(null)
    parsed.execute = false
    expect(ALL_BREAKPOINT_CHECKS.execute).toBe(true)
  })
})

describe('breakpoints-core > breakpointChecksOf', () => {
  it("asks the core for the CPU's checks, with the core as `this`", () => {
    const emulator = {
      sound: NONE,
      getBreakpointChecks(this: { sound: BreakpointChecks }, cpuId: number) {
        return cpuId === 1 ? this.sound : ALL
      },
    }
    expect(breakpointChecksOf(emulator, 0)).toEqual(ALL)
    expect(breakpointChecksOf(emulator, 1)).toEqual(NONE)
  })

  it('offers everything when the core cannot tell', () => {
    expect(breakpointChecksOf(null, 1)).toEqual(ALL)
    expect(breakpointChecksOf(undefined, 1)).toEqual(ALL)
    expect(breakpointChecksOf(7, 1)).toEqual(ALL)
    expect(breakpointChecksOf({}, 1)).toEqual(ALL)
    expect(breakpointChecksOf({ getBreakpointChecks: NONE }, 1)).toEqual(ALL)
    const throwing = {
      getBreakpointChecks() {
        throw new Error('trap')
      },
    }
    expect(breakpointChecksOf(throwing, 1)).toEqual(ALL)
  })
})

describe('breakpoints-core > checkedAccess / isAccessOffered', () => {
  it('lets a user set only the flags a CPU checks', () => {
    expect(checkedAccess(ALL)).toEqual(XRW)
    expect(checkedAccess(NONE)).toEqual(EMPTY_ACCESS)
    expect(checkedAccess(EXEC_ONLY)).toEqual(X)
    expect(checkedAccess(MEMORY_ONLY)).toEqual(RW)
  })

  it('offers a breakpoint only when its CPU checks all it watches', () => {
    for (const flags of [X, R, W, RW, XRW]) expect(isAccessOffered(flags, ALL)).toBe(true)
    for (const flags of [X, R, W, RW, XRW]) expect(isAccessOffered(flags, NONE)).toBe(false)
    expect(isAccessOffered(X, EXEC_ONLY)).toBe(true)
    expect(isAccessOffered(R, EXEC_ONLY)).toBe(false)
    expect(isAccessOffered(XRW, EXEC_ONLY)).toBe(false)
    expect(isAccessOffered(W, MEMORY_ONLY)).toBe(true)
    expect(isAccessOffered(X, MEMORY_ONLY)).toBe(false)
  })

  it('never offers a breakpoint that watches nothing', () => {
    expect(isAccessOffered(EMPTY_ACCESS, ALL)).toBe(false)
  })
})

describe('breakpoints-core > breakpointChecksNote', () => {
  it('says nothing for a CPU that checks everything', () => {
    expect(breakpointChecksNote(ALL, 'Z80')).toBeNull()
  })

  it('names the CPU, or says "this CPU"', () => {
    expect(breakpointChecksNote(NONE, 'Z80')).toBe('The emulator checks no breakpoints on the Z80.')
    expect(breakpointChecksNote(NONE)).toBe('The emulator checks no breakpoints on this CPU.')
    expect(breakpointChecksNote(NONE, null)).toBe('The emulator checks no breakpoints on this CPU.')
    expect(breakpointChecksNote(NONE, '')).toBe('The emulator checks no breakpoints on this CPU.')
  })

  it('names what a CPU leaves unchecked', () => {
    expect(breakpointChecksNote(EXEC_ONLY, 'GSU')).toBe(
      'The emulator does not check read and write watchpoints on the GSU.',
    )
    expect(breakpointChecksNote(MEMORY_ONLY, 'GSU')).toBe(
      'The emulator does not check execute breakpoints on the GSU.',
    )
    expect(breakpointChecksNote(NO_CONDITIONS, 'GSU')).toBe('The emulator does not check conditions on the GSU.')
    expect(breakpointChecksNote({ execute: true, memory: false, conditions: false }, 'GSU')).toBe(
      'The emulator does not check read and write watchpoints or conditions on the GSU.',
    )
  })
})

describe('breakpoints-core > breakpointUncheckedNote', () => {
  it('says nothing when the CPU checks all of the breakpoint', () => {
    expect(breakpointUncheckedNote(XRW, true, ALL, '68000')).toBeNull()
    expect(breakpointUncheckedNote(X, false, EXEC_ONLY, 'GSU')).toBeNull()
    expect(breakpointUncheckedNote(RW, false, MEMORY_ONLY, 'GSU')).toBeNull()
    expect(breakpointUncheckedNote(X, false, NO_CONDITIONS, 'GSU')).toBeNull()
    // A breakpoint that watches nothing and holds no condition asks nothing of its CPU.
    expect(breakpointUncheckedNote(EMPTY_ACCESS, false, NONE, 'Z80')).toBeNull()
  })

  it("names the CPU's checks when any part of the breakpoint is unchecked", () => {
    const none = 'The emulator checks no breakpoints on the Z80.'
    expect(breakpointUncheckedNote(X, false, NONE, 'Z80')).toBe(none)
    expect(breakpointUncheckedNote(W, false, NONE, 'Z80')).toBe(none)
    expect(breakpointUncheckedNote(R, false, EXEC_ONLY)).toBe(
      'The emulator does not check read and write watchpoints on this CPU.',
    )
    expect(breakpointUncheckedNote(XRW, false, EXEC_ONLY, 'GSU')).toBe(
      'The emulator does not check read and write watchpoints on the GSU.',
    )
    expect(breakpointUncheckedNote(X, false, MEMORY_ONLY, 'GSU')).toBe(
      'The emulator does not check execute breakpoints on the GSU.',
    )
    expect(breakpointUncheckedNote(X, true, NO_CONDITIONS, 'GSU')).toBe(
      'The emulator does not check conditions on the GSU.',
    )
    expect(breakpointUncheckedNote(EMPTY_ACCESS, true, NONE, 'Z80')).toBe(none)
  })
})

describe('breakpoints-core > gutterExecToggle', () => {
  it('adds an execute breakpoint where the CPU has none, when it checks execute breakpoints', () => {
    expect(gutterExecToggle(EMPTY_ACCESS, ALL)).toEqual(X)
    expect(gutterExecToggle(EMPTY_ACCESS, EXEC_ONLY)).toEqual(X)
  })

  it('adds the exec bit to a watchpoint, keeping its flags', () => {
    expect(gutterExecToggle(RW, ALL)).toEqual(XRW)
  })

  it('clears the exec bit, leaving an empty access when it was the last flag', () => {
    expect(gutterExecToggle(X, ALL)).toEqual(EMPTY_ACCESS)
    expect(gutterExecToggle(XRW, ALL)).toEqual(RW)
  })

  it('refuses to set the bit on a CPU that checks no execute breakpoints', () => {
    expect(gutterExecToggle(EMPTY_ACCESS, NONE)).toBeNull()
    expect(gutterExecToggle(RW, MEMORY_ONLY)).toBeNull()
  })

  it('still clears it there: a breakpoint that never stops the CPU can go', () => {
    expect(gutterExecToggle(X, NONE)).toEqual(EMPTY_ACCESS)
    expect(gutterExecToggle(XRW, MEMORY_ONLY)).toEqual(RW)
  })
})
