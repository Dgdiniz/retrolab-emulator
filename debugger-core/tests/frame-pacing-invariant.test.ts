import { describe, expect, it } from 'vitest'
import SOURCE from '../src/frame-pacing-core.ts?raw'

/**
 * A cap on the frame loop is expressed in time or in a frame count, never in
 * a measured cost.
 *
 * This is pinned against the source rather than against behaviour because,
 * once the cost is gone from the signature, there is nothing left to vary:
 * no input means no test case. The guarantee is structural, and what can
 * still regress is somebody adding the input back.
 *
 * It is worth a file because the failure is silent. A budget derived from
 * the p50 frame cost and divided by a frame period, rather than by the
 * interval it rations, halves the emulation rate on every display running
 * below the core's rate, and a cadence suite passes anyway when every case
 * runs at a cost of zero, the single value at which a cost-based cap cannot
 * bind.
 *
 * Two properties, of which the first is the load-bearing one:
 *   - the decision takes no cost;
 *   - it reports no reason that implies one.
 */
/** Field names declared on an exported interface, ignoring doc comments. */
function fieldsOf(source: string, interfaceName: string): string[] {
  const start = source.indexOf(`export interface ${interfaceName} {`)
  expect(start, `${interfaceName} not found`).toBeGreaterThanOrEqual(0)
  const body = source.slice(start, source.indexOf('\n}', start))
  return [...body.matchAll(/^ {2}(\w+)\??:/gm)].map((m) => m[1]!)
}

describe('the frame pacer takes no measured cost', () => {
  it('declares no cost or budget field on its input', () => {
    const fields = fieldsOf(SOURCE, 'FramePacingInput')
    // Non-vacuity: the parse found real fields before we assert about them.
    expect(fields).toContain('frameTimeMs')
    expect(fields).toContain('maxFrames')
    expect(fields.filter((f) => /cost|budget/i.test(f))).toEqual([])
  })

  it('derives the default cap from a fixed time and the core period, and from nothing measured', () => {
    // The cap is a time. Where the number comes from must not change: a
    // literal time, converted by the core's own period, never a frame cost,
    // and never a measured display rate, which is the other measurement a
    // cap could be tempted to follow.
    const decl = SOURCE.match(/export const DEFAULT_MAX_CATCH_UP_MS = ([^\n]+)/)
    expect(decl, 'DEFAULT_MAX_CATCH_UP_MS not found').not.toBeNull()
    expect(decl![1]!.trim()).toMatch(/^[\d\s/*.]+$/)
    expect(SOURCE).toContain('export function maxFramesForCatchUp(catchUpMs: number, frameTimeMs: number)')
  })

  it('reports no reason that implies a cost was weighed', () => {
    const union = SOURCE.slice(
      SOURCE.indexOf('export type FramePacingReason'),
      SOURCE.indexOf('export interface FramePacingResult'),
    )
    const reasons = [...union.matchAll(/'([a-z-]+)'/g)].map((m) => m[1]!)
    expect(reasons).toContain('capped-count')
    expect(reasons.filter((r) => /budget|cost/.test(r))).toEqual([])
  })
})
