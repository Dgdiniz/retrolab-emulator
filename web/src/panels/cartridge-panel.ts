/**
 * Cartridge panel, thin wrapper around the `cartridge-debugger.js`
 * class, which owns the ROM library + cartridge-info UI. Mirrors
 * RetroLab's cartridge panel.
 *
 * In addition to the bridge's panel.update() tick, we expose
 * `updateCartridgeInfo()` directly so `main.ts` can ping it after a
 * fresh ROM load or a hot-reload (when no panel-wide refresh is
 * scheduled).
 */

import { CartridgeDebugger } from '../cartridge-debugger.js'
import type { DebugBridge, PanelHandle } from '../debug-bridge'

export interface CartridgePanelHandle extends PanelHandle {
  /** Refresh just the cartridge-info pane after a ROM load / hot-reload. */
  refreshCartridgeInfo(): void
}

export function initCartridgePanel(bridge: DebugBridge): CartridgePanelHandle {
  const emulator = bridge.getEmulator()
  if (!emulator) {
    throw new Error('cartridge panel requires an initialised emulator')
  }

  const cartridgeDebugger = new (CartridgeDebugger as any)(emulator)
  void cartridgeDebugger.init()

  /**
   * Point the debugger at the emulator the bridge holds now. A force
   * reset replaces that instance, and the class keeps the one it was
   * built with, so its next ROM load or SRAM edit would reach the old one.
   */
  function followEmulator(): void {
    const current = bridge.getEmulator()
    if (current && cartridgeDebugger.emulator !== current) {
      cartridgeDebugger.emulator = current
    }
  }

  function update(): void {
    // The cartridge debugger refreshes its UI on its own events, so the
    // bridge tick only keeps it on the live emulator (the force reset's
    // closing refresh lands here).
    followEmulator()
  }

  return {
    update,
    dispose() {
      // The class has no teardown; rely on page unload.
    },
    refreshCartridgeInfo() {
      followEmulator()
      try {
        cartridgeDebugger.updateCartridgeInfo()
      } catch (err) {
        console.warn('CartridgeDebugger.updateCartridgeInfo() threw:', err)
      }
    },
  }
}
