// SG1 Genesis Emulator, debugger bootstrap.
//
// This module is intentionally thin: it boots WASM, creates the
// emulator + renderer, wires the toolbar listeners, and registers each
// panel into the `DebugBridge`. The actual rendering work lives in the
// panel modules under `panels/` and the loops/timers under
// `game-loop.ts`, `tracing.ts`, `debug-bridge.ts`, `debugger-api.ts`.
//
// The bootstrap stays in JS-ish territory (loose types, `any` on the
// emulator boundary because the WASM bindings are untyped) but it
// type-checks without `// @ts-nocheck`.

import { AudioManager } from './audio.js'
import { InputHandler } from './input-handler.js'
import { KeyBindings } from './keybindings.js'
import { HelpModal } from './help-modal.js'
import { PerformanceOverlay } from './perf-overlay.js'
import { DebuggerSettings } from './debugger-settings.js'
import { SettingsModal } from './settings-modal.js'
import { WebGLRenderer } from './webgl-renderer.js'
import { SHADER_DEFS } from './shaders/shader-defs.js'
import { sramManager } from './sram-manager.js'

import {
  ACCEPTED_ROM_EXTENSIONS,
  DEFAULT_CORE,
  computeByteHash,
  coreDisplayName,
  deriveRomIdentity,
  hasAcceptedRomExtension,
  resolveCoreFromExtension,
  resolveCoreFromQuery,
  systemString,
  wasmModulePath,
  type WebCore,
} from './core-resolver'
import { cpuSelectorLabels } from './trace-ops'
import { cpuAddressableMemoryTabs } from './memory-regions'

import { createDebugBridge } from './debug-bridge'
import { createGameLoop } from './game-loop'
import { setupTracing } from './tracing'
import { createDebuggerApi } from './debugger-api'
import { activateInfoTab } from './info-tabs'
import { initMemoryPanel } from './panels/memory-panel'
import { initDisassemblyPanel } from './panels/disassembly-panel'
import { initRegistersPanel } from './panels/registers-panel'
import { initBreakpointsPanel } from './panels/breakpoints-panel'
import { initWatchpointsPanel } from './panels/watchpoints-panel'
import { initHistoryPanel } from './panels/history-panel'
import { initCallStackPanel } from './panels/callstack-panel'
import { initVdpPanel } from './panels/vdp-panel'
import { initAudioPanel } from './panels/audio-panel'
import { initInputPanel } from './panels/input-panel'
import { initCartridgePanel } from './panels/cartridge-panel'
import { initSnesCartridgePanel, readStoredManifest } from './panels/snes-cartridge-panel'

// The standalone debugger boots ONE native core per page load. It is
// chosen from `?core=` (then the default), switching cores after boot
// is a full reload with the query param set, so this is fixed for the
// lifetime of the page.
const bootCore: WebCore = resolveCoreFromQuery(location.search) ?? DEFAULT_CORE
const isGenesisCore = bootCore === 'sg1'

// wasm-bindgen artifacts live under public/pkg/ and are served as
// static assets at `${import.meta.env.BASE_URL}pkg/`. The URL is
// computed at runtime so it picks up Vite's `base` prefix in both
// dev and the GH Pages production build (where base is
// `/retrolab-emulator/`). A static `import` would resolve to a
// fixed path that ignores BASE_URL and 404 under the subpath. The
// `pkg/<core>_wasm.js` leaf is core-specific (`build-wasm.sh` emits
// both bundles side by side).
let _wasmModulePromise: Promise<any> | null = null
let RetroEmulator: any

async function initWasm(core: WebCore): Promise<{ memory: WebAssembly.Memory } & Record<string, unknown>> {
  if (!_wasmModulePromise) {
    const url = new URL(import.meta.env.BASE_URL + wasmModulePath(core), location.origin).href
    _wasmModulePromise = import(/* @vite-ignore */ url)
  }
  const mod = await _wasmModulePromise
  RetroEmulator = mod.RetroEmulator
  return await mod.default()
}

// -- DOM element refs ------------------------------------------------
const $ = (id: string) => document.getElementById(id) as HTMLElement | null

