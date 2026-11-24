/**
 * Memory panel, mounts the shared hex editor + optional
 * interpretation pane inside the `#memory-view` slot. Region
 * tabs and the goto input are wired here; the actual byte
 * rendering, editing, undo/redo, auto-fit, modified colouring, and
 * windowed scrolling all live in `components/hex-editor.ts` (which
 * itself consumes the shared pure-core helpers).
 *
 * Region sizing uses the `HexEditorDataSource` window: RAM/Z80/VRAM
 * load in one shot; ROM scrolls the cartridge's whole image by
 * re-reading a 1 MiB window as the user scrolls or jumps with the
 * address bar. Goto stays within the current region when possible,
 * no forced switch to "custom" for every jump. The currently-clicked
 * byte's address is written into `#memory-cursor-addr` on every cursor
 * move. Hex-editor preferences (`memoryBytesPerLine`,
 * `editorShowOldByteOnEdit`) are read from the debugger settings store.
 *
 * On the Genesis two tabs do not read the 68000's bus, and neither is
 * edited (`@debugger-core/memory-core`). The VRAM tab slices the core's
 * copy of VRAM (`getVram()`), at VRAM addresses from 0: the 68000
 * reaches VRAM only through the VDP's ports, where every read moves the
 * VDP's address. The ROM tab slices the cartridge's ROM image
 * (`getRomBytes()`), at offsets from 0, fetched once for each ROM the
 * core holds: the bus shows 4 MiB of ROM at most, a larger cartridge's
 * through its mapper's banks, and a save RAM switched in over the ROM
 * hides the bytes under it.
 */

import { parseHexAddress } from '@debugger-core/format-core'
import {
  isImageRegion,
  isReadonlyRegion,
  isSnapshotRegion,
  regionBaseAddress,
  type MemoryRegionId,
} from '@debugger-core/memory-core'
import { sliceMemorySnapshot } from '@debugger-core/memory-regions-core'
import type { HexEditorDataSource } from '@debugger-core/hex-editor-core'
import { createHexEditor, type HexEditorHandle } from '../components/hex-editor'
import { createInterpretationPane, type InterpretationPaneHandle } from '../components/interpretation-pane'
import type { DebugBridge, PanelHandle } from '../debug-bridge'

const INTERP_TOGGLE_KEY = 'retrolab-memory-show-interpretation'
/**
 * Region sizes. RAM/Z80/VRAM are small enough that a single read
 * covers everything. The Genesis ROM tab is as long as the cartridge's
 * image (`regionTotalSize`); a tab named `rom` on another core reads
 * that core's bus over 16 MiB, as the goto window does.
 */
const REGION_TOTAL_SIZE: Record<MemoryRegionId, number> = {
  ram: 0x10000,    // 64 KiB Work RAM
  rom: 0x1000000,  // 16 MiB, the bus of a core whose tabs are not the Genesis ones
  vram: 0x10000,   // 64 KiB VRAM
  z80: 0x2000,     // 8 KiB Z80 RAM
}
const CUSTOM_TOTAL_SIZE = 0x1000000 // 16 MiB virtual window for goto-anywhere
const NO_BYTES = new Uint8Array(0)

// Settings reader. The web shell exposes a `Settings` instance on
// `window.settings`; tests / early boot may not, so fall back to
// sensible defaults. Kept inside the module so the panel doesn't
// import the settings module directly (avoids a circular dep).
interface SettingsReader {
  get(key: string): unknown
  subscribe?(fn: (path: string, value: unknown) => void): () => void
}
function getSettings(): SettingsReader | null {
  if (typeof window === 'undefined') return null
  return (window as unknown as { settings?: SettingsReader }).settings ?? null
}
function readSetting<T>(key: string, fallback: T): T {
  const settings = getSettings()
  if (!settings) return fallback
  const v = settings.get(key)
  return (v as T) ?? fallback
}

function readBytesPerRow(): number | 'auto' {
  const v = readSetting<unknown>('debugger.memoryBytesPerLine', 'auto')
  if (v === 'auto') return 'auto'
  const n = Number(v)
  if (!Number.isFinite(n) || n <= 0) return 'auto'
  return n | 0
}

function readShowOldByte(): boolean {
  return readSetting<boolean>('debugger.editorShowOldByteOnEdit', true)
}

export interface MemoryPanelOptions {
  /**
   * The `data-region` tabs are the Genesis ones (RAM, ROM, VRAM, Z80 of
   * `@debugger-core/memory-core`). Only then are the VRAM and ROM tabs
   * read from the core's copies; another core's tabs of those names read
   * its bus.
   */
  genesisRegions: boolean
}

