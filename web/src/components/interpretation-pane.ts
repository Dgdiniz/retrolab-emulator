/**
 * Vanilla DOM companion to the hex editor: decodes the bytes at the
 * current cursor as the primitive types a debugger user actually
 * cares about. Mirrors RetroLab's interpretation pane and
 * consumes `@debugger-core/byte-interpretation-core` for the
 * decoding math so both shells render the exact same numbers.
 */

import {
  decodeBytes,
  formatDecimalHex,
  formatSigned,
} from '@debugger-core/byte-interpretation-core'

export interface InterpretationPaneHandle {
  /**
   * Refresh the pane with a new buffer and / or cursor. The buffer
   * snapshot is taken at call time, callers re-invoke this on every
   * panel tick (or on the editor's `onCursorChange`).
   */
  update(buffer: Uint8Array, offset: number, baseAddress?: number): void
  dispose(): void
}

const ROW_DEFS: ReadonlyArray<{
  key: keyof ReturnType<typeof decodeBytes>
  label: string
  format: 'hex' | 'u8' | 'u16' | 'u32' | 'i' | 'char' | 'bin'
}> = [
  { key: 'hex',     label: 'hex',    format: 'hex'  },
  { key: 'u8',      label: 'u8',     format: 'u8'   },
  { key: 'i8',      label: 'i8',     format: 'i'    },
  { key: 'charAscii', label: 'char', format: 'char' },
  { key: 'binary',  label: 'bin',    format: 'bin'  },
  { key: 'u16le',   label: 'u16 LE', format: 'u16'  },
  { key: 'u16be',   label: 'u16 BE', format: 'u16'  },
  { key: 'i16le',   label: 'i16 LE', format: 'i'    },
  { key: 'i16be',   label: 'i16 BE', format: 'i'    },
  { key: 'u32le',   label: 'u32 LE', format: 'u32'  },
  { key: 'u32be',   label: 'u32 BE', format: 'u32'  },
  { key: 'i32le',   label: 'i32 LE', format: 'i'    },
  { key: 'i32be',   label: 'i32 BE', format: 'i'    },
]

function formatField(value: unknown, format: typeof ROW_DEFS[number]['format']): string {
  switch (format) {
    case 'hex':  return String(value).toUpperCase()
    case 'char': return String(value)
    case 'bin':  return String(value)
    case 'u8':   return formatDecimalHex(value as number, 2)
    case 'u16':  return formatDecimalHex(value as number, 4)
    case 'u32':  return formatDecimalHex(value as number, 8)
    case 'i':    return formatSigned(value as number)
  }
}

export function createInterpretationPane(root: HTMLElement): InterpretationPaneHandle {
  root.classList.add('interpretation-pane', 'mono')
  root.setAttribute('aria-label', 'Byte interpretation')

  const header = document.createElement('header')
  header.className = 'interp-header'
  const title = document.createElement('span')
  title.className = 'interp-title'
  title.textContent = 'Interpretation'
  const offsetEl = document.createElement('span')
  offsetEl.className = 'interp-offset'
  header.appendChild(title)
  header.appendChild(offsetEl)

  const grid = document.createElement('dl')
  grid.className = 'interp-grid'

  // One dt/dd pair per row, kept in the same order as `ROW_DEFS`.
  const rowNodes: HTMLElement[] = []
  for (const def of ROW_DEFS) {
    const dt = document.createElement('dt')
    dt.textContent = def.label
    const dd = document.createElement('dd')
    grid.appendChild(dt)
    grid.appendChild(dd)
    rowNodes.push(dd)
  }

  root.appendChild(header)
  root.appendChild(grid)

  function update(buffer: Uint8Array, offset: number, baseAddress = 0): void {
    const decoded = decodeBytes(buffer, offset)
    const abs = (baseAddress + offset).toString(16).toUpperCase().padStart(6, '0')
    offsetEl.textContent = `@ 0x${abs}`
    for (let i = 0; i < ROW_DEFS.length; i++) {
      const def = ROW_DEFS[i]
      rowNodes[i].textContent = formatField(decoded[def.key], def.format)
    }
  }

  return {
    update,
    dispose() {
      root.innerHTML = ''
      root.classList.remove('interpretation-pane', 'mono')
      root.removeAttribute('aria-label')
    },
  }
}
