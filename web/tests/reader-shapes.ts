// What the emulator's readers give, as the binding crates' own tests record
// it, so a panel's test can hold its fixture to the record: a test writes
// `expect(recordedIn(file)).toContain(describeShape(fixture))`, and a
// reader whose keys change fails the binding's test until its record
// follows, then this one until the fixture and the panel follow.

import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

/**
 * `value` described as `retro_wasm::test_support::shape` describes a
 * plain value: `null`, `undefined`, `true`, `false`; a finite number as
 * Rust prints an `f64` (`-0` for negative zero); a bigint with an `n`; a
 * string quoted (as Rust's `Debug` quotes text with no control character
 * in it); an array element by element; an object's own properties sorted
 * by name, `{a: 1, b: [2]}`.
 */
export function describeShape(value: unknown): string {
  if (value === null) return 'null'
  if (value === undefined) return 'undefined'
  if (typeof value === 'boolean') return value ? 'true' : 'false'
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) throw new Error(`no description for ${value}`)
    return Object.is(value, -0) ? '-0' : String(value)
  }
  if (typeof value === 'bigint') return `${value}n`
  if (typeof value === 'string') return JSON.stringify(value)
  if (Array.isArray(value)) return `[${value.map(describeShape).join(', ')}]`
  if (typeof value === 'object') {
    const object = value as Record<string, unknown>
    const fields = Object.keys(object)
      .sort()
      .map((key) => `${key}: ${describeShape(object[key])}`)
    return `{${fields.join(', ')}}`
  }
  throw new Error(`no description for a ${typeof value}`)
}

/**
 * The text of `path`, a file of the repository named from its root, as
 * its records spell it: a string continued on the next line (a backslash
 * ending the line, then the indent) joined, and an escaped quote plain.
 */
export function recordedIn(path: string): string {
  const text = readFileSync(fileURLToPath(new URL(`../../${path}`, import.meta.url)), 'utf8')
  return text.replace(/\\\r?\n\s*/g, '').replace(/\\"/g, '"')
}