const elements = {
  romFile: $('rom-file') as HTMLInputElement | null,
  romName: $('rom-name'),
  runBtn: $('run-btn') as HTMLButtonElement | null,
  pauseBtn: $('pause-btn') as HTMLButtonElement | null,
  stepBtn: $('step-btn') as HTMLButtonElement | null,
  resetBtn: $('reset-btn') as HTMLButtonElement | null,
  audioEnabled: $('audio-enabled') as HTMLInputElement | null,
  status: $('status'),
  fpsDisplay: $('fps'),
  canvas: $('display') as HTMLCanvasElement | null,
  canvasPanel: $('canvas-panel'),
  shaderSelect: $('shader-select') as HTMLSelectElement | null,
  fullscreenBtn: $('fullscreen-btn'),
  cpuSelect: $('cpu-select') as HTMLSelectElement | null,
  saveStateBtn: $('save-state-btn') as HTMLButtonElement | null,
  loadStateBtn: $('load-state-btn') as HTMLButtonElement | null,
  saveSlotSelect: $('save-slot-select') as HTMLSelectElement | null,
  saveStateInfo: $('save-state-info'),
  infoTabs: document.querySelectorAll<HTMLElement>('.info-tab'),
}

function setStatus(text: string): void {
  if (elements.status) elements.status.textContent = text
}

function setSaveStateInfo(text: string, kind: 'ok' | 'error' | 'clear'): void {
  if (!elements.saveStateInfo) return
  elements.saveStateInfo.textContent = text
  elements.saveStateInfo.className =
    kind === 'ok' ? 'save-state-info saved'
      : kind === 'error' ? 'save-state-info error'
        : 'save-state-info'
}

/**
 * Hide the Genesis-only info-tab buttons (VDP / Audio / Cartridge) for
 * cores that don't register those panels. The breakpoints tab is the
 * default-active one, so hiding the others never strands the UI on a
 * blank pane.
 */
function hideTabs(tabIds: string[]): void {
  elements.infoTabs.forEach((tab) => {
    if (tab.dataset.tab && tabIds.includes(tab.dataset.tab)) {
      tab.style.display = 'none'
    }
  })
}

function hideGenesisOnlyTabs(): void {
  hideTabs(['vdp', 'audio', 'cartridge'])
}

/**
 * Relabel the CPU focus selector for the active core. The underlying
 * option *values* (`0` primary, `1` secondary) are stable across cores,
 * only the human label changes (68000/Z80 vs 65816/SPC700).
 */
function relabelCpuSelector(core: WebCore): void {
  const select = elements.cpuSelect
  if (!select) return
  const [primary, secondary] = cpuSelectorLabels(core)
  const opt0 = select.querySelector('option[value="0"]')
  const opt1 = select.querySelector('option[value="1"]')
  if (opt0) opt0.textContent = primary
  if (opt1) opt1.textContent = secondary
}

/**
 * Replace the static Genesis memory-region tabs (ram/rom/vram/z80) with
 * tabs derived from the emulator's `getMemoryRegions()`, keeping only the
 * regions `readMemory()` can faithfully show (a non-zero CPU-bus
 * `start_address`). Each tab carries a `data-addr` the memory panel
 * resolves via its goto/custom path. Runs BEFORE `initMemoryPanel` so the
 * panel binds its click handlers to the rebuilt buttons.
 *
 * No-op when the core exposes no `getMemoryRegions()` or yields no
 * addressable region, the Genesis tabs stay as authored.
 */
function rebuildMemoryTabsFromRegions(emulator: any): void {
  const container = document.querySelector<HTMLElement>('.memory-tabs')
  if (!container || typeof emulator?.getMemoryRegions !== 'function') return
  let tabs: ReturnType<typeof cpuAddressableMemoryTabs> = []
  try {
    tabs = cpuAddressableMemoryTabs(emulator.getMemoryRegions())
  } catch (err) {
    console.warn('getMemoryRegions() failed; keeping default memory tabs:', err)
    return
  }
  if (tabs.length === 0) return
  container.innerHTML = ''
  tabs.forEach((tab, i) => {
    const btn = document.createElement('button')
    btn.className = i === 0 ? 'tab-btn active' : 'tab-btn'
    btn.dataset.addr = `0x${tab.address.toString(16).toUpperCase()}`
    btn.textContent = tab.label
    container.appendChild(btn)
  })
}

