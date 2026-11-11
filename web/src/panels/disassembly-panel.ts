/**
 * Disassembly panel, bsnes-style instruction list around the focused
 * CPU's PC, with breakpoint gutters and click-to-toggle.
 * Mirrors RetroLab's disassembly panel.
 *
 * Consumes `@debugger-core/disassembly-core` for the breakpoint classify
 * helpers and `@debugger-core/format-core` for address/byte formatting.
 *
 * Row count, PC highlight, auto-scroll and opcode visibility are driven
 * by the shared debugger settings (`debugger.disassemblyLines`,
 * `behavior.*`); the panel subscribes so the user can flip the toggles
 * in the settings modal without a reload. Output is always padded /
 * truncated to exactly `disassemblyLines` rows so the panel never
 * grows / shrinks as we step through code.
 */

import { hex, hexBytes, parseHexAddress } from '@debugger-core/format-core'
import {
  enabledBreakpointAddresses,
  padDisasmRows,
  splitWindowCounts,
  type BreakpointLike,
} from '@debugger-core/disassembly-core'
import {
  isEmptyAccess,
  parseAccessFlags,
  toggleFlag,
  type RawBpType,
} from '@debugger-core/breakpoints-core'
import { parseEventTag } from '@debugger-core/watchpoints-core'
import type { DebugBridge, PanelHandle } from '../debug-bridge'
import { saveBreakpoints } from './breakpoint-persistence'

const DEFAULT_ROWS = 24
const MIN_ROWS = 8

interface SettingsReader {
  get(key: string): unknown
  subscribe?(fn: (path: string, value: unknown) => void): () => void
}
function getSettings(): SettingsReader | null {
  if (typeof window === 'undefined') return null
  return (window as unknown as { settings?: SettingsReader }).settings ?? null
}
function readSetting<T>(key: string, fallback: T): T {
  const settings = getSettings()
  if (!settings) return fallback
  const v = settings.get(key)
  return (v as T) ?? fallback
}

function readRowCount(): number {
  const raw = readSetting<unknown>('debugger.disassemblyLines', DEFAULT_ROWS)
  const n = Number(raw)
  if (!Number.isFinite(n) || n < MIN_ROWS) return DEFAULT_ROWS
  return Math.floor(n)
}

function escapeAttr(value: string): string {
  return value.replaceAll('"', '&quot;')
}

function escapeText(value: string): string {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
}

function applyCase(value: string, upper: boolean): string {
  return upper ? value : value.toLowerCase()
}

interface InstructionRow {
  kind: 'instruction'
  rowKind: 'code' | 'unknown'
  address: number
  bytes: number[] | Uint8Array
  mnemonic: string
  operands: string
  comment: string | null
}
interface SpacerRow {
  kind: 'spacer'
  index: number
}
type Row = InstructionRow | SpacerRow

