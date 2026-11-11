/**
 * `window.debuggerAPI` factory. The keybindings + settings modal both
 * dispatch user actions through this single dictionary, a plain object
 * (not a class) of named actions. `createDebuggerApi(...)` returns the
 * object so `main.ts` can assign it to `window.debuggerAPI` once.
 *
 * Dependencies are injected explicitly so the factory has no global
 * state: the bridge supplies emulator + ROM info, the modals and the
 * settings come through getters, and the toolbar actions and panel
 * refreshes call back into the bootstrap through the `trigger*` /
 * `refresh*` hooks.
 */

import type { DebugBridge } from './debug-bridge'
import { findFocusTarget, focusPageTarget } from './info-tabs'
import { sramManager } from './sram-manager.js'
import { saveStateManager } from './save-state-manager.js'

export interface DebuggerApiDeps {
  /** "Click Run", used by keybindings + the hot-reload auto-resume path. */
  triggerRun(): void
  /** "Click Pause". */
  triggerPause(): void
  /** "Click Step". */
  triggerStep(): void
  /** "Click Reset". */
  triggerReset(): void
  /**
   * Recreate the emulator from scratch and reload the ROM, its SRAM and
   * its breakpoints: the recovery for a reset that fails.
   */
  forceReset(): void | Promise<void>
  /**
   * Stop a running emulator at once (Escape): pause it and stop the
   * frame loop and the audio.
   */
  emergencyStop(): void
  /** Toggle the fullscreen state of the display panel. */
  toggleFullscreen(): void
  /** Open the system file picker for `#rom-file`. */
  openRomDialog(): void
  /** Focus the disassembly "Go to" address bar (matches Ctrl+G). */
  focusGotoAddress(): void
  /**
   * Refresh every panel after a hot reload / save-state load. Wired by
   * the bootstrap to `bridge.refreshAll()`.
   */
  refreshAllPanels(): void
  /** Push a status message into the toolbar (`#status`). */
  setStatus(text: string): void
  /** Update the save-state info label (success/error/transient). */
  setSaveStateInfo(text: string, kind: 'ok' | 'error' | 'clear'): void
  /** Re-feed the cartridge metadata panel after a hot reload. */
  refreshCartridge(): void

  /** Cached help / settings modals (lazy because they boot after init). */
  getHelpModal(): any
  getSettingsModal(): any
  /** Live debugger-settings instance, used by the hot-reload auto-resume rule. */
  getSettings(): any
}

