/**
 * History panel, instruction trace history for the focused CPU.
 * Mirrors RetroLab's history panel.
 *
 * Consumes `@debugger-core/format-core` for address formatting and the
 * `DebugBridge` for emulator access + focused CPU lookup.
 */

import { hex } from '@debugger-core/format-core'
import type { DebugBridge, PanelHandle } from '../debug-bridge'

const HISTORY_LIMIT = 50

function escapeHtml(value: string): string {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;')
}

export function initHistoryPanel(bridge: DebugBridge): PanelHandle {
  const list = document.getElementById('history-list')
  if (!list) {
    throw new Error('history-list element not found in DOM')
  }

  function update(): void {
    const emulator = bridge.getEmulator()
    if (!emulator) return
    try {
      const focusedCpuId = bridge.getFocusedCpu()
      const history = emulator.getInstructionHistory(focusedCpuId, HISTORY_LIMIT)

      if (!history || history.length === 0) {
        list!.innerHTML = '<div style="color: #858585;">No history</div>'
        return
      }

      const addrPad = focusedCpuId === 0 ? 6 : 4

      list!.innerHTML = history.slice().reverse().map((trace: any) => {
        const addr = trace.address >>> 0
        let instText: string = trace.instruction

        // 68K: re-disassemble from current memory so the listing matches
        // what the disassembly panel shows (ROM is immutable, so the
        // address always resolves the same way). Z80: keep the trace
        // text from capture time because Z80 RAM is routinely rewritten
        // by the 68000 with different code, and re-disassembling later
        // would render mismatched mnemonics.
        if (focusedCpuId === 0) {
          try {
            const disasmFn = emulator.disassembleCpu
              ? (a: number, c: number) => emulator.disassembleCpu(focusedCpuId, a, c)
              : (a: number, c: number) => emulator.disassembleAt(a, c)
            const one = disasmFn(addr, 1)
            if (one && one.length) {
              const inst = one[0]
              const operands = inst.operands ? ` ${inst.operands}` : ''
              const comment = inst.comment ? ` ; ${inst.comment}` : ''
              instText = `${inst.mnemonic}${operands}${comment}`
            }
          } catch {
            // fall back to trace.instruction
          }
        }

        return `
          <div class="history-item">
            <span class="history-address">${hex(addr, addrPad)}</span>
            <span class="history-instruction">${escapeHtml(instText)}</span>
            <span class="history-cycles">${trace.cycle_count} cycles</span>
          </div>
        `
      }).join('')
    } catch (err) {
      console.error('Failed to update history:', err)
    }
  }

  return {
    update,
    dispose() {
      // no DOM listeners or timers owned by this panel
    },
  }
}
