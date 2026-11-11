import { describe, expect, it } from 'vitest'
import fc from 'fast-check'
import {
  DEFAULT_MAX_CATCH_UP_MS,
  DEFAULT_STALL_MS,
  decideFramePacing,
  defaultEpsilonMs,
  frameTimeMsFor,
  maxFramesForCatchUp,
  seedAnchor,
  type FramePacingInput,
} from '../src/frame-pacing-core'

const FT60 = 1000 / 60
/**
 * The count the single-callback cases pin directly. They test the cap as a
 * mechanism, which takes a count; the time the loops derive it from is tested
 * on its own below.
 */
const COUNT_CAP = 4
/** The cap every loop runs with, at a core's period. */
const capAt = (frameTimeMs: number) => maxFramesForCatchUp(DEFAULT_MAX_CATCH_UP_MS, frameTimeMs)

function input(over: Partial<FramePacingInput> = {}): FramePacingInput {
  return {
    now: 0,
    anchor: 0,
    frameTimeMs: FT60,
    maxFrames: COUNT_CAP,
    ...over,
  }
}

describe('helpers', () => {
  it('derives the period from the refresh rate and rejects a bad rate', () => {
    expect(frameTimeMsFor(60)).toBeCloseTo(16.6667, 4)
    expect(frameTimeMsFor(50.007)).toBeCloseTo(19.9972, 4)
    for (const bad of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(() => frameTimeMsFor(bad)).toThrow(RangeError)
    }
  })

  it('epsilon is an eighth of the period', () => {
    expect(defaultEpsilonMs(FT60)).toBeCloseTo(FT60 / 8, 9)
    expect(seedAnchor(123.4)).toBe(123.4)
    expect(DEFAULT_STALL_MS).toBe(500)
    expect(DEFAULT_MAX_CATCH_UP_MS).toBe(5000 / 60)
  })

  it('rejects a non-positive frame period', () => {
    expect(() => decideFramePacing(input({ frameTimeMs: 0 }))).toThrow(RangeError)
    expect(() => decideFramePacing(input({ frameTimeMs: Number.NaN }))).toThrow(RangeError)
  })
})

