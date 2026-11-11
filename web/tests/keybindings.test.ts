import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import { createDebuggerApi, type DebuggerApiDeps } from '../src/debugger-api'
import type { DebugBridge } from '../src/debug-bridge'
import {
  keyEvent,
  loadKeyBindings,
  recordingApi,
  type LoadedKeyBindings,
} from './key-bindings-harness'

let loaded: LoadedKeyBindings

beforeAll(async () => {
  loaded = await loadKeyBindings()
})

afterAll(() => {
  vi.unstubAllGlobals()
})

/**
 * Press `combo` once: the event reaches every listener the map added. A
 * `key` overrides the key the event carries. Returns the event.
 */
function press(listeners: Array<(e: unknown) => void>, combo: string, key?: string) {
  const event = keyEvent(combo, key)
  for (const listener of listeners) listener(event)
  return event
}

/** A fresh map over a recording API, with the listeners it added. */
function freshMap() {
  const { api, calls } = recordingApi()
  const downBefore = loaded.keydown.length
  const upBefore = loaded.keyup.length
  const keyBindings = new loaded.KeyBindings(api)
  return {
    calls,
    keyBindings,
    down: loaded.keydown.slice(downBefore),
    up: loaded.keyup.slice(upBefore),
  }
}

describe('KeyBindings: one key press, one action', () => {
  it('runs exactly one debugger action for each available key it binds', () => {
    const { calls, keyBindings, down } = freshMap()
    expect(down).toHaveLength(1)

    const available = Object.entries(keyBindings.bindings).filter(([, b]) => b.available !== false)
    expect(available.length).toBeGreaterThan(0)
    for (const [combo] of available) {
      calls.length = 0
      const event = press(down, combo)
      expect(calls.map((c) => c.name), combo).toHaveLength(1)
      expect(event.defaultPrevented, combo).toBe(true)
    }
  })

  // The keys the page's own handler also took, each of which did a second
  // thing (F8 loaded the selected slot and also saved slot 4).
  const formerlyDoubled: Array<[string, string, unknown[]]> = [
    ['F5', 'quickSave', [1]],
    ['F8', 'saveToSlot', [4]],
    ['Ctrl+Shift+R', 'hotReloadRom', []],
    ['Escape', 'closeModalOrStop', []],
  ]
  for (const [combo, method, args] of formerlyDoubled) {
    it(`${combo} runs ${method} and nothing else`, () => {
      const { api, calls } = recordingApi()
      const before = loaded.keydown.length
      new loaded.KeyBindings(api)
      press(loaded.keydown.slice(before), combo)
      expect(calls).toEqual([{ name: method, args }])
    })
  }

  it('binds the force reset to a key of its own, listed in the help under Execution', () => {
    const { api, calls } = recordingApi()
    const before = loaded.keydown.length
    const keyBindings = new loaded.KeyBindings(api)
    press(loaded.keydown.slice(before), 'Ctrl+Shift+Backspace')
    expect(calls.map((c) => c.name)).toEqual(['forceReset'])
    const execution = keyBindings.getBindingsByCategory().Execution.map((b) => b.key)
    expect(execution).toContain('Ctrl+Shift+Backspace')
  })
})

describe('KeyBindings: a letter whatever its case', () => {
  // A browser sends Ctrl+R as the key 'r' (Shift and Caps Lock set a
  // letter's case, Ctrl does not); with Caps Lock on it sends 'R'.
  const letters: Array<[string, string, string]> = [
    ['Ctrl+R', 'r', 'reset'],
    ['Ctrl+O', 'o', 'openRomDialog'],
    ['Ctrl+G', 'g', 'showGoToDialog'],
  ]
  for (const [combo, lower, method] of letters) {
    it(`${combo} runs ${method} with the letter in lower case, as a browser sends it`, () => {
      const { calls, down } = freshMap()
      const event = press(down, combo, lower)
      expect(calls.map((c) => c.name)).toEqual([method])
      expect(event.defaultPrevented).toBe(true)
    })

    it(`${combo} runs ${method} with Caps Lock on`, () => {
      const { calls, down } = freshMap()
      press(down, combo, lower.toUpperCase())
      expect(calls.map((c) => c.name)).toEqual([method])
    })
  }

  it('keeps Ctrl+Shift+R on the hot reload, with Caps Lock off or on', () => {
    const off = freshMap()
    press(off.down, 'Ctrl+Shift+R', 'R')
    expect(off.calls.map((c) => c.name)).toEqual(['hotReloadRom'])
    // Caps Lock with Shift sends the lower-case letter.
    const on = freshMap()
    press(on.down, 'Ctrl+Shift+R', 'r')
    expect(on.calls.map((c) => c.name)).toEqual(['hotReloadRom'])
  })
})

