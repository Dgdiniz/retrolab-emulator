/**
 * Audio panel, thin wrapper around the
 * `audio-debugger-multi-chip.js` class, which owns the Channels /
 * Waveform / Spectrum tabs for the YM2612 + PSG (the spectrum tab is a
 * placeholder). Mirrors RetroLab's audio panel.
 *
 * The wrapper exists so the umbrella `bridge.registerPanel(...)`
 * registry can manage the audio surface identically to the typed
 * panels, and so the JS module can be ported to TS without disturbing
 * the panel API. It is the only owner of the audio view: one instance
 * on `#audio-debugger-container`, one set of listeners, one refresh
 * timer.
 */

import { AudioDebuggerMultiChip } from '../audio-debugger-multi-chip.js'
import type { DebugBridge, PanelHandle } from '../debug-bridge'

export function initAudioPanel(bridge: DebugBridge): PanelHandle {
  const emulator = bridge.getEmulator()
  if (!emulator) {
    throw new Error('audio panel requires an initialised emulator')
  }

  const audioDebugger = new (AudioDebuggerMultiChip as any)(emulator, 'audio-debugger-container')

  // init() builds the tabs, binds their controls and starts the refresh
  // timer. It catches and logs its own errors, so a failure leaves the
  // audio tab empty instead of stopping the emulator's boot.
  void audioDebugger.init()

  function update(): void {
    // The multi-chip debugger redraws itself from a 30 Hz timer (skipped
    // while the emulator runs), so the bridge tick only keeps it on the
    // live emulator: a force reset replaces the instance the bridge
    // holds, and its closing refresh lands here.
    const current = bridge.getEmulator()
    if (current && audioDebugger.emulator !== current) {
      audioDebugger.emulator = current
    }
  }

  return {
    update,
    dispose() {
      // Stops the refresh timer and empties the container.
      audioDebugger.destroy()
    },
  }
}
