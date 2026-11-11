import { describe, expect, it } from 'vitest'
import {
  GENESIS_MANIFEST_SCHEMA,
  GENESIS_MANIFEST_VERSION,
  MAX_SRAM_BYTES,
  PRESET_OPTIONS,
  REGION_OPTIONS,
  SRAM_SIZE_OPTIONS,
  applyFormToManifest,
  coprocessorLabel,
  formatSramSize,
  manifestDiffersFromDetected,
  manifestFormFromJson,
  normalizePreset,
  parseDetectedManifest,
  parseManifestJson,
  stringifyManifest,
  validateManifestForm,
  type GenesisManifest,
} from '../src/genesis-manifest-core'

/** A minimal valid plain manifest with 8 KiB SRAM (Sonic 3-shaped). */
function plainManifest(): GenesisManifest {
  return {
    $schema: GENESIS_MANIFEST_SCHEMA,
    core: 'sg1',
    version: GENESIS_MANIFEST_VERSION,
    rom: { sha256: 'abc', size: 0x20_0000, title: 'SONIC THE HEDGEHOG', region: 'USA' },
    mapping: { preset: 'plain', sram_size: 8192, slots: [] },
    chips: [],
    metadata: null,
  }
}

/** An SVP manifest (Virtua Racing-shaped). */
function svpManifest(): GenesisManifest {
  return {
    $schema: GENESIS_MANIFEST_SCHEMA,
    core: 'sg1',
    version: GENESIS_MANIFEST_VERSION,
    rom: { sha256: 'def', size: 0x20_0000, title: 'VIRTUA RACING', region: 'USA' },
    mapping: { preset: 'svp', sram_size: 0, slots: [] },
    chips: [
      {
        id: 0,
        kind: 'svp',
        mapping: {
          addr_range: ['0xA15000', '0xA1500B'],
          target: { kind: 'ChipMmio', chip_id: 0, base: 0x100000 },
        },
      },
    ],
    metadata: null,
  }
}

describe('genesis-manifest-core > constants', () => {
  it('exposes the schema/version/limit constants', () => {
    expect(GENESIS_MANIFEST_SCHEMA).toBe('cartridge-manifest-v1')
    expect(GENESIS_MANIFEST_VERSION).toBe('1.0')
    expect(MAX_SRAM_BYTES).toBe(1 << 20)
  })

  it('ships option lists for the form selects', () => {
    expect(PRESET_OPTIONS.map((o) => o.value)).toEqual([
      'plain',
      'sega',
      'ssf2',
      'codemasters',
      'eeprom',
      'jcart',
      'svp',
      'custom',
    ])
    expect(SRAM_SIZE_OPTIONS[0]).toEqual({ value: 0, label: 'None' })
    expect(SRAM_SIZE_OPTIONS.some((o) => o.value === 8192)).toBe(true)
    expect(REGION_OPTIONS.some((o) => o.value === 'Japan')).toBe(true)
  })
})

describe('genesis-manifest-core > normalizePreset', () => {
  it('passes through known presets (case-insensitively)', () => {
    expect(normalizePreset('plain')).toBe('plain')
    expect(normalizePreset('SEGA')).toBe('sega')
    expect(normalizePreset('Ssf2')).toBe('ssf2')
    expect(normalizePreset('SVP')).toBe('svp')
    expect(normalizePreset('custom')).toBe('custom')
  })

  it('maps anything unknown to custom', () => {
    expect(normalizePreset('realtec')).toBe('custom')
    expect(normalizePreset('')).toBe('custom')
  })
})