describe('decideFramePacing - one callback', () => {
  it.each([
    ['exactly one period', FT60, 1, 'on-time'],
    ['one period, a hair early (within epsilon)', FT60 - 1, 1, 'on-time'],
    ['two periods (30 Hz display)', 2 * FT60, 2, 'catch-up'],
    ['four periods (15 Hz display)', 4 * FT60, 4, 'catch-up'],
    ['six periods (a 100 ms hitch)', 6 * FT60, 4, 'capped-count'],
  ] as const)('%s -> runs %i frames (%s)', (_, now, frames, reason) => {
    const r = decideFramePacing(input({ now }))
    expect(r.framesToRun).toBe(frames)
    expect(r.reason).toBe(reason)
    expect(r.present).toBe(true)
  })

  it('an early callback runs nothing and keeps the anchor', () => {
    const r = decideFramePacing(input({ now: FT60 / 2, anchor: 0 }))
    expect(r).toEqual({ framesToRun: 0, newAnchor: 0, present: false, dropped: 0, reason: 'early' })
  })

  it('the epsilon absorbs an early callback up to its size, not past it', () => {
    const eps = defaultEpsilonMs(FT60)
    expect(decideFramePacing(input({ now: FT60 - eps + 0.001 })).framesToRun).toBe(1)
    expect(decideFramePacing(input({ now: FT60 - eps - 0.001 })).framesToRun).toBe(0)
    expect(decideFramePacing(input({ now: FT60 - 3, epsilonMs: 3.5 })).framesToRun).toBe(1)
    expect(decideFramePacing(input({ now: FT60 - 3, epsilonMs: 2 })).framesToRun).toBe(0)
  })

  it('advances the anchor by every owed frame, dropped ones included', () => {
    const r = decideFramePacing(input({ now: 6 * FT60 + 1 }))
    expect(r.framesToRun).toBe(4)
    expect(r.dropped).toBe(2)
    expect(r.newAnchor).toBeCloseTo(6 * FT60, 9)
    // Feeding the new anchor back with the same clock owes nothing more.
    expect(decideFramePacing(input({ now: 6 * FT60 + 1, anchor: r.newAnchor })).reason).toBe('early')
  })

  it('runs everything owed up to the count cap, whatever a frame costs', () => {
    // Four periods elapsed and four owed: all of them run. How expensive a
    // frame is cannot enter here, the decision has no cost to read.
    const four = decideFramePacing(input({ now: 4 * FT60 }))
    expect(four.framesToRun).toBe(4)
    expect(four.dropped).toBe(0)
    expect(four.reason).toBe('catch-up')
  })

  it('drops the backlog past the count cap, and says so', () => {
    const r = decideFramePacing(input({ now: 8 * FT60 }))
    expect(r.framesToRun).toBe(4)
    expect(r.dropped).toBe(4)
    expect(r.reason).toBe('capped-count')
  })

  it('a gap past the stall threshold resets the anchor and runs one frame', () => {
    const r = decideFramePacing(input({ now: 800, anchor: 0 }))
    expect(r.framesToRun).toBe(1)
    expect(r.newAnchor).toBe(800)
    expect(r.present).toBe(true)
    expect(r.reason).toBe('stall-reset')
    expect(r.dropped).toBe(Math.floor((800 + defaultEpsilonMs(FT60)) / FT60) - 1)
    // The threshold is configurable; at the threshold itself it is a catch-up.
    expect(decideFramePacing(input({ now: 800, stallMs: 1000 })).reason).toBe('capped-count')
    expect(decideFramePacing(input({ now: DEFAULT_STALL_MS })).reason).toBe('capped-count')
  })

  it('treats a non-positive or fractional maxFrames as 1', () => {
    expect(decideFramePacing(input({ now: 3 * FT60, maxFrames: 0 })).framesToRun).toBe(1)
    expect(decideFramePacing(input({ now: 3 * FT60, maxFrames: 2.9 })).framesToRun).toBe(2)
    expect(decideFramePacing(input({ now: 3 * FT60, maxFrames: Number.NaN })).framesToRun).toBe(1)
  })

  it('re-seeds an unusable anchor or clock at now without running', () => {
    expect(decideFramePacing(input({ now: 50, anchor: Number.NaN }))).toEqual({
      framesToRun: 0, newAnchor: 50, present: false, dropped: 0, reason: 'early',
    })
    expect(decideFramePacing(input({ now: Number.NaN, anchor: 0 })).framesToRun).toBe(0)
  })
})

describe('decideFramePacing - invariants', () => {
  const arbInput = fc.record({
    anchor: fc.double({ min: 0, max: 1e6, noNaN: true }),
    gap: fc.double({ min: -30, max: 2000, noNaN: true }),
    frameTimeMs: fc.double({ min: 4, max: 70, noNaN: true }),
    maxFrames: fc.integer({ min: 1, max: 8 }),
  })

  it('never runs more than the cap, accounts every owed frame, and lands the anchor inside one period of now', () => {
    fc.assert(
      fc.property(arbInput, ({ anchor, gap, frameTimeMs, maxFrames }) => {
        const now = anchor + gap
        const eps = defaultEpsilonMs(frameTimeMs)
        const r = decideFramePacing({ now, anchor, frameTimeMs, maxFrames })
        expect(r.framesToRun).toBeLessThanOrEqual(maxFrames)
        expect(r.dropped).toBeGreaterThanOrEqual(0)
        expect(r.present).toBe(r.framesToRun > 0)
        if (r.reason === 'early') {
          expect(r.framesToRun).toBe(0)
          expect(r.newAnchor).toBe(anchor)
        } else if (r.reason === 'stall-reset') {
          expect(r.framesToRun).toBe(1)
          expect(r.newAnchor).toBe(now)
        } else {
          const owed = Math.floor((gap + eps) / frameTimeMs)
          expect(r.framesToRun + r.dropped).toBe(owed)
          expect(r.newAnchor).toBeGreaterThanOrEqual(anchor)
          expect(r.newAnchor).toBeLessThanOrEqual(now + eps + 1e-9)
          expect(r.newAnchor).toBeGreaterThan(now + eps - frameTimeMs - 1e-9)
          // Feeding the new anchor back with the same clock owes nothing.
          const again = decideFramePacing({
            now, anchor: r.newAnchor, frameTimeMs, maxFrames,
          })
          expect(again.reason).toBe('early')
        }
      }),
      { numRuns: 500 },
    )
  })
})

// -- Cadence table ----------------------------------------------------
// A synthetic requestAnimationFrame timeline drives the core exactly as a
// loop would; the frames it runs are counted against the wall clock.

