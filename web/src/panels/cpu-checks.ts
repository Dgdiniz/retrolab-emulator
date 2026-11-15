/**
 * What the breakpoints and watchpoints panels need to offer each CPU only
 * the breakpoints it checks: the CPU's checks (`getBreakpointChecks`, read
 * through `@debugger-core/breakpoints-core`) and its name, to say why the
 * panel offers less.
 */

import { breakpointChecksOf, type BreakpointChecks } from '@debugger-core/breakpoints-core'
import type { Emulator } from '../debug-bridge'

/** One CPU's checks, and its name as the core reports it (`null` when it reports none). */
export interface CpuChecks {
  checks: BreakpointChecks
  name: string | null
}

/**
 * A reader of each CPU's checks and name, which asks the emulator once per
 * CPU: a panel makes one for each redraw, so a CPU's checks are read again
 * after a new game, whose cartridge may bring another CPU.
 */
export function cpuChecksReader(emulator: Emulator): (cpuId: number) => CpuChecks {
  const read = new Map<number, CpuChecks>()
  let cpus: unknown[] | null = null
  return (cpuId) => {
    const known = read.get(cpuId)
    if (known) return known
    if (cpus === null) {
      try {
        const listed = emulator?.getSystemInfo?.()?.cpus
        cpus = Array.isArray(listed) ? listed : []
      } catch {
        cpus = []
      }
    }
    const name = (cpus[cpuId] as { name?: unknown } | undefined)?.name
    const found: CpuChecks = {
      checks: breakpointChecksOf(emulator, cpuId),
      name: typeof name === 'string' && name !== '' ? name : null,
    }
    read.set(cpuId, found)
    return found
  }
}
