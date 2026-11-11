import { readFileSync } from 'node:fs'
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest'
import { actionsByKey, loadKeyBindings } from './key-bindings-harness'

// A toolbar button's tooltip may name a key only when the keybindings map
// binds that key to the button's own action, the action the help modal
// (F1) lists for it.
//
// The action of each button, as the map's actions reach it: F9's
// toggleRunPause clicks Run or Pause, F10's stepOver clicks Step, the
// reset action clicks Reset. The Save and Load buttons act on the slot the
// selector shows, which no key does (the map's F5 to F8 and Shift+F5 to
// Shift+F8 act on fixed slots), so their tooltips name no key.
const BUTTON_ACTIONS: Record<string, string | null> = {
  'run-btn': 'toggleRunPause',
  'pause-btn': 'toggleRunPause',
  'step-btn': 'stepOver',
  'reset-btn': 'reset',
  'save-state-btn': null,
  'load-state-btn': null,
  'fullscreen-btn': 'toggleFullscreen',
}

const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8')

/** The `title` of the button with this id. */
function titleOf(id: string): string {
  const tag = html.match(new RegExp(`<button\\b[^>]*\\bid="${id}"[^>]*>`))?.[0]
  if (!tag) throw new Error(`no button #${id} in index.html`)
  const title = tag.match(/\btitle="([^"]*)"/)?.[1]
  if (title === undefined) throw new Error(`button #${id} has no title`)
  return title
}

/** The key a tooltip names in parentheses at its end, or null. */
function namedKey(title: string): string | null {
  return title.match(/\(([^()]+)\)\s*$/)?.[1] ?? null
}

let keysByAction: Record<string, string[]>

beforeAll(async () => {
  const loaded = await loadKeyBindings()
  keysByAction = {}
  for (const [key, methods] of Object.entries(actionsByKey(loaded))) {
    for (const method of methods) (keysByAction[method] ??= []).push(key)
  }
})

afterAll(() => {
  vi.unstubAllGlobals()
})

describe('toolbar tooltips name the key the map binds to the button', () => {
  for (const [id, action] of Object.entries(BUTTON_ACTIONS)) {
    it(`#${id}`, () => {
      const title = titleOf(id)
      const named = namedKey(title)
      const bound = action ? keysByAction[action] ?? [] : []
      if (bound.length === 0) {
        expect(named, `"${title}" names a key, and no key does what #${id} does`).toBeNull()
      } else {
        expect(bound, `"${title}" names a key that does something else`).toContain(named)
      }
    })
  }

  it('keeps fullscreen on F11, as the help modal lists it', () => {
    expect(namedKey(titleOf('fullscreen-btn'))).toBe('F11')
    expect(keysByAction.toggleFullscreen).toEqual(['F11'])
  })
})
