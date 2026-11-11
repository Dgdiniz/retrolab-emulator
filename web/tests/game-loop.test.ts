import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createGameLoop, readCorePeriodMs, type GameLoopDeps } from '../src/game-loop'
import type { DebugBridge } from '../src/debug-bridge'

const FT60 = 1000 / 60

function harness(opts: { refreshRate?: number } = {}) {
  let running = true
  const emulator = {
    runFrame: vi.fn(),
    getExecutionMode: vi.fn(() => 'Running'),
    getFramebufferWidth: () => 320,
    getFramebufferHeight: () => 224,
    getFramebufferPtr: () => 0,
    getFramebufferLen: () => 320 * 224 * 4,
    getDisplayInfo: opts.refreshRate === undefined ? undefined : () => ({ refresh_rate: opts.refreshRate }),
  }
  const bridge = {
    isRunning: () => running,
    setRunning: (value: boolean) => { running = value },
    getEmulator: () => emulator,
    getWasmMemory: () => ({ buffer: new ArrayBuffer(320 * 224 * 4) }),
  } as unknown as DebugBridge
  const renderer = { uploadFrame: vi.fn(), drawFrame: vi.fn() }
  const audio = { isRunning: true, sendSamples: vi.fn(), stop: vi.fn() }
  const deps: GameLoopDeps = {
    getRenderer: () => renderer,
    getAudioManager: () => audio,
    getPerfOverlay: () => null,
    checkTraceFlush: vi.fn(),
    refreshRegisters: vi.fn(),
    refreshAllPanels: vi.fn(),
    setStatus: vi.fn(),
    setFpsLabel: vi.fn(),
  }
  return { emulator, bridge, renderer, audio, deps, isRunning: () => running }
}

describe('createGameLoop', () => {
  const rafCbs: FrameRequestCallback[] = []
  let nowMs = 0

  beforeEach(() => {
    rafCbs.length = 0
    nowMs = 1000
    vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => {
      rafCbs.push(cb)
      return rafCbs.length
    })
    vi.stubGlobal('cancelAnimationFrame', vi.fn())
    vi.spyOn(performance, 'now').mockImplementation(() => nowMs)
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'error').mockImplementation(() => {})
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  function tick(time: number) {
    const cb = rafCbs.shift()
    if (cb) cb(time)
  }

  it('start/stop are idempotent and report isRunning', () => {
    const h = harness()
    const loop = createGameLoop(h.bridge, h.deps)
    expect(loop.isRunning()).toBe(false)
    loop.start()
    loop.start()
    expect(rafCbs).toHaveLength(1)
    expect(loop.isRunning()).toBe(true)
    loop.stop()
    loop.stop()
    expect(loop.isRunning()).toBe(false)
    expect(cancelAnimationFrame).toHaveBeenCalledTimes(1)
  })

  it('a 33 ms callback (30 Hz display) runs two frames, drains audio + trace per frame, and blits once', () => {
    const h = harness()
    const loop = createGameLoop(h.bridge, h.deps)
    loop.start()
    tick(1000 + 2 * FT60)
    expect(h.emulator.runFrame).toHaveBeenCalledTimes(2)
    expect(h.audio.sendSamples).toHaveBeenCalledTimes(2)
    expect(h.deps.checkTraceFlush).toHaveBeenCalledTimes(2)
    expect(h.renderer.drawFrame).toHaveBeenCalledTimes(1)
    expect(rafCbs).toHaveLength(1)
  })

  it('an early callback runs nothing and keeps the chain alive', () => {
    const h = harness()
    const loop = createGameLoop(h.bridge, h.deps)
    loop.start()
    tick(1008)
    expect(h.emulator.runFrame).not.toHaveBeenCalled()
    expect(h.renderer.drawFrame).not.toHaveBeenCalled()
    expect(rafCbs).toHaveLength(1)
    tick(1000 + FT60)
    expect(h.emulator.runFrame).toHaveBeenCalledTimes(1)
  })

  it('a breakpoint mid-burst halts on that frame: loop stopped, status set, panels refreshed', () => {
    const h = harness()
    h.emulator.getExecutionMode
      .mockReturnValueOnce('Running')
      .mockReturnValueOnce('Paused')
    const loop = createGameLoop(h.bridge, h.deps)
    loop.start()
    tick(1000 + 4 * FT60)
    expect(h.emulator.runFrame).toHaveBeenCalledTimes(2)
    expect(h.isRunning()).toBe(false)
    expect(loop.isRunning()).toBe(false)
    expect(h.audio.stop).toHaveBeenCalledTimes(1)
    expect(h.deps.setStatus).toHaveBeenCalledWith('Breakpoint hit')
    expect(h.deps.refreshAllPanels).toHaveBeenCalledTimes(1)
    expect(h.renderer.drawFrame).toHaveBeenCalledTimes(1)
    expect(rafCbs).toHaveLength(0)
  })

  it('a runFrame error stops the loop with the reset hint', () => {
    const h = harness()
    h.emulator.runFrame.mockImplementationOnce(() => { throw new Error('trap') })
    const loop = createGameLoop(h.bridge, h.deps)
    loop.start()
    tick(1000 + FT60)
    expect(h.isRunning()).toBe(false)
    expect(h.deps.setStatus).toHaveBeenCalledWith('Error - use Reset')
    expect(rafCbs).toHaveLength(0)
  })

  it('paces against the period the core reports', () => {
    const h = harness({ refreshRate: 50.007 })
    const loop = createGameLoop(h.bridge, h.deps)
    loop.start()
    // 36 ms is two 60 Hz frames but only one PAL (50 Hz) frame.
    tick(1036)
    expect(h.emulator.runFrame).toHaveBeenCalledTimes(1)
  })

  it('a stopped bridge ends the chain without touching the core', () => {
    const h = harness()
    const loop = createGameLoop(h.bridge, h.deps)
    loop.start()
    h.bridge.setRunning(false)
    tick(1000 + 2 * FT60)
    expect(h.emulator.runFrame).not.toHaveBeenCalled()
    expect(rafCbs).toHaveLength(0)
  })

  it('refreshes the registers and the FPS label as the frame count crosses their strides, bursts included', () => {
    const h = harness()
    const loop = createGameLoop(h.bridge, h.deps)
    loop.start()
    // Ten frames at 30 Hz: five callbacks of two frames each cross the
    // 10-frame registers stride exactly once.
    for (let i = 1; i <= 5; i++) tick(1000 + i * 2 * FT60)
    expect(h.emulator.runFrame).toHaveBeenCalledTimes(10)
    expect(h.deps.refreshRegisters).toHaveBeenCalledTimes(1)
    for (let i = 6; i <= 30; i++) tick(1000 + i * 2 * FT60)
    expect(h.deps.setFpsLabel).toHaveBeenCalledTimes(1)
  })
})

describe('readCorePeriodMs', () => {
  it('derives the period from the core display info and falls back to 60 Hz', () => {
    expect(readCorePeriodMs({ getDisplayInfo: () => ({ refresh_rate: 50.007 }) })).toBeCloseTo(1000 / 50.007, 6)
    expect(readCorePeriodMs({ getDisplayInfo: () => ({ refresh_rate: 0 }) })).toBeCloseTo(FT60, 6)
    expect(readCorePeriodMs({})).toBeCloseTo(FT60, 6)
    expect(readCorePeriodMs(null)).toBeCloseTo(FT60, 6)
    expect(readCorePeriodMs({ getDisplayInfo: () => { throw new Error('gone') } })).toBeCloseTo(FT60, 6)
  })
})
