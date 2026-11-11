/**
 * VDP palette viewer, renders the 4 CRAM palettes x 16 colours as a
 * grid of swatches with hex tooltips. Mirrors the palette viewer of
 * RetroLab's VDP panel.
 */

import type { DebugBridge, PanelHandle } from '../../debug-bridge'

export function initPaletteViewer(bridge: DebugBridge): PanelHandle {
  const root = document.getElementById('palette-viewer')
  if (!root) {
    throw new Error('palette-viewer element not found in DOM')
  }

  function update(): void {
    if (!bridge.isRomLoaded()) return
    const emulator = bridge.getEmulator()
    if (!emulator) return

    try {
      const palettes: Uint8Array = emulator.getAllPalettesRgba()

      let html = ''
      for (let p = 0; p < 4; p++) {
        html += '<div class="palette-group">'
        html += `<div class="palette-title">Palette ${p}</div>`
        html += '<div class="palette-colors">'

        for (let c = 0; c < 16; c++) {
          const index = (p * 16 + c) * 4
          const r = palettes[index]
          const g = palettes[index + 1]
          const b = palettes[index + 2]
          const hexColor = `#${r.toString(16).padStart(2, '0')}${g.toString(16).padStart(2, '0')}${b.toString(16).padStart(2, '0')}`

          html += `
            <div class="palette-color" style="background: ${hexColor};">
              <span class="palette-color-info">P${p}:${c} ${hexColor.toUpperCase()}</span>
            </div>
          `
        }

        html += '</div></div>'
      }

      root!.innerHTML = html
    } catch (err) {
      console.error('Failed to update palette viewer:', err)
    }
  }

  return {
    update,
    dispose() {
      // no listeners
    },
  }
}
