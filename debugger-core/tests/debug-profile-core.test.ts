import { describe, expect, it } from 'vitest'
import {
  DEFAULT_DEBUG_PROFILE_ID,
  INFO_TAB_LABELS,
  SNES_FAMILY_PROFILE_IDS,
  cpuForSlot,
  cpuKindForSlot,
  cpuLabelForSlot,
  cpuSteppableForSlot,
  infoTabLabel,
  isSnesFamilyProfile,
  resolveDebugProfile,
  slotForCpuKind,
  type DebugInfoTabId,
  type DebugProfile,
} from '../src/debug-profile-core'

describe('debug-profile-core > resolveDebugProfile', () => {
  it('resolves the Genesis profile (68000 + Z80, tabs incl. vdp/audio/cartridge)', () => {
    const p = resolveDebugProfile('genesis')
    expect(p.id).toBe('genesis')
    expect(p.cpus.map((c) => [c.id, c.label, c.kind])).toEqual([
      [0, '68000', 'm68k'],
      [1, 'Z80', 'z80'],
    ])
    expect(p.infoTabs).toEqual([
      'breakpoints', 'watchpoints', 'history', 'callstack', 'vdp', 'audio', 'input', 'cartridge',
    ])
  })

  it('resolves the SNES profile (65816 + SPC700, with ppu/apu/snesCartridge viewers)', () => {
    const p = resolveDebugProfile('snes')
    expect(p.cpus.map((c) => [c.id, c.label, c.kind])).toEqual([
      [0, '65816', 'w65816'],
      [1, 'SPC700', 'spc700'],
    ])
    expect(p.infoTabs).toEqual([
      'breakpoints', 'watchpoints', 'history', 'callstack', 'input', 'ppu', 'apu', 'snesCartridge',
    ])
    // Uses the SNES-specific viewer ids, not the Genesis ones.
    expect(p.infoTabs).not.toContain('vdp')
    expect(p.infoTabs).not.toContain('audio')
    expect(p.infoTabs).not.toContain('cartridge')
  })

  it('resolves the SNES SA-1 profile (adds the SA-1 second 65816 as slot 2)', () => {
    const p = resolveDebugProfile('snes-sa1')
    expect(p.id).toBe('snes-sa1')
    expect(p.cpus.map((c) => [c.id, c.label, c.kind])).toEqual([
      [0, '65816', 'w65816'],
      [1, 'SPC700', 'spc700'],
      [2, 'SA-1', 'w65816'],
    ])
    // Same viewer set as the plain SNES profile.
    expect(p.infoTabs).toEqual(resolveDebugProfile('snes').infoTabs)
  })

  it('resolves the SNES Super FX profile (adds the GSU as a steppable slot 2)', () => {
    const p = resolveDebugProfile('snes-superfx')
    expect(p.id).toBe('snes-superfx')
    expect(p.cpus.map((c) => [c.id, c.label, c.kind])).toEqual([
      [0, '65816', 'w65816'],
      [1, 'SPC700', 'spc700'],
      [2, 'GSU', 'superfx'],
    ])
    // Same viewer set as the plain SNES profile.
    expect(p.infoTabs).toEqual(resolveDebugProfile('snes').infoTabs)
  })

  it('resolves the SNES Cx4 profile (adds the HG51B169 as a steppable slot 2)', () => {
    const p = resolveDebugProfile('snes-cx4')
    expect(p.id).toBe('snes-cx4')
    expect(p.cpus.map((c) => [c.id, c.label, c.kind])).toEqual([
      [0, '65816', 'w65816'],
      [1, 'SPC700', 'spc700'],
      [2, 'Cx4', 'hg51b'],
    ])
    // Same viewer set as the plain SNES profile.
    expect(p.infoTabs).toEqual(resolveDebugProfile('snes').infoTabs)
  })

  it('falls back to Genesis for unknown / null / undefined / empty ids', () => {
    expect(resolveDebugProfile('nope').id).toBe(DEFAULT_DEBUG_PROFILE_ID)
    expect(resolveDebugProfile(null).id).toBe('genesis')
    expect(resolveDebugProfile(undefined).id).toBe('genesis')
    expect(resolveDebugProfile('').id).toBe('genesis')
  })

  it('DEFAULT_DEBUG_PROFILE_ID is genesis', () => {
    expect(DEFAULT_DEBUG_PROFILE_ID).toBe('genesis')
  })
})

