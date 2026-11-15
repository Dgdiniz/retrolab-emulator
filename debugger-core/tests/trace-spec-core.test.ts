import { describe, expect, it } from 'vitest'
import { traceCpuSpecForKind, traceEntrySize } from '../src/trace-spec-core'

describe('trace-spec-core > traceCpuSpecForKind', () => {
  it('m68k -> 68000 generic trace bindings (sg1 cpu0)', () => {
    const s = traceCpuSpecForKind('m68k')
    expect(s.setEnabled).toBe('setTracingEnabled')
    expect(s.needsFlush).toBe('needsTraceFlush')
    expect(s.getFlushData).toBe('getTraceFlushData')
    expect(s.clearFlushed).toBe('clearTraceFlushed')
    expect(s.convertChunkToText).toBe('convertTraceChunkToText')
    expect(s.getEntrySize).toBe('getTraceEntrySize')
    expect(s.uiLabel).toBe('68K')
    expect(s.fileLabel).toBe('68k')
  })

  it('z80 -> Z80 bindings (sg1 cpu1)', () => {
    const s = traceCpuSpecForKind('z80')
    expect(s.setEnabled).toBe('setZ80TracingEnabled')
    expect(s.needsFlush).toBe('needsZ80TraceFlush')
    expect(s.getFlushData).toBe('getZ80TraceFlushData')
    expect(s.clearFlushed).toBe('clearZ80TraceFlushed')
    expect(s.convertChunkToText).toBe('convertZ80TraceChunkToText')
    expect(s.getEntrySize).toBe('getZ80TraceEntrySize')
    expect(s.uiLabel).toBe('Z80')
    expect(s.fileLabel).toBe('z80')
  })

  it('w65816 -> primary bindings reused (snes1 cpu0, same names as sg1 68K)', () => {
    const s = traceCpuSpecForKind('w65816')
    expect(s.setEnabled).toBe('setTracingEnabled')
    expect(s.needsFlush).toBe('needsTraceFlush')
    expect(s.getFlushData).toBe('getTraceFlushData')
    expect(s.clearFlushed).toBe('clearTraceFlushed')
    expect(s.convertChunkToText).toBe('convertTraceChunkToText')
    expect(s.getEntrySize).toBe('getTraceEntrySize')
    expect(s.uiLabel).toBe('65816')
    expect(s.fileLabel).toBe('65816')
  })

  it('spc700 -> SPC700 bindings (snes1 cpu1, shared needsTraceFlush)', () => {
    const s = traceCpuSpecForKind('spc700')
    expect(s.setEnabled).toBe('setSpc700TracingEnabled')
    expect(s.needsFlush).toBe('needsTraceFlush')
    expect(s.getFlushData).toBe('getSpc700TraceFlushData')
    expect(s.clearFlushed).toBe('clearSpc700TraceFlushed')
    expect(s.convertChunkToText).toBe('convertSpc700TraceChunkToText')
    expect(s.getEntrySize).toBe('getSpc700TraceEntrySize')
    expect(s.uiLabel).toBe('SPC700')
    expect(s.fileLabel).toBe('spc700')
  })

  it('superfx -> Super FX GSU bindings (snes1 cpu2, shared needsTraceFlush)', () => {
    const s = traceCpuSpecForKind('superfx')
    expect(s.setEnabled).toBe('setSuperfxTracingEnabled')
    expect(s.needsFlush).toBe('needsTraceFlush')
    expect(s.getFlushData).toBe('getSuperfxTraceFlushData')
    expect(s.clearFlushed).toBe('clearSuperfxTraceFlushed')
    expect(s.convertChunkToText).toBe('convertSuperfxTraceChunkToText')
    expect(s.getEntrySize).toBe('getSuperfxTraceEntrySize')
    expect(s.uiLabel).toBe('GSU')
    expect(s.fileLabel).toBe('gsu')
  })

  it('hg51b -> Cx4 HG51B169 bindings (snes1 cpu2, shared needsTraceFlush)', () => {
    const s = traceCpuSpecForKind('hg51b')
    expect(s.setEnabled).toBe('setHg51bTracingEnabled')
    expect(s.needsFlush).toBe('needsTraceFlush')
    expect(s.getFlushData).toBe('getHg51bTraceFlushData')
    expect(s.clearFlushed).toBe('clearHg51bTraceFlushed')
    expect(s.convertChunkToText).toBe('convertHg51bTraceChunkToText')
    expect(s.getEntrySize).toBe('getHg51bTraceEntrySize')
    expect(s.uiLabel).toBe('Cx4')
    expect(s.fileLabel).toBe('cx4')
  })
})

describe('trace-spec-core > traceEntrySize', () => {
  const z80 = traceCpuSpecForKind('z80')

  it('asks the core through the spec method, with the core as `this`', () => {
    const emulator = {
      size: 38,
      getZ80TraceEntrySize(this: { size: number }) {
        return this.size
      },
      getTraceEntrySize: () => 84,
    }
    expect(traceEntrySize(emulator, z80)).toBe(38)
    expect(traceEntrySize(emulator, traceCpuSpecForKind('m68k'))).toBe(84)
  })

  it('reads 0 with no emulator or no such method', () => {
    expect(traceEntrySize(null, z80)).toBe(0)
    expect(traceEntrySize(undefined, z80)).toBe(0)
    expect(traceEntrySize(42, z80)).toBe(0)
    expect(traceEntrySize({}, z80)).toBe(0)
    expect(traceEntrySize({ getZ80TraceEntrySize: 38 }, z80)).toBe(0)
  })

  it('reads 0 for a size that is not a positive whole number', () => {
    for (const size of [0, -4, 2.5, Number.NaN, '38', undefined]) {
      expect(traceEntrySize({ getZ80TraceEntrySize: () => size }, z80)).toBe(0)
    }
  })
})