// -- Bridge + module-level handles ----------------------------------
const bridge = createDebugBridge()
let glRenderer: any = null
let audioManager: any = null
let inputHandler: any = null
let keyBindings: any = null
let helpModal: any = null
let perfOverlay: any = null
let settings: any = null
let settingsModal: any = null
let cartridgePanel: ReturnType<typeof initCartridgePanel> | null = null
let snesCartridgePanel: ReturnType<typeof initSnesCartridgePanel> | null = null
let breakpointsPanel: ReturnType<typeof initBreakpointsPanel> | null = null
let tracing: ReturnType<typeof setupTracing> | null = null
let gameLoop: ReturnType<typeof createGameLoop> | null = null
let sramAutoSaveTimer: number | null = null

// -- Initialization -------------------------------------------------
async function initEmulator(): Promise<void> {
  try {
    setStatus('Loading WASM...')

    // Keep the picker's accept list in sync with the resolver's single
    // source of truth (the static HTML attr is just a sensible default).
    if (elements.romFile) elements.romFile.accept = ACCEPTED_ROM_EXTENSIONS.join(',')

    const wasmExports = await initWasm(bootCore)
    const wasmMemory = wasmExports.memory as WebAssembly.Memory

    const emulator = new RetroEmulator(systemString(bootCore))
    bridge.setEmulator(emulator, wasmMemory)

    if (elements.canvas) {
      glRenderer = new WebGLRenderer(elements.canvas)
    }

    // The playback path is core-agnostic and always created; the Genesis
    // YM2612 + PSG view belongs to the audio panel registered below.
    audioManager = new AudioManager()
    console.log('Audio manager created (will init on user interaction)')

    inputHandler = new InputHandler(emulator)
    inputHandler.init()
    console.log('Input handler initialized')

    // Non-Genesis cores rebuild the memory-region tabs from
    // `getMemoryRegions()` before the panel binds its handlers (Genesis
    // keeps its static ram/rom/vram/z80 tabs + memory-core machinery).
    if (!isGenesisCore) rebuildMemoryTabsFromRegions(emulator)

    // Wire the generic debugger panels into the bridge. The order
    // matters only for the typed panels (they pick up emulator state via
    // bridge.getEmulator() already set above), the wrapper panels
    // (audio/input/cartridge) each instantiate their JS class
    // internally. Disassembly/registers/breakpoints/history/callstack are
    // CPU-agnostic and run for every native core.
    bridge.registerPanel('memory', initMemoryPanel(bridge, { genesisRegions: isGenesisCore }))
    bridge.registerPanel('disassembly', initDisassemblyPanel(bridge))
    bridge.registerPanel('registers', initRegistersPanel(bridge))
    breakpointsPanel = initBreakpointsPanel(bridge)
    bridge.registerPanel('breakpoints', breakpointsPanel)
    bridge.registerPanel('watchpoints', initWatchpointsPanel(bridge))
    bridge.registerPanel('history', initHistoryPanel(bridge))
    bridge.registerPanel('callstack', initCallStackPanel(bridge))
    bridge.registerPanel('input', initInputPanel(bridge))

    // The VDP / Audio / Cartridge panels are Genesis-specific (they read
    // sg1-only VRAM/CRAM, the YM2612/PSG multi-chip view, and the Sega
    // mapper/manifest editor). Register them only for sg1 and hide their
    // info-tabs on other cores. snes1 gets its own cartridge/manifest
    // panel (LoROM/HiROM presets, mirrored SRAM, FastROM); both cores
    // carry a "Cartridge" tab but only one is wired per page load.
    if (isGenesisCore) {
      bridge.registerPanel('vdp', initVdpPanel(bridge))
      bridge.registerPanel('audio', initAudioPanel(bridge))
      cartridgePanel = initCartridgePanel(bridge)
      bridge.registerPanel('cartridge', cartridgePanel)
      hideTabs(['snes-cartridge'])
    } else {
      hideGenesisOnlyTabs()
      if (bootCore === 'snes1') {
        snesCartridgePanel = initSnesCartridgePanel(bridge)
        bridge.registerPanel('snes-cartridge', snesCartridgePanel)
      } else {
        hideTabs(['snes-cartridge'])
      }
    }

    relabelCpuSelector(bootCore)

    settings = new DebuggerSettings()
    settings.applyAppearance()
    console.log('Debugger settings loaded')

    initShaderUI()

    perfOverlay = new PerformanceOverlay(emulator)
    console.log('Performance overlay initialized (dblclick title to hide)')

    tracing = setupTracing(bridge, bootCore)

    gameLoop = createGameLoop(bridge, {
      getRenderer: () => glRenderer,
      getAudioManager: () => audioManager,
      getPerfOverlay: () => perfOverlay,
      checkTraceFlush: () => tracing?.checkFlush(),
      refreshRegisters: () => bridge.refreshPanel('registers'),
      refreshAllPanels: () => bridge.refreshAll(),
      setStatus,
      setFpsLabel: (text) => {
        if (elements.fpsDisplay) elements.fpsDisplay.textContent = text
      },
    })

    // Let panels blit the current framebuffer once (used by the VDP panel
    // to make a paused-state layer toggle visible immediately).
    bridge.setDisplayRefresher(() => gameLoop?.updateDisplayOnce())

    const debuggerAPI = createDebuggerApi(bridge, {
      triggerRun: () => elements.runBtn?.click(),
      triggerPause: () => elements.pauseBtn?.click(),
      triggerStep: () => elements.stepBtn?.click(),
      triggerReset: () => elements.resetBtn?.click(),
      forceReset: () => forceResetEmulator(),
      emergencyStop,
      toggleFullscreen,
      openRomDialog: () => elements.romFile?.click(),
      focusGotoAddress: () => {
        const goto = $('goto-address') as HTMLInputElement | null
        if (goto) { goto.focus(); goto.select() }
      },
      refreshAllPanels: () => bridge.refreshAll(),
      setStatus,
      setSaveStateInfo,
      refreshCartridge: () => cartridgePanel?.refreshCartridgeInfo(),
      getHelpModal: () => helpModal,
      getSettingsModal: () => settingsModal,
      getSettings: () => settings,
    })

    keyBindings = new KeyBindings(debuggerAPI)
    console.log(`Keybindings initialized (${Object.keys(keyBindings.bindings).length} shortcuts)`)

    helpModal = new HelpModal(keyBindings)
    console.log('Help modal initialized (press F1)')

    settingsModal = new SettingsModal(settings, debuggerAPI)
    console.log('Settings modal initialized (press Ctrl+,)')

    ;(window as any).debuggerAPI = debuggerAPI
    ;(window as any).keyBindings = keyBindings
    ;(window as any).helpModal = helpModal
    ;(window as any).settingsModal = settingsModal
    ;(window as any).settings = settings
    ;(window as any).perfOverlay = perfOverlay

    setStatus('Ready')
    console.log('Emulator initialized')

    // Boot-time refresh so the History / CallStack / VDP panels show
    // current state if a ROM is already loaded (auto-load, SRAM
    // restore, save-state hydrate). Without this the panels stay
    // empty until the first step / pause event fires `refreshAll`.
    if (bridge.isRomLoaded()) {
      bridge.refreshAll()
    }
  } catch (err) {
    console.error('Failed to initialize emulator:', err)
    setStatus(`Error: ${(err as Error).message}`)
  }
}

