/**
 * Registers panel, grouped CPU register table with change highlighting
 * and a "Z80 cannot execute" banner. Mirrors RetroLab's registers panel.
 *
 * Note on `@debugger-core` reuse: RetroLab's debugger builds register rows
 * via `@debugger-core/registers-core`'s two-column layout, but the
 * vanilla HTML markup here groups by Data / Address / Special and uses
 * the per-row `changed` indicator. The bespoke grouped rendering is
 * intentional and only borrows the `hex()` helper for value
 * formatting; switching to `buildRegistersLayout` would only make
 * sense once the vanilla debugger adopts the same two-column UI.
 */

import { hex } from '@debugger-core/format-core'
import type { DebugBridge, PanelHandle } from '../debug-bridge'

interface RegisterEntry {
  name: string
  value: number
}

const Z80_BYTE_REGS = new Set(['I', 'R', 'IM', 'IFF1', 'IFF2', 'HALT'])

export function initRegistersPanel(bridge: DebugBridge): PanelHandle {
  const view = document.getElementById('registers-view')
  if (!view) {
    throw new Error('registers-view element not found in DOM')
  }

  // The previous-values map is a closure var, not module-level, so two
  // panel instances would not stomp on each other (we only ever create
  // one, but the bridge contract allows recreation after a force reset).
  let previous: Record<string, number> = {}

  function update(): void {
    const emulator = bridge.getEmulator()
    if (!emulator) return
    try {
      const cpuStates = emulator.getCpuStates()
      if (!cpuStates || cpuStates.length === 0) return

      const focusedCpu = bridge.getFocusedCpu()
      const cpu = cpuStates[Math.min(focusedCpu, cpuStates.length - 1)]
      if (!cpu) return

      const current: Record<string, number> = {}
      const is68K = focusedCpu === 0

      if (is68K) {
        const dataRegs: string[] = []
        const addrRegs: string[] = []
        const specialRegs: string[] = []

        cpu.registers.forEach((reg: RegisterEntry) => {
          current[reg.name] = reg.value
          const changed = previous[reg.name] !== reg.value
          const className = changed ? 'register-row changed' : 'register-row'

          const html = `
            <div class="${className}">
              <span class="register-name">${reg.name}:</span>
              <span class="register-value">${hex(reg.value, 8)}</span>
            </div>
          `

          if (reg.name.startsWith('D')) dataRegs.push(html)
          else if (reg.name.startsWith('A')) addrRegs.push(html)
          else specialRegs.push(html)
        })

        view!.innerHTML = `
          <div class="register-group">
            <div class="register-group-title">Data Registers</div>
            ${dataRegs.join('')}
          </div>
          <div class="register-group">
            <div class="register-group-title">Address Registers</div>
            ${addrRegs.join('')}
          </div>
          <div class="register-group">
            <div class="register-group-title">Special</div>
            ${specialRegs.join('')}
          </div>
        `
      } else {
        const mainRegs: string[] = []
        const indexRegs: string[] = []
        const specialRegs: string[] = []

        cpu.registers.forEach((reg: RegisterEntry) => {
          current[reg.name] = reg.value
          const changed = previous[reg.name] !== reg.value
          const className = changed ? 'register-row changed' : 'register-row'
          const padLen = Z80_BYTE_REGS.has(reg.name) ? 2 : 4

          const html = `
            <div class="${className}">
              <span class="register-name">${reg.name}:</span>
              <span class="register-value">${hex(reg.value, padLen)}</span>
            </div>
          `

          if (['AF', 'BC', 'DE', 'HL'].includes(reg.name)) {
            mainRegs.push(html)
          } else if (['IX', 'IY', 'SP', 'PC'].includes(reg.name)) {
            indexRegs.push(html)
          } else {
            specialRegs.push(html)
          }
        })

        // Z80 cannot make progress when held in reset or when the 68K
        // owns the bus: surface a clear visual banner instead of just
        // showing stale registers, which would leave users thinking the
        // step button was broken.
        let banner = ''
        try {
          const inReset = emulator.isZ80Reset && emulator.isZ80Reset()
          const busRequested = emulator.isZ80BusRequested && emulator.isZ80BusRequested()
          if (inReset) {
            banner = '<div style="background:#6b2020;color:#ff8080;padding:4px 8px;margin-bottom:6px;border-radius:3px;font-size:11px;text-align:center;font-weight:bold;">Z80 IN RESET</div>'
          } else if (busRequested) {
            banner = '<div style="background:#6b4d20;color:#ffcc80;padding:4px 8px;margin-bottom:6px;border-radius:3px;font-size:11px;text-align:center;font-weight:bold;">68K HAS BUS</div>'
          }
        } catch {
          /* bindings missing on older builds; skip the banner silently */
        }

        view!.innerHTML = `
          ${banner}
          <div class="register-group">
            <div class="register-group-title">Main Registers</div>
            ${mainRegs.join('')}
          </div>
          <div class="register-group">
            <div class="register-group-title">Index / Stack / PC</div>
            ${indexRegs.join('')}
          </div>
          <div class="register-group">
            <div class="register-group-title">Special</div>
            ${specialRegs.join('')}
          </div>
        `
      }

      previous = current
    } catch (err) {
      console.error('Failed to update registers:', err)
    }
  }

  return {
    update,
    dispose() {
      previous = {}
    },
  }
}
