/**
 * Call-stack panel, read-only list of call frames for the focused CPU.
 * Mirrors RetroLab's call-stack panel.
 */

import { hex } from '@debugger-core/format-core'
import type { DebugBridge, PanelHandle } from '../debug-bridge'

export function initCallStackPanel(bridge: DebugBridge): PanelHandle {
  const list = document.getElementById('callstack-list')
  if (!list) {
    throw new Error('callstack-list element not found in DOM')
  }

  function update(): void {
    const emulator = bridge.getEmulator()
    if (!emulator) return
    try {
      const focusedCpu = bridge.getFocusedCpu()
      const callstack = emulator.getCallStack(focusedCpu)

      if (!callstack || callstack.length === 0) {
        list!.innerHTML = '<div style="color: #858585;">Call stack empty</div>'
        return
      }

      list!.innerHTML = callstack.map((frame: any) => `
        <div class="callstack-item">
          <span class="callstack-address">${hex(frame.address, 6)}</span>
          <span class="callstack-function">${frame.function_name || '(unknown)'}</span>
        </div>
      `).join('')
    } catch (err) {
      console.error('Failed to update call stack:', err)
    }
  }

  return {
    update,
    dispose() {
      // no DOM listeners or timers owned by this panel
    },
  }
}
