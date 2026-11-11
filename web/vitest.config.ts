import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vitest/config'

// The standalone web debugger is mostly DOM-bound bootstrap glue, but a
// handful of decisions are pure functions worth pinning down with tests
// (core selection by URL/extension, ROM-identity hashing, the frame-loop
// pacing over the shared `@debugger-core` pacer). Those live in `src/`
// modules and are exercised from `tests/`.
//
// Tests live OUTSIDE `src/` on purpose: the `typecheck` gate
// (`tsc --noEmit`) only compiles `src` + `vite.config.ts`, so importing
// `vitest` from a test never leaks into the production typecheck/build.
export default defineConfig({
  resolve: {
    alias: {
      // Same alias as vite.config.ts: the shared pure-core package.
      '@debugger-core': fileURLToPath(new URL('../debugger-core/src', import.meta.url)),
    },
  },
  test: {
    include: ['tests/**/*.test.ts'],
    environment: 'node',
    globals: false,
  },
})
