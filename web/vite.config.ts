import { defineConfig } from 'vite';
import { fileURLToPath } from 'node:url';

// Defensive COOP/COEP headers so `SharedArrayBuffer`-backed WASM works if
// we ever opt-in to it. The emulator does not require them, but the
// `vite preview` / dev server sends them anyway, as RetroLab's dev server
// does, to keep both debugger UIs aligned.
const crossOriginHeaders = {
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'credentialless',
};

// Recommended dev loop: `npm run watch` (this config) + any static file
// server pointed at `dist/` (`python3 -m http.server`, `npx serve`,
// `vite preview`, etc.). Using a plain static server avoids the Vite dev
// server's HMR client + module transform pipeline, which races badly
// with the code's long-lived singletons (WASM memory, AudioContext
// + AudioWorklet, WebGL, Web Worker).
//
// `base` is conditional on mode so a watch build for local serving
// emits root-relative asset URLs, while the production build for
// GitHub Pages keeps the `/retrolab-emulator/` subpath the deploy
// workflow expects.
export default defineConfig(({ mode }) => ({
  base: mode === 'production' ? '/retrolab-emulator/' : '/',
  resolve: {
    alias: {
      // Shared pure-core modules. Panels import these via
      // `@debugger-core/<module>` to keep numeric logic separable from
      // any DOM/Vite specifics.
      '@debugger-core': fileURLToPath(
        new URL('../debugger-core/src', import.meta.url),
      ),
    },
  },
  server: {
    headers: crossOriginHeaders,
    // HMR injects a WebSocket client into every page that participates
    // in the lifecycle. Disabled because the code isn't written with HMR
    // in mind; a full page reload is fast enough for this loop.
    hmr: false,
  },
  preview: { headers: crossOriginHeaders },
  build: {
    outDir: 'dist',
    sourcemap: true,
    target: 'es2022',
  },
  // Workers stay as ES modules so `new URL(..., import.meta.url)` works
  // identically in dev and prod, and Rollup can split them into hashed
  // assets at build time.
  worker: { format: 'es' },
}));
