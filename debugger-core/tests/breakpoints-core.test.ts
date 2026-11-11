import { describe, expect, it } from 'vitest'
import {
  EMPTY_ACCESS,
  accessEquals,
  accessKey,
  accessLabel,
  accessToShorthand,
  isEmptyAccess,
  isEventBpType,
  parseAccessFlags,
  shorthandToAccess,
  toggleFlag,
  type AccessFlags,
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