// -- Force reset (emergency recovery) -------------------------------
async function forceResetEmulator(): Promise<void> {
  console.log('Force reset - recreating emulator...')
  setStatus('Force resetting...')

  bridge.setRunning(false)
  gameLoop?.stop()

  if (audioManager) audioManager.stop()

  try {
    const wasmMemory = bridge.getWasmMemory()
    if (!wasmMemory) throw new Error('WASM memory not initialised')

    const emulator = new RetroEmulator(systemString(bootCore))
    bridge.setEmulator(emulator, wasmMemory)

    const meta = bridge.getRomMeta()
    if (meta.data) {
      if (bootCore === 'snes1' && emulator.setCartridgeManifest) {
        try {
          const override = await readStoredManifest(computeByteHash(meta.data))
          if (override) emulator.setCartridgeManifest(override)
        } catch (err) {
          console.warn('Failed to re-apply manifest override after force reset:', err)
        }
      }
      emulator.loadRom(meta.data)
      if (elements.romName) elements.romName.textContent = meta.name
      bridge.setRomLoaded(true)
    }

    if (meta.hash) {
      try {
        await sramManager.init()
        const savedSram = await sramManager.loadSRAM(meta.hash)
        if (savedSram && savedSram.length > 0) {
          emulator.setSaveRam(savedSram)
          console.log(`SRAM restored from IndexedDB after force reset (${savedSram.length} bytes)`)
        }
      } catch (e) {
        console.warn('Failed to restore SRAM after force reset:', e)
      }

      try {
        await breakpointsPanel?.loadFromStorage()
      } catch (e) {
        console.warn('Failed to restore breakpoints after force reset:', e)
      }
    }

    if (inputHandler) {
      inputHandler.emulator = emulator
    }

    setStatus('Force reset complete')
    console.log('Force reset complete')
    // Panels read the emulator from the bridge; this refresh also moves
    // the audio, input and cartridge panels' debuggers onto the new
    // instance.
    bridge.refreshAll()
  } catch (err) {
    console.error('Force reset failed:', err)
    setStatus('Force reset failed: ' + (err as Error).message)
  }
}