describe('genesis-manifest-core > parseManifestJson', () => {
  it('parses a well-formed manifest', () => {
    const m = parseManifestJson(JSON.stringify(plainManifest()))
    expect(m.core).toBe('sg1')
    expect(m.mapping.preset).toBe('plain')
    expect(m.rom.size).toBe(0x20_0000)
  })

  it('throws on malformed JSON', () => {
    expect(() => parseManifestJson('{ not json }')).toThrow(/not valid JSON/)
  })

  it('throws when the root is not an object', () => {
    expect(() => parseManifestJson('42')).toThrow(/must be a JSON object/)
    expect(() => parseManifestJson('null')).toThrow(/must be a JSON object/)
  })

  it('throws on a missing/typed-wrong core', () => {
    expect(() => parseManifestJson('{"version":"1.0","rom":{},"mapping":{}}')).toThrow(/"core"/)
  })

  it('throws on a missing version', () => {
    expect(() => parseManifestJson('{"core":"sg1","rom":{},"mapping":{}}')).toThrow(/"version"/)
  })

  it('throws when rom is not an object', () => {
    expect(() =>
      parseManifestJson('{"core":"sg1","version":"1.0","rom":7,"mapping":{}}'),
    ).toThrow(/"rom" must be an object/)
  })

  it('throws when rom.sha256 is not a string', () => {
    expect(() =>
      parseManifestJson('{"core":"sg1","version":"1.0","rom":{"sha256":1,"size":0},"mapping":{}}'),
    ).toThrow(/"rom.sha256"/)
  })

  it('throws when rom.size is not a number', () => {
    expect(() =>
      parseManifestJson(
        '{"core":"sg1","version":"1.0","rom":{"sha256":"x","size":"big"},"mapping":{}}',
      ),
    ).toThrow(/"rom.size"/)
  })

  it('throws when mapping is not an object', () => {
    expect(() =>
      parseManifestJson('{"core":"sg1","version":"1.0","rom":{"sha256":"x","size":0},"mapping":3}'),
    ).toThrow(/"mapping" must be an object/)
  })

  it('throws when mapping.preset is not a string', () => {
    expect(() =>
      parseManifestJson(
        '{"core":"sg1","version":"1.0","rom":{"sha256":"x","size":0},"mapping":{"preset":9}}',
      ),
    ).toThrow(/"mapping.preset"/)
  })
})

describe('genesis-manifest-core > parseDetectedManifest', () => {
  it('returns null for the JSON literal null', () => {
    expect(parseDetectedManifest('null')).toBeNull()
  })

  it('returns null for an empty/whitespace string', () => {
    expect(parseDetectedManifest('   ')).toBeNull()
  })

  it('parses a real manifest', () => {
    const m = parseDetectedManifest(JSON.stringify(svpManifest()))
    expect(m?.mapping.preset).toBe('svp')
  })
})

describe('genesis-manifest-core > manifestFormFromJson', () => {
  it('extracts the editable knobs', () => {
    const form = manifestFormFromJson(plainManifest())
    expect(form).toEqual({ preset: 'plain', sramSize: 8192 })
  })

  it('clamps a NaN/negative SRAM size to 0', () => {
    const m = plainManifest()
    // @ts-expect-error deliberately feeding a bad value
    m.mapping.sram_size = 'oops'
    expect(manifestFormFromJson(m)).toEqual({ preset: 'plain', sramSize: 0 })

    m.mapping.sram_size = -5
    expect(manifestFormFromJson(m).sramSize).toBe(0)
  })
})

describe('genesis-manifest-core > applyFormToManifest', () => {
  it('overlays the form knobs while preserving everything else', () => {
    const base = svpManifest()
    const next = applyFormToManifest({ preset: 'sega', sramSize: 32768 }, base)
    expect(next.mapping.preset).toBe('sega')
    expect(next.mapping.sram_size).toBe(32768)
    // Preserved.
    expect(next.rom.sha256).toBe('def')
    expect(next.chips).toHaveLength(1)
    // Non-mutating.
    expect(base.mapping.preset).toBe('svp')
  })
})

describe('genesis-manifest-core > stringifyManifest', () => {
  it('pretty-prints with two-space indent', () => {
    const json = stringifyManifest(plainManifest())
    expect(json).toContain('\n  "core": "sg1"')
    expect(JSON.parse(json).mapping.preset).toBe('plain')
  })
})

describe('genesis-manifest-core > formatSramSize', () => {
  it('labels zero as None, bytes below 1 KiB, and KiB above', () => {
    expect(formatSramSize(0)).toBe('None')
    expect(formatSramSize(-1)).toBe('None')
    expect(formatSramSize(512)).toBe('512 B')
    expect(formatSramSize(8192)).toBe('8 KiB')
    expect(formatSramSize(32768)).toBe('32 KiB')
  })
})

