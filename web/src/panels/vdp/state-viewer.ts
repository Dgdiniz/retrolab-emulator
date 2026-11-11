/**
 * VDP state viewer, per-layer visibility toggles (Plane A/B, Window,
 * Sprites) plus the VDP control-register snapshot. Mirrors
 * RetroLab's VDP state panel and shares the deterministic
 * parsing with it via `@debugger-core/video-layers-core`
 * (`parseLayerRows` / `parseVideoRegisters`). Toggling a layer routes to
 * the live compositor through `emulator.toggleLayer(id, visible)`, a
 * debugger-only mask that suppresses the layer without touching the
 * game's real VDP registers.
 */

import type { DebugBridge, PanelHandle } from '../../debug-bridge'
import {
  parseLayerRows,
  parseVideoRegisters,
  type LayerRow,
  type VideoRegisterRow,
} from '@debugger-core/video-layers-core'

export function initVdpStateViewer(bridge: DebugBridge): PanelHandle {
  const host = document.getElementById('vdp-state-viewer') as HTMLDivElement | null

  if (!host) {
    throw new Error('vdp state viewer host (#vdp-state-viewer) missing')
  }

  function renderEmpty(message: string): void {
    host!.innerHTML = `<div class="vdp-state-empty">${message}</div>`
  }

  function escapeHtml(s: string): string {
    return s.replace(/[&<>"]/g, (c) =>
      c === '&' ? '&amp;' : c === '<' ? '&lt;' : c === '>' ? '&gt;' : '&quot;',
    )
  }

  function render(layers: LayerRow[], registers: VideoRegisterRow[]): void {
    const layerHtml = layers.length
      ? layers
          .map(
            (l) => `<label class="vdp-layer">
              <input type="checkbox" class="vdp-layer-toggle" data-layer-id="${l.id}"${l.enabled ? ' checked' : ''}>
              <span class="vdp-layer-name">${escapeHtml(l.name)}</span>
              <span class="vdp-layer-meta">${escapeHtml(l.size)}</span>
            </label>`,
          )
          .join('')
      : '<div class="vdp-state-empty">No layer data</div>'

    const regRows = registers
      .map(
        (r) => `<tr>
          <td class="col-index">${r.index}</td>
          <td class="col-name">${escapeHtml(r.name)}</td>
          <td class="col-value">${r.hex}</td>
        </tr>`,
      )
      .join('')

    host!.innerHTML = `
      <div class="vdp-state-section-title">Layers</div>
      <div class="vdp-layers">${layerHtml}</div>
      <div class="vdp-state-section-title">Registers</div>
      <table class="vdp-reg-table">
        <thead><tr><th>#</th><th>Register</th><th>Value</th></tr></thead>
        <tbody>${regRows}</tbody>
      </table>`

    host!.querySelectorAll<HTMLInputElement>('.vdp-layer-toggle').forEach((cb) => {
      cb.addEventListener('change', () => {
        const id = Number(cb.dataset.layerId)
        const emulator = bridge.getEmulator()
        try {
          emulator?.toggleLayer?.(id, cb.checked)
          // A running loop re-composites on its own next frame. While
          // paused nothing repaints, so re-render the held frame and blit
          // it once so the toggle is visible immediately.
          if (!bridge.isRunning()) {
            emulator?.repaintFrame?.()
            bridge.requestDisplayRefresh()
          }
        } catch (err) {
          console.error('Failed to toggle VDP layer:', err)
        }
      })
    })
  }

  function update(): void {
    if (!bridge.isRomLoaded()) {
      renderEmpty('Load a ROM to inspect VDP state.')
      return
    }
    const emulator = bridge.getEmulator()
    if (!emulator) {
      renderEmpty('Emulator not initialised.')
      return
    }

    try {
      const layers = parseLayerRows(emulator.getLayerInfo?.())
      const registers = parseVideoRegisters(emulator.getVdpRegisters?.())
      render(layers, registers)
    } catch (err) {
      console.error('Failed to refresh VDP state viewer:', err)
      renderEmpty('Failed to read VDP state.')
    }
  }

  return {
    update,
    dispose() {
      if (host) host.innerHTML = ''
    },
  }
}
