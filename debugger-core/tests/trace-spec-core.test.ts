import { describe, expect, it } from 'vitest'
import { traceCpuSpecForKind } from '../src/trace-spec-core'

describe('trace-spec-core > traceCpuSpecForKind', () => {
  it('m68k -> 68000 generic trace bindings (sg1 cpu0)', () => {
    const s = traceCpuSpecForKind('m68k')
    expect(s.setEnabled).toBe('setTracingEnabled')
    expect(s.needsFlush).toBe('needsTraceFlush')
    expect(s.getFlushData).toBe('getTraceFlushData')
    expect(s.clearFlushed).toBe('clearTraceFlushed')
    expect(s.convertChunkToText).toBe('convertTraceChunkToText')
    expect(s.entrySize).toBe(84)
    expect(s.statsEntrySize).toBe(74)
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
    expect(s.entrySize).toBe(38)
    expect(s.statsEntrySize).toBe(34)
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
    expect(s.entrySize).toBe(44)
    expect(s.statsEntrySize).toBe(44)
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
    expect(s.entrySize).toBe(24)
    expect(s.statsEntrySize).toBe(24)
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
    // Must equal the Rust TRACE_ENTRY_SIZE_GSU.
    expect(s.entrySize).toBe(52)
    expect(s.statsEntrySize).toBe(52)
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
    // Must equal the Rust TRACE_ENTRY_SIZE_CX4.
    expect(s.entrySize).toBe(93)
    expect(s.statsEntrySize).toBe(93)
    expect(s.uiLabel).toBe('Cx4')
    expect(s.fileLabel).toBe('cx4')
  })
})
