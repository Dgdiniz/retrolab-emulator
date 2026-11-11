/// <reference types="vite/client" />

// Static assets served verbatim from `public/`:
//   - `pkg/sg1_wasm.js` + `pkg/sg1_wasm_bg.wasm`, and the same pair for
//     snes1 (wasm-bindgen output, populated by build-wasm.sh), loaded via runtime
//     `import()` from `main.ts`.
//   - `audio-worklet-processor.js`: loaded via
//     `audioWorklet.addModule()` from `src/audio.js`. Lives in `public/`
//     so Vite never transforms it; the AudioWorkletGlobalScope lacks
//     `import.meta.env` and other Vite injectables, so bundler wrappers
//     would silently break the worklet.
// All are addressed at runtime with `${import.meta.env.BASE_URL}...` so
// they pick up Vite's `base` prefix in both dev and prod. No ambient
// module declaration is needed because nothing imports them statically.
