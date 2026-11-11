/**
 * Game loop, owns the requestAnimationFrame loop that drives
 * `emulator.runFrame()`, blits the framebuffer via WebGL, paces the
 * emulation to the core's own refresh rate, surfaces breakpoint hits, and
 * forwards per-frame trace flushes to the tracing handle.
 *
 * `requestAnimationFrame` is the display's clock, not the machine's: on a
 * 30 Hz display (a laptop on battery, Chrome's Energy Saver) every callback
 * owes two frames of a 60 Hz console. Each callback runs the frames the
 * wall clock owes since the last one (paced by the shared
 * `decideFramePacing`, capped in emulated time, the backlog past the cap
 * dropped explicitly), drains audio and the trace once per frame, and
 * blits once.
 *
 * Dependencies are injected so the loop can stay free of module-level
 * globals: the bridge gives us emulator + run state, the WebGL renderer
 * is supplied once at create, and audio + tracing + perf overlay + the
 * "panel" refresher are all `Deps`. Status updates and the FPS HUD are
 * also pushed in to avoid `document.getElementById` calls per frame.
 */

import type { DebugBridge } from './debug-bridge'
import {
  DEFAULT_MAX_CATCH_UP_MS,
  decideFramePacing,
  frameTimeMsFor,
  maxFramesForCatchUp,
  seedAnchor,
} from '@debugger-core/frame-pacing-core'

const REGISTERS_REFRESH_EVERY = 10
const FPS_AVG_FRAMES = 60

export interface GameLoopDeps {
  /** WebGLRenderer instance (defined in webgl-renderer.js). */
  getRenderer(): any | null
  /** AudioManager wrapper (defined in audio.js); may be null when audio is disabled. */
  getAudioManager(): any | null
  /** Performance overlay; may be null in early bootstrap. */
  getPerfOverlay(): any | null
  /** Trace flush, called once per frame, no-ops when tracing is off. */
  checkTraceFlush(): void
  /** Refresh just the registers panel (called every 10 frames during run). */
  refreshRegisters(): void
  /** Full panel refresh on breakpoint hit / pause. */
  refreshAllPanels(): void
  /** Status label setter, used for "Breakpoint hit" / "Error - use Reset". */
  setStatus(text: string): void
  /** FPS HUD setter, updates `#fps`. */
  setFpsLabel(text: string): void
}

export interface GameLoopHandle {
  /** Idempotent, calling while running is a no-op. */
  start(): void
  /** Idempotent, calling while stopped is a no-op. */
  stop(): void
  /** Force a single display blit using current framebuffer (used post-step / post-load). */
  updateDisplayOnce(): void
  /** Read-only check. */
  isRunning(): boolean
}

/** The core's frame period from its display info, or the nominal 60 Hz when it cannot be read. */
export function readCorePeriodMs(emulator: any): number {
  try {
    const hz = emulator?.getDisplayInfo?.()?.refresh_rate
    return typeof hz === 'number' && Number.isFinite(hz) && hz > 0
      ? frameTimeMsFor(hz)
      : frameTimeMsFor(60)
  } catch {
    return frameTimeMsFor(60)
  }
}

/** Whether `count` crossed a multiple of `every` during its last `ran` increments. */
function crossedMultiple(count: number, ran: number, every: number): boolean {
  return Math.floor(count / every) > Math.floor((count - ran) / every)
}