// -- ROM Loading ----------------------------------------------------
// `ACCEPTED_ROM_EXTENSIONS` + `hasAcceptedRomExtension` come from
// `core-resolver` so the picker, drag-drop, and core selection all agree
// on the same extension set.

/**
 * Reload the page onto a different core when the dropped ROM's
 * extension doesn't match the core we booted with. The OSS debugger is
 * single-core-per-page-load, so a core switch is a navigation with
 * `?core=` set; the user re-drops the ROM on the freshly-booted core.
 * Returns true when a redirect was triggered (caller must bail out).
 */
function maybeRedirectForCore(file: File): boolean {
  const wanted = resolveCoreFromExtension(file.name)
  if (!wanted || wanted === bootCore) return false
  setStatus(`Switching to ${coreDisplayName(wanted)} — drop the ROM again after reload…`)
  const url = new URL(location.href)
  url.searchParams.set('core', wanted)
  location.assign(url.href)
  return true
}

async function loadRomFromFile(file: File): Promise<void> {
  const emulator = bridge.getEmulator()
  if (!emulator) return
  if (maybeRedirectForCore(file)) return

  try {
    setStatus('Loading ROM...')
    const buffer = await file.arrayBuffer()
    const data = new Uint8Array(buffer)

    // snes1: apply a persisted cartridge-manifest override (keyed by the
    // byte hash) before loadRom so the core builds the cartridge with the
    // user's mapping (e.g. the DKC 2 KiB mirrored SRAM fix) instead of
    // pure heuristic detection. No-op on cores without the binding.
    if (bootCore === 'snes1' && emulator.setCartridgeManifest) {
      try {
        const override = await readStoredManifest(computeByteHash(data))
        if (override) emulator.setCartridgeManifest(override)
      } catch (err) {
        console.warn('Failed to apply stored manifest override:', err)
      }
    }

    emulator.loadRom(data)
    bridge.setRomLoaded(true)
    bridge.setRomMeta({ data, name: file.name })
    if (elements.romName) elements.romName.textContent = file.name

    // SG1 has native cartridge hash/title; snes1 does not, so
    // `deriveRomIdentity` hashes the bytes + reads `getRomInfo().title`.
    const { hash, title } = deriveRomIdentity(emulator, data, file.name, bootCore)
    bridge.setRomMeta({ hash, title })

    if (hash) {
      try {
        await sramManager.init()
        const savedSram = await sramManager.loadSRAM(hash)
        if (savedSram) {
          emulator.setSaveRam(savedSram)
          console.log(`SRAM auto-loaded for ${title} (${savedSram.length} bytes)`)
        }
      } catch (err) {
        console.warn('Failed to auto-load SRAM:', err)
      }
      startSramAutoSave()

      try {
        await breakpointsPanel?.loadFromStorage()
      } catch (err) {
        console.warn('Failed to auto-load breakpoints:', err)
      }
    }

    setStatus('ROM loaded')
    tracing?.refreshUi()
    if (settings) settings.addRecentRom(file.name)
    cartridgePanel?.refreshCartridgeInfo()
    snesCartridgePanel?.refresh()

    if (elements.runBtn) elements.runBtn.disabled = false
    if (elements.pauseBtn) elements.pauseBtn.disabled = false
    if (elements.stepBtn) elements.stepBtn.disabled = false
    if (elements.resetBtn) elements.resetBtn.disabled = false
    if (elements.cpuSelect) elements.cpuSelect.disabled = false
    if (elements.saveStateBtn) elements.saveStateBtn.disabled = false
    if (elements.loadStateBtn) elements.loadStateBtn.disabled = false

    bridge.refreshAll()
  } catch (err) {
    console.error('Failed to load ROM:', err)
    setStatus(`Error loading ROM: ${(err as Error).message}`)
  }
}

