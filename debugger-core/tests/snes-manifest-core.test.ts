import { describe, expect, it } from 'vitest'
import {
  MAX_SRAM_BYTES,
  PRESET_OPTIONS,
  REGION_OPTIONS,
  SNES_MANIFEST_SCHEMA,
  SNES_MANIFEST_VERSION,
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
  type SnesManifest,
} from '../src/snes-manifest-core'

/** A minimal valid HiROM manifest with 2 KiB SRAM (DKC-shaped). */
function dkcManifest(): SnesManifest {
  return {
    $schema: SNES_MANIFEST_SCHEMA,
    core: 'snes1',
    version: SNES_MANIFEST_VERSION,
    rom: { sha256: 'abc', size: 0x40_0000, title: 'DONKEY KONG', region: 'USA' },
    mapping: { preset: 'hirom', fastrom: true, sram_size: 2048, slots: [] },
    chips: [],
    metadata: null,
  }
}

describe('snes-manifest-core > constants', () => {
  it('exposes the schema/version/limit constants', () => {
    expect(SNES_MANIFEST_SCHEMA).toBe('cartridge-manifest-v1')
    expect(SNES_MANIFEST_VERSION).toBe('1.0')
    expect(MAX_SRAM_BYTES).toBe(1 << 20)
  })

  it('ships option lists for the form selects', () => {
    expect(PRESET_OPTIONS.map((o) => o.value)).toEqual(['lorom', 'hirom', 'exhirom', 'custom'])
    expect(SRAM_SIZE_OPTIONS[0]).toEqual({ value: 0, label: 'None' })
    expect(SRAM_SIZE_OPTIONS.some((o) => o.value === 2048)).toBe(true)
    expect(REGION_OPTIONS.some((o) => o.value === 'USA')).toBe(true)
  })
})

describe('snes-manifest-core > normalizePreset', () => {
  it('passes through known presets (case-insensitively)', () => {
    expect(normalizePreset('lorom')).toBe('lorom')
    expect(normalizePreset('HiROM')).toBe('hirom')
    expect(normalizePreset('ExHiROM')).toBe('exhirom')
    expect(normalizePreset('custom')).toBe('custom')
  })

  it('maps anything unknown to custom', () => {
    expect(normalizePreset('exlorom')).toBe('custom')
    expect(normalizePreset('')).toBe('custom')
  })
})

describe('snes-manifest-core > parseManifestJson', () => {
  it('parses a well-formed manifest', () => {
    const m = parseManifestJson(JSON.stringify(dkcManifest()))
    expect(m.core).toBe('snes1')
    expect(m.mapping.preset).toBe('hirom')
    expect(m.rom.size).toBe(0x40_0000)
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
    expect(() => parseManifestJson('{"core":"snes1","rom":{},"mapping":{}}')).toThrow(/"version"/)
  })

  it('throws when rom is not an object', () => {
    expect(() =>
      parseManifestJson('{"core":"snes1","version":"1.0","rom":7,"mapping":{}}'),
    ).toThrow(/"rom" must be an object/)
  })

  it('throws when rom.sha256 is not a string', () => {
    expect(() =>
      parseManifestJson('{"core":"snes1","version":"1.0","rom":{"sha256":1,"size":0},"mapping":{}}'),
    ).toThrow(/"rom.sha256"/)
  })

  it('throws when rom.size is not a number', () => {
    expect(() =>
      parseManifestJson(
        '{"core":"snes1","version":"1.0","rom":{"sha256":"x","size":"big"},"mapping":{}}',
      ),
    ).toThrow(/"rom.size"/)
  })

  it('throws when mapping is not an object', () => {
    expect(() =>
      parseManifestJson('{"core":"snes1","version":"1.0","rom":{"sha256":"x","size":0},"mapping":3}'),
    ).toThrow(/"mapping" must be an object/)
  })

  it('throws when mapping.preset is not a string', () => {
    expect(() =>
      parseManifestJson(
        '{"core":"snes1","version":"1.0","rom":{"sha256":"x","size":0},"mapping":{"preset":9}}',
      ),
    ).toThrow(/"mapping.preset"/)
  })
})