describe('debug-profile-core > slot helpers', () => {
  const snes = resolveDebugProfile('snes')

  it('cpuForSlot returns the matching descriptor', () => {
    expect(cpuForSlot(snes, 1).label).toBe('SPC700')
  })

  it('cpuForSlot falls back to the primary CPU for an out-of-range slot', () => {
    expect(cpuForSlot(snes, 9).id).toBe(0)
    expect(cpuForSlot(snes, -1).label).toBe('65816')
  })

  it('cpuKindForSlot maps slot -> CpuKind', () => {
    expect(cpuKindForSlot(snes, 0)).toBe('w65816')
    expect(cpuKindForSlot(snes, 1)).toBe('spc700')
    expect(cpuKindForSlot(resolveDebugProfile('genesis'), 0)).toBe('m68k')
    expect(cpuKindForSlot(resolveDebugProfile('genesis'), 1)).toBe('z80')
  })

  it('slotForCpuKind is the inverse of cpuKindForSlot', () => {
    expect(slotForCpuKind(snes, 'w65816')).toBe(0)
    expect(slotForCpuKind(snes, 'spc700')).toBe(1)
    const genesis = resolveDebugProfile('genesis')
    expect(slotForCpuKind(genesis, 'm68k')).toBe(0)
    expect(slotForCpuKind(genesis, 'z80')).toBe(1)
  })

  it('slotForCpuKind returns null for a kind absent from the profile', () => {
    // The SNES profile has no Z80; the Genesis profile has no SPC700.
    expect(slotForCpuKind(snes, 'z80')).toBeNull()
    expect(slotForCpuKind(resolveDebugProfile('genesis'), 'spc700')).toBeNull()
  })

  it('cpuLabelForSlot maps slot -> label', () => {
    expect(cpuLabelForSlot(snes, 0)).toBe('65816')
    expect(cpuLabelForSlot(snes, 1)).toBe('SPC700')
  })

  it('cpuSteppableForSlot: SNES 65816 steppable, SPC700 not', () => {
    expect(cpuSteppableForSlot(snes, 0)).toBe(true)
    expect(cpuSteppableForSlot(snes, 1)).toBe(false)
  })

  it('cpuSteppableForSlot defaults to true when the descriptor omits the flag', () => {
    const genesis = resolveDebugProfile('genesis')
    expect(cpuSteppableForSlot(genesis, 0)).toBe(true)
    expect(cpuSteppableForSlot(genesis, 1)).toBe(true)
    // Out-of-range slot falls back to the (steppable) primary CPU.
    expect(cpuSteppableForSlot(snes, 9)).toBe(true)
  })

  it('SNES SA-1 slot 2 is a steppable w65816 (matrix: 0 true / 1 false / 2 true)', () => {
    const sa1 = resolveDebugProfile('snes-sa1')
    expect(cpuSteppableForSlot(sa1, 0)).toBe(true)
    expect(cpuSteppableForSlot(sa1, 1)).toBe(false)
    expect(cpuSteppableForSlot(sa1, 2)).toBe(true)
    expect(cpuKindForSlot(sa1, 2)).toBe('w65816')
    expect(cpuLabelForSlot(sa1, 2)).toBe('SA-1')
    // Both 65816 slots share the kind; the inverse lookup returns the first.
    expect(slotForCpuKind(sa1, 'w65816')).toBe(0)
    expect(slotForCpuKind(sa1, 'spc700')).toBe(1)
  })

  it('SNES Super FX slot 2 is a steppable GSU (matrix: 0 true / 1 false / 2 true)', () => {
    const fx = resolveDebugProfile('snes-superfx')
    // Unlike the SPC700 (slot 1), the GSU *is* single-steppable.
    expect(cpuSteppableForSlot(fx, 0)).toBe(true)
    expect(cpuSteppableForSlot(fx, 1)).toBe(false)
    expect(cpuSteppableForSlot(fx, 2)).toBe(true)
    expect(cpuKindForSlot(fx, 2)).toBe('superfx')
    expect(cpuLabelForSlot(fx, 2)).toBe('GSU')
    // The GSU is the only `superfx` CPU, so the inverse lookup finds slot 2.
    expect(slotForCpuKind(fx, 'superfx')).toBe(2)
    expect(slotForCpuKind(fx, 'w65816')).toBe(0)
    expect(slotForCpuKind(fx, 'spc700')).toBe(1)
  })

  it('SNES Cx4 slot 2 is a steppable HG51B169 (matrix: 0 true / 1 false / 2 true)', () => {
    const cx4 = resolveDebugProfile('snes-cx4')
    // Like the GSU (and unlike the SPC700 at slot 1), the Cx4 *is* steppable.
    expect(cpuSteppableForSlot(cx4, 0)).toBe(true)
    expect(cpuSteppableForSlot(cx4, 1)).toBe(false)
    expect(cpuSteppableForSlot(cx4, 2)).toBe(true)
    expect(cpuKindForSlot(cx4, 2)).toBe('hg51b')
    expect(cpuLabelForSlot(cx4, 2)).toBe('Cx4')
    // The Cx4 is the only `hg51b` CPU, so the inverse lookup finds slot 2.
    expect(slotForCpuKind(cx4, 'hg51b')).toBe(2)
    expect(slotForCpuKind(cx4, 'w65816')).toBe(0)
    expect(slotForCpuKind(cx4, 'spc700')).toBe(1)
  })
})