describe('KeyBindings: bindings not available yet', () => {
  // The bindings whose API method only logs. They stay in the map and in
  // the help, marked, and leave their key to the browser.
  const notYet = [
    'Shift+F11', 'Ctrl+B', 'Ctrl+F', 'Ctrl+H', 'F3', 'Shift+F3', 'Ctrl+Tab',
    'Ctrl+Shift+Tab', 'Tab', 'Ctrl+=', 'Ctrl+-', 'Ctrl+0', 'Ctrl+S',
  ]

  it('marks exactly the bindings whose action only logs', () => {
    const { keyBindings } = freshMap()
    const marked = Object.entries(keyBindings.bindings)
      .filter(([, b]) => b.available === false)
      .map(([key]) => key)
    expect(marked.sort()).toEqual([...notYet].sort())
  })

  it('runs nothing and leaves the default of every marked key, down and up', () => {
    const { calls, keyBindings, down, up } = freshMap()
    for (const [combo, binding] of Object.entries(keyBindings.bindings)) {
      if (binding.available !== false) continue
      calls.length = 0
      const pressed = press(down, combo)
      const released = press(up, combo)
      expect(calls, combo).toEqual([])
      expect(pressed.defaultPrevented, combo).toBe(false)
      expect(released.defaultPrevented, combo).toBe(false)
    }
  })

  it('lists them in the help modal, marked as not available yet', async () => {
    const { HelpModal } = await import('../src/help-modal.js')
    const { keyBindings } = freshMap()
    // getModalHTML reads only the map, so the modal is built without its
    // DOM (the constructor would append it to the page).
    const modal = Object.create(HelpModal.prototype)
    modal.keyBindings = keyBindings
    const html: string = modal.getModalHTML()

    for (const [combo, binding] of Object.entries(keyBindings.bindings)) {
      const keyCell = `<td class="key-combo">${modal.renderKeyCombination(combo)}</td>`
      const row = html.split('<tr').find((r) => r.includes(keyCell))
      expect(row, `${combo} is listed`).toBeDefined()
      expect(row!.includes('not available yet'), combo).toBe(binding.available === false)
    }
  })
})

describe('the map is the only handler of the shortcut keys', () => {
  const srcDir = fileURLToPath(new URL('../src', import.meta.url))

  function sourceFiles(dir: string): string[] {
    return readdirSync(dir).flatMap((name) => {
      const path = join(dir, name)
      if (statSync(path).isDirectory()) return sourceFiles(path)
      return /\.(ts|js)$/.test(name) ? [path] : []
    })
  }

  it('leaves the document and window keyboard listeners to keybindings.js and the game input', () => {
    const found: string[] = []
    const listener = /\b(document|window)\.addEventListener\(\s*['"](keydown|keyup)['"]/g
    for (const file of sourceFiles(srcDir)) {
      const text = readFileSync(file, 'utf8')
      for (const match of text.matchAll(listener)) {
        found.push(`${relative(srcDir, file)} ${match[1]} ${match[2]}`)
      }
    }
    // input-handler.js feeds every key to the emulator's controller ports;
    // it runs no debugger action.
    expect(found.sort()).toEqual([
      'input-handler.js window keydown',
      'input-handler.js window keyup',
      'keybindings.js document keydown',
      'keybindings.js document keyup',
    ])
  })
})

describe('Escape (closeModalOrStop), one layer at a time', () => {
  interface State {
    running: boolean
    helpOpen: boolean
    settingsOpen: boolean
    fullscreen: boolean
  }

  function apiFor(state: State) {
    const helpModal = {
      isVisible: () => state.helpOpen,
      close: vi.fn(() => { state.helpOpen = false }),
    }
    const settingsModal = {
      isOpen: () => state.settingsOpen,
      close: vi.fn(() => { state.settingsOpen = false }),
    }
    const exitFullscreen = vi.fn(() => { state.fullscreen = false })
    vi.stubGlobal('document', {
      get fullscreenElement() {
        return state.fullscreen ? {} : null
      },
      exitFullscreen,
    })
    const emergencyStop = vi.fn()
    const deps = {
      emergencyStop,
      getHelpModal: () => helpModal,
      getSettingsModal: () => settingsModal,
    } as unknown as DebuggerApiDeps
    const bridge = {
      isRunning: () => state.running,
      isRomLoaded: () => true,
    } as unknown as DebugBridge
    const api = createDebuggerApi(bridge, deps)
    return { api, helpModal, settingsModal, exitFullscreen, emergencyStop }
  }

  const idle: State = { running: false, helpOpen: false, settingsOpen: false, fullscreen: false }

  beforeEach(() => {
    vi.unstubAllGlobals()
  })

  it('closes an open help modal and leaves the running emulator running', () => {
    const h = apiFor({ ...idle, running: true, helpOpen: true })
    h.api.closeModalOrStop()
    expect(h.helpModal.close).toHaveBeenCalledTimes(1)
    expect(h.emergencyStop).not.toHaveBeenCalled()
  })

  it('closes an open settings modal and leaves the running emulator running', () => {
    const h = apiFor({ ...idle, running: true, settingsOpen: true })
    h.api.closeModalOrStop()
    expect(h.settingsModal.close).toHaveBeenCalledTimes(1)
    expect(h.emergencyStop).not.toHaveBeenCalled()
  })

  it('leaves fullscreen first, without stopping the emulator', () => {
    const h = apiFor({ ...idle, running: true, fullscreen: true })
    h.api.closeModalOrStop()
    expect(h.exitFullscreen).toHaveBeenCalledTimes(1)
    expect(h.emergencyStop).not.toHaveBeenCalled()
  })

  it('stops a running emulator when nothing is open', () => {
    const h = apiFor({ ...idle, running: true })
    h.api.closeModalOrStop()
    expect(h.emergencyStop).toHaveBeenCalledTimes(1)
  })

  it('does nothing when nothing is open and the emulator is paused', () => {
    const h = apiFor(idle)
    h.api.closeModalOrStop()
    expect(h.emergencyStop).not.toHaveBeenCalled()
    expect(h.helpModal.close).not.toHaveBeenCalled()
    expect(h.settingsModal.close).not.toHaveBeenCalled()
  })
})
