import { describe, expect, it } from 'vitest'
import { resolveDebugProfile } from '../src/debug-profile-core'
import { traceCpuSpecForKind, traceCpuSpecForSlot, traceEntrySize } from '../src/trace-spec-core'

describe('trace-spec-core > traceCpuSpecForKind', () => {
  it('names each kind of CPU for the tracing UI, with the id it is given', () => {
    const labels = (['m68k', 'z80', 'w65816', 'spc700', 'superfx', 'hg51b'] as const).map(
      (kind, cpuId) => traceCpuSpecForKind(kind, cpuId),
    )
    expect(labels).toEqual([
      { cpuId: 0, uiLabel: '68K', fileLabel: '68k' },
      { cpuId: 1, uiLabel: 'Z80', fileLabel: 'z80' },
      { cpuId: 2, uiLabel: '65816', fileLabel: '65816' },
      { cpuId: 3, uiLabel: 'SPC700', fileLabel: 'spc700' },
      { cpuId: 4, uiLabel: 'GSU', fileLabel: 'gsu' },
      { cpuId: 5, uiLabel: 'Cx4', fileLabel: 'cx4' },
    ])
  })

  it('gives each call a spec of its own', () => {
    const z80 = traceCpuSpecForKind('z80', 1)
    z80.uiLabel = 'changed'
    expect(traceCpuSpecForKind('z80', 1).uiLabel).toBe('Z80')
  })
})

describe('trace-spec-core > traceCpuSpecForSlot', () => {
  it('takes the kind and the id of the CPU at the slot', () => {
    const genesis = resolveDebugProfile('genesis')
    expect(traceCpuSpecForSlot(genesis, 0)).toEqual({ cpuId: 0, uiLabel: '68K', fileLabel: '68k' })
    expect(traceCpuSpecForSlot(genesis, 1)).toEqual({ cpuId: 1, uiLabel: 'Z80', fileLabel: 'z80' })
    const snes = resolveDebugProfile('snes')
    expect(traceCpuSpecForSlot(snes, 1)).toEqual({ cpuId: 1, uiLabel: 'SPC700', fileLabel: 'spc700' })
  })

  it('names the cartridge CPU at slot 2 by the cartridge profile', () => {
    expect(traceCpuSpecForSlot(resolveDebugProfile('snes-superfx'), 2)).toEqual({
      cpuId: 2,
      uiLabel: 'GSU',
      fileLabel: 'gsu',
    })
    expect(traceCpuSpecForSlot(resolveDebugProfile('snes-cx4'), 2).uiLabel).toBe('Cx4')
    expect(traceCpuSpecForSlot(resolveDebugProfile('snes-sa1'), 2)).toEqual({
      cpuId: 2,
      uiLabel: '65816',
      fileLabel: '65816',
    })
  })

  it('takes the primary CPU for a slot the profile lacks', () => {
    const snes = resolveDebugProfile('snes')
    expect(traceCpuSpecForSlot(snes, 2)).toEqual({ cpuId: 0, uiLabel: '65816', fileLabel: '65816' })
  })
})

describe('trace-spec-core > traceEntrySize', () => {
  const z80 = traceCpuSpecForKind('z80', 1)

  it('asks the core for the CPU of the spec, with the core as `this`', () => {
    const emulator = {
      sizes: [84, 38],
      getCpuTraceEntrySize(this: { sizes: number[] }, cpuId: number) {
        return this.sizes[cpuId]
      },
    }
    expect(traceEntrySize(emulator, z80)).toBe(38)
    expect(traceEntrySize(emulator, traceCpuSpecForKind('m68k', 0))).toBe(84)
  })

  it('reads 0 with no emulator or no such method', () => {
    expect(traceEntrySize(null, z80)).toBe(0)
    expect(traceEntrySize(undefined, z80)).toBe(0)
    expect(traceEntrySize(42, z80)).toBe(0)
    expect(traceEntrySize({}, z80)).toBe(0)
    expect(traceEntrySize({ getCpuTraceEntrySize: 38 }, z80)).toBe(0)
  })

  it('reads 0 for a size that is not a positive whole number', () => {
    for (const size of [0, -4, 2.5, Number.NaN, '38', undefined]) {
      expect(traceEntrySize({ getCpuTraceEntrySize: () => size }, z80)).toBe(0)
    }
  })
})