describe('debug-profile-core > SNES family', () => {
  it('SNES_FAMILY_PROFILE_IDS lists the base SNES roster and its supersets', () => {
    expect([...SNES_FAMILY_PROFILE_IDS]).toEqual([
      'snes',
      'snes-sa1',
      'snes-superfx',
      'snes-cx4',
    ])
  })

  it('isSnesFamilyProfile accepts snes + snes-sa1 + snes-superfx + snes-cx4, rejects everything else', () => {
    expect(isSnesFamilyProfile('snes')).toBe(true)
    expect(isSnesFamilyProfile('snes-sa1')).toBe(true)
    expect(isSnesFamilyProfile('snes-superfx')).toBe(true)
    expect(isSnesFamilyProfile('snes-cx4')).toBe(true)
    expect(isSnesFamilyProfile('genesis')).toBe(false)
    expect(isSnesFamilyProfile('nes')).toBe(false)
    expect(isSnesFamilyProfile('')).toBe(false)
    expect(isSnesFamilyProfile(null)).toBe(false)
    expect(isSnesFamilyProfile(undefined)).toBe(false)
  })
})

describe('debug-profile-core > info tab labels', () => {
  it('labels every tab id', () => {
    const ids: DebugInfoTabId[] = [
      'breakpoints', 'watchpoints', 'history', 'callstack', 'input', 'vdp', 'audio', 'cartridge',
      'ppu', 'apu', 'snesCartridge',
    ]
    expect(ids.map(infoTabLabel)).toEqual([
      'Breakpoints', 'Watchpoints', 'History', 'Call Stack', 'Input', 'VDP', 'Audio', 'Cartridge',
      // The SNES cartridge tab shares the 'Cartridge' label (distinct id).
      'PPU', 'APU', 'Cartridge',
    ])
  })

  it('INFO_TAB_LABELS covers exactly the tab id union', () => {
    expect(Object.keys(INFO_TAB_LABELS).sort()).toEqual([
      'apu', 'audio', 'breakpoints', 'callstack', 'cartridge',
      'history', 'input', 'ppu', 'snesCartridge', 'vdp', 'watchpoints',
    ])
  })
})

describe('debug-profile-core > multi-console seam', () => {
  it('adding a console = adding one profile; the helpers stay data-driven', () => {
    // Proves the NES debugger drops in as a single profile entry with NO
    // component edits: the pure helpers consume any DebugProfile shape
    // without per-console branching. A real NES profile would add a
    // `mos6502` CpuKind + its flag/register tables; the kind below is a
    // placeholder so this test needs no source change to exist.
    const nesLike: DebugProfile = {
      id: 'nes',
      cpus: [{ id: 0, label: '6502', kind: 'm68k' }],
      infoTabs: ['breakpoints', 'history', 'callstack', 'input'],
    }
    expect(cpuLabelForSlot(nesLike, 0)).toBe('6502')
    expect(cpuForSlot(nesLike, 7).id).toBe(0)
    expect(nesLike.infoTabs.map(infoTabLabel)).toEqual([
      'Breakpoints', 'History', 'Call Stack', 'Input',
    ])
  })
})
