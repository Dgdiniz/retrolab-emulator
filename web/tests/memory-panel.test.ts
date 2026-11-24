import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// The hex editor and the interpretation pane draw into the page's DOM; the
// stand-ins record what the panel hands them instead.
const mounted = vi.hoisted(() => ({
  editors: [] as Array<{ opts: any; refresh: () => void; setCursor: (offset: number) => void }>,
  panes: [] as Array<{ update: (...args: unknown[]) => void }>,
}))

vi.mock('../src/components/hex-editor', () => ({
  createHexEditor: (_root: unknown, opts: unknown) => {
    const editor = {
      opts,
      refresh: vi.fn(),
      setCursor: vi.fn(),
      getScrollTop: () => 0,
      setScrollTop: vi.fn(),
      setBytesPerRow: vi.fn(),
      setShowOldByteOnEdit: vi.fn(),
      dispose: vi.fn(),
    }
    mounted.editors.push(editor)
    return editor
  },
}))

vi.mock('../src/components/interpretation-pane', () => ({
  createInterpretationPane: () => {
    const pane = { update: vi.fn(), dispose: vi.fn() }
    mounted.panes.push(pane)
    return pane
  },
}))

import { initMemoryPanel } from '../src/panels/memory-panel'
import type { DebugBridge } from '../src/debug-bridge'

interface FakeElement {
  handlers: Record<string, Array<(e?: unknown) => void>>
  hidden: boolean
  checked: boolean
  textContent: string
  dataset: Record<string, string>
  classList: { add(): void; remove(): void }
  addEventListener(type: string, fn: (e?: unknown) => void): void
}

function element(dataset: Record<string, string> = {}): FakeElement {
  return {
    handlers: {},
    hidden: true,
    checked: false,
    textContent: '',
    dataset,
    classList: { add() {}, remove() {} },
    addEventListener(type, fn) {
      ;(this.handlers[type] ??= []).push(fn)
    },
  }
}

const VRAM = Uint8Array.from({ length: 0x10000 }, (_, i) => (i * 7) & 0xff)

/**
 * `length` bytes counting by `step`: no two in a row are equal, so a window
 * of them is never the bus stand-in's run of 0x11.
 */
function pattern(length: number, step: number): Uint8Array {
  const bytes = new Uint8Array(length)
  for (let i = 0; i < length; i++) bytes[i] = (i * step + 5) & 0xff
  return bytes
}

/**
 * A 5 MiB ROM image: larger than the cartridge's 4 MiB window on the
 * 68000's bus, which shows its last MiB only through a mapper's banks.
 */
const IMAGE = pattern(0x500000, 13)

/** What the core says of the cartridge it holds. */
interface Cartridge {
  image: Uint8Array
  hash: string | undefined
}

/** An emulator holding `cartridge`, its VRAM `VRAM`, its bus all 0x11. */
function makeEmulator(cartridge: Cartridge) {
  return {
    readMemory: vi.fn((_address: number, length: number) => new Uint8Array(length).fill(0x11)),
    getVram: vi.fn(() => VRAM.slice()),
    // A copy of the whole image at each call, as the binding hands it.
    getRomBytes: vi.fn(() => cartridge.image.slice()),
    getCartridgeHash: vi.fn(() => cartridge.hash),
    writeMemoryDebug: vi.fn(),
  }
}

/**
 * The memory panel over a stand-in page holding the Genesis tabs, with the
 * interpretation pane switched on, and an emulator whose VRAM is `VRAM` and
 * whose cartridge holds `IMAGE` (or what `loaded` says).
 */
