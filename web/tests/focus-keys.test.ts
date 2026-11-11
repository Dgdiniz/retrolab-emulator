import { readFileSync } from 'node:fs'
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest'
import { createDebuggerApi, type DebuggerApiDeps } from '../src/debugger-api'
import type { DebugBridge } from '../src/debug-bridge'
import { keyEvent, loadKeyBindings, type LoadedKeyBindings } from './key-bindings-harness'

// Ctrl+1 to Ctrl+5 against the real page: the tab buttons, their panes
// and the two views are read out of index.html into stand-in elements
// (Node has no DOM), and the keys run through the real map, the real
// debugger API and the real tab switcher.

const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8')

interface PageNode {
  id?: string
  dataset: { tab?: string }
  classes: Set<string>
  classList: { add(token: string): void; remove(token: string): void; contains(token: string): boolean }
  style: { display: string }
  /** The element's tabindex, null when it cannot take the keyboard. */
  tabIndex: number | null
  scrolledIntoView: number
  scrollIntoView(): void
  focus(): void
}

function node(page: { activeElement: PageNode | null }, init: { id?: string; tab?: string; classes?: string[]; tabIndex?: number | null }): PageNode {
  const classes = new Set(init.classes ?? [])
  const self: PageNode = {
    id: init.id,
    dataset: init.tab === undefined ? {} : { tab: init.tab },
    classes,
    classList: {
      add: (token) => { classes.add(token) },
      remove: (token) => { classes.delete(token) },
      contains: (token) => classes.has(token),
    },
    style: { display: '' },
    tabIndex: init.tabIndex ?? null,
    scrolledIntoView: 0,
    scrollIntoView() {
      self.scrolledIntoView++
    },
    focus() {
      // A browser moves the keyboard only to an element that can take it.
      if (self.tabIndex !== null) page.activeElement = self
    },
  }
  return self
}

/**
 * The page as index.html builds it. `hidden` lists the tabs main.ts hides
 * for the core (the Genesis hides `snes-cartridge`; the SNES hides `vdp`,
 * `audio` and `cartridge`), and `without` drops a tab button altogether.
 */
function page(opts: { hidden: string[]; without?: string[] }) {
  const doc = {
    activeElement: null as PageNode | null,
    fullscreenElement: null,
    keydown: [] as Array<(e: unknown) => void>,
    buttons: [] as PageNode[],
    panes: [] as PageNode[],
    views: [] as PageNode[],
    addEventListener(type: string, fn: (e: unknown) => void) {
      if (type === 'keydown') doc.keydown.push(fn)
    },
    querySelectorAll(selector: string) {
      if (selector === '.info-tab') return doc.buttons
      if (selector === '.info-tab-content') return doc.panes
      throw new Error(`the page stand-in knows no selector ${selector}`)
    },
    getElementById(id: string) {
      return [...doc.panes, ...doc.views].find((n) => n.id === id) ?? null
    },
  }
  for (const [, classAttr, tab] of html.matchAll(/<button class="(info-tab[^"]*)" data-tab="([^"]+)"/g)) {
    if (opts.without?.includes(tab)) continue
    const button = node(doc, { tab, classes: classAttr.split(/\s+/) })
    if (opts.hidden.includes(tab)) button.style.display = 'none'
    doc.buttons.push(button)
  }
  for (const [, id, classAttr] of html.matchAll(/<div id="([^"]+)" class="(info-tab-content[^"]*)"/g)) {
    doc.panes.push(node(doc, { id, classes: classAttr.split(/\s+/) }))
  }
  for (const id of ['disassembly-view', 'memory-view']) {
    const tag = html.match(new RegExp(`<div id="${id}"[^>]*>`))?.[0]
    if (!tag) continue
    const attr = tag.match(/\btabindex="(-?\d+)"/)?.[1]
    // The hex editor makes #memory-view focusable when it mounts.
    const tabIndex = attr !== undefined ? Number(attr) : id === 'memory-view' ? 0 : null
    doc.views.push(node(doc, { id, tabIndex }))
  }
  return doc
}

const GENESIS = { hidden: ['snes-cartridge'] }
const SNES = { hidden: ['vdp', 'audio', 'cartridge'] }

let loaded: LoadedKeyBindings

beforeAll(async () => {
  loaded = await loadKeyBindings()
})

afterAll(() => {
  vi.unstubAllGlobals()
})

/** The map and the real API over `doc`, and a press of one key. */
function debuggerOn(doc: ReturnType<typeof page>) {
  vi.stubGlobal('document', doc)
  const refreshPanel = vi.fn()
  const bridge = {
    refreshPanel,
    isRomLoaded: () => true,
    isRunning: () => false,
  } as unknown as DebugBridge
  const api = createDebuggerApi(bridge, {} as DebuggerApiDeps)
  const keyBindings = new loaded.KeyBindings(api)
  function press(combo: string, key: string) {
    const event = keyEvent(combo, key)
    for (const listener of doc.keydown) listener(event)
    return event
  }
  return { keyBindings, press, refreshPanel }
}

