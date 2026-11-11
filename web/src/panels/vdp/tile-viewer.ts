/**
 * VDP tile viewer, renders the VRAM tile cache (2048 tiles, 16 columns
 * x 128 rows, 4x zoom) with click-to-select highlighting. Mirrors the
 * tile viewer of RetroLab's VDP panel.
 */

import type { DebugBridge, PanelHandle } from '../../debug-bridge'

const TILE_COUNT = 2048
const TILES_PER_ROW = 16
const TILE_PX = 8
const SCALE = 4

export function initTileViewer(bridge: DebugBridge): PanelHandle {
  const canvas = document.getElementById('tile-viewer-canvas') as HTMLCanvasElement | null
  const tilePaletteSelect = document.getElementById('tile-palette-select') as HTMLSelectElement | null
  const tileIndexInput = document.getElementById('tile-index-input') as HTMLInputElement | null

  if (!canvas || !tilePaletteSelect || !tileIndexInput) {
    throw new Error('tile viewer DOM elements missing')
  }

  let currentPalette = 0
  let currentTileIndex = 0

  tilePaletteSelect.addEventListener('change', (e) => {
    currentPalette = parseInt((e.target as HTMLSelectElement).value, 10)
    update()
  })

  tileIndexInput.addEventListener('change', (e) => {
    currentTileIndex = parseInt((e.target as HTMLInputElement).value, 10)
    update()
  })

  function update(): void {
    if (!bridge.isRomLoaded()) return
    const emulator = bridge.getEmulator()
    if (!emulator) return

    try {
      const ctx = canvas!.getContext('2d')
      if (!ctx) return

      ctx.fillStyle = '#1e1e1e'
      ctx.fillRect(0, 0, canvas!.width, canvas!.height)

      const scaledTileSize = TILE_PX * SCALE

      // Each tile is drawn at 8x8 into a scratch canvas then drawn back
      // into the grid at the scaled size. Done per-tile to avoid the
      // extra allocation cost of a single 16x128*32 ImageData buffer.
      for (let i = 0; i < TILE_COUNT; i++) {
        const tileRgba: Uint8Array = emulator.getTileRgba(i, currentPalette)
        const imageData = ctx.createImageData(TILE_PX, TILE_PX)
        imageData.data.set(tileRgba)

        const col = i % TILES_PER_ROW
        const row = Math.floor(i / TILES_PER_ROW)
        const x = col * scaledTileSize
        const y = row * scaledTileSize

        const tempCanvas = document.createElement('canvas')
        tempCanvas.width = TILE_PX
        tempCanvas.height = TILE_PX
        const tempCtx = tempCanvas.getContext('2d')
        if (!tempCtx) continue
        tempCtx.putImageData(imageData, 0, 0)

        ctx.imageSmoothingEnabled = false
        ctx.drawImage(tempCanvas, x, y, scaledTileSize, scaledTileSize)

        if (i === currentTileIndex) {
          ctx.strokeStyle = '#0e639c'
          ctx.lineWidth = 2
          ctx.strokeRect(x, y, scaledTileSize, scaledTileSize)
        }
      }

      canvas!.onclick = (e: MouseEvent) => {
        const rect = canvas!.getBoundingClientRect()
        const x = e.clientX - rect.left
        const y = e.clientY - rect.top
        const col = Math.floor(x / scaledTileSize)
        const row = Math.floor(y / scaledTileSize)
        const idx = row * TILES_PER_ROW + col

        if (idx >= 0 && idx < TILE_COUNT) {
          currentTileIndex = idx
          tileIndexInput!.value = String(idx)
          update()
        }
      }
    } catch (err) {
      console.error('Failed to update tile viewer:', err)
    }
  }

  return {
    update,
    dispose() {
      if (canvas) canvas.onclick = null
    },
  }
}
