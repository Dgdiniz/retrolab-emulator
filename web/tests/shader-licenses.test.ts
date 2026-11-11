import { describe, it, expect } from 'vitest'
// The registry is plain JavaScript, kept outside the typecheck on purpose.
// @ts-expect-error, no declaration file for the shader registry
import { SHADER_DEFS } from '../src/shaders/shader-defs.js'

/**
 * The license of every display shader this debugger ships.
 *
 * Each shader is a file of libretro/glsl-shaders embedded exactly as its
 * author published it, so its own header is the record of its license. The
 * statement expected of each entry is written down here: a new shader fails
 * until someone has read its file and added its line, and any text naming a
 * copyleft or non-commercial license fails outright. The test reads the
 * published text itself, since a comment beside a shader can say otherwise.
 */
const LICENSE_STATEMENT: Record<string, RegExp> = {
  'crt-lottes': /PUBLIC DOMAIN CRT STYLED SCAN-LINE SHADER\s*\n\/\/\s*\n\/\/\s+by Timothy Lottes/,
  'crt-lottes-fast': /LICENSE = UNLICENSE \(aka PUBLIC DOMAIN\)/,
  'crt-nobody': /Copyright \(C\) 2011-2022 Hyllian[\s\S]{0,80}Permission is hereby granted, free of charge/,
  'omniscale': /MIT License\s+Copyright \(c\) 2015-2016 Lior Halphon/,
  'bicubic': /The following code is licensed under the MIT license/,
}

const FORBIDDEN_LICENSE = /GNU (Lesser |Library |Affero )?General Public|\b[AL]?GPL\b|Creative Commons|\bCC[ -]BY\b|NonCommercial/i

type ShaderDef = { name: string; filterLinear: boolean; source: string }
const defs = SHADER_DEFS as Record<string, ShaderDef>

describe('display shaders', () => {
  it('are exactly the ones whose license is written down here', () => {
    expect(Object.keys(defs).sort()).toEqual(Object.keys(LICENSE_STATEMENT).sort())
  })

  for (const key of Object.keys(LICENSE_STATEMENT)) {
    describe(key, () => {
      const def = defs[key]

      it('has a name, a filter flag and a combined vertex + fragment source', () => {
        expect(def.name.length).toBeGreaterThan(0)
        expect(typeof def.filterLinear).toBe('boolean')
        expect(def.source).toContain('#if defined(VERTEX)')
        expect(def.source).toContain('#elif defined(FRAGMENT)')
      })

      it('states its license in its own published header', () => {
        const header = def.source.slice(0, def.source.indexOf('#if defined(VERTEX)'))
        expect(header).toMatch(LICENSE_STATEMENT[key])
      })

      it('names no copyleft or non-commercial license anywhere in its text', () => {
        expect(def.source).not.toMatch(FORBIDDEN_LICENSE)
      })

      it('has at most one #version line, which the renderer drops', () => {
        const versions = def.source.match(/^[ \t]*#version\b/gm) || []
        expect(versions.length).toBeLessThanOrEqual(1)
      })
    })
  }
})