elements.romFile?.addEventListener('change', async (e) => {
  const target = e.target as HTMLInputElement
  const file = target.files?.[0]
  if (!file) return
  await loadRomFromFile(file)
})

// Drag-and-drop ROM loading on the canvas panel. Picks the first accepted
// file silently (mirrors the file picker, which is also single-file).
if (elements.canvasPanel) {
  const panel = elements.canvasPanel

  panel.addEventListener('dragover', (e) => {
    e.preventDefault()
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy'
    panel.classList.add('dragover')
  })

  panel.addEventListener('dragleave', (e) => {
    if (e.target === panel) panel.classList.remove('dragover')
  })

  panel.addEventListener('drop', async (e) => {
    e.preventDefault()
    panel.classList.remove('dragover')
    const files = Array.from(e.dataTransfer?.files ?? [])
    const rom = files.find((f) => hasAcceptedRomExtension(f.name))
    if (!rom) return
    await loadRomFromFile(rom)
  })
}

// -- Execution Control ----------------------------------------------
elements.runBtn?.addEventListener('click', async () => {
  if (!bridge.isRomLoaded()) return
  const emulator = bridge.getEmulator()
  if (!emulator) return

  const audioEnabled = !!elements.audioEnabled?.checked
  if (audioEnabled && audioManager && !audioManager.isInitialized) {
    try {
      console.log('Initializing Web Audio API...')
      await audioManager.init(emulator)
      audioManager.start()
      console.log('Audio initialized and started')
    } catch (err) {
      console.error('Failed to initialize audio:', err)
    }
  } else if (audioEnabled && audioManager && !audioManager.isRunning) {
    audioManager.start()
    console.log('Audio resumed')
  } else if (!audioEnabled) {
    console.log('Audio disabled (checkbox unchecked)')
  }

  bridge.setRunning(true)
  emulator.setExecutionMode('Running')
  setStatus('Running')
  gameLoop?.start()
})

elements.pauseBtn?.addEventListener('click', async () => {
  const emulator = bridge.getEmulator()
  if (!emulator) return
  bridge.setRunning(false)
  emulator.setExecutionMode('Paused')
  setStatus('Paused')
  if (audioManager?.isRunning) audioManager.stop()
  await (window as any).debuggerAPI?.saveSramIfDirty?.()
  gameLoop?.stop()
  bridge.refreshAll()
})