describe('snes-manifest-core > parseDetectedManifest', () => {
  it('returns null for the JSON literal null', () => {
    expect(parseDetectedManifest('null')).toBeNull()
  })

  it('returns null for an empty/whitespace string', () => {
    expect(parseDetectedManifest('   ')).toBeNull()
  })

  it('parses a real manifest', () => {
    const m = parseDetectedManifest(JSON.stringify(dkcManifest()))
    expect(m?.mapping.preset).toBe('hirom')
  })
})

describe('snes-manifest-core > manifestFormFromJson', () => {
  it('extracts the editable knobs', () => {
    const form = manifestFormFromJson(dkcManifest())
    expect(form).toEqual({ preset: 'hirom', sramSize: 2048, fastrom: true })
  })

  it('clamps a NaN/negative SRAM size to 0 and coerces fastrom', () => {
    const m = dkcManifest()
    // @ts-expect-error deliberately feeding a bad value
    m.mapping.sram_size = 'oops'
    m.mapping.fastrom = false
    expect(manifestFormFromJson(m)).toEqual({ preset: 'hirom', sramSize: 0, fastrom: false })

    m.mapping.sram_size = -5
    expect(manifestFormFromJson(m).sramSize).toBe(0)
  })
})

describe('snes-manifest-core > applyFormToManifest', () => {
  it('overlays the form knobs while preserving everything else', () => {
    const base = dkcManifest()
    base.mapping.slots = [
      {
        bank_range: ['0x60', '0x6F'],
        offset_range: ['0x0000', '0x7FFF'],
        target: { kind: 'Sram', base_offset: 0 },
        speed: 'Slow',
      },
    ]
    const next = applyFormToManifest({ preset: 'lorom', sramSize: 32768, fastrom: false }, base)
    expect(next.mapping.preset).toBe('lorom')
    expect(next.mapping.sram_size).toBe(32768)
    expect(next.mapping.fastrom).toBe(false)
    // Preserved.
    expect(next.rom.sha256).toBe('abc')
    expect(next.mapping.slots).toHaveLength(1)
    // Non-mutating.
    expect(base.mapping.preset).toBe('hirom')
  })
})

describe('snes-manifest-core > stringifyManifest', () => {
  it('pretty-prints with two-space indent', () => {
    const json = stringifyManifest(dkcManifest())
    expect(json).toContain('\n  "core": "snes1"')
    expect(JSON.parse(json).mapping.preset).toBe('hirom')
  })
})

describe('snes-manifest-core > formatSramSize', () => {
  it('labels zero as None, bytes below 1 KiB, and KiB above', () => {
    expect(formatSramSize(0)).toBe('None')
    expect(formatSramSize(-1)).toBe('None')
    expect(formatSramSize(512)).toBe('512 B')
    expect(formatSramSize(2048)).toBe('2 KiB')
    expect(formatSramSize(32768)).toBe('32 KiB')
  })
})

describe('snes-manifest-core > validateManifestForm', () => {
  it('accepts a sane form', () => {
    expect(validateManifestForm({ preset: 'hirom', sramSize: 2048, fastrom: true })).toEqual([])
    expect(validateManifestForm({ preset: 'lorom', sramSize: 0, fastrom: false })).toEqual([])
  })

  it('rejects an unknown preset', () => {
    const errs = validateManifestForm({
      preset: 'exlorom' as never,
      sramSize: 0,
      fastrom: false,
    })
    expect(errs.some((e) => e.includes('Unknown preset'))).toBe(true)
  })

  it('rejects a non-integer or negative SRAM size', () => {
    expect(validateManifestForm({ preset: 'hirom', sramSize: 2.5, fastrom: false })[0]).toMatch(
      /non-negative whole number/,
    )
    expect(validateManifestForm({ preset: 'hirom', sramSize: -8, fastrom: false })[0]).toMatch(
      /non-negative whole number/,
    )
  })

  it('rejects an SRAM size over the cap', () => {
    expect(
      validateManifestForm({ preset: 'hirom', sramSize: MAX_SRAM_BYTES + 1024, fastrom: false })[0],
    ).toMatch(/must not exceed/)
  })

  it('warns when a non-zero SRAM size is not a power of two', () => {
    expect(validateManifestForm({ preset: 'hirom', sramSize: 3000, fastrom: false })[0]).toMatch(
      /power of two/,
    )
  })
})