function harness(genesisRegions: boolean, loaded: Partial<Cartridge> = {}) {
  const tabs = ['ram', 'rom', 'vram', 'z80'].map((region) => element({ region }))
  const elements: Record<string, FakeElement> = {
    'memory-view': element(),
    'memory-interp': element(),
    'memory-address': element(),
    'memory-interp-toggle': element(),
    'memory-cursor-addr': element(),
    'memory-download-rom': element(),
  }
  const anchor = { href: '', download: '', click: vi.fn() }
  vi.stubGlobal('document', {
    getElementById: (id: string) => elements[id] ?? null,
    querySelectorAll: () => tabs,
    createElement: () => anchor,
    body: { appendChild() {}, removeChild() {} },
  })
  vi.stubGlobal('window', { localStorage: { getItem: () => '1', setItem() {} } })

  const cartridge: Cartridge = { image: IMAGE, hash: 'image-hash', ...loaded }
  const emulator = makeEmulator(cartridge)
  let current = emulator
  const meta = { hash: null, title: '', data: null as Uint8Array | null, name: 'game' }
  const bridge = { getEmulator: () => current, getRomMeta: () => meta } as unknown as DebugBridge
  const panel = initMemoryPanel(bridge, { genesisRegions })

  function selectTab(region: string): void {
    tabs.find((t) => t.dataset.region === region)!.handlers.click[0]()
  }
  function goto(text: string): void {
    elements['memory-address'].handlers.keypress[0]({ key: 'Enter', target: { value: text } })
  }
  function download(): void {
    elements['memory-download-rom'].handlers.click[0]()
  }
  /** A new emulator over the same cartridge, as a forced reset makes one. */
  function replaceEmulator() {
    current = makeEmulator(cartridge)
    return current
  }
  const editor = () => mounted.editors.at(-1)!
  const pane = () => mounted.panes.at(-1)!

  return { emulator, cartridge, meta, anchor, panel, selectTab, goto, download, replaceEmulator, editor, pane }
}

/** The bytes the download hands the browser, one Blob per click. */
function captureDownloads(): Blob[] {
  const blobs: Blob[] = []
  vi.spyOn(URL, 'createObjectURL').mockImplementation((blob) => {
    blobs.push(blob as Blob)
    return 'blob:rom'
  })
  vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {})
  return blobs
}

/** Whether a Blob holds exactly the bytes of `expected`. */
async function holds(blob: Blob, expected: Uint8Array): Promise<boolean> {
  const got = new Uint8Array(await blob.arrayBuffer())
  return got.length === expected.length && got.every((b, i) => b === expected[i])
}