export function initDisassemblyPanel(bridge: DebugBridge): PanelHandle {
  const view = document.getElementById('disassembly-view')
  const gotoInput = document.getElementById('goto-address') as HTMLInputElement | null
  if (!view || !gotoInput) {
    throw new Error('disassembly panel DOM elements missing (#disassembly-view / #goto-address)')
  }

  let lastExplicitAddress: number | null = null
  let unsubscribeSettings: (() => void) | null = null

  gotoInput.addEventListener('keypress', (e) => {
    if (e.key !== 'Enter') return
    const addr = parseHexAddress((e.target as HTMLInputElement).value)
    if (addr === null) return
    lastExplicitAddress = addr
    update()
  })

  // Live settings: when the user flips Auto-scroll / Show opcodes / row
  // count / hex case in the modal we want the panel to redraw without a
  // reload. The settings reader hands back an unsubscribe fn, store it
  // for `dispose()` parity with other panels.
  const settings = getSettings()
  if (settings?.subscribe) {
    unsubscribeSettings = settings.subscribe((path) => {
      if (
        path === 'debugger.disassemblyLines' ||
        path === 'behavior.autoScroll' ||
        path === 'behavior.highlightPc' ||
        path === 'behavior.showOpcodes' ||
        path === 'behavior.hexUppercase'
      ) {
        update()
      }
    })
  }

  function update(): void {
    const emulator = bridge.getEmulator()
    if (!emulator) {
      renderEmpty()
      return
    }
    try {
      const cpuStates = emulator.getCpuStates()
      if (!cpuStates || cpuStates.length === 0) {
        renderEmpty()
        return
      }

      const focusedCpu = bridge.getFocusedCpu()
      const cpu = cpuStates[Math.min(focusedCpu, cpuStates.length - 1)]
      if (!cpu) {
        renderEmpty()
        return
      }
      const pc = cpu.pc >>> 0

      const targetRows = readRowCount()
      const { before, after } = splitWindowCounts(targetRows)

      let instructions: any[] = []
      const address = lastExplicitAddress
      lastExplicitAddress = null

      if (address !== null) {
        // Goto: jump to the typed address and disassemble forward.
        // We deliberately bypass `disassembleWindow` here because the
        // user is exploring an arbitrary address, they want raw
        // bytes decoded as instructions, not `....` for uncovered
        // regions.
        const count = before + after
        instructions = emulator.disassembleCpu
          ? emulator.disassembleCpu(focusedCpu, address >>> 0, count)
          : emulator.disassembleAt(address >>> 0, count)
      } else if (typeof emulator.disassembleWindow === 'function') {
        // Preferred path: bitmap-aware window. Returns rows tagged
        // `kind: 'code' | 'unknown'` so the renderer below can show
        // `....` for never-executed regions instead of fake `ori`s.
        instructions = emulator.disassembleWindow(focusedCpu, pc, before, after) || []
      } else if (focusedCpu === 0) {
        instructions = emulator.disassembleAroundPc(pc, before, after) || []
      } else if (emulator.disassembleCpu) {
        instructions = emulator.disassembleCpu(focusedCpu, pc, before + after)
      } else {
        instructions = emulator.disassembleAt(pc, before + after)
      }

      // The gutter marks the breakpoints a click on it toggles: this CPU's.
      const shown = breakpointsOnCpu(emulator.listBreakpoints() ?? [], focusedCpu)
      const enabledAddrs = enabledBreakpointAddresses(shown)
      const allAddrs = new Set<number>(shown.map((bp) => bp.breakpoint.address))

      const addrPad = focusedCpu === 0 ? 6 : 4
      const pcMask = focusedCpu === 0 ? 0x00ffffff : 0xffff
      const highlightPc = readSetting<boolean>('behavior.highlightPc', true)
      const autoScroll = readSetting<boolean>('behavior.autoScroll', true)
      const showOpcodes = readSetting<boolean>('behavior.showOpcodes', true)
      const hexUpper = readSetting<boolean>('behavior.hexUppercase', true)

      const instructionRows: InstructionRow[] = instructions.map((inst: any) => ({
        kind: 'instruction',
        rowKind: inst.kind === 'unknown' ? 'unknown' : 'code',
        address: inst.address >>> 0,
        bytes: inst.bytes ?? [],
        mnemonic: inst.mnemonic ?? '',
        operands: inst.operands ?? '',
        comment: inst.comment ?? null,
      }))

      const rows: Row[] = padDisasmRows<Row>(
        instructionRows,
        targetRows,
        (index): SpacerRow => ({ kind: 'spacer', index }),
      )

      view!.innerHTML = rows.map((row) => renderRow(row, {
        pc,
        pcMask,
        addrPad,
        allAddrs,
        enabledAddrs,
        highlightPc,
        showOpcodes,
        hexUpper,
      })).join('')

      if (autoScroll) {
        const currentLine = view!.querySelector('.current')
        if (currentLine && typeof (currentLine as HTMLElement).scrollIntoView === 'function') {
          (currentLine as HTMLElement).scrollIntoView({ block: 'center', behavior: 'smooth' })
        }
      }

      // Only `code` rows are clickable: `unknown` rows describe never-executed
      // regions and have no real instruction to break on.
      view!.querySelectorAll<HTMLElement>('.disasm-line[data-address]:not(.unknown)').forEach((line) => {
        line.addEventListener('click', () => {
          const addrAttr = line.dataset.address
          if (!addrAttr) return
          const addr = parseInt(addrAttr, 16)
          if (Number.isNaN(addr)) return
          toggleExecBreakpoint(bridge, addr, focusedCpu)
        })
      })
    } catch (err) {
      console.error('Failed to update disassembly:', err)
    }
  }

  function renderEmpty(): void {
    const targetRows = readRowCount()
    const rows: Row[] = padDisasmRows<Row>(
      [],
      targetRows,
      (index): SpacerRow => ({ kind: 'spacer', index }),
    )
    view!.innerHTML = rows
      .map((row) => renderRow(row as SpacerRow, null))
      .join('')
  }

  return {
    update,
    dispose() {
      if (unsubscribeSettings) {
        unsubscribeSettings()
        unsubscribeSettings = null
      }
    },
  }
}

/** A breakpoint as `listBreakpoints()` returns it. */
export interface ListedBreakpoint extends BreakpointLike {
  id: number
  breakpoint: { address: number; bp_type?: unknown; cpu_id?: number | null }
}