describe('snes-manifest-core > manifestDiffersFromDetected', () => {
  it('is false for a null override', () => {
    expect(manifestDiffersFromDetected(dkcManifest(), null)).toBe(false)
  })

  it('is true for a non-null override against a null baseline', () => {
    expect(manifestDiffersFromDetected(null, dkcManifest())).toBe(true)
  })

  it('is false when nothing changed', () => {
    expect(manifestDiffersFromDetected(dkcManifest(), dkcManifest())).toBe(false)
  })

  it('treats a zero SRAM size on both sides as unchanged', () => {
    const a = dkcManifest()
    const b = dkcManifest()
    a.mapping.sram_size = 0
    b.mapping.sram_size = 0
    expect(manifestDiffersFromDetected(a, b)).toBe(false)
  })

  it('detects a preset change', () => {
    const over = dkcManifest()
    over.mapping.preset = 'lorom'
    expect(manifestDiffersFromDetected(dkcManifest(), over)).toBe(true)
  })

  it('detects an SRAM size change', () => {
    const over = dkcManifest()
    over.mapping.sram_size = 32768
    expect(manifestDiffersFromDetected(dkcManifest(), over)).toBe(true)
  })

  it('detects a fastrom change', () => {
    const over = dkcManifest()
    over.mapping.fastrom = false
    expect(manifestDiffersFromDetected(dkcManifest(), over)).toBe(true)
  })

  it('detects a custom-slot count change', () => {
    const over = dkcManifest()
    over.mapping.slots = [
      {
        bank_range: ['0x60', '0x6F'],
        offset_range: ['0x0000', '0x7FFF'],
        target: { kind: 'OpenBus' },
        speed: 'Slow',
      },
    ]
    expect(manifestDiffersFromDetected(dkcManifest(), over)).toBe(true)
  })

  it('detects a chip count change (and treats missing arrays as empty)', () => {
    const detected = dkcManifest()
    detected.chips = undefined
    detected.mapping.slots = undefined
    const over = dkcManifest()
    over.chips = [
      {
        id: 1,
        kind: 'dsp1',
        mapping: {
          bank_range: ['0x30', '0x3F'],
          offset_range: ['0x8000', '0xBFFF'],
          target: { kind: 'ChipMmio', chip_id: 1, base: 0 },
          speed: 'Fast',
        },
      },
    ]
    over.mapping.slots = undefined
    expect(manifestDiffersFromDetected(detected, over)).toBe(true)
  })
})

describe('snes-manifest-core > coprocessorLabel', () => {
  it('returns null when the manifest or metadata is absent', () => {
    expect(coprocessorLabel(null)).toBeNull()
    expect(coprocessorLabel(dkcManifest())).toBeNull()
  })

  it('returns null when metadata has no coprocessor object', () => {
    const m = dkcManifest()
    m.metadata = { other: 1 }
    expect(coprocessorLabel(m)).toBeNull()
    m.metadata = { coprocessor: 'dsp1' }
    expect(coprocessorLabel(m)).toBeNull()
  })

  it('returns null when the coprocessor kind is missing', () => {
    const m = dkcManifest()
    m.metadata = { coprocessor: { layout: 'HiRom' } }
    expect(coprocessorLabel(m)).toBeNull()
  })

  it('formats kind alone and kind + layout', () => {
    const m = dkcManifest()
    m.metadata = { coprocessor: { kind: 'dsp1' } }
    expect(coprocessorLabel(m)).toBe('DSP1')
    m.metadata = { coprocessor: { kind: 'dsp1', layout: 'HiRom' } }
    expect(coprocessorLabel(m)).toBe('DSP1 (HiRom)')
  })
})