function makeRng(seed: number) {
  let s = seed >>> 0
  const next = () => {
    s = (Math.imul(1664525, s) + 1013904223) >>> 0
    return (s + 0.5) / 4294967296
  }
  const gauss = () => {
    const u = next()
    const v = next()
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v)
  }
  return { gauss }
}

function timeline(hz: number, seconds: number, jitterSigmaMs: number, stalls: boolean, seed = 7): number[] {
  const rng = makeRng(seed)
  const period = 1000 / hz
  const out: number[] = []
  let t = 0
  let offset = 0
  while (t < seconds * 1000) {
    t += period
    if (stalls && Math.abs(t - 5000) < period / 2) offset += 100
    if (stalls && Math.abs(t - 12000) < period / 2) offset += 400
    out.push(t + offset + (jitterSigmaMs > 0 ? rng.gauss() * jitterSigmaMs : 0))
  }
  return out
}

function energySaverFlip(seconds: number): number[] {
  const out: number[] = []
  let t = 0
  while (t < seconds * 1000) {
    t += 1000 / (Math.floor(t / 2000) % 2 === 0 ? 60 : 30)
    out.push(t)
  }
  return out
}

function runLoop(ts: number[], frameTimeMs: number) {
  let anchor = seedAnchor(0)
  let frames = 0
  let dropped = 0
  let presented = 0
  let stallResets = 0
  for (const now of ts) {
    const r = decideFramePacing({
      now, anchor, frameTimeMs, maxFrames: capAt(frameTimeMs),
    })
    anchor = r.newAnchor
    frames += r.framesToRun
    dropped += r.dropped
    if (r.present) presented++
    if (r.reason === 'stall-reset') stallResets++
  }
  const elapsedSec = ts[ts.length - 1]! / 1000
  return { frames, dropped, presented, stallResets, elapsedSec }
}

const SECONDS = 20
const CADENCES = [15, 24, 30, 48, 50, 59.94, 60, 60.05, 72, 75, 90, 100, 120, 144, 165, 240]
const JITTERS = [0, 0.5, 1.5]
const RATES = [
  ['60 Hz nominal', 60],
  ['sg1 NTSC 59.9227 Hz', 59.9227],
  // Faster than 60 Hz, so a 15 Hz callback owes a little over four frames
  // and now and then five. Without this row, that case goes untested.
  ['snes1 NTSC 60.0985 Hz', 60.0985],
  ['snes1 PAL 50.007 Hz', 50.007],
  ['sg1 PAL 49.7015 Hz', 49.7015],
] as const