elements.stepBtn?.addEventListener('click', () => {
  if (!bridge.isRomLoaded()) return
  const emulator = bridge.getEmulator()
  if (!emulator) return

  bridge.setRunning(false)
  gameLoop?.stop()

  // Skip the actual `step()` call when the focused CPU can't make
  // progress (Z80 held in reset or 68K owns the bus). The bindings
  // throw or no-op silently otherwise, leaving the user staring at a
  // status line that didn't change.
  const focusedCpu = bridge.getFocusedCpu()
  if (focusedCpu === 1) {
    try {
      if (emulator.isZ80Reset && emulator.isZ80Reset()) {
        setStatus('Z80 in reset - step has no effect')
        bridge.refreshAll()
        return
      }
      if (emulator.isZ80BusRequested && emulator.isZ80BusRequested()) {
        setStatus('68K has Z80 bus - step has no effect')
        bridge.refreshAll()
        return
      }
    } catch {
      // bindings may not be present on older builds; fall through
    }
  }

  emulator.step()
  emulator.setExecutionMode('Paused')
  setStatus('Stepped')
  bridge.refreshAll()
})

elements.cpuSelect?.addEventListener('change', () => {
  const emulator = bridge.getEmulator()
  if (!emulator || !elements.cpuSelect) return
  const cpuId = parseInt(elements.cpuSelect.value, 10)
  emulator.setFocusedCpu(cpuId)
  bridge.refreshAll()
})

elements.resetBtn?.addEventListener('click', async () => {
  const emulator = bridge.getEmulator()
  if (!emulator) return
  console.log('Reset requested...')

  bridge.setRunning(false)
  gameLoop?.stop()
  if (audioManager?.isRunning) audioManager.stop()
  await (window as any).debuggerAPI?.saveSramIfDirty?.()

  setStatus('Resetting...')
  try {
    emulator.setExecutionMode('Paused')
    emulator.reset()
    setStatus('Reset complete')
    console.log('Reset complete')
    await new Promise((resolve) => setTimeout(resolve, 10))
    bridge.refreshAll()
  } catch (err) {
    console.error('Reset failed:', err)
    setStatus('Reset failed - try Force Reset (Ctrl+Shift+Backspace)')
    if (confirm('Reset failed. Recreate emulator from scratch?')) {
      await forceResetEmulator()
    }
  }
})

// -- Save State Controls --------------------------------------------
elements.saveStateBtn?.addEventListener('click', () => {
  const slot = parseInt(elements.saveSlotSelect?.value ?? '1', 10)
  ;(window as any).debuggerAPI?.quickSave(slot)
})

elements.loadStateBtn?.addEventListener('click', () => {
  const slot = parseInt(elements.saveSlotSelect?.value ?? '1', 10)
  ;(window as any).debuggerAPI?.quickLoad(slot)
})

// -- Emergency stop -------------------------------------------------
// Escape's last layer (`debuggerAPI.closeModalOrStop`, bound in
// `keybindings.js` with every other shortcut): pause a running emulator
// at once, stopping the frame loop and the audio.
function emergencyStop(): void {
  if (!bridge.isRunning()) return
  console.log('Escape pressed - Emergency Stop')
  bridge.setRunning(false)
  gameLoop?.stop()
  if (audioManager?.isRunning) audioManager.stop()
  try {
    bridge.getEmulator()?.setExecutionMode('Paused')
  } catch (err) {
    console.error('Failed to pause:', err)
  }
  setStatus('Stopped (Escape)')
  bridge.refreshAll()
}

// -- Fullscreen ----------------------------------------------------
function toggleFullscreen(): void {
  const panel = elements.canvasPanel
  if (!panel) return

  const inFullscreen = !!(
    document.fullscreenElement ||
    (document as any).webkitFullscreenElement
  )
  if (inFullscreen) {
    if (document.exitFullscreen) document.exitFullscreen()
    else if ((document as any).webkitExitFullscreen) (document as any).webkitExitFullscreen()
  } else {
    if (panel.requestFullscreen) panel.requestFullscreen()
    else if ((panel as any).webkitRequestFullscreen) (panel as any).webkitRequestFullscreen()
  }
}

elements.fullscreenBtn?.addEventListener('click', toggleFullscreen)
document.addEventListener('fullscreenchange', updateFullscreenIcon)
document.addEventListener('webkitfullscreenchange', updateFullscreenIcon as EventListener)

