/**
 * VDP VRAM viewer, hex dump of VRAM bytes around a goto address.
 * Mirrors the VRAM viewer of RetroLab's VDP panel.
 */

import { hex, hexBytes, parseHexAddress } from '@debugger-core/format-core'
import type { DebugBridge, PanelHandle } from '../../debug-bridge'

const BYTES_PER_ROW = 16
const ROWS = 16
const VIEW_BYTES = BYTES_PER_ROW * ROWS

export function initVramViewer(bridge: DebugBridge): PanelHandle {
  const root = document.getElementById('vram-viewer')
  const addrInput = document.getElementById('vram-address-input') as HTMLInputElement | null
  const gotoBtn = document.getElementById('vram-goto-btn')

  if (!root || !addrInput || !gotoBtn) {
    throw new Error('vram viewer DOM elements missing')
  }

  let startAddr = 0x0000

  gotoBtn.addEventListener('click', () => {
    const addr = parseHexAddress(addrInput.value)
    if (addr === null) return
    startAddr = addr
    update()
  })

  addrInput.addEventListener('keypress', (e) => {
    if (e.key === 'Enter') {
      gotoBtn.click()
    }
  })

  function update(): void {
    if (!bridge.isRomLoaded()) return
    const emulator = bridge.getEmulator()
    if (!emulator) return

    try {
      const vram: Uint8Array = emulator.getVram()

      let html = ''
      for (let i = 0; i < VIEW_BYTES; i += BYTES_PER_ROW) {
        const addr = startAddr + i
        if (addr >= vram.length) break

        const sliceEnd = Math.min(addr + BYTES_PER_ROW, vram.length)
        const bytes = vram.slice(addr, sliceEnd)
        const ascii = Array.from(bytes)
          .map((b) => (b >= 32 && b < 127 ? String.fromCharCode(b) : '.'))
          .join('')

        html += `
          <div class="vram-row">
            <span class="vram-address">${hex(addr, 4)}:</span>
            <span class="vram-hex">${hexBytes(bytes)}</span>
            <span class="vram-ascii">${ascii}</span>
          </div>
        `
      }

      root!.innerHTML = html
    } catch (err) {
      console.error('Failed to update VRAM viewer:', err)
      root!.innerHTML = '<div style="color: #f48771;">VRAM unavailable</div>'
    }
  }

  return {
    update,
    dispose() {
      // no listeners
    },
  }
}
