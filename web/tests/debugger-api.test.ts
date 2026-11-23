import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// The save states and the save RAM live in IndexedDB, which Node lacks.
vi.mock('../src/save-state-manager.js', () => ({
  saveStateManager: { init: vi.fn(async () => {}), saveSaveState: vi.fn(async () => {}) },
}))
vi.mock('../src/sram-manager.js', () => ({ sramManager: {} }))

import { createDebuggerApi } from '../src/debugger-api'
import { saveStateManager } from '../src/save-state-manager.js'
import { recordedIn } from './reader-shapes'

describe("the debugger's quick save", () => {
  beforeEach(() => {
    vi.useFakeTimers()
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it("reads the core's frame number, which the bindings give as a bigint", () => {
    expect(recordedIn('retro-wasm/src/machine.rs')).toMatch(
      /#\[wasm_bindgen\(js_name = getFrameNumber\)\]\s*pub fn get_frame_number\(&self\) -> u64/,
    )
  })

  it('stores the frame number with the state, as a number', async () => {
    const state = new Uint8Array([1, 2, 3])
    const emulator = { saveState: () => state, getFrameNumber: () => 1234n }
    const bridge = {
      getRomMeta: () => ({ hash: 'abc', title: 'GAME' }),
      getEmulator: () => emulator,
      isRomLoaded: () => true,
    }
    const api = createDebuggerApi(bridge as any, { setSaveStateInfo: vi.fn() } as any)
    await api.quickSave(2)
    expect(saveStateManager.saveSaveState).toHaveBeenCalledWith('abc', 'GAME', 2, state, {
      frameCount: 1234,
    })
  })
})
