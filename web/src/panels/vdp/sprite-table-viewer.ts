/**
 * VDP sprite-table viewer, renders the linked sprite chain (starting
 * at SAT slot 0) as a compact 8-column table (`#`, `Pos`, `Size`,
 * `Tile`, `Pal`, `Pri`, `Flip`, `Link`). Mirrors the sprite table of
 * RetroLab's VDP panel and shares the deterministic column
 * decoding with it via `@retrolab/debugger-core/sprite-table-core`.
 */

import type { DebugBridge, PanelHandle } from '../../debug-bridge'
import {
  buildSpriteTable,
  type SpriteAttrInput,
  type SpriteTableRow,
} from '@debugger-core/sprite-table-core'

export function initSpriteTableViewer(bridge: DebugBridge): PanelHandle {
  const host = document.getElementById('sprite-table-viewer') as HTMLDivElement | null

  if (!host) {
    throw new Error('sprite-table viewer host (#sprite-table-viewer) missing')
  }

  function renderEmpty(message: string): void {
    host!.innerHTML = `<div class="sprite-table-empty">${message}</div>`
  }

  function renderRows(rows: SpriteTableRow[]): void {
    const header = `
      <thead>
        <tr>
          <th>#</th>
          <th>Pos</th>
          <th>Size</th>
          <th>Tile</th>
          <th>Pal</th>
          <th>Pri</th>
          <th>Flip</th>
          <th>Link</th>
        </tr>
      </thead>`

    const body = rows
      .map((row) => {
        const priClass = row.raw.priority ? ' is-high-priority' : ''
        return `<tr class="sprite-row" data-sprite-index="${row.index}">
          <td class="col-index">${row.columns.index}</td>
          <td class="col-pos">${row.columns.pos}</td>
          <td class="col-size">${row.columns.size}</td>
          <td class="col-tile">${row.columns.tile}</td>
          <td class="col-palette">${row.columns.palette}</td>
          <td class="col-priority${priClass}">${row.columns.priority}</td>
          <td class="col-flips">${row.columns.flips}</td>
          <td class="col-link">${row.columns.link}</td>
        </tr>`
      })
      .join('')

    host!.innerHTML = `<table class="sprite-table">${header}<tbody>${body}</tbody></table>`
  }

  function update(): void {
    if (!bridge.isRomLoaded()) {
      renderEmpty('Load a ROM to inspect the sprite table.')
      return
    }
    const emulator = bridge.getEmulator()
    if (!emulator) {
      renderEmpty('Emulator not initialised.')
      return
    }

    try {
      const entries = emulator.getSpriteAttrTable() as SpriteAttrInput[] | undefined
      if (!entries || entries.length === 0) {
        renderEmpty('No sprites linked from SAT slot 0.')
        return
      }
      renderRows(buildSpriteTable(entries))
    } catch (err) {
      console.error('Failed to refresh sprite-table viewer:', err)
      renderEmpty('Failed to read sprite attribute table.')
    }
  }

  return {
    update,
    dispose() {
      if (host) host.innerHTML = ''
    },
  }
}
