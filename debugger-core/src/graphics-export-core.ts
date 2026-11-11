/**
 * Shared, host-agnostic helpers for turning raw RGBA the emulator core
 * hands over into something a UI can preview and a user can save.
 *
 * This is the presentation side of graphics extraction, used by RetroLab's
 * capture tools: the emulator core decodes hardware memory into straight
 * RGBA8888 (palette index 0 already keyed to alpha 0); everything here is
 * pure presentation/packaging: PNG encoding, the transparent-pixel preview
 * backdrop and the filename scheme. None of it touches emulator state.
 *
 * The package runs its tests under Node, so the two DOM dependencies
 * (the `<canvas>` used for PNG encoding and the `ImageData` used to blit
 * pixels) are injected through small factory types with browser defaults.
 * That keeps the geometry/colour maths unit-testable without a DOM while
 * the real app passes an `OffscreenCanvas` / `document` automatically.
 */

/** A tight RGBA8888 image: `data` is row-major, `width * height * 4` bytes. */
export interface RgbaImage {
  width: number
  height: number
  data: Uint8Array | Uint8ClampedArray
}

/**
 * Validate an RGBA image before we paint or encode it: positive integer
 * dimensions and a byte length that matches `w * h * 4`. Rejecting here
 * keeps a stale/zero-area buffer from throwing deep inside `putImageData`.
 */
export function isValidRgbaImage(img: RgbaImage | null | undefined): img is RgbaImage {
  if (!img || !img.data) return false
  if (!Number.isInteger(img.width) || !Number.isInteger(img.height)) return false
  if (img.width <= 0 || img.height <= 0) return false
  return img.data.length === img.width * img.height * 4
}

/** RGB triple, each channel 0-255. */
export type Rgb = [number, number, number]

/** Options for {@link compositeCheckerboard}. */
export interface CheckerboardOptions {
  /** Square size in pixels (clamped to >= 1). Default 8. */
  cellSize?: number
  /** Light square colour. Default `0xCC` grey. */
  light?: Rgb
  /** Dark square colour. Default `0x99` grey. */
  dark?: Rgb
}

/**
 * Alpha-composite an RGBA image over a checkerboard so transparent
 * pixels are *visible* in a preview. This is preview chrome only, the
 * exported PNG always keeps the real alpha channel, so the checkerboard
 * is never baked into a saved file.
 *
 * Returns a fresh, fully-opaque RGBA buffer (`width * height * 4`).
 */
export function compositeCheckerboard(
  data: Uint8Array | Uint8ClampedArray,
  width: number,
  height: number,
  options: CheckerboardOptions = {},
): Uint8ClampedArray {
  const cell = Math.max(1, Math.floor(options.cellSize ?? 8))
  const light = options.light ?? [0xcc, 0xcc, 0xcc]
  const dark = options.dark ?? [0x99, 0x99, 0x99]
  const out = new Uint8ClampedArray(width * height * 4)

  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = (y * width + x) * 4
      const onLight = (Math.floor(x / cell) + Math.floor(y / cell)) % 2 === 0
      const bg = onLight ? light : dark
      const a = data[i + 3] / 255
      out[i] = data[i] * a + bg[0] * (1 - a)
      out[i + 1] = data[i + 1] * a + bg[1] * (1 - a)
      out[i + 2] = data[i + 2] * a + bg[2] * (1 - a)
      out[i + 3] = 255
    }
  }
  return out
}

/** Lowercase, filesystem-safe token: keeps `[a-z0-9._-]`, collapses the rest to `-`. */
function sanitizeToken(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9._-]+/g, '-')
    .replace(/^-+|-+$/g, '')
}

/**
 * Deterministic filename for an extracted asset: `<kind>_<id>.<ext>`
 * (e.g. `sprite_12.png`, `palette_all.png`). Both `kind` and `id` are
 * sanitized; empty inputs fall back to `capture` / `0` so we never emit a
 * dangling separator or a hidden dotfile.
 */
export function buildExtractionFilename(kind: string, id: string | number, ext = 'png'): string {
  const safeKind = sanitizeToken(kind) || 'capture'
  const safeId = sanitizeToken(String(id)) || '0'
  const safeExt = sanitizeToken(ext) || 'png'
  return `${safeKind}_${safeId}.${safeExt}`
}

/** The subset of `ImageData` this module reads/writes. */
export interface ImageDataLike {
  readonly width: number
  readonly height: number
  readonly data: Uint8ClampedArray
}

