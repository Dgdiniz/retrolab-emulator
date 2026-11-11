/**
 * Frame pacing: how many emulated frames a presentation callback owes.
 *
 * The browser's `requestAnimationFrame` is the display's clock, not the
 * machine's. A 30 Hz callback (Chrome's Energy Saver, a MacBook on
 * battery) still owes two frames of a 60 Hz console; a 144 Hz callback
 * owes one frame less than half the time. A loop that runs at most one
 * frame per callback, and discards whatever backlog the sub-period
 * remainder does not cover, runs the game at the presentation rate.
 *
 * This core keeps an **anchor**, the wall time up to which frames have
 * been accounted for, and on every callback runs the frames the wall
 * clock owes since the anchor, bounded by a cap on how much emulated time one
 * callback may catch up, so a long hitch never becomes a fast-forward. The
 * decision takes that cap as a frame count; the loops derive the count from a
 * time with {@link maxFramesForCatchUp}.
 *
 * The cap is a time or a frame count, never a measured cost. Work the machine cannot
 * afford is not predicted and pre-emptively refused: the callback runs what
 * it owes up to the cap, and a machine too slow for that simply produces
 * fewer frames per second. Deciding from a cost estimate instead ranks the
 * frames a fast machine could have run against a p50 that lags behind the
 * work it describes, which is a cap that fires hardest on the displays that
 * need it least.
 *
 * Frames past the cap are **dropped**, and the anchor still advances past
 * them: a discard is explicit and visible, never a debt that would make
 * the next callback run a burst. A gap longer than `stallMs` (the tab was
 * hidden, the compositor froze) resets the anchor to now and runs one
 * frame, so a paused game never sprints on resume.
 *
 * A small epsilon absorbs an early callback: a 60 Hz callback that lands
 * half a millisecond ahead of its period still owes its frame; without the
 * epsilon, sub-millisecond jitter makes a loop skip every other tick.
 *
 * Pure: every input, the clock included, is passed in. Shared by the
 * platform's frame loop, the lesson embed and the OSS shell.
 */

export interface FramePacingInput {
  /** The callback's timestamp (ms, any monotonic clock). */
  now: number
  /** Wall time up to which frames are accounted for; seed with {@link seedAnchor}. */
  anchor: number
  /** The core's frame period in ms ({@link frameTimeMsFor}). */
  frameTimeMs: number
  /**
   * Most frames one callback may run (>= 1). The loops pass
   * `maxFramesForCatchUp(DEFAULT_MAX_CATCH_UP_MS, frameTimeMs)`, or 1 when
   * something has to observe every frame.
   */
  maxFrames: number
  /** Early-callback tolerance in ms; defaults to {@link defaultEpsilonMs}. */
  epsilonMs?: number
  /** A gap longer than this resets the anchor; defaults to {@link DEFAULT_STALL_MS}. */
  stallMs?: number
}

export type FramePacingReason =
  /** Exactly one frame owed. */
  | 'on-time'
  /** Several frames owed and all of them run. */
  | 'catch-up'
  /** Frames owed beyond `maxFrames`; the excess is dropped. */
  | 'capped-count'
  /** The gap exceeded `stallMs`: anchor reset to now, one frame run, the rest dropped. */
  | 'stall-reset'
  /** Nothing owed yet (callback ahead of the period); anchor untouched. */
  | 'early'

export interface FramePacingResult {
  framesToRun: number
  newAnchor: number
  /** Whether this callback should present (draw) after its frames. */
  present: boolean
  /** Owed frames deliberately not run this callback. */
  dropped: number
  reason: FramePacingReason
}

/**
 * A gap this long between callbacks is a pause, not a backlog to catch up.
 *
 * It looks redundant with the catch-up cap, both forgive the whole debt in one
 * callback, and it is not, which was measured rather than argued. The only
 * difference is how much runs on resume: one frame here, the full cap there. A
 * long pause empties the audio queue and drives the rate correction to its
 * limit, and without this reset the cap then lands a whole burst on top of a
 * correction the pause already saturated. With a five-frame cap that made a
 * three-second stall the ring's worst case (E+ 4 159 slots against 3 708 with
 * the reset); on sg1 NTSC at 120 Hz a stall of 0.6-3 s went from E+ 276 to
 * 3 700.
 */