describe('memory panel: the Genesis VRAM tab', () => {
  beforeEach(() => {
    mounted.editors.length = 0
    mounted.panes.length = 0
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  it('reads its window from getVram, at VRAM addresses from 0, and never calls readMemory', () => {
    const h = harness(true)
    h.selectTab('vram')
    h.emulator.readMemory.mockClear()
    expect(h.editor().opts.baseAddress).toBe(0)
    const bytes = h.editor().opts.dataSource.read(0x100, 16)
    expect(Array.from(bytes)).toEqual(Array.from(VRAM.slice(0x100, 0x110)))
    expect(h.editor().opts.dataSource.totalSize).toBe(0x10000)
    expect(h.emulator.getVram).toHaveBeenCalled()
    expect(h.emulator.readMemory).not.toHaveBeenCalled()
  })

  it('is read-only: the editor is told so and no edit reaches writeMemoryDebug', () => {
    const h = harness(true)
    h.selectTab('vram')
    expect(h.editor().opts.readonly).toBe(true)
    h.editor().opts.onByteChange(0x10, 0xaa, VRAM[0x10])
    expect(h.emulator.writeMemoryDebug).not.toHaveBeenCalled()
  })

  it('gives the interpretation pane the snapshot bytes at the cursor', () => {
    const h = harness(true)
    h.selectTab('vram')
    h.emulator.readMemory.mockClear()
    h.editor().opts.onCursorChange(0x40)
    const [bytes, cursor, base] = vi.mocked(h.pane().update).mock.calls.at(-1)!
    expect(Array.from(bytes as Uint8Array)).toEqual(Array.from(VRAM.slice(0x40, 0x48)))
    expect(cursor).toBe(0x40)
    expect(base).toBe(0)
    expect(h.emulator.readMemory).not.toHaveBeenCalled()
  })

  it('shows an empty window when getVram throws, without falling back to the bus', () => {
    const h = harness(true)
    vi.spyOn(console, 'error').mockImplementation(() => {})
    h.emulator.getVram.mockImplementation(() => {
      throw new Error('no VRAM')
    })
    h.selectTab('vram')
    h.emulator.readMemory.mockClear()
    expect(h.editor().opts.dataSource.read(0, 16).length).toBe(0)
    expect(h.emulator.readMemory).not.toHaveBeenCalled()
  })

  it('scrolls within the tab on a goto inside 0x0000-0xFFFF', () => {
    const h = harness(true)
    h.selectTab('vram')
    const editors = mounted.editors.length
    h.goto('1234')
    expect(mounted.editors.length).toBe(editors)
    expect(h.editor().setCursor).toHaveBeenCalledWith(0x1234)
  })

  it('leaves the bus tabs reading readMemory and editable', () => {
    const h = harness(true)
    h.emulator.getVram.mockClear()
    expect(h.editor().opts.readonly).toBe(false)
    h.editor().opts.dataSource.read(0x10, 4)
    expect(h.emulator.readMemory).toHaveBeenCalledWith(0xff0010, 4)
    h.editor().opts.onByteChange(0x10, 0xaa, 0)
    expect(h.emulator.writeMemoryDebug).toHaveBeenCalledWith(0xff0010, new Uint8Array([0xaa]))
    expect(h.emulator.getVram).not.toHaveBeenCalled()
  })

  it('reads a tab named vram through the bus on a core whose tabs are not the Genesis ones', () => {
    const h = harness(false)
    h.selectTab('vram')
    h.emulator.getVram.mockClear()
    expect(h.editor().opts.readonly).toBe(false)
    h.editor().opts.dataSource.read(0x100, 16)
    expect(h.emulator.readMemory).toHaveBeenCalledWith(0x100, 16)
    expect(h.emulator.getVram).not.toHaveBeenCalled()
  })
})

describe('memory panel: the Genesis ROM tab', () => {
  beforeEach(() => {
    mounted.editors.length = 0
    mounted.panes.length = 0
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  it('reads its window from the cartridge image, at offsets from 0, and never calls readMemory', () => {
    const h = harness(true)
    h.selectTab('rom')
    h.emulator.readMemory.mockClear()
    expect(h.editor().opts.baseAddress).toBe(0)
    expect(h.editor().opts.dataSource.totalSize).toBe(IMAGE.length)
    // Past the 4 MiB the 68000's bus shows of a cartridge.
    const bytes = h.editor().opts.dataSource.read(0x4abc00, 16)
    expect(Array.from(bytes)).toEqual(Array.from(IMAGE.slice(0x4abc00, 0x4abc10)))
    expect(h.emulator.readMemory).not.toHaveBeenCalled()
  })

  it('fetches the image once for the ROM, not at each read or refresh', () => {
    const h = harness(true)
    h.selectTab('rom')
    const editors = mounted.editors.length
    const ds = h.editor().opts.dataSource
    ds.read(0, 16)
    ds.read(0x100000, 0x100000)
    h.editor().opts.onCursorChange(0x20)
    h.panel.update()
    h.panel.update()
    expect(h.emulator.getRomBytes).toHaveBeenCalledTimes(1)
    // The same ROM refreshes the editor in place.
    expect(mounted.editors.length).toBe(editors)
    expect(h.editor().refresh).toHaveBeenCalledTimes(2)
  })

  it('fetches it again when the core holds another ROM, and mounts the editor over it at its size', () => {
    const h = harness(true)
    h.selectTab('rom')
    const editors = mounted.editors.length
    const other = pattern(0x80000, 3)
    h.cartridge.image = other
    h.cartridge.hash = 'other-hash'
    h.panel.update()
    expect(h.emulator.getRomBytes).toHaveBeenCalledTimes(2)
    expect(mounted.editors.length).toBe(editors + 1)
    expect(h.editor().opts.dataSource.totalSize).toBe(other.length)
    expect(Array.from(h.editor().opts.dataSource.read(0x10, 4))).toEqual(Array.from(other.slice(0x10, 0x14)))
  })

  it('fetches it from a new emulator that replaces the old one', () => {
    const h = harness(true)
    h.selectTab('rom')
    const next = h.replaceEmulator()
    h.panel.update()
    expect(next.getRomBytes).toHaveBeenCalledTimes(1)
    expect(Array.from(h.editor().opts.dataSource.read(0x100, 4))).toEqual(Array.from(IMAGE.slice(0x100, 0x104)))
  })

  it('is read-only: the editor is told so and no edit reaches writeMemoryDebug', () => {
    const h = harness(true)
    h.selectTab('rom')
    expect(h.editor().opts.readonly).toBe(true)
    h.editor().opts.onByteChange(0x10, 0xaa, IMAGE[0x10])
    expect(h.emulator.writeMemoryDebug).not.toHaveBeenCalled()
  })

  it('gives the interpretation pane the image bytes at the cursor', () => {
    const h = harness(true)
    h.selectTab('rom')
    h.emulator.readMemory.mockClear()
    h.editor().opts.onCursorChange(0x400040)
    const [bytes, cursor, base] = vi.mocked(h.pane().update).mock.calls.at(-1)!
    expect(Array.from(bytes as Uint8Array)).toEqual(Array.from(IMAGE.slice(0x400040, 0x400048)))
    expect(cursor).toBe(0x400040)
    expect(base).toBe(0)
    expect(h.emulator.readMemory).not.toHaveBeenCalled()
  })

  it('is empty when the core holds no cartridge', () => {
    const h = harness(true, { image: new Uint8Array(0), hash: undefined })
    h.selectTab('rom')
    h.emulator.readMemory.mockClear()
    expect(h.editor().opts.dataSource.totalSize).toBe(0)
    expect(h.editor().opts.dataSource.read(0, 16).length).toBe(0)
    expect(h.emulator.readMemory).not.toHaveBeenCalled()
  })

  it('shows an empty window when getRomBytes throws, without falling back to the bus', () => {
    const h = harness(true)
    vi.spyOn(console, 'error').mockImplementation(() => {})
    h.emulator.getRomBytes.mockImplementation(() => {
      throw new Error('no image')
    })
    h.selectTab('rom')
    h.emulator.readMemory.mockClear()
    expect(h.editor().opts.dataSource.totalSize).toBe(0)
    expect(h.editor().opts.dataSource.read(0, 16).length).toBe(0)
    expect(h.emulator.readMemory).not.toHaveBeenCalled()
    // A refresh does not mount the editor again while the image stays out
    // of reach.
    const editors = mounted.editors.length
    h.panel.update()
    expect(mounted.editors.length).toBe(editors)
  })

  it('scrolls within the tab on a goto inside the image, and leaves it for one past its end', () => {
    const h = harness(true)
    h.selectTab('rom')
    const editors = mounted.editors.length
    h.goto('4abcde')
    expect(mounted.editors.length).toBe(editors)
    expect(h.editor().setCursor).toHaveBeenCalledWith(0x4abcde)

    h.goto('500000')
    expect(mounted.editors.length).toBe(editors + 1)
    expect(h.editor().opts.baseAddress).toBe(0x500000)
    h.editor().opts.dataSource.read(0, 4)
    expect(h.emulator.readMemory).toHaveBeenCalledWith(0x500000, 4)
  })

  it('downloads the image the tab shows, without fetching it again or touching the editor', async () => {
    const h = harness(true)
    const blobs = captureDownloads()
    h.selectTab('rom')
    const refreshes = vi.mocked(h.editor().refresh).mock.calls.length
    h.download()
    expect(blobs).toHaveLength(1)
    expect(await holds(blobs[0], IMAGE)).toBe(true)
    expect(h.anchor.download).toBe('game.bin')
    expect(h.anchor.click).toHaveBeenCalledTimes(1)
    expect(h.emulator.getRomBytes).toHaveBeenCalledTimes(1)
    expect(h.editor().refresh).toHaveBeenCalledTimes(refreshes)
  })

  it('downloads the file the page loaded when the core holds no cartridge', async () => {
    const h = harness(true, { image: new Uint8Array(0), hash: undefined })
    const blobs = captureDownloads()
    const file = pattern(0x1000, 5)
    h.meta.data = file
    h.download()
    expect(blobs).toHaveLength(1)
    expect(await holds(blobs[0], file)).toBe(true)
  })

  it('reads a tab named rom through the bus on a core whose tabs are not the Genesis ones', () => {
    const h = harness(false)
    h.selectTab('rom')
    expect(h.editor().opts.readonly).toBe(false)
    expect(h.editor().opts.dataSource.totalSize).toBe(0x1000000)
    h.editor().opts.dataSource.read(0x100, 16)
    expect(h.emulator.readMemory).toHaveBeenCalledWith(0x100, 16)
    expect(h.emulator.getRomBytes).not.toHaveBeenCalled()
  })
})