export function initMemoryPanel(bridge: DebugBridge, options: MemoryPanelOptions): PanelHandle {
  const memoryView = document.getElementById('memory-view') as HTMLDivElement | null
  const memoryInterp = document.getElementById('memory-interp') as HTMLDivElement | null
  const memoryAddress = document.getElementById('memory-address') as HTMLInputElement | null
  const interpToggle = document.getElementById('memory-interp-toggle') as HTMLInputElement | null
  const downloadBtn = document.getElementById('memory-download-rom') as HTMLButtonElement | null
  const cursorAddrEl = document.getElementById('memory-cursor-addr') as HTMLSpanElement | null

  if (!memoryView || !memoryAddress) {
    throw new Error('memory panel DOM elements missing (#memory-view / #memory-address)')
  }

  /**
   * Per-region UI state. Each region tab owns its own base address,
   * cursor offset, and scroll position so switching RAM -> ROM -> RAM
   * lands the user back on the exact line they were last looking at,
   * instead of resetting everything to the region base. `custom` is
   * special: `currentAddress` is the address the user typed into the
   * goto box (becomes the editor's baseAddress so the address gutter
   * starts there).
   */
  interface RegionState {
    currentAddress: number
    cursorOffset: number
    scrollTop: number
  }
  const regionState: Record<string, RegionState> = {
    ram: { currentAddress: regionBaseAddress('ram'), cursorOffset: 0, scrollTop: 0 },
    rom: { currentAddress: regionBaseAddress('rom'), cursorOffset: 0, scrollTop: 0 },
    vram: { currentAddress: regionBaseAddress('vram'), cursorOffset: 0, scrollTop: 0 },
    z80: { currentAddress: regionBaseAddress('z80'), cursorOffset: 0, scrollTop: 0 },
    custom: { currentAddress: 0, cursorOffset: 0, scrollTop: 0 },
  }
  let currentRegion: MemoryRegionId | 'custom' = 'ram'

  function activeState(): RegionState {
    return regionState[currentRegion] ?? regionState.custom
  }

  // Per-region modified-offset sets. Tracking by region keeps a write
  // to RAM from colouring a Z80 byte at the same offset and vice-versa.
  const modifiedByRegion: Record<string, Set<number>> = {
    ram: new Set<number>(),
    rom: new Set<number>(),
    vram: new Set<number>(),
    z80: new Set<number>(),
    custom: new Set<number>(),
  }
  let activeModified: Set<number> = modifiedByRegion.ram

  /**
   * The cartridge's ROM image, which the ROM tab slices and the download
   * saves. `getRomBytes()` copies the whole image at each call, so the
   * copy is kept until the core is replaced or holds another file
   * (`getCartridgeHash()`). Empty with no emulator or no cartridge, and
   * when the core cannot hand one.
   */
  let romImage: { emulator: unknown; hash: unknown; bytes: Uint8Array } | null = null

  function readRomImage(): Uint8Array {
    const emu = bridge.getEmulator()
    if (!emu) return NO_BYTES
    try {
      const hash = emu.getCartridgeHash()
      if (!romImage || romImage.emulator !== emu || romImage.hash !== hash) {
        romImage = { emulator: emu, hash, bytes: emu.getRomBytes() as Uint8Array }
      }
      return romImage.bytes
    } catch (err) {
      console.error('Failed to read the ROM image:', err)
      return NO_BYTES
    }
  }

  function regionTotalSize(): number {
    if (currentRegion === 'custom') return CUSTOM_TOTAL_SIZE
    // The image is the whole ROM; with no cartridge it is empty.
    if (readsImage()) return readRomImage().length
    return REGION_TOTAL_SIZE[currentRegion]
  }

  /** The active tab is the Genesis VRAM tab, read from the core's snapshot. */
  function readsSnapshot(): boolean {
    return options.genesisRegions && isSnapshotRegion(currentRegion)
  }

  /** The active tab is the Genesis ROM tab, read from the cartridge's image. */
  function readsImage(): boolean {
    return options.genesisRegions && isImageRegion(currentRegion)
  }

  /** The active tab is a Genesis tab read from a copy, which no edit reaches. */
  function isReadonly(): boolean {
    return options.genesisRegions && isReadonlyRegion(currentRegion)
  }

  function readWindow(offset: number, length: number): Uint8Array {
    const emu = bridge.getEmulator()
    if (!emu) return new Uint8Array(0)
    try {
      // The snapshot and the image are addressed from 0, so a window
      // offset is a VRAM address or an offset in the image.
      if (readsSnapshot()) return sliceMemorySnapshot(emu.getVram() as Uint8Array, offset, length)
      if (readsImage()) return sliceMemorySnapshot(readRomImage(), offset, length)
      const start = activeState().currentAddress + (offset | 0)
      return emu.readMemory(start, length | 0) as Uint8Array
    } catch (err) {
      console.error('Failed to read memory window:', err)
      return new Uint8Array(0)
    }
  }

  function buildDataSource(): HexEditorDataSource {
    return {
      get totalSize() {
        return regionTotalSize()
      },
      read(offset, length) {
        return readWindow(offset, length)
      },
    }
  }

  let interp: InterpretationPaneHandle | null = null
  let hex: HexEditorHandle
  /** The image the editor was mounted over, when it shows the ROM tab. */
  let mountedImage: Uint8Array | null = null

  function updateCursorAddr(): void {
    if (!cursorAddrEl) return
    const st = activeState()
    const abs = (st.currentAddress + st.cursorOffset) >>> 0
    cursorAddrEl.textContent = `@ 0x${abs.toString(16).toUpperCase().padStart(8, '0')}`
  }

  function mountEditor(): HexEditorHandle {
    const st = activeState()
    mountedImage = readsImage() ? readRomImage() : null
    return createHexEditor(memoryView!, {
      dataSource: buildDataSource(),
      baseAddress: st.currentAddress,
      bytesPerRow: readBytesPerRow(),
      showOldByteOnEdit: readShowOldByte(),
      enableUndo: true,
      // The VRAM snapshot and the ROM image are copies: an edit there
      // would never reach the machine.
      readonly: isReadonly(),
      modifiedOffsets: activeModified,
      onByteChange(offset, value) {
        if (isReadonly()) return
        const emu = bridge.getEmulator()
        if (!emu) return
        const cur = activeState()
        const absoluteAddr = (cur.currentAddress + offset) >>> 0
        try {
          emu.writeMemoryDebug(absoluteAddr, new Uint8Array([value]))
          activeModified.add(offset)
          // Re-read the window so the next paint shows the new value
          // next to the orange "modified" marker.
          hex.refresh()
          interp?.update(readForInterp(), cur.cursorOffset, cur.currentAddress)
        } catch (err) {
          console.error('Memory write failed:', err)
        }
      },
      onCursorChange(offset) {
        const cur = activeState()
        cur.cursorOffset = offset
        updateCursorAddr()
        interp?.update(readForInterp(), offset, cur.currentAddress)
      },
    })
  }

  hex = mountEditor()

  /**
   * Snapshot for the interpretation pane. The pane only needs 4-8
   * bytes around the cursor, so reading a fresh tiny window is cheap
   * and dodges the question of "does the editor's window cover this
   * offset?" entirely.
   */
  function readForInterp(): Uint8Array {
    return readWindow(Math.max(0, activeState().cursorOffset), 8)
  }

  function ensureInterp(show: boolean): void {
    if (!memoryInterp) return
    if (show && !interp) {
      memoryInterp.hidden = false
      interp = createInterpretationPane(memoryInterp)
      const st = activeState()
      interp.update(readForInterp(), st.cursorOffset, st.currentAddress)
    } else if (!show && interp) {
      interp.dispose()
      interp = null
      memoryInterp.hidden = true
    }
  }

  // Restore toggle state from localStorage; default off so first-time
  // users see the wider hex view.
  if (interpToggle) {
    const persisted = (() => {
      try { return window.localStorage?.getItem(INTERP_TOGGLE_KEY) === '1' } catch { return false }
    })()
    interpToggle.checked = persisted
    ensureInterp(persisted)
    interpToggle.addEventListener('change', () => {
      const show = interpToggle.checked
      try { window.localStorage?.setItem(INTERP_TOGGLE_KEY, show ? '1' : '0') } catch { /* ignore quota */ }
      ensureInterp(show)
    })
  }

  function ensureInterpUpdate(): void {
    const st = activeState()
    interp?.update(readForInterp(), st.cursorOffset, st.currentAddress)
  }

  // -- Region tabs / goto -------------------------------------------

  // Genesis ships fixed `data-region` tabs (ram/rom/vram/z80) backed by
  // the shared memory-core region machinery. Cores whose regions are
  // built dynamically (from `getMemoryRegions()`) instead carry a
  // `data-addr`: a bare absolute CPU-bus address that lands in the
  // `custom` region. Either attribute is honoured here.
  const allMemoryTabs = document.querySelectorAll<HTMLElement>('.tab-btn[data-region], .tab-btn[data-addr]')
  allMemoryTabs.forEach((tab) => {
    tab.addEventListener('click', () => {
      allMemoryTabs.forEach((t) => t.classList.remove('active'))
      tab.classList.add('active')
      const addrAttr = tab.dataset.addr
      if (addrAttr !== undefined) {
        const addr = parseHexAddress(addrAttr)
        if (addr === null) return
        regionState.custom.currentAddress = addr
        regionState.custom.cursorOffset = 0
        regionState.custom.scrollTop = 0
        switchRegion('custom')
        return
      }
      switchRegion(tab.dataset.region as MemoryRegionId)
    })
  })

  function switchRegion(region: MemoryRegionId | 'custom'): void {
    // Save scrollTop of the region we're leaving (cursorOffset +
    // currentAddress are already in `regionState` via the
    // cursor-change handler and the goto path).
    regionState[currentRegion].scrollTop = hex.getScrollTop()

    currentRegion = region
    activeModified = modifiedByRegion[region] ?? (modifiedByRegion[region] = new Set<number>())

    // Re-mount the editor with the new baseAddress / dataSource /
    // modifiedOffsets. Re-mounting (instead of mutating in place) is
    // simpler than poking the editor's internal window state and
    // keeps the public API minimal.
    hex.dispose()
    hex = mountEditor()

    // Restore the previously captured scroll + cursor for this
    // region. setCursor centres the row in view; the explicit
    // setScrollTop afterwards wins, so we land on the user's exact
    // previous scroll position rather than a re-centred one.
    const st = activeState()
    if (st.cursorOffset > 0) hex.setCursor(st.cursorOffset)
    hex.setScrollTop(st.scrollTop)

    updateCursorAddr()
    ensureInterpUpdate()
  }

  function inCurrentRegion(addr: number): boolean {
    if (currentRegion === 'custom') return false
    const base = regionBaseAddress(currentRegion as MemoryRegionId)
    return addr >= base && addr < base + regionTotalSize()
  }

  memoryAddress.addEventListener('keypress', (e) => {
    if (e.key !== 'Enter') return
    const addr = parseHexAddress((e.target as HTMLInputElement).value)
    if (addr === null) return
    if (inCurrentRegion(addr)) {
      // Scroll within the existing region, preserves modifiedOffsets,
      // undo stack, and avoids the re-mount flash.
      const base = regionBaseAddress(currentRegion as MemoryRegionId)
      hex.setCursor(addr - base)
      return
    }
    // Out-of-region jump: park in `custom` so the address gutter
    // starts at the typed address. Seed `custom`'s state BEFORE
    // calling `switchRegion` so the freshly mounted editor reads
    // the new base address on its first paint.
    regionState.custom.currentAddress = addr
    regionState.custom.cursorOffset = 0
    regionState.custom.scrollTop = 0
    switchRegion('custom')
  })

  // -- Download ROM -------------------------------------------------

  downloadBtn?.addEventListener('click', () => {
    const emu = bridge.getEmulator()
    if (!emu) return
    try {
      // The cartridge's ROM image, as the ROM tab shows it; when the core
      // hands none, the file the page loaded, if any.
      const image = readRomImage()
      const meta = bridge.getRomMeta()
      const bytes = image.length > 0 ? image : meta.data
      if (!bytes || bytes.length === 0) return
      // `Blob` rejects `Uint8Array<SharedArrayBuffer>` even though the
      // payload is identical. Slicing into a fresh `Uint8Array` over a
      // plain `ArrayBuffer` sidesteps the wasm-bindgen SAB typing.
      const payload = new Uint8Array(bytes.slice().buffer as ArrayBuffer)
      const blob = new Blob([payload], { type: 'application/octet-stream' })
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = (meta.name || 'rom') + '.bin'
      document.body.appendChild(a)
      a.click()
      document.body.removeChild(a)
      URL.revokeObjectURL(url)
    } catch (err) {
      console.error('Failed to download ROM:', err)
    }
  })

  // Initial render so the editor shows something before the first
  // emulator tick, refresh() repeats this on every panel update.
  updateCursorAddr()

  // React to settings changes: the user toggles "Auto-fit" or "Show
  // old byte while editing" in the modal and we propagate the change
  // to the live editor without forcing a reload.
  const settings = getSettings()
  const unsubscribeSettings = settings?.subscribe?.((path) => {
    if (path === 'debugger.memoryBytesPerLine') {
      hex.setBytesPerRow(readBytesPerRow())
    } else if (path === 'debugger.editorShowOldByteOnEdit') {
      hex.setShowOldByteOnEdit(readShowOldByte())
    }
  })

  return {
    update() {
      // The editor takes the tab's size when it is mounted: over a ROM
      // loaded since, mount it again where it was.
      if (readsImage() && readRomImage() !== mountedImage) {
        switchRegion(currentRegion)
        return
      }
      hex.refresh()
      ensureInterpUpdate()
    },
    dispose() {
      unsubscribeSettings?.()
      hex.dispose()
      interp?.dispose()
    },
  }
}
