/**
 * Call-stack panel, read-only list of call frames for the focused CPU.
 * Mirrors RetroLab's call-stack panel.
 */

import { hex } from '@debugger-core/format-core'
import type { DebugBridge, PanelHandle } from '../debug-bridge'

/**
 * One call the CPU made and has not returned from, as `getCallStack(cpu)`
 * gives it: the address of the call, the address it returns to, and the
 * name of the function it called when the core knows one. An interrupt's
 * frame holds the interrupted address as both.
 */
export interface CallFrame {
  call_address: number
  return_address: number
  function_name?: string | null
}

function escapeHtml(value: string): string {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
}

/**
 * The panel's rows for the frames `getCallStack(cpu)` gives, in its order
 * (the outermost call first): each call's address, the address it returns
 * to, and the function's name, `(unknown)` when the core gives none.
 */
export function callStackHtml(frames: readonly CallFrame[] | null | undefined): string {
  if (!frames || frames.length === 0) {
    return '<div style="color: #858585;">Call stack empty</div>'
  }
  return frames.map((frame) => `
        <div class="callstack-item">
          <span class="callstack-address">${hex(frame.call_address, 6)}</span>
          <span class="callstack-return" title="Return address">→ ${hex(frame.return_address, 6)}</span>
          <span class="callstack-function">${frame.function_name ? escapeHtml(frame.function_name) : '(unknown)'}</span>
        </div>
      `).join('')
}

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
      list!.innerHTML = callStackHtml(emulator.getCallStack(focusedCpu))
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