describe('decideFramePacing - cadence table', () => {
  // 15 Hz with jitter is in the table on purpose. A count cap of four is
  // exactly what a 60 Hz core owes there, so an early callback owes three, the
  // next five, and a frame drops. Leaving 15 Hz out would hide the very case
  // the cap exists to decide; a cap in time holds it.
  const cases = RATES.flatMap(([label, hz]) =>
    CADENCES.flatMap((cadence) =>
      JITTERS.map((jitter) => [label, hz, cadence, jitter] as const),
    ),
  )

  it.each(cases)('%s on a %s Hz display (jitter sigma=%s ms) runs the core rate within 0.2 %% with no drops', (_, hz, cadence, jitter) => {
    const ft = frameTimeMsFor(hz)
    const r = runLoop(timeline(cadence, SECONDS, jitter, false), ft)
    const fps = r.frames / r.elapsedSec
    expect(Math.abs(fps - hz) / hz).toBeLessThanOrEqual(0.002)
    expect(r.dropped).toBe(0)
    expect(r.stallResets).toBe(0)
  })

  it.each(CADENCES.map((c) => [c] as const))('with two hitches (100 + 400 ms) on a %s Hz display, the wall clock is fully accounted and only the hitches drop frames', (cadence) => {
    const ft = frameTimeMsFor(60)
    const r = runLoop(timeline(cadence, SECONDS, 0, true), ft)
    // Every owed frame was either run or explicitly dropped.
    expect(Math.abs((r.frames + r.dropped) / r.elapsedSec - 60) / 60).toBeLessThanOrEqual(0.002)
    // Both gaps are under the 500 ms stall threshold: capped catch-ups, not resets.
    expect(r.stallResets).toBe(0)
    expect(r.dropped).toBeGreaterThanOrEqual(1)
    // 500 ms of hitch is at most 30 frames; nothing else was lost.
    expect(r.dropped).toBeLessThanOrEqual(30)
  })

  it('a 700 ms freeze is a stall reset: one frame on resume, the backlog dropped, then back on pace', () => {
    const ts = timeline(60, SECONDS, 0, false).map((t) => (t > 8000 ? t + 700 : t))
    const r = runLoop(ts, frameTimeMsFor(60))
    expect(r.stallResets).toBe(1)
    expect(r.dropped).toBeGreaterThanOrEqual(40)
    expect(r.dropped).toBeLessThanOrEqual(43)
    expect(Math.abs((r.frames + r.dropped) / r.elapsedSec - 60) / 60).toBeLessThanOrEqual(0.002)
  })

  it('the Energy Saver flip (60/30 Hz every 2 s) runs at 100 % speed', () => {
    const r = runLoop(energySaverFlip(SECONDS), frameTimeMsFor(60))
    expect(Math.abs(r.frames / r.elapsedSec - 60) / 60).toBeLessThanOrEqual(0.002)
    expect(r.dropped).toBe(0)
  })

  it('a 30 Hz display presents every callback and runs two frames each', () => {
    const ts = timeline(30, SECONDS, 0, false)
    const r = runLoop(ts, frameTimeMsFor(60))
    expect(r.presented).toBe(ts.length)
    expect(r.frames / r.presented).toBeCloseTo(2, 1)
  })

  // 5 ms a frame is not "too slow for the budget" on a 30 Hz display: it is
  // two frames costing 10 ms out of the 33 ms the callback is accounting for.
  // Budgeting against the frame period would shrink the allowance exactly
  // where the work grows, and a suite of green cadence cases that all cost
  // nothing would not show the core running at half speed.
  it('holds full speed on a 30 Hz display', () => {
    const ts = timeline(30, SECONDS, 0, false)
    const r = runLoop(ts, frameTimeMsFor(60))
    expect(r.frames / r.elapsedSec).toBeCloseTo(60, 0)
    expect(r.dropped).toBe(0)
  })
})

/**
 * The pacer has to hold each core's own rate on the display cadences that
 * actually occur in the field. Both cores below have been measured in a
 * browser running at half speed or dropping frames under a cost-based cap.
 *
 * The frame cost that causes that is deliberately absent here: the decision
 * takes no cost, so there is no value of it that could bind, and no sweep
 * over costs is needed; it is a property of the signature.
 */
describe('holding the core rate on a real display cadence', () => {
  const SG1_NTSC = 59.9227
  const SNES1_NTSC = 60.0985

  /** Run a display cadence for `seconds` and report what the core managed. */
  function drive(coreHz: number, displayHz: number, seconds: number) {
    const ft = frameTimeMsFor(coreHz)
    const period = 1000 / displayHz
    let anchor = 0
    let frames = 0
    let dropped = 0
    for (let t = period; t <= seconds * 1000; t += period) {
      const r = decideFramePacing({
        now: t, anchor, frameTimeMs: ft, maxFrames: capAt(ft),
      })
      anchor = r.newAnchor
      frames += r.framesToRun
      dropped += r.dropped
    }
    return { fps: frames / seconds, dropped }
  }

  // A display at half the core's rate owes two frames per callback. That is
  // the steady state, not a backlog: nothing about it should be capped.
  it.each([
    ['a Genesis core', SG1_NTSC],
    ['a SNES core', SNES1_NTSC],
  ])('%s holds full speed on a 30 Hz display', (_label, coreHz) => {
    const r = drive(coreHz, 30, 60)
    expect(Math.abs(r.fps - coreHz) / coreHz).toBeLessThanOrEqual(0.002)
    expect(r.dropped).toBe(0)
  })

  // A core that runs slightly FASTER than the display cannot fit every frame
  // into one callback: roughly every ten seconds a callback owes two. Refusing
  // that second frame loses it for good, and the audio queue drains by the
  // difference until it underruns.
  it('never drops the extra frame a faster-than-display core owes at 60 Hz', () => {
    const r = drive(SNES1_NTSC, 60, 120)
    expect(r.dropped).toBe(0)
    expect(Math.abs(r.fps - SNES1_NTSC) / SNES1_NTSC).toBeLessThanOrEqual(0.002)
  })

  it.each([30, 59.94, 60, 120, 144])('holds the core rate on a %s Hz display', (displayHz) => {
    for (const coreHz of [SG1_NTSC, SNES1_NTSC]) {
      const r = drive(coreHz, displayHz, 60)
      expect(Math.abs(r.fps - coreHz) / coreHz).toBeLessThanOrEqual(0.002)
    }
  })

  // The one bound that remains is the catch-up cap, and it is what keeps a
  // callback from turning a long hitch into a fast-forward.
  it('bounds a callback by the catch-up cap and nothing else', () => {
    const ft = frameTimeMsFor(SG1_NTSC)
    const cap = capAt(ft)
    const r = decideFramePacing({
      now: 10 * ft, anchor: 0, frameTimeMs: ft, maxFrames: cap,
    })
    expect(r.framesToRun).toBe(cap)
    expect(r.dropped).toBe(10 - cap)
    expect(r.reason).toBe('capped-count')
  })
})

