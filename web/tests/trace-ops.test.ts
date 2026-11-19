import { describe, it, expect } from 'vitest'
import { traceCpuSpecForKind } from '@debugger-core/trace-spec-core'
import { traceCpuSpec, cpuSelectorLabels } from '../src/trace-ops'

describe('traceCpuSpec - sg1', () => {
  it('cpu0 -> the 68000, CPU 0', () => {
    expect(traceCpuSpec('sg1', 0)).toEqual({ cpuId: 0, uiLabel: '68K', fileLabel: '68k' })
  })

  it('cpu1 -> the Z80, CPU 1', () => {
    expect(traceCpuSpec('sg1', 1)).toEqual({ cpuId: 1, uiLabel: 'Z80', fileLabel: 'z80' })
  })
})

describe('traceCpuSpec - snes1', () => {
  it('cpu0 -> the 65C816, CPU 0', () => {
    expect(traceCpuSpec('snes1', 0)).toEqual({ cpuId: 0, uiLabel: '65816', fileLabel: '65816' })
  })

  it('cpu1 -> the SPC700, CPU 1', () => {
    expect(traceCpuSpec('snes1', 1)).toEqual({ cpuId: 1, uiLabel: 'SPC700', fileLabel: 'spc700' })
  })
})

describe('traceCpuSpec - one table for both debugger UIs', () => {
  it('takes every selector slot spec from trace-spec-core, by CPU kind', () => {
    expect(traceCpuSpec('sg1', 0)).toEqual(traceCpuSpecForKind('m68k', 0))
    expect(traceCpuSpec('sg1', 1)).toEqual(traceCpuSpecForKind('z80', 1))
    expect(traceCpuSpec('snes1', 0)).toEqual(traceCpuSpecForKind('w65816', 0))
    expect(traceCpuSpec('snes1', 1)).toEqual(traceCpuSpecForKind('spc700', 1))
  })
})

describe('traceCpuSpec - cpu id normalisation', () => {
  it('treats any non-1 cpu id as the primary CPU, CPU 0', () => {
    expect(traceCpuSpec('sg1', 0).uiLabel).toBe('68K')
    expect(traceCpuSpec('sg1', 2)).toEqual(traceCpuSpec('sg1', 0))
    expect(traceCpuSpec('snes1', 99)).toEqual(traceCpuSpec('snes1', 0))
  })
})

describe('cpuSelectorLabels', () => {
  it('returns the two CPU labels in id order', () => {
    expect(cpuSelectorLabels('sg1')).toEqual(['68000', 'Z80'])
    expect(cpuSelectorLabels('snes1')).toEqual(['65816', 'SPC700'])
  })
})
