import { describe, it, expect } from 'vitest'
import {
  ACCEPTED_ROM_EXTENSIONS,
  DEFAULT_CORE,
  SG1_EXTENSIONS,
  SNES1_EXTENSIONS,
  computeByteHash,
  coreDisplayName,
  deriveRomIdentity,
  hasAcceptedRomExtension,
  normalizeCoreId,
  resolveBootCore,
  resolveCoreFromExtension,
  resolveCoreFromQuery,
  systemString,
  wasmModulePath,
  type RomIdentitySource,
} from '../src/core-resolver'

describe('normalizeCoreId', () => {
  it('maps canonical ids', () => {
    expect(normalizeCoreId('sg1')).toBe('sg1')
    expect(normalizeCoreId('snes1')).toBe('snes1')
  })

  it('maps system-name aliases (case/space-insensitive)', () => {
    expect(normalizeCoreId('genesis')).toBe('sg1')
    expect(normalizeCoreId('MegaDrive')).toBe('sg1')
    expect(normalizeCoreId('  md ')).toBe('sg1')
    expect(normalizeCoreId('snes')).toBe('snes1')
    expect(normalizeCoreId('SFC')).toBe('snes1')
  })

  it('returns null for unknown / empty values', () => {
    expect(normalizeCoreId('nes')).toBeNull()
    expect(normalizeCoreId('')).toBeNull()
    expect(normalizeCoreId(null)).toBeNull()
    expect(normalizeCoreId(undefined)).toBeNull()
  })
})

describe('resolveCoreFromQuery', () => {
  it('reads the core param', () => {
    expect(resolveCoreFromQuery('?core=snes1')).toBe('snes1')
    expect(resolveCoreFromQuery('core=sg1')).toBe('sg1')
    expect(resolveCoreFromQuery('?foo=1&core=snes')).toBe('snes1')
  })

  it('returns null without a recognised core param', () => {
    expect(resolveCoreFromQuery('?other=1')).toBeNull()
    expect(resolveCoreFromQuery('?core=zzz')).toBeNull()
    expect(resolveCoreFromQuery('')).toBeNull()
    expect(resolveCoreFromQuery(null)).toBeNull()
  })
})

describe('resolveCoreFromExtension', () => {
  it('selects snes1 for SNES extensions', () => {
    for (const ext of SNES1_EXTENSIONS) {
      expect(resolveCoreFromExtension(`game${ext}`)).toBe('snes1')
      expect(resolveCoreFromExtension(`GAME${ext.toUpperCase()}`)).toBe('snes1')
    }
  })

  it('selects sg1 for Genesis extensions', () => {
    for (const ext of SG1_EXTENSIONS) {
      expect(resolveCoreFromExtension(`game${ext}`)).toBe('sg1')
    }
  })

  it('returns null for unknown / empty filenames', () => {
    expect(resolveCoreFromExtension('game.nes')).toBeNull()
    expect(resolveCoreFromExtension('noext')).toBeNull()
    expect(resolveCoreFromExtension('')).toBeNull()
    expect(resolveCoreFromExtension(null)).toBeNull()
  })
})

describe('resolveBootCore', () => {
  it('prefers the query param over the extension', () => {
    expect(resolveBootCore('?core=sg1', 'mario.smc')).toBe('sg1')
    expect(resolveBootCore('?core=snes1', 'sonic.md')).toBe('snes1')
  })

  it('falls back to the extension when no query param', () => {
    expect(resolveBootCore('', 'mario.smc')).toBe('snes1')
    expect(resolveBootCore(null, 'sonic.md')).toBe('sg1')
  })

  it('falls back to the default when nothing hints at a core', () => {
    expect(resolveBootCore('', undefined)).toBe(DEFAULT_CORE)
    expect(resolveBootCore('?x=1', 'unknown.rom')).toBe(DEFAULT_CORE)
  })
})

describe('hasAcceptedRomExtension', () => {
  it('accepts every known extension', () => {
    for (const ext of ACCEPTED_ROM_EXTENSIONS) {
      expect(hasAcceptedRomExtension(`rom${ext}`)).toBe(true)
    }
  })

  it('rejects unknown / empty', () => {
    expect(hasAcceptedRomExtension('rom.nes')).toBe(false)
    expect(hasAcceptedRomExtension('')).toBe(false)
    expect(hasAcceptedRomExtension(null)).toBe(false)
  })
})