describe('the catch-up cap, in time', () => {
  const RATE_LIST = [60, 59.9227, 60.0985, 50.007, 49.7015]

  it('converts with the pacer\'s own epsilon, so a debt the size of the cap is never refused by rounding', () => {
    const ft = frameTimeMsFor(59.9227)
    // The hazard is real: floored bare, 5/60 s is four frames of sg1 NTSC.
    expect(Math.floor(DEFAULT_MAX_CATCH_UP_MS / ft)).toBe(4)
    expect(capAt(ft)).toBe(5)
    expect(capAt(frameTimeMsFor(60))).toBe(5)
    expect(capAt(frameTimeMsFor(60.0985))).toBe(5)
    expect(capAt(frameTimeMsFor(50.007))).toBe(4)
    expect(capAt(frameTimeMsFor(49.7015))).toBe(4)
  })

  it('bounds the same emulated time on every core, which a frame count cannot', () => {
    // The audio a burst hands the ring is its time times the device rate, so
    // a cap in time bounds the ring identically whatever the core's rate.
    for (const hz of RATE_LIST) {
      const ft = frameTimeMsFor(hz)
      expect(capAt(ft) * ft).toBeLessThanOrEqual(DEFAULT_MAX_CATCH_UP_MS + defaultEpsilonMs(ft))
    }
  })

  it('never allows less than one frame', () => {
    expect(maxFramesForCatchUp(1, FT60)).toBe(1)
    expect(maxFramesForCatchUp(0, FT60)).toBe(1)
  })

  it('refuses a frame period that is not a positive finite number', () => {
    expect(() => maxFramesForCatchUp(DEFAULT_MAX_CATCH_UP_MS, 0)).toThrow(RangeError)
    expect(() => maxFramesForCatchUp(DEFAULT_MAX_CATCH_UP_MS, Number.NaN)).toThrow(RangeError)
  })

  it('holds a 15 Hz display with jitter on every core, where a count of four dropped frames', () => {
    // The test this block is named for. Measured over a minute, three seeds,
    // five core rates: the count cap of four dropped frames here, even
    // jitter-free on a core faster than 60 Hz.
    const run = (hz: number, sigma: number, seed: number, maxFrames: number) => {
      const ft = frameTimeMsFor(hz)
      let anchor = seedAnchor(0)
      let frames = 0
      let dropped = 0
      const ts = timeline(15, 60, sigma, false, seed)
      for (const now of ts) {
        const r = decideFramePacing({ now, anchor, frameTimeMs: ft, maxFrames })
        anchor = r.newAnchor
        frames += r.framesToRun
        dropped += r.dropped
      }
      return { err: Math.abs(frames / (ts[ts.length - 1]! / 1000) - hz) / hz, dropped }
    }
    for (const hz of RATE_LIST) {
      for (const sigma of [0, 0.5, 1.5]) {
        for (const seed of [7, 11, 23]) {
          const r = run(hz, sigma, seed, capAt(frameTimeMsFor(hz)))
          expect(r.dropped).toBe(0)
          expect(r.err).toBeLessThanOrEqual(0.002)
        }
      }
    }
    // Non-vacuity: a count cap of four really does fail these cases.
    expect(run(60.0985, 0, 7, COUNT_CAP).dropped).toBeGreaterThan(0)
    expect(run(59.9227, 0.5, 7, COUNT_CAP).dropped).toBeGreaterThan(0)
  })
})