export function createDebuggerApi(bridge: DebugBridge, deps: DebuggerApiDeps): Record<string, any> {
  const api: Record<string, any> = {
    // -- Execution Control ------------------------------------------
    toggleRunPause() {
      if (!bridge.isRomLoaded()) return
      if (bridge.isRunning()) {
        deps.triggerPause()
      } else {
        deps.triggerRun()
      }
    },

    stepOver() {
      if (!bridge.isRomLoaded()) return
      deps.triggerStep()
    },

    stepInto() {
      if (!bridge.isRomLoaded()) return
      // Identical to step-over today: bsnes-style step-into would need
      // disasm classification of the current op (BSR/JSR vs everything
      // else), which the WASM bindings don't expose yet.
      deps.triggerStep()
    },

    stepOut() {
      if (!bridge.isRomLoaded()) return
      console.warn('Step Out not implemented yet')
    },

    reset() {
      if (!bridge.isRomLoaded()) return
      deps.triggerReset()
    },

    forceReset() {
      return deps.forceReset()
    },

    async hotReloadRom() {
      const meta = bridge.getRomMeta()
      const emulator = bridge.getEmulator()
      if (!bridge.isRomLoaded() || !meta.data || !emulator) {
        console.warn('No ROM to reload')
        return
      }
      try {
        console.log('Hot reloading ROM...')

        const wasRunning = bridge.isRunning()
        if (wasRunning) deps.triggerPause()

        emulator.loadRom(meta.data)
        deps.setStatus(`ROM reloaded: ${meta.name}`)
        console.log('ROM reloaded successfully')

        deps.refreshCartridge()

        const settings = deps.getSettings()
        const autoResume = wasRunning && settings && settings.get('debugger.breakOnStart') === false
        if (autoResume) {
          setTimeout(() => deps.triggerRun(), 100)
        } else {
          bridge.setRunning(false)
          deps.refreshAllPanels()
        }
      } catch (err) {
        console.error('Failed to hot reload ROM:', err)
        deps.setStatus(`Error reloading ROM: ${(err as Error).message}`)
      }
    },

    // -- Save States ------------------------------------------------
    async quickSave(slot: number = 1) {
      const meta = bridge.getRomMeta()
      const emulator = bridge.getEmulator()
      if (!bridge.isRomLoaded() || !emulator || !meta.hash) {
        console.warn('Cannot save state: no ROM loaded')
        return
      }
      try {
        const stateData = emulator.saveState()
        if (!stateData || stateData.length === 0) {
          throw new Error('saveState returned empty data')
        }
        await saveStateManager.init()
        await saveStateManager.saveSaveState(
          meta.hash,
          meta.title || 'Unknown',
          slot,
          stateData,
          { frameCount: emulator.getFrameCount ? emulator.getFrameCount() : 0 },
        )
        const slotName = slot === 0 ? 'Quick' : `Slot ${slot}`
        deps.setSaveStateInfo(`${slotName} saved`, 'ok')
        setTimeout(() => deps.setSaveStateInfo('', 'clear'), 3000)
        console.log(`State saved to slot ${slot} (${stateData.length} bytes)`)
      } catch (err) {
        console.error('Failed to save state:', err)
        deps.setSaveStateInfo('Save failed', 'error')
        setTimeout(() => deps.setSaveStateInfo('', 'clear'), 3000)
      }
    },

    async saveToSlot(slot: number) {
      await api.quickSave(slot)
    },

    async quickLoad(slot: number = 1) {
      const meta = bridge.getRomMeta()
      const emulator = bridge.getEmulator()
      if (!bridge.isRomLoaded() || !emulator || !meta.hash) {
        console.warn('Cannot load state: no ROM loaded')
        return
      }
      try {
        await saveStateManager.init()
        const stateObj = await saveStateManager.loadSaveState(meta.hash, slot)
        if (!stateObj) {
          const slotName = slot === 0 ? 'Quick' : `Slot ${slot}`
          deps.setSaveStateInfo(`${slotName} empty`, 'error')
          setTimeout(() => deps.setSaveStateInfo('', 'clear'), 3000)
          return
        }
        emulator.loadState(stateObj.data)
        const slotName = slot === 0 ? 'Quick' : `Slot ${slot}`
        deps.setSaveStateInfo(`${slotName} loaded`, 'ok')
        setTimeout(() => deps.setSaveStateInfo('', 'clear'), 3000)
        console.log(`State loaded from slot ${slot} (${stateObj.data.length} bytes)`)
        deps.refreshAllPanels()
      } catch (err) {
        console.error('Failed to load state:', err)
        deps.setSaveStateInfo('Load failed', 'error')
        setTimeout(() => deps.setSaveStateInfo('', 'clear'), 3000)
      }
    },

    async loadFromSlot(slot: number) {
      await api.quickLoad(slot)
    },

    // -- Debugger Control -------------------------------------------
    toggleBreakpoint() {
      if (!bridge.isRomLoaded()) return
      console.log('Toggle breakpoint at current PC')
    },

    showGoToDialog() {
      if (!bridge.isRomLoaded()) return
      deps.focusGotoAddress()
    },

    showFindDialog() {
      console.log('Find dialog (TODO)')
    },

    showFindReplaceDialog() {
      console.log('Find/Replace dialog (TODO)')
    },

    findNext() {
      console.log('Find next (TODO)')
    },

    findPrevious() {
      console.log('Find previous (TODO)')
    },

    // -- View Control -----------------------------------------------
    /**
     * Bring forward what a focus key names (`disassembly`, `memory`,
     * `vdp`, `audio`, `cartridge`): show its bottom info tab as a click
     * on it does, or scroll its pane into view and give it the keyboard.
     */
    focusTab(tabName: string) {
      focusPageTarget(document, bridge, tabName)
    },

    /**
     * Whether this page has what `focusTab(tabName)` brings forward. The
     * SNES page hides the VDP and Audio tabs, so their keys have nothing
     * to show there.
     */
    canFocusTab(tabName: string): boolean {
      return findFocusTarget(document, tabName) !== null
    },

    nextTab() {
      console.log('Next tab (TODO)')
    },

    previousTab() {
      console.log('Previous tab (TODO)')
    },

    // -- Performance ------------------------------------------------
    setFastForward(enabled: boolean) {
      console.log(`Fast forward: ${enabled}`)
    },

    increaseSpeed() {
      console.log('Increase speed (TODO)')
    },

    decreaseSpeed() {
      console.log('Decrease speed (TODO)')
    },

    normalSpeed() {
      console.log('Normal speed (TODO)')
    },

    // -- General ----------------------------------------------------
    showHelp() {
      const helpModal = deps.getHelpModal()
      if (helpModal) helpModal.toggle()
    },

    openRomDialog() {
      deps.openRomDialog()
    },

    takeScreenshot() {
      console.log('Screenshot (TODO)')
    },

    showSettings() {
      const settingsModal = deps.getSettingsModal()
      if (settingsModal) settingsModal.open()
    },

    /**
     * Leave fullscreen if the display is fullscreen, otherwise close the
     * help and settings modals that are open. Returns whether it did
     * either.
     */
    closeModal(): boolean {
      // Exit fullscreen first so Escape feels natural, most users
      // expect "Escape" to back out of one layer at a time.
      const fsActive = !!(
        document.fullscreenElement ||
        (document as any).webkitFullscreenElement
      )
      if (fsActive) {
        if (document.exitFullscreen) {
          document.exitFullscreen()
        } else if ((document as any).webkitExitFullscreen) {
          (document as any).webkitExitFullscreen()
        }
        return true
      }
      const helpModal = deps.getHelpModal()
      const settingsModal = deps.getSettingsModal()
      let closed = false
      if (helpModal && helpModal.isVisible()) {
        helpModal.close()
        closed = true
      }
      if (settingsModal && settingsModal.isOpen()) {
        settingsModal.close()
        closed = true
      }
      return closed
    },

    /**
     * What Escape does, one layer at a time: leave fullscreen or close an
     * open modal, and only when there is neither, stop the emulator if it
     * is running.
     */
    closeModalOrStop() {
      if (api.closeModal()) return
      if (bridge.isRunning()) deps.emergencyStop()
    },

    toggleFullscreen() {
      deps.toggleFullscreen()
    },

    showError(message: string) {
      console.error('Debugger Error:', message)
      deps.setStatus(`Error: ${message}`)
    },

    /**
     * Auto-save SRAM helper, exposed so the unload handler in
     * `main.ts` doesn't need to import sramManager directly.
     */
    async saveSramIfDirty() {
      const meta = bridge.getRomMeta()
      const emulator = bridge.getEmulator()
      if (!emulator || !meta.hash) return
      try {
        if (emulator.isCartridgeSramDirty && emulator.isCartridgeSramDirty()) {
          const sramData = emulator.getSaveRam()
          if (sramData) {
            await sramManager.saveSRAM(meta.hash, meta.title, sramData)
            emulator.clearCartridgeSramDirty()
          }
        }
      } catch (err) {
        console.warn('Failed to auto-save SRAM:', err)
      }
    },
  }

  return api
}