describe('wasmModulePath / systemString / coreDisplayName', () => {
  it('maps each core to its module path', () => {
    expect(wasmModulePath('sg1')).toBe('pkg/sg1_wasm.js')
    expect(wasmModulePath('snes1')).toBe('pkg/snes1_wasm.js')
  })

  it('maps each core to its RetroEmulator system string', () => {
    expect(systemString('sg1')).toBe('genesis')
    expect(systemString('snes1')).toBe('snes')
  })

  it('maps each core to a display label', () => {
    expect(coreDisplayName('sg1')).toBe('Genesis')
    expect(coreDisplayName('snes1')).toBe('SNES')
  })
})

describe('computeByteHash', () => {
  it('is deterministic for the same bytes', () => {
    const a = new Uint8Array([1, 2, 3, 4, 5])
    const b = new Uint8Array([1, 2, 3, 4, 5])
    expect(computeByteHash(a)).toBe(computeByteHash(b))
  })

  it('differs for different content and for different lengths', () => {
    expect(computeByteHash(new Uint8Array([1, 2, 3]))).not.toBe(
      computeByteHash(new Uint8Array([1, 2, 4])),
    )
    // Length salt: a prefix must not collide with the longer ROM.
    expect(computeByteHash(new Uint8Array([1, 2, 3]))).not.toBe(
      computeByteHash(new Uint8Array([1, 2, 3, 0])),
    )
  })

  it('returns a 16-char lowercase hex string', () => {
    const h = computeByteHash(new Uint8Array([9, 9, 9]))
    expect(h).toMatch(/^[0-9a-f]{16}$/)
  })

  it('handles the empty ROM without throwing', () => {
    expect(computeByteHash(new Uint8Array([]))).toMatch(/^[0-9a-f]{16}$/)
  })
})

describe('deriveRomIdentity', () => {
  const data = new Uint8Array([10, 20, 30])

  it('uses the SG1 native cartridge hash + title when available', () => {
    const emu: RomIdentitySource = {
      getCartridgeHash: () => 'sg1hash',
      getCartridgeTitle: () => 'SONIC THE HEDGEHOG',
    }
    expect(deriveRomIdentity(emu, data, 'sonic.md', 'sg1')).toEqual({
      hash: 'sg1hash',
      title: 'SONIC THE HEDGEHOG',
    })
  })

  it('falls back to the filename when SG1 has no title/hash methods', () => {
    const emu: RomIdentitySource = {}
    expect(deriveRomIdentity(emu, data, 'mystery.bin', 'sg1')).toEqual({
      hash: null,
      title: 'mystery.bin',
    })
  })

  it('treats an SG1 null cartridge hash as not-persistable', () => {
    const emu: RomIdentitySource = {
      getCartridgeHash: () => null,
      getCartridgeTitle: () => '',
    }
    const id = deriveRomIdentity(emu, data, 'rom.md', 'sg1')
    expect(id.hash).toBeNull()
    expect(id.title).toBe('rom.md')
  })

  it('hashes bytes + reads getRomInfo().title for snes1', () => {
    const emu: RomIdentitySource = {
      getRomInfo: () => ({ title: 'SUPER MARIOKART' }),
      // snes1 must NOT consult these even if a stray method exists.
      getCartridgeHash: () => 'should-be-ignored',
    }
    const id = deriveRomIdentity(emu, data, 'smk.sfc', 'snes1')
    expect(id.hash).toBe(computeByteHash(data))
    expect(id.title).toBe('SUPER MARIOKART')
  })

  it('falls back to the filename for snes1 with blank/missing rom info', () => {
    expect(deriveRomIdentity({ getRomInfo: () => ({ title: '   ' }) }, data, 'a.smc', 'snes1').title).toBe('a.smc')
    expect(deriveRomIdentity({ getRomInfo: () => null }, data, 'b.smc', 'snes1').title).toBe('b.smc')
    expect(deriveRomIdentity({}, data, 'c.smc', 'snes1').title).toBe('c.smc')
  })
})