/** Builds an `ImageData`-like object owning a copy of `data`. */
export type ImageDataFactory = (
  data: Uint8ClampedArray,
  width: number,
  height: number,
) => ImageDataLike

/** Default factory: the browser `ImageData` constructor. */
export const defaultImageDataFactory: ImageDataFactory = (data, width, height) => {
  // Reach for the browser global through `globalThis` rather than the DOM
  // lib types so this package keeps compiling without `lib: dom` (it runs
  // its tests under Node, where the global is stubbed).
  const Ctor = (globalThis as unknown as {
    ImageData: new (d: Uint8ClampedArray, w: number, h: number) => ImageDataLike
  }).ImageData
  return new Ctor(data, width, height)
}

/**
 * Copy a raw RGBA buffer into an `ImageData` (via the injected factory).
 * The buffer is always copied into a fresh `Uint8ClampedArray` so the
 * result owns its pixels, safe to hold after a zero-copy WASM view is
 * invalidated by the emulator resuming.
 */
export function rgbaToImageData(
  data: Uint8Array | Uint8ClampedArray,
  width: number,
  height: number,
  make: ImageDataFactory = defaultImageDataFactory,
): ImageDataLike {
  return make(new Uint8ClampedArray(data), width, height)
}

/** The subset of a 2D context this module uses. */
export interface Context2dLike {
  createImageData(width: number, height: number): ImageDataLike
  putImageData(image: ImageDataLike, dx: number, dy: number): void
}

/** The subset of a canvas this module uses to encode a PNG. */
export interface PngCanvasLike {
  width: number
  height: number
  getContext(contextId: '2d'): Context2dLike | null
  /** `OffscreenCanvas` path. */
  convertToBlob?(options?: { type?: string }): Promise<Blob>
  /** `HTMLCanvasElement` path. */
  toBlob?(callback: (blob: Blob | null) => void, type?: string): void
}

/** Allocates a canvas of the given size for PNG encoding. */
export type CanvasFactory = (width: number, height: number) => PngCanvasLike

/**
 * Default canvas factory: an `OffscreenCanvas` when available (workers +
 * modern main threads), falling back to a detached `<canvas>` element.
 */
export const defaultCanvasFactory: CanvasFactory = (width, height) => {
  // Access the browser globals via `globalThis` so the package compiles
  // without the DOM lib (Node tests stub these).
  const g = globalThis as unknown as {
    OffscreenCanvas?: new (w: number, h: number) => PngCanvasLike
    document?: { createElement(tag: 'canvas'): PngCanvasLike }
  }
  if (typeof g.OffscreenCanvas !== 'undefined') {
    return new g.OffscreenCanvas(width, height)
  }
  const canvas = g.document!.createElement('canvas')
  canvas.width = width
  canvas.height = height
  return canvas
}

/** Resolve a canvas to a PNG `Blob`, handling both canvas flavours. */
function canvasToPngBlob(canvas: PngCanvasLike): Promise<Blob> {
  if (typeof canvas.convertToBlob === 'function') {
    return canvas.convertToBlob({ type: 'image/png' })
  }
  if (typeof canvas.toBlob === 'function') {
    const toBlob = canvas.toBlob.bind(canvas)
    return new Promise<Blob>((resolve, reject) => {
      toBlob((blob) => {
        if (blob) resolve(blob)
        else reject(new Error('canvas.toBlob returned null'))
      }, 'image/png')
    })
  }
  throw new Error('canvas exposes neither convertToBlob nor toBlob')
}

/**
 * Encode a raw RGBA buffer as a PNG `Blob`, preserving the alpha channel
 * (so hardware transparency survives into the saved file).
 *
 * The canvas is injectable so the geometry is testable under Node; the
 * real app uses {@link defaultCanvasFactory}. Throws if the dimensions
 * don't match the buffer or the 2D context is unavailable.
 */
export async function rgbaToPngBlob(
  data: Uint8Array | Uint8ClampedArray,
  width: number,
  height: number,
  makeCanvas: CanvasFactory = defaultCanvasFactory,
): Promise<Blob> {
  if (!isValidRgbaImage({ data, width, height })) {
    throw new Error('rgbaToPngBlob: invalid image dimensions for buffer')
  }
  const canvas = makeCanvas(width, height)
  const ctx = canvas.getContext('2d')
  if (!ctx) {
    throw new Error('rgbaToPngBlob: 2D context unavailable')
  }
  const image = ctx.createImageData(width, height)
  image.data.set(data)
  ctx.putImageData(image, 0, 0)
  return canvasToPngBlob(canvas)
}