/**
 * The address breakpoints of one CPU: the ones its disassembly marks and
 * its gutter toggles. The 68000 and the Z80 (or the 65816 and the SPC700)
 * share numeric addresses, and the cores fire a breakpoint only on the
 * CPU it names. Event breakpoints have no address and are left out. A
 * breakpoint listed with no CPU counts as the primary CPU's:
 * `addBreakpoint` stores CPU 0 when it is given none.
 */
export function breakpointsOnCpu<T extends ListedBreakpoint>(list: readonly T[], cpu: number): T[] {
  return list.filter(
    (bp) => parseEventTag(bp.breakpoint.bp_type) === null && (bp.breakpoint.cpu_id ?? 0) === cpu,
  )
}

/**
 * Toggle the execute breakpoint of CPU `cpu` at `address`, as a click on
 * a code row of that CPU's disassembly does. A breakpoint of that CPU
 * already at the address has only its exec bit flipped, so a read/write
 * watchpoint there keeps its other flags; clearing the last flag removes
 * it. Otherwise an execute breakpoint is added for that CPU. Another
 * CPU's breakpoint at the same address is left alone.
 *
 * The change then follows the same path as one made in the breakpoints
 * panel: the breakpoints list, the watchpoints list and the gutter are
 * redrawn, and the whole list is saved under the ROM's hash, so the next
 * load of the ROM restores it.
 */
export function toggleExecBreakpoint(bridge: DebugBridge, address: number, cpu: number): void {
  const emulator = bridge.getEmulator()
  if (!emulator) return
  try {
    const existing = breakpointsOnCpu<ListedBreakpoint>(emulator.listBreakpoints() ?? [], cpu)
      .find((bp) => bp.breakpoint.address === address)

    if (existing) {
      const flags = toggleFlag(parseAccessFlags(existing.breakpoint.bp_type as RawBpType), 'exec')
      if (isEmptyAccess(flags)) {
        emulator.removeBreakpoint(existing.id)
      } else {
        emulator.setBreakpointAccess(existing.id, flags.exec, flags.read, flags.write)
      }
    } else {
      emulator.addBreakpoint(address, 'execute', cpu)
    }

    bridge.refreshPanel('breakpoints')
    bridge.refreshPanel('watchpoints')
    bridge.refreshPanel('disassembly')
    void saveBreakpoints(bridge)
  } catch (err) {
    console.error('Failed to toggle breakpoint:', err)
  }
}

interface RenderContext {
  pc: number
  pcMask: number
  addrPad: number
  allAddrs: Set<number>
  enabledAddrs: Set<number>
  highlightPc: boolean
  showOpcodes: boolean
  hexUpper: boolean
}

function renderRow(row: Row, ctx: RenderContext | null): string {
  if (row.kind === 'spacer') {
    return `<div class="disasm-line spacer" aria-hidden="true">
      <span class="disasm-bp-gutter"></span>
      <span class="disasm-address"></span>
      <span class="disasm-bytes"></span>
      <span class="disasm-instruction"></span>
      <span class="disasm-operands"></span>
    </div>`
  }
  if (!ctx) return ''

  const addr = row.address >>> 0
  const isUnknown = row.rowKind === 'unknown'
  const isCurrent =
    !isUnknown && ctx.highlightPc && (addr === ctx.pc || addr === (ctx.pc & ctx.pcMask))
  const hasBreakpoint = !isUnknown && ctx.allAddrs.has(addr)
  const bpEnabled = ctx.enabledAddrs.has(addr)

  let className = 'disasm-line'
  if (isUnknown) className += ' unknown'
  if (isCurrent) className += ' current'
  if (hasBreakpoint) {
    className += ' breakpoint'
    if (!bpEnabled) className += ' bp-disabled'
  }

  const addrText = applyCase(hex(addr, ctx.addrPad), ctx.hexUpper)
  let bytesText: string
  if (isUnknown) {
    bytesText = '....'
  } else {
    bytesText = ctx.showOpcodes ? applyCase(hexBytes(row.bytes), ctx.hexUpper) : ''
  }
  const mnemonicText = isUnknown ? '' : escapeText(row.mnemonic)
  const operandsText = isUnknown ? '' : escapeText(row.operands)
  const commentText = !isUnknown && row.comment ? escapeText(row.comment) : ''

  let gutter = ''
  if (hasBreakpoint) gutter = bpEnabled ? '●' : '○'

  return `<div class="${className}" data-address="${escapeAttr(hex(addr, ctx.addrPad))}">
    <span class="disasm-bp-gutter">${gutter}</span>
    <span class="disasm-address">${addrText}:</span>
    <span class="disasm-bytes">${bytesText}</span>
    <span class="disasm-instruction">${mnemonicText}</span>
    <span class="disasm-operands">${operandsText}</span>
    ${commentText ? `<span class="disasm-comment">; ${commentText}</span>` : ''}
  </div>`
}
