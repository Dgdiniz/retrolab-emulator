import { describe, expect, it, vi } from 'vitest'

// The input, cartridge and audio panels wrap a JS class that is handed an
// emulator once, when it is built. The stand-ins below record the emulator
// each instance holds; the real classes need the page's DOM.
const made = vi.hoisted(() => ({
  input: [] as Array<{ emulator: unknown }>,
  cartridge: [] as Array<{ emulator: unknown; infoFrom: unknown[] }>,
  audio: [] as Array<{ emulator: unknown; destroyed: number }>,
}))

vi.mock('../src/input-debugger.js', () => ({
  InputDebugger: class {
    emulator: unknown
    constructor(emulator: unknown) {
      this.emulator = emulator
      made.input.push(this)
    }
    init() {}
    destroy() {}
  },
}))

vi.mock('../src/cartridge-debugger.js', () => ({
  CartridgeDebugger: class {
    emulator: unknown
    infoFrom: unknown[] = []
    constructor(emulator: unknown) {
      this.emulator = emulator
      made.cartridge.push(this)
    }
    async init() {}
    updateCartridgeInfo() {
      this.infoFrom.push(this.emulator)
    }
  },
}))

vi.mock('../src/audio-debugger-multi-chip.js', () => ({
  AudioDebuggerMultiChip: class {
    emulator: unknown
    destroyed = 0
    constructor(emulator: unknown) {
      this.emulator = emulator
      made.audio.push(this)
    }
    async init() {}
    destroy() {
      this.destroyed++
    }
  },
}))

import { initInputPanel } from '../src/panels/input-panel'
import { initCartridgePanel } from '../src/panels/cartridge-panel'
import { initAudioPanel } from '../src/panels/audio-panel'
import type { DebugBridge } from '../src/debug-bridge'

/** A bridge whose emulator a test replaces, as a force reset does. */
function bridgeWithReplaceableEmulator() {
  let emulator: object = { name: 'first' }
  const bridge = { getEmulator: () => emulator } as unknown as DebugBridge
  return {
    bridge,
    replace(next: object) {
      emulator = next
    },
  }
}

describe('wrapper panels follow the emulator the bridge holds', () => {
  it('the input panel moves its debugger onto the new emulator on its refresh', () => {
    const b = bridgeWithReplaceableEmulator()
    const panel = initInputPanel(b.bridge)
    const inputDebugger = made.input.at(-1)!
    const second = { name: 'second' }
    b.replace(second)
    panel.update()
    expect(inputDebugger.emulator).toBe(second)
  })

  it('the cartridge panel moves its debugger onto the new emulator on its refresh', () => {
    const b = bridgeWithReplaceableEmulator()
    const panel = initCartridgePanel(b.bridge)
    const cartridgeDebugger = made.cartridge.at(-1)!
    const second = { name: 'second' }
    b.replace(second)
    panel.update()
    expect(cartridgeDebugger.emulator).toBe(second)
  })

  it('the cartridge info refresh reads the new emulator even before a panel refresh', () => {
    const b = bridgeWithReplaceableEmulator()
    const panel = initCartridgePanel(b.bridge)
    const cartridgeDebugger = made.cartridge.at(-1)!
    const second = { name: 'second' }
    b.replace(second)
    panel.refreshCartridgeInfo()
    expect(cartridgeDebugger.infoFrom).toEqual([second])
  })

  it('the audio panel moves its debugger onto the new emulator, and its teardown destroys it', () => {
    const b = bridgeWithReplaceableEmulator()
    const panel = initAudioPanel(b.bridge)
    const audioDebugger = made.audio.at(-1)!
    const second = { name: 'second' }
    b.replace(second)
    panel.update()
    expect(audioDebugger.emulator).toBe(second)
    panel.dispose()
    expect(audioDebugger.destroyed).toBe(1)
  })

  it('makes one audio debugger per panel', () => {
    const before = made.audio.length
    initAudioPanel(bridgeWithReplaceableEmulator().bridge)
    expect(made.audio.length - before).toBe(1)
  })
})
