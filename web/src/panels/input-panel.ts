/**
 * Input panel, thin wrapper around the `input-debugger.js`
 * class, which renders the controller schematic + button history. The
 * wrapper aligns the input surface with the bridge.registerPanel
 * registry so all panels look the same from the outside. Mirrors
 * RetroLab's input panel.
 *
 * The InputHandler (the live keyboard/gamepad -> emulator pipe) is owned
 * by `main.ts` since it has to exist before the user opens the input
 * tab, only the debugger UI lives here.
 */

import { InputDebugger } from '../input-debugger.js'
import type { DebugBridge, PanelHandle } from '../debug-bridge'

export function initInputPanel(bridge: DebugBridge): PanelHandle {
  const emulator = bridge.getEmulator()
  if (!emulator) {
    throw new Error('input panel requires an initialised emulator')
  }

  const inputDebugger = new (InputDebugger as any)(emulator)
  inputDebugger.init()

  function update(): void {
    // The InputDebugger redraws itself from a 30 Hz timer, so the bridge
    // tick only keeps it on the live emulator: a force reset replaces the
    // instance the bridge holds, and its closing refresh lands here.
    const current = bridge.getEmulator()
    if (current && inputDebugger.emulator !== current) {
      inputDebugger.emulator = current
    }
  }

  return {
    update,
    dispose() {
      if (typeof inputDebugger.destroy === 'function') {
        try {
          inputDebugger.destroy()
        } catch (err) {
          console.warn('InputDebugger.destroy() threw:', err)
        }
      }
    },
  }
}
