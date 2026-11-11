import { vi } from 'vitest'

// Helpers for the tests that load `src/keybindings.js` under Node. The
// module publishes its class on `window` as it loads and listens on
// `document` when constructed; Node has neither, so both are stand-ins
// that keep the listeners a test can then call.

export type KeyListener = (event: unknown) => void

export interface Binding {
  name: string
  category: string
  /** `false` for an action the debugger does not have yet. */
  available?: boolean
  action?: () => void
  down?: () => void
  up?: () => void
}

export interface KeyBindingsInstance {
  bindings: Record<string, Binding>
  getBindingsByCategory(): Record<string, Array<{ key: string; name: string; available: boolean }>>
}

export type KeyBindingsClass = new (api: unknown) => KeyBindingsInstance

export interface LoadedKeyBindings {
  KeyBindings: KeyBindingsClass
  /** Every keydown listener added to the stand-in document, in order. */
  keydown: KeyListener[]
  /** Every keyup listener added to the stand-in document, in order. */
  keyup: KeyListener[]
}

export async function loadKeyBindings(): Promise<LoadedKeyBindings> {
  const keydown: KeyListener[] = []
  const keyup: KeyListener[] = []
  vi.stubGlobal('window', {})
  vi.stubGlobal('document', {
    addEventListener(type: string, fn: KeyListener) {
      if (type === 'keydown') keydown.push(fn)
      if (type === 'keyup') keyup.push(fn)
    },
  })
  const { KeyBindings } = await import('../src/keybindings.js')
  // The module is plain JS, whose inferred types are loose
  // (`getBindingsByCategory()` reads as `{}`); this is the shape it has.
  return { KeyBindings: KeyBindings as unknown as KeyBindingsClass, keydown, keyup }
}

/**
 * An API that records which of its methods were called, with what. A
 * query (`canFocusTab` and the like, which a binding's `available` asks)
 * is not an action: it answers yes and is not recorded.
 */
export function recordingApi() {
  const calls: Array<{ name: string; args: unknown[] }> = []
  const api = new Proxy(
    {},
    {
      get: (_target, name) => {
        if (String(name).startsWith('can')) return () => true
        return (...args: unknown[]) => {
          calls.push({ name: String(name), args })
        }
      },
    },
  )
  return { api, calls }
}

/**
 * A key event for a combination as the map spells it (`Ctrl+Shift+R`
 * carries the key `R` with Ctrl and Shift held), aimed at the page body
 * rather than an input. `key` overrides the key the event carries, for
 * the lower-case letter a browser sends for Ctrl+R. The event records
 * whether its default was prevented.
 */
export function keyEvent(combo: string, key?: string) {
  const parts = combo.split('+')
  const spelled = parts.pop() as string
  return {
    key: key ?? (spelled === 'Space' ? ' ' : spelled),
    ctrlKey: parts.includes('Ctrl'),
    shiftKey: parts.includes('Shift'),
    altKey: parts.includes('Alt'),
    target: { tagName: 'BODY', isContentEditable: false },
    defaultPrevented: false,
    preventDefault() {
      this.defaultPrevented = true
    },
  }
}

/**
 * The debugger API methods each key of the map calls when pressed,
 * found by pressing every key through a fresh instance's keydown
 * listener against a recording API. A key whose binding is not
 * available yet calls none.
 */
export function actionsByKey(loaded: LoadedKeyBindings): Record<string, string[]> {
  const { api, calls } = recordingApi()
  const before = loaded.keydown.length
  const keyBindings = new loaded.KeyBindings(api)
  const listeners = loaded.keydown.slice(before)
  const out: Record<string, string[]> = {}
  for (const key of Object.keys(keyBindings.bindings)) {
    calls.length = 0
    const event = keyEvent(key)
    for (const listener of listeners) listener(event)
    out[key] = calls.map((call) => call.name)
  }
  return out
}