describe('genesis-manifest-core > validateManifestForm', () => {
  it('accepts a sane form', () => {
    expect(validateManifestForm({ preset: 'plain', sramSize: 8192 })).toEqual([])
    expect(validateManifestForm({ preset: 'svp', sramSize: 0 })).toEqual([])
  })

  it('rejects an unknown preset', () => {
    const errs = validateManifestForm({ preset: 'realtec' as never, sramSize: 0 })
    expect(errs.some((e) => e.includes('Unknown preset'))).toBe(true)
  })

  it('rejects a non-integer or negative SRAM size', () => {
    expect(validateManifestForm({ preset: 'plain', sramSize: 2.5 })[0]).toMatch(
      /non-negative whole number/,
    )
    expect(validateManifestForm({ preset: 'plain', sramSize: -8 })[0]).toMatch(
      /non-negative whole number/,
    )
  })

  it('rejects an SRAM size over the cap', () => {
    expect(validateManifestForm({ preset: 'plain', sramSize: MAX_SRAM_BYTES + 1024 })[0]).toMatch(
      /must not exceed/,
    )
  })

  it('warns when a non-zero SRAM size is not a power of two', () => {
    expect(validateManifestForm({ preset: 'plain', sramSize: 3000 })[0]).toMatch(/power of two/)
  })
})

describe('genesis-manifest-core > manifestDiffersFromDetected', () => {
  it('is false for a null override', () => {
    expect(manifestDiffersFromDetected(plainManifest(), null)).toBe(false)
  })

  it('is true for a non-null override against a null baseline', () => {
    expect(manifestDiffersFromDetected(null, plainManifest())).toBe(true)
  })

  it('is false when nothing changed', () => {
    expect(manifestDiffersFromDetected(plainManifest(), plainManifest())).toBe(false)
  })

  it('treats a zero SRAM size on both sides as unchanged', () => {
    const a = plainManifest()
    const b = plainManifest()
    a.mapping.sram_size = 0
    b.mapping.sram_size = 0
    expect(manifestDiffersFromDetected(a, b)).toBe(false)
  })

  it('detects a preset change', () => {
    const over = plainManifest()
    over.mapping.preset = 'ssf2'
    expect(manifestDiffersFromDetected(plainManifest(), over)).toBe(true)
  })

  it('detects an SRAM size change', () => {
    const over = plainManifest()
    over.mapping.sram_size = 32768
    expect(manifestDiffersFromDetected(plainManifest(), over)).toBe(true)
  })

  it('detects a custom-slot count change', () => {
    const over = plainManifest()
    over.mapping.slots = [
      {
        addr_range: ['0x200000', '0x20FFFF'],
        target: { kind: 'OpenBus' },
      },
    ]
    expect(manifestDiffersFromDetected(plainManifest(), over)).toBe(true)
  })

  it('detects a chip count change (and treats missing arrays as empty)', () => {
    const detected = plainManifest()
    detected.chips = undefined
    detected.mapping.slots = undefined
    const over = svpManifest()
    over.mapping.slots = undefined
    over.mapping.preset = 'plain'
    over.mapping.sram_size = 8192
    expect(manifestDiffersFromDetected(detected, over)).toBe(true)
  })
})

describe('genesis-manifest-core > coprocessorLabel', () => {
  it('returns null when the manifest is absent or plain/custom', () => {
    expect(coprocessorLabel(null)).toBeNull()
    expect(coprocessorLabel(plainManifest())).toBeNull()
    const custom = plainManifest()
    custom.mapping.preset = 'custom'
    expect(coprocessorLabel(custom)).toBeNull()
  })

  it('labels every coprocessor-bearing preset', () => {
    const cases: Array<[string, string]> = [
      ['sega', 'SEGA mapper'],
      ['ssf2', 'SSF2 mapper'],
      ['codemasters', 'Codemasters mapper'],
      ['eeprom', 'Serial EEPROM'],
      ['jcart', 'J-Cart'],
      ['svp', 'SVP'],
    ]
    for (const [preset, label] of cases) {
      const m = plainManifest()
      m.mapping.preset = preset
      expect(coprocessorLabel(m)).toBe(label)
    }
  })
})