function activeTabs(doc: ReturnType<typeof page>) {
  return {
    buttons: doc.buttons.filter((b) => b.classes.has('active')).map((b) => b.dataset.tab),
    panes: doc.panes.filter((p) => p.classes.has('active')).map((p) => p.id),
  }
}

describe('Ctrl+3 to Ctrl+5 show their bottom tab, as a click on it does', () => {
  const tabKeys: Array<[string, string, string, typeof GENESIS]> = [
    ['Ctrl+3', '3', 'vdp', GENESIS],
    ['Ctrl+4', '4', 'audio', GENESIS],
    ['Ctrl+5', '5', 'cartridge', GENESIS],
    ['Ctrl+5', '5', 'snes-cartridge', SNES],
  ]
  for (const [combo, key, tab, core] of tabKeys) {
    it(`${combo} activates the ${tab} tab (${core === SNES ? 'SNES' : 'Genesis'} page)`, () => {
      const doc = page(core)
      expect(activeTabs(doc)).toEqual({ buttons: ['breakpoints'], panes: ['breakpoints-tab'] })
      const d = debuggerOn(doc)
      const event = d.press(combo, key)
      expect(activeTabs(doc)).toEqual({ buttons: [tab], panes: [`${tab}-tab`] })
      expect(d.refreshPanel).toHaveBeenCalledWith(tab)
      expect(event.defaultPrevented).toBe(true)
    })
  }
})

describe('Ctrl+1 and Ctrl+2 bring their pane into view and give it the keyboard', () => {
  const paneKeys: Array<[string, string, string]> = [
    ['Ctrl+1', '1', 'disassembly-view'],
    ['Ctrl+2', '2', 'memory-view'],
  ]
  for (const [combo, key, viewId] of paneKeys) {
    it(`${combo} focuses #${viewId}`, () => {
      const doc = page(GENESIS)
      const d = debuggerOn(doc)
      const event = d.press(combo, key)
      const view = doc.views.find((v) => v.id === viewId)!
      expect(view.scrolledIntoView).toBe(1)
      expect(doc.activeElement).toBe(view)
      expect(activeTabs(doc).buttons).toEqual(['breakpoints'])
      expect(event.defaultPrevented).toBe(true)
    })
  }
})

describe('a focus key whose target the page does not show is not available', () => {
  it('leaves Ctrl+3 and Ctrl+4 to the browser on the SNES page, which hides the VDP and Audio tabs', () => {
    const doc = page(SNES)
    const d = debuggerOn(doc)
    for (const [combo, key] of [['Ctrl+3', '3'], ['Ctrl+4', '4']]) {
      const event = d.press(combo, key)
      expect(event.defaultPrevented, combo).toBe(false)
      expect(activeTabs(doc).buttons, combo).toEqual(['breakpoints'])
    }
    expect(d.refreshPanel).not.toHaveBeenCalled()
    const listed = Object.fromEntries(
      Object.values(d.keyBindings.getBindingsByCategory()).flat().map((b) => [b.key, b.available]),
    )
    expect(listed['Ctrl+3']).toBe(false)
    expect(listed['Ctrl+4']).toBe(false)
    expect(listed['Ctrl+5']).toBe(true)
  })

  it('leaves a key to the browser when the page has no button for its tab', () => {
    const doc = page({ hidden: ['snes-cartridge'], without: ['vdp'] })
    const d = debuggerOn(doc)
    const event = d.press('Ctrl+3', '3')
    expect(event.defaultPrevented).toBe(false)
    expect(activeTabs(doc).buttons).toEqual(['breakpoints'])
  })

  it('shows the unavailable focus keys as not available yet in the help', async () => {
    const { HelpModal } = await import('../src/help-modal.js')
    const doc = page(SNES)
    const d = debuggerOn(doc)
    const modal = Object.create(HelpModal.prototype)
    modal.keyBindings = d.keyBindings
    const htmlOut: string = modal.getModalHTML()
    const rowOf = (combo: string) =>
      htmlOut.split('<tr').find((r) => r.includes(`<td class="key-combo">${modal.renderKeyCombination(combo)}</td>`))!
    expect(rowOf('Ctrl+3')).toContain('not available yet')
    expect(rowOf('Ctrl+4')).toContain('not available yet')
    expect(rowOf('Ctrl+5')).not.toContain('not available yet')
    expect(rowOf('Ctrl+1')).not.toContain('not available yet')
  })
})
