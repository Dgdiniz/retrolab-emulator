import { describe, it, expect } from 'vitest'
import { traceCpuSpecForKind } from '@debugger-core/trace-spec-core'
import { traceCpuSpec, cpuSelectorLabels } from '../src/trace-ops'

describe('traceCpuSpec - sg1', () => {
  it('cpu0 -> 68000 generic trace bindings', () => {
    const s = traceCpuSpec('sg1', 0)
    expect(s.setEnabled).toBe('setTracingEnabled')
    expect(s.getFlushData).toBe('getTraceFlushData')
    expect(s.convertChunkToText).toBe('convertTraceChunkToText')
    expect(s.entrySize).toBe(84)
    expect(s.uiLabel).toBe('68K')
    expect(s.fileLabel).toBe('68k')
  })

  it('cpu1 -> Z80 bindings', () => {
    const s = traceCpuSpec('sg1', 1)
    expect(s.setEnabled).toBe('setZ80TracingEnabled')
    expect(s.needsFlush).toBe('needsZ80TraceFlush')
    expect(s.getFlushData).toBe('getZ80TraceFlushData')
    expect(s.clearFlushed).toBe('clearZ80TraceFlushed')
    expect(s.convertChunkToText).toBe('convertZ80TraceChunkToText')
    expect(s.entrySize).toBe(38)
    expect(s.uiLabel).toBe('Z80')
    expect(s.fileLabel).toBe('z80')
  })
})

describe('traceCpuSpec - snes1', () => {
  it('cpu0 -> 65C816 reuses the generic primary bindings (same names as sg1 68K)', () => {
    const s = traceCpuSpec('snes1', 0)
    expect(s.setEnabled).toBe('setTracingEnabled')
    expect(s.needsFlush).toBe('needsTraceFlush')
    expect(s.getFlushData).toBe('getTraceFlushData')
    expect(s.clearFlushed).toBe('clearTraceFlushed')
    expect(s.convertChunkToText).toBe('convertTraceChunkToText')
    expect(s.entrySize).toBe(44)
    expect(s.uiLabel).toBe('65816')
    expect(s.fileLabel).toBe('65816')
  })

  it('cpu1 -> SPC700 bindings (shared needsTraceFlush, dedicated flush/convert)', () => {
    const s = traceCpuSpec('snes1', 1)
    expect(s.setEnabled).toBe('setSpc700TracingEnabled')
    expect(s.needsFlush).toBe('needsTraceFlush')
    expect(s.getFlushData).toBe('getSpc700TraceFlushData')
    expect(s.clearFlushed).toBe('clearSpc700TraceFlushed')
    expect(s.convertChunkToText).toBe('convertSpc700TraceChunkToText')
    expect(s.entrySize).toBe(24)
    expect(s.uiLabel).toBe('SPC700')
    expect(s.fileLabel).toBe('spc700')
  })
})

describe('traceCpuSpec - one table for both debugger UIs', () => {
  it('takes every selector slot spec from trace-spec-core, by CPU kind', () => {
    expect(traceCpuSpec('sg1', 0)).toBe(traceCpuSpecForKind('m68k'))
    expect(traceCpuSpec('sg1', 1)).toBe(traceCpuSpecForKind('z80'))
    expect(traceCpuSpec('snes1', 0)).toBe(traceCpuSpecForKind('w65816'))
    expect(traceCpuSpec('snes1', 1)).toBe(traceCpuSpecForKind('spc700'))
  })

  it('gives the SNES CPUs entry sizes of their own, not the Genesis ones', () => {
    expect(traceCpuSpec('snes1', 0).entrySize).not.toBe(traceCpuSpec('sg1', 0).entrySize)
    expect(traceCpuSpec('snes1', 1).entrySize).not.toBe(traceCpuSpec('sg1', 1).entrySize)
  })
})

describe('traceCpuSpec - cpu id normalisation', () => {
  it('treats any non-1 cpu id as the primary CPU', () => {
    expect(traceCpuSpec('sg1', 0).uiLabel).toBe('68K')
    expect(traceCpuSpec('sg1', 2).uiLabel).toBe('68K')
    expect(traceCpuSpec('snes1', 99).uiLabel).toBe('65816')
  })
})

describe('cpuSelectorLabels', () => {
  it('returns the two CPU labels in id order', () => {
    expect(cpuSelectorLabels('sg1')).toEqual(['68000', 'Z80'])
    expect(cpuSelectorLabels('snes1')).toEqual(['65816', 'SPC700'])
  })
})
