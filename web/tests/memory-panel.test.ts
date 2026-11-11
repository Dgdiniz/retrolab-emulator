import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// The hex editor and the interpretation pane draw into the page's DOM; the
// stand-ins record what the panel hands them instead.
const mounted = vi.hoisted(() => ({
  editors: [] as Array<{ opts: any; setCursor: (offset: number) => void }>,
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
 * The memory panel over a stand-in page holding the Genesis tabs, with the
 * interpretation pane switched on, and an emulator whose VRAM is `VRAM`.
 */
function harness(genesisRegions: boolean) {
  const tabs = ['ram', 'rom', 'vram', 'z80'].map((region) => element({ region }))
  const elements: Record<string, FakeElement> = {
    'memory-view': element(),
    'memory-interp': element(),
    'memory-address': element(),
    'memory-interp-toggle': element(),
    'memory-cursor-addr': element(),
  }
  vi.stubGlobal('document', {
    getElementById: (id: string) => elements[id] ?? null,
    querySelectorAll: () => tabs,
  })
  vi.stubGlobal('window', { localStorage: { getItem: () => '1', setItem() {} } })

  const emulator = {
    readMemory: vi.fn((_address: number, length: number) => new Uint8Array(length).fill(0x11)),
    getVram: vi.fn(() => VRAM.slice()),
    writeMemoryDebug: vi.fn(),
  }
  const bridge = { getEmulator: () => emulator } as unknown as DebugBridge
  const panel = initMemoryPanel(bridge, { genesisRegions })

  function selectTab(region: string): void {
    tabs.find((t) => t.dataset.region === region)!.handlers.click[0]()
  }
  function goto(text: string): void {
    elements['memory-address'].handlers.keypress[0]({ key: 'Enter', target: { value: text } })
  }
  const editor = () => mounted.editors.at(-1)!
  const pane = () => mounted.panes.at(-1)!

  return { emulator, panel, selectTab, goto, editor, pane }
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
