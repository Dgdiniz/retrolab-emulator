import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

// main.ts is the page's bootstrap and cannot run under Node, so it is read
// as text: every name it imports must be used in the file.
const source = readFileSync(new URL('../src/main.ts', import.meta.url), 'utf8')

const importStatement = /^import\s+([\s\S]*?)\s+from\s+['"][^'"]+['"]/gm

/** The local names an import clause binds (`A`, `B as C` -> `C`, `type D`). */
function importedNames(clause: string): string[] {
  const names: string[] = []
  const braced = clause.match(/\{([\s\S]*)\}/)
  if (braced) {
    for (const part of braced[1].split(',')) {
      const spec = part.trim().replace(/^type\s+/, '')
      if (!spec) continue
      const alias = spec.split(/\s+as\s+/)
      names.push(alias[alias.length - 1].trim())
    }
  }
  const outside = clause.replace(/\{[\s\S]*\}/, '').replace(/,/g, ' ').trim()
  const namespace = outside.match(/\*\s+as\s+(\w+)/)
  if (namespace) names.push(namespace[1])
  else if (outside && outside !== 'type') names.push(outside.replace(/^type\s+/, ''))
  return names
}

describe('main.ts imports', () => {
  it('uses every name it imports', () => {
    const statements = [...source.matchAll(importStatement)]
    expect(statements.length).toBeGreaterThan(0)
    const body = source.replace(importStatement, '')
    const unused = statements
      .flatMap((statement) => importedNames(statement[1]))
      .filter((name) => !new RegExp(`\\b${name}\\b`).test(body))
    expect(unused).toEqual([])
  })
})