function updateFullscreenIcon(): void {
  if (!elements.fullscreenBtn) return
  const isFS = !!(
    document.fullscreenElement ||
    (document as any).webkitFullscreenElement
  )
  const enterPath = elements.fullscreenBtn.querySelector('.fs-enter') as HTMLElement | null
  const exitPath = elements.fullscreenBtn.querySelector('.fs-exit') as HTMLElement | null
  if (enterPath) enterPath.style.display = isFS ? 'none' : 'inline'
  if (exitPath) exitPath.style.display = isFS ? 'inline' : 'none'
}

// -- Audio toggle ---------------------------------------------------
elements.audioEnabled?.addEventListener('change', async (e) => {
  const enabled = (e.target as HTMLInputElement).checked
  const emulator = bridge.getEmulator()
  if (emulator) emulator.setAudioEnabled(enabled)

  if (enabled) {
    if (audioManager && !audioManager.isInitialized) {
      try {
        console.log('Enabling audio...')
        await audioManager.init(emulator)
        if (bridge.isRunning()) audioManager.start()
        console.log('Audio enabled')
      } catch (err) {
        console.error('Failed to enable audio:', err)
        ;(e.target as HTMLInputElement).checked = false
      }
    } else if (audioManager && bridge.isRunning()) {
      audioManager.start()
      console.log('Audio resumed')
    }
  } else if (audioManager) {
    audioManager.stop()
    console.log('Audio disabled')
  }
})

// -- SRAM auto-save ------------------------------------------------
function startSramAutoSave(): void {
  stopSramAutoSave()
  sramAutoSaveTimer = window.setInterval(() => {
    ;(window as any).debuggerAPI?.saveSramIfDirty?.()
  }, 5000)
}

function stopSramAutoSave(): void {
  if (sramAutoSaveTimer !== null) {
    clearInterval(sramAutoSaveTimer)
    sramAutoSaveTimer = null
  }
}

window.addEventListener('beforeunload', () => {
  ;(window as any).debuggerAPI?.saveSramIfDirty?.()
})

// -- Info tabs -----------------------------------------------------
// A click on an info tab shows its pane and refreshes its panel, through
// the same `activateInfoTab` the focus keys (Ctrl+3 to Ctrl+5) use. The
// VDP / audio / input / cartridge panels do their own sub-tab routing
// internally.
elements.infoTabs.forEach((tab) => {
  tab.addEventListener('click', () => {
    const tabName = tab.dataset.tab
    if (tabName) activateInfoTab(document, bridge, tabName)
  })
})

// -- Shader dropdown -----------------------------------------------
function initShaderUI(): void {
  const select = elements.shaderSelect
  if (!select) return

  for (const [key, def] of Object.entries(SHADER_DEFS as Record<string, any>)) {
    const option = document.createElement('option')
    option.value = key
    option.textContent = def.name
    select.appendChild(option)
  }

  const savedShader = settings ? settings.get('appearance.shader') : 'none'
  if (savedShader && savedShader !== 'none' && (SHADER_DEFS as any)[savedShader]) {
    select.value = savedShader
    if (glRenderer) {
      const ok = glRenderer.setShader((SHADER_DEFS as any)[savedShader])
      if (ok) console.log('Shader restored:', (SHADER_DEFS as any)[savedShader].name)
    }
  }

  select.addEventListener('change', () => {
    const key = select.value
    if (key === 'none') {
      glRenderer?.setShader(null)
    } else if ((SHADER_DEFS as any)[key]) {
      if (glRenderer) {
        const ok = glRenderer.setShader((SHADER_DEFS as any)[key])
        if (!ok) {
          select.value = 'none'
          console.warn('Shader compilation failed, reverted to no shader')
        }
      }
    }
    if (settings) settings.set('appearance.shader', key)
  })

  if (glRenderer && !glRenderer.isWebGL2) {
    select.disabled = true
    select.title = 'Shaders require WebGL2'
  }
}

// -- Boot ----------------------------------------------------------
initEmulator()
