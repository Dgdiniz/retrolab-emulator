/**
 * The bottom info tabs of the debugger page, and what the focus keys
 * (Ctrl+1 to Ctrl+5) bring forward.
 *
 * A tab is a `.info-tab[data-tab=NAME]` button over a `#NAME-tab` pane,
 * and the shown one of each carries the class `active`. A click on a
 * button and a focus key both go through {@link activateInfoTab}, so a
 * key shows a tab exactly as its click does. `main.ts` hides the tabs a
 * core has no panel for (`style.display = 'none'`); a focus key whose tab
 * is hidden or absent finds no target, and the keybindings map then
 * leaves the key to the browser.
 */

import type { DebugBridge } from './debug-bridge'

/** The part of the document this module reads (the page's `document`). */
export type PageRoot = Pick<Document, 'querySelectorAll' | 'getElementById'>

/** The info tabs a focus key shows: the first one the page shows wins. */
const FOCUS_TABS: Record<string, readonly string[]> = {
  vdp: ['vdp'],
  audio: ['audio'],
  // The Genesis and the SNES each have a cartridge tab, and a page shows
  // one of them.
  cartridge: ['cartridge', 'snes-cartridge'],
}

/**
 * The panes a focus key brings into view, by the id of the view inside
 * each: the view is scrolled into sight and given the keyboard.
 */
const FOCUS_PANES: Record<string, string> = {
  disassembly: 'disassembly-view',
  memory: 'memory-view',
}

export type FocusTarget =
  | { kind: 'tab'; tab: string }
  | { kind: 'pane'; view: HTMLElement }

function tabButton(root: PageRoot, name: string): HTMLElement | null {
  const buttons = Array.from(root.querySelectorAll<HTMLElement>('.info-tab'))
  return buttons.find((button) => button.dataset.tab === name) ?? null
}

/** The page has a button for tab `name`, and does not hide it. */
export function isInfoTabShown(root: PageRoot, name: string): boolean {
  const button = tabButton(root, name)
  return button !== null && button.style.display !== 'none'
}

/**
 * Make `name` the active info tab: its button and its `#name-tab` pane
 * carry `active`, every other tab's do not. Returns false, changing
 * nothing, when the page has no button for it.
 */
export function showInfoTab(root: PageRoot, name: string): boolean {
  const button = tabButton(root, name)
  if (!button) return false
  root.querySelectorAll<HTMLElement>('.info-tab').forEach((t) => t.classList.remove('active'))
  button.classList.add('active')
  root.querySelectorAll<HTMLElement>('.info-tab-content').forEach((c) => c.classList.remove('active'))
  root.getElementById(`${name}-tab`)?.classList.add('active')
  return true
}

/**
 * Show tab `name` as a click on its button does: the tab, then a refresh
 * of its panel, which is registered under the tab's own name.
 */
export function activateInfoTab(
  root: PageRoot,
  bridge: Pick<DebugBridge, 'refreshPanel'>,
  name: string,
): boolean {
  if (!showInfoTab(root, name)) return false
  bridge.refreshPanel(name)
  return true
}

/**
 * What the focus key named `name` brings forward on this page: the first
 * of its tabs the page shows, or its pane's view. Null when the page has
 * neither, or the name is not a focus key's.
 */
export function findFocusTarget(root: PageRoot, name: string): FocusTarget | null {
  const tabs = FOCUS_TABS[name]
  if (tabs) {
    const tab = tabs.find((t) => isInfoTabShown(root, t))
    return tab ? { kind: 'tab', tab } : null
  }
  const viewId = FOCUS_PANES[name]
  const view = viewId ? root.getElementById(viewId) : null
  return view ? { kind: 'pane', view } : null
}

/**
 * Bring forward what the focus key named `name` targets: show its tab, or
 * scroll its pane's view into sight and give it the keyboard. Returns
 * false when the page has no target for it.
 */
export function focusPageTarget(
  root: PageRoot,
  bridge: Pick<DebugBridge, 'refreshPanel'>,
  name: string,
): boolean {
  const target = findFocusTarget(root, name)
  if (!target) return false
  if (target.kind === 'tab') return activateInfoTab(root, bridge, target.tab)
  target.view.scrollIntoView?.({ block: 'nearest' })
  target.view.focus?.()
  return true
}