export const DEFAULT_STALL_MS = 500
/**
 * Emulated time one callback may catch up: 5/60 s.
 *
 * In time rather than frames because a frame is not a fixed amount of
 * anything, a PAL frame is 20 ms, an NTSC one 16.7, and what a burst costs
 * downstream is time: the audio it hands the ring is `time x device rate`,
 * whatever the core's rate. One cap therefore bounds the ring the same way on
 * every core, which a frame count cannot.
 *
 * The value is binjgb's `MAX_UPDATE_SEC`, and it was measured rather than
 * borrowed. Over the cadence table on five core rates it is the smallest cap
 * that holds a 15 Hz display, jitter included, with no dropped frame; a
 * count of four drops frames there on any core faster than 60 Hz,
 * and a cap below this one leaves the PAL cores three frames a callback and
 * a tenth of their speed.
 */
export const DEFAULT_MAX_CATCH_UP_MS = 5000 / 60
/**
 * Frames of audio the loop aims to keep queued.
 *
 * Deep enough that a main-thread hiccup has something to play through,
 * shallow enough that the latency is not felt, and the same number the
 * initial cushion is built to, so playback starts on target with nothing to
 * correct.
 *
 * The pacer itself does not read it: holding the queue is the rate
 * controller's job, and it does so by bending the resampling ratio rather
 * than the frame count. The cushion is sized from it, and that cushion and
 * the controller's target have to remain the same expression.
 */
export const AUDIO_TARGET_FRAMES = 3

/**
 * The frame count a catch-up time allows at a core's frame period.
 *
 * Rounded with the same epsilon the pacer applies to the debt. A cap and a
 * debt of the same duration have to round the same way, or a debt exactly the
 * size of the cap is refused by the rounding alone: floored bare, 5/60 s is
 * four frames of a 59.92 Hz core, which then drops a frame on every 15 Hz
 * callback that owes five.
 */
export function maxFramesForCatchUp(catchUpMs: number, frameTimeMs: number): number {
  if (!(Number.isFinite(frameTimeMs) && frameTimeMs > 0)) {
    throw new RangeError(`frameTimeMs must be a positive finite number, got ${frameTimeMs}`)
  }
  return Math.max(1, Math.floor((catchUpMs + defaultEpsilonMs(frameTimeMs)) / frameTimeMs))
}

/** Early-callback tolerance: an eighth of the period (2.1 ms at 60 Hz). */
export function defaultEpsilonMs(frameTimeMs: number): number {
  return frameTimeMs / 8
}

/** The anchor a freshly started (or resumed) loop begins from. */
export function seedAnchor(now: number): number {
  return now
}

/** Frame period for a refresh rate; throws on a rate that is not a positive finite number. */
export function frameTimeMsFor(refreshRateHz: number): number {
  if (!(Number.isFinite(refreshRateHz) && refreshRateHz > 0)) {
    throw new RangeError(`refresh rate must be a positive finite number, got ${refreshRateHz}`)
  }
  return 1000 / refreshRateHz
}

export function decideFramePacing(input: FramePacingInput): FramePacingResult {
  const ft = input.frameTimeMs
  if (!(Number.isFinite(ft) && ft > 0)) {
    throw new RangeError(`frameTimeMs must be a positive finite number, got ${ft}`)
  }
  const { now } = input
  // An unusable anchor (never seeded, or poisoned by a NaN clock) is
  // re-seeded at now and this callback runs nothing.
  if (!Number.isFinite(input.anchor) || !Number.isFinite(now)) {
    return { framesToRun: 0, newAnchor: now, present: false, dropped: 0, reason: 'early' }
  }
  const epsilon = input.epsilonMs ?? defaultEpsilonMs(ft)
  const stallMs = input.stallMs ?? DEFAULT_STALL_MS
  const maxFrames = Math.max(1, Math.floor(input.maxFrames) || 1)
  const elapsed = now - input.anchor
  const owed = Math.floor((elapsed + epsilon) / ft)

  if (elapsed > stallMs) {
    return {
      framesToRun: 1,
      newAnchor: now,
      present: true,
      dropped: Math.max(0, owed - 1),
      reason: 'stall-reset',
    }
  }
  if (owed <= 0) {
    return { framesToRun: 0, newAnchor: input.anchor, present: false, dropped: 0, reason: 'early' }
  }

  const framesToRun = Math.max(1, Math.min(owed, maxFrames))
  const dropped = owed - framesToRun

  let reason: FramePacingReason
  if (owed === 1) reason = 'on-time'
  else if (dropped === 0) reason = 'catch-up'
  else reason = 'capped-count'

  return {
    framesToRun,
    // The anchor consumes every owed frame, dropped ones included: the
    // time is spent, not owed back.
    newAnchor: input.anchor + owed * ft,
    present: framesToRun > 0,
    dropped,
    reason,
  }
}