export function createGameLoop(bridge: DebugBridge, deps: GameLoopDeps): GameLoopHandle {
  let animationFrameId: number | null = null
  /** Wall time up to which frames are accounted for (see `decideFramePacing`). */
  let anchor = 0
  let frameTimeMs = frameTimeMsFor(60)
  /** Measured mean cost of the last burst's frames, for the perf read-out. */
  let lastFrameCostMs = 0
  let frameCount = 0
  let fpsFrameMark = 0
  let fpsTimestamp = 0

  function updateDisplay(): void {
    const emulator = bridge.getEmulator()
    const wasmMemory = bridge.getWasmMemory()
    const glRenderer = deps.getRenderer()
    if (!emulator || !wasmMemory || !glRenderer) return

    try {
      const width = emulator.getFramebufferWidth()
      const height = emulator.getFramebufferHeight()

      // Zero-copy view into WASM linear memory. We have to re-create
      // the Uint8Array view each frame because `wasm.memory.buffer`
      // can be detached if the linear memory grows; using a cached
      // view would throw "detached buffer" the first time the
      // emulator reallocates internally.
      const ptr = emulator.getFramebufferPtr()
      const len = emulator.getFramebufferLen()
      const pixels = new Uint8Array(wasmMemory.buffer, ptr, len)

      glRenderer.uploadFrame(pixels, width, height)
      glRenderer.drawFrame()
    } catch (err) {
      console.error('Failed to update display:', err)
    }
  }

  /**
   * Breakpoint detection happens in WASM: when a breakpoint fires,
   * runFrame returns with the execution mode flipped to Paused. Tear the
   * loop down so the user sees the pause as immediate UI feedback
   * (status + panel refresh).
   */
  function haltOnBreakpoint(): void {
    bridge.setRunning(false)
    stop()
    const audioManager = deps.getAudioManager()
    if (audioManager && audioManager.isRunning) audioManager.stop()
    deps.setStatus('Breakpoint hit')
    updateDisplay()
    deps.refreshAllPanels()
  }

  function gameLoop(currentTime: number): void {
    if (!bridge.isRunning()) return

    const emulator = bridge.getEmulator()
    if (!emulator) {
      bridge.setRunning(false)
      return
    }

    const pacing = decideFramePacing({
      now: currentTime,
      anchor,
      frameTimeMs,
      maxFrames: maxFramesForCatchUp(DEFAULT_MAX_CATCH_UP_MS, frameTimeMs),
    })
    anchor = pacing.newAnchor
    if (pacing.framesToRun === 0) {
      animationFrameId = requestAnimationFrame(gameLoop)
      return
    }

    let ran = 0
    const t0 = performance.now()
    for (let i = 0; i < pacing.framesToRun; i++) {
      try {
        emulator.runFrame()
      } catch (err) {
        console.error('Error in runFrame:', err)
        bridge.setRunning(false)
        stop()
        deps.setStatus('Error - use Reset')
        return
      }
      ran++

      // A breakpoint stops the burst on the frame it hit, the frames
      // after it did not happen.
      let paused = false
      try {
        paused = emulator.getExecutionMode() === 'Paused'
      } catch {
        // emulator may be mid-shutdown; skip the mode check
      }
      if (paused) {
        haltOnBreakpoint()
        return
      }

      const audioManager = deps.getAudioManager()
      if (audioManager && audioManager.isRunning) {
        audioManager.sendSamples()
      }
      deps.checkTraceFlush()
    }
    const t1 = performance.now()
    lastFrameCostMs = (t1 - t0) / ran

    const t2 = performance.now()
    updateDisplay()
    const t3 = performance.now()

    frameCount += ran
    if (crossedMultiple(frameCount, ran, FPS_AVG_FRAMES)) {
      const now = performance.now()
      const elapsed = now - fpsTimestamp
      const framesSince = frameCount - fpsFrameMark
      const fps = elapsed > 0 ? Math.round((framesSince * 1000) / elapsed) : 0
      fpsTimestamp = now
      fpsFrameMark = frameCount
      deps.setFpsLabel(`FPS: ${fps}`)

      const displayMs = t3 - t2
      console.log(
        `[perf] FPS:${fps} | emulate:${lastFrameCostMs.toFixed(1)}ms/frame | display:${displayMs.toFixed(1)}ms | frames:${ran}`,
      )
    }

    const perfOverlay = deps.getPerfOverlay()
    if (perfOverlay) {
      perfOverlay.recordFrameTime(lastFrameCostMs)
      perfOverlay.update()
    }

    // Registers panel refreshes every 10 frames so the user can watch
    // values change during run, without paying the cost of a full
    // panel sweep every frame.
    if (crossedMultiple(frameCount, ran, REGISTERS_REFRESH_EVERY)) {
      deps.refreshRegisters()
    }

    animationFrameId = requestAnimationFrame(gameLoop)
  }

  function start(): void {
    if (animationFrameId !== null) return
    // A fresh anchor on every start: a resume must not owe the frames of
    // the pause. The period follows the core (a PAL cartridge is 50 Hz).
    anchor = seedAnchor(performance.now())
    frameTimeMs = readCorePeriodMs(bridge.getEmulator())
    lastFrameCostMs = 0
    fpsTimestamp = performance.now()
    frameCount = 0
    fpsFrameMark = 0
    animationFrameId = requestAnimationFrame(gameLoop)
  }

  function stop(): void {
    if (animationFrameId !== null) {
      cancelAnimationFrame(animationFrameId)
      animationFrameId = null
    }
  }

  return {
    start,
    stop,
    updateDisplayOnce: updateDisplay,
    isRunning: () => animationFrameId !== null,
  }
}
