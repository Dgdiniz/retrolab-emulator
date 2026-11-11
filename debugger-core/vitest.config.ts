import { defineConfig } from 'vitest/config'

// `@retrolab/debugger-core` is a pure-TS pure-core package, no Vue, no
// DOM, no Web APIs beyond what Node ships. Tests import the sources via
// relative paths (`../src/<module>`) so we don't need an alias inside
// the package itself; the `@debugger-core` alias is only useful when
// the package is being consumed from outside (platform + web/).
export default defineConfig({
  test: {
    include: ['tests/**/*.test.ts'],
    environment: 'node',
    globals: false,
    testTimeout: 10_000,
    hookTimeout: 10_000,
    coverage: {
      provider: 'v8',
      reporter: ['text', 'html', 'lcov', 'json-summary'],
      reportsDirectory: './coverage',
      include: ['src/**/*.ts'],
      exclude: ['src/index.ts'],
      // Per-file floors. `format-core.ts` is driven through every branch by
      // its property-based suite and is held to the same envelope as
      // hex-editor-core.
      thresholds: {
        'src/breakpoints-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
        'src/hex-editor-core.ts': {
          statements: 100, branches: 95, functions: 100, lines: 100,
        },
        'src/memory-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
        'src/disassembly-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
        'src/registers-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
        'src/cpu-flags-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
        'src/debug-profile-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
        'src/trace-spec-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
        'src/memory-regions-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
        'src/format-core.ts': {
          statements: 100, branches: 95, functions: 100, lines: 100,
        },
        'src/frame-pacing-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
        'src/graphics-export-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
        'src/edit-history-core.ts': {
          statements: 100, branches: 95, functions: 100, lines: 100,
        },
        'src/byte-interpretation-core.ts': {
          statements: 100, branches: 95, functions: 100, lines: 100,
        },
        'src/sprite-table-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
        'src/snes-manifest-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
        'src/genesis-manifest-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
        'src/video-layers-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
        'src/watchpoints-core.ts': {
          statements: 100, branches: 100, functions: 100, lines: 100,
        },
      },
    },
  },
})
