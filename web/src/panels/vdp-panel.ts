/**
 * VDP panel, umbrella around three sub-viewers (tiles / palettes /
 * VRAM). Owns the sub-tab routing and forwards the bridge update tick
 * to whichever viewer the user is currently looking at (refreshing all
 * three would burn cycles repainting hidden canvases). Mirrors
 * RetroLab's VDP panel.
 */

import type { DebugBridge, PanelHandle } from '../debug-bridge'
import { initVdpStateViewer } from './vdp/state-viewer'
import { initTileViewer } from './vdp/tile-viewer'
import { initPaletteViewer } from './vdp/palette-viewer'
import { initVramViewer } from './vdp/vram-viewer'
import { initSpriteTableViewer } from './vdp/sprite-table-viewer'

type SubTab = 'state' | 'tiles' | 'palettes' | 'vram' | 'sprites'

export function initVdpPanel(bridge: DebugBridge): PanelHandle {
  const subtabs = document.querySelectorAll<HTMLElement>('.vdp-subtab')
  const subtabContents = document.querySelectorAll<HTMLElement>('.vdp-subtab-content')

  if (subtabs.length === 0) {
    throw new Error('vdp sub-tabs missing in DOM')
  }

  const stateViewer = initVdpStateViewer(bridge)
  const tileViewer = initTileViewer(bridge)
  const paletteViewer = initPaletteViewer(bridge)
  const vramViewer = initVramViewer(bridge)
  const spriteTableViewer = initSpriteTableViewer(bridge)

  let activeTab: SubTab = 'state'

  subtabs.forEach((tab) => {
    tab.addEventListener('click', () => {
      const tabName = tab.dataset.vdpTab as SubTab | undefined
      if (!tabName) return

      subtabs.forEach((t) => t.classList.remove('active'))
      tab.classList.add('active')

      subtabContents.forEach((content) => content.classList.remove('active'))
      const targetContent = document.getElementById(`vdp-${tabName}-tab`)
      if (targetContent) targetContent.classList.add('active')

      activeTab = tabName
      refreshActive()
    })
  })

  function refreshActive(): void {
    if (activeTab === 'state') stateViewer.update()
    else if (activeTab === 'tiles') tileViewer.update()
    else if (activeTab === 'palettes') paletteViewer.update()
    else if (activeTab === 'vram') vramViewer.update()
    else if (activeTab === 'sprites') spriteTableViewer.update()
  }

  function update(): void {
    // Repaint just the visible sub-tab. The other three redraw lazily on
    // tab switch, the tile viewer in particular allocates 2048 scratch
    // canvases per pass, so painting it while hidden would be wasteful.
    refreshActive()
  }

  return {
    update,
    dispose() {
      stateViewer.dispose()
      tileViewer.dispose()
      paletteViewer.dispose()
      vramViewer.dispose()
      spriteTableViewer.dispose()
    },
  }
}
