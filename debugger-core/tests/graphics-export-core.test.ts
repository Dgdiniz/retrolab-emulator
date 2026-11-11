/// <reference lib="dom" />
// The package itself is DOM-free (`lib: ["ES2022"]`); this suite fakes a
// canvas whose `toBlob` hands back a real `Blob`, so it opts into the DOM
// types for itself only.
import { describe, it, expect, afterEach } from 'vitest'
import {
  isValidRgbaImage,
  compositeCheckerboard,
  buildExtractionFilename,
  rgbaToImageData,
  rgbaToPngBlob,
  defaultImageDataFactory,
  defaultCanvasFactory,
  type ImageDataLike,
  type PngCanvasLike,
  type Context2dLike,
} from '../src/graphics-export-core'

// -- Fakes --------------------------------------------------------------
// The package runs under Node, so canvas/ImageData are injected. These
// minimal fakes record what the encoder writes so tests can assert the
// pixels round-trip and the right blob path is taken.

function makeImageData(data: Uint8ClampedArray, width: number, height: number): ImageDataLike {
  return { width, height, data }
}

class FakeContext implements Context2dLike {
  public put: { image: ImageDataLike; dx: number; dy: number } | null = null
  createImageData(width: number, height: number): ImageDataLike {
    return { width, height, data: new Uint8ClampedArray(width * height * 4) }
  }
  putImageData(image: ImageDataLike, dx: number, dy: number): void {
    this.put = { image, dx, dy }
  }
}

/** Canvas using the `convertToBlob` (OffscreenCanvas) path. */
class FakeOffscreenCanvas implements PngCanvasLike {
  public ctx = new FakeContext()
  constructor(public width: number, public height: number) {}
  getContext(): Context2dLike | null {
    return this.ctx
  }
  convertToBlob(options?: { type?: string }): Promise<Blob> {
    return Promise.resolve(new Blob(['offscreen'], { type: options?.type ?? 'image/png' }))
  }
}

/** Canvas using the `toBlob` (HTMLCanvasElement) path. */
class FakeHtmlCanvas implements PngCanvasLike {
  public ctx = new FakeContext()
  constructor(public width: number, public height: number) {}
  getContext(): Context2dLike | null {
    return this.ctx
  }
  toBlob(callback: (blob: Blob | null) => void, type?: string): void {
    callback(new Blob(['html'], { type: type ?? 'image/png' }))
  }
}

describe('isValidRgbaImage', () => {
  it('accepts a tightly-packed buffer', () => {
    expect(isValidRgbaImage({ width: 2, height: 2, data: new Uint8Array(16) })).toBe(true)
  })

  it('rejects null / missing data', () => {
    expect(isValidRgbaImage(null)).toBe(false)
    expect(isValidRgbaImage(undefined)).toBe(false)
    expect(isValidRgbaImage({ width: 1, height: 1 } as never)).toBe(false)
  })

  it('rejects non-integer or non-positive dimensions', () => {
    expect(isValidRgbaImage({ width: 1.5, height: 2, data: new Uint8Array(12) })).toBe(false)
    expect(isValidRgbaImage({ width: 2, height: 0, data: new Uint8Array(0) })).toBe(false)
    expect(isValidRgbaImage({ width: -1, height: 2, data: new Uint8Array(8) })).toBe(false)
  })

  it('rejects a length mismatch', () => {
    expect(isValidRgbaImage({ width: 2, height: 2, data: new Uint8Array(15) })).toBe(false)
  })
})

describe('compositeCheckerboard', () => {
  it('shows the checkerboard through fully transparent pixels', () => {
    // 2x1, both pixels transparent; default cell 8 -> both land on the
    // light square, both rows/cols start at cell 0.
    const out = compositeCheckerboard(new Uint8Array([0, 0, 0, 0, 0, 0, 0, 0]), 2, 1)
    expect(Array.from(out)).toEqual([0xcc, 0xcc, 0xcc, 255, 0xcc, 0xcc, 0xcc, 255])
  })

  it('keeps fully opaque pixels untouched (and forces alpha 255)', () => {
    const out = compositeCheckerboard(new Uint8Array([10, 20, 30, 255]), 1, 1)
    expect(Array.from(out)).toEqual([10, 20, 30, 255])
  })

  it('blends a half-transparent pixel over the background', () => {
    // alpha 128/255 ~ 0.50196; src red over light grey.
    const out = compositeCheckerboard(new Uint8Array([255, 0, 0, 128]), 1, 1)
    const a = 128 / 255
    expect(out[0]).toBe(Math.round(255 * a + 0xcc * (1 - a)) | 0)
    expect(out[3]).toBe(255)
  })

  it('alternates light/dark squares by cell and honours custom colours', () => {
    const light: [number, number, number] = [200, 200, 200]
    const dark: [number, number, number] = [50, 50, 50]
    // cellSize 1 -> adjacent pixels alternate squares.
    const out = compositeCheckerboard(new Uint8Array(8), 2, 1, { cellSize: 1, light, dark })
    expect(Array.from(out.slice(0, 4))).toEqual([200, 200, 200, 255]) // (0,0) light
    expect(Array.from(out.slice(4, 8))).toEqual([50, 50, 50, 255]) // (1,0) dark
  })

  it('clamps a zero/negative cell size to 1 instead of dividing by zero', () => {
    const out = compositeCheckerboard(new Uint8Array(8), 2, 1, { cellSize: 0 })
    // With cell clamped to 1, (0,0) is light and (1,0) is dark.
    expect(Array.from(out.slice(0, 4))).toEqual([0xcc, 0xcc, 0xcc, 255])
    expect(Array.from(out.slice(4, 8))).toEqual([0x99, 0x99, 0x99, 255])
  })
})

describe('buildExtractionFilename', () => {
  it('builds <kind>_<id>.<ext> with the default png extension', () => {
    expect(buildExtractionFilename('sprite', 12)).toBe('sprite_12.png')
  })

  it('honours an explicit extension', () => {
    expect(buildExtractionFilename('tile-sheet', 'planeA', 'bmp')).toBe('tile-sheet_planea.bmp')
  })

  it('sanitizes unsafe characters', () => {
    expect(buildExtractionFilename('My Layer!', 'a/b\\c')).toBe('my-layer_a-b-c.png')
  })

  it('falls back to capture/0/png for empty inputs', () => {
    expect(buildExtractionFilename('', '')).toBe('capture_0.png')
    expect(buildExtractionFilename('!!!', '@@@', '###')).toBe('capture_0.png')
  })
})

describe('rgbaToImageData', () => {
  it('copies the buffer through an injected factory', () => {
    const src = new Uint8Array([1, 2, 3, 4])
    const img = rgbaToImageData(src, 1, 1, makeImageData)
    expect(img.width).toBe(1)
    expect(img.height).toBe(1)
    expect(Array.from(img.data)).toEqual([1, 2, 3, 4])
    // It must be a copy: mutating the source doesn't change the result.
    src[0] = 99
    expect(img.data[0]).toBe(1)
  })

  it('uses the global ImageData constructor by default', () => {
    const captured: unknown[] = []
    const original = (globalThis as Record<string, unknown>).ImageData
    ;(globalThis as Record<string, unknown>).ImageData = class {
      width: number
      height: number
      data: Uint8ClampedArray
      constructor(data: Uint8ClampedArray, width: number, height: number) {
        captured.push([data, width, height])
        this.data = data
        this.width = width
        this.height = height
      }
    }
    try {
      const img = rgbaToImageData(new Uint8Array([5, 6, 7, 8]), 1, 1)
      expect(img.width).toBe(1)
      expect(Array.from(img.data)).toEqual([5, 6, 7, 8])
      expect(captured).toHaveLength(1)
    } finally {
      ;(globalThis as Record<string, unknown>).ImageData = original
    }
  })
})

describe('defaultImageDataFactory', () => {
  it('delegates to the global ImageData constructor', () => {
    const original = (globalThis as Record<string, unknown>).ImageData
    ;(globalThis as Record<string, unknown>).ImageData = class {
      constructor(
        public data: Uint8ClampedArray,
        public width: number,
        public height: number,
      ) {}
    }
    try {
      const out = defaultImageDataFactory(new Uint8ClampedArray(4), 1, 1)
      expect(out.width).toBe(1)
      expect(out.height).toBe(1)
    } finally {
      ;(globalThis as Record<string, unknown>).ImageData = original
    }
  })
})

describe('rgbaToPngBlob', () => {
  it('rejects an image whose dimensions do not match the buffer', async () => {
    await expect(rgbaToPngBlob(new Uint8Array(3), 2, 2)).rejects.toThrow(/invalid image dimensions/)
  })

  it('throws when the 2D context is unavailable', async () => {
    const factory = () =>
      ({
        width: 1,
        height: 1,
        getContext: () => null,
      }) as unknown as PngCanvasLike
    await expect(rgbaToPngBlob(new Uint8Array(4), 1, 1, factory)).rejects.toThrow(
      /2D context unavailable/,
    )
  })

  it('paints the pixels and encodes via convertToBlob (OffscreenCanvas path)', async () => {
    const canvas = new FakeOffscreenCanvas(1, 1)
    const blob = await rgbaToPngBlob(new Uint8Array([9, 8, 7, 6]), 1, 1, () => canvas)
    expect(canvas.ctx.put).not.toBeNull()
    expect(Array.from(canvas.ctx.put!.image.data)).toEqual([9, 8, 7, 6])
    expect(blob.type).toBe('image/png')
  })

  it('encodes via toBlob (HTMLCanvasElement path)', async () => {
    const canvas = new FakeHtmlCanvas(1, 1)
    const blob = await rgbaToPngBlob(new Uint8Array([1, 1, 1, 1]), 1, 1, () => canvas)
    expect(blob.type).toBe('image/png')
  })

  it('rejects when toBlob yields null', async () => {
    const canvas: PngCanvasLike = {
      width: 1,
      height: 1,
      getContext: () => new FakeContext(),
      toBlob: (cb) => cb(null),
    }
    await expect(rgbaToPngBlob(new Uint8Array(4), 1, 1, () => canvas)).rejects.toThrow(
      /returned null/,
    )
  })

  it('throws when the canvas exposes no blob export', async () => {
    const canvas: PngCanvasLike = {
      width: 1,
      height: 1,
      getContext: () => new FakeContext(),
    }
    await expect(rgbaToPngBlob(new Uint8Array(4), 1, 1, () => canvas)).rejects.toThrow(
      /neither convertToBlob nor toBlob/,
    )
  })
})

describe('defaultCanvasFactory', () => {
  const original = {
    OffscreenCanvas: (globalThis as Record<string, unknown>).OffscreenCanvas,
    document: (globalThis as Record<string, unknown>).document,
  }
  afterEach(() => {
    ;(globalThis as Record<string, unknown>).OffscreenCanvas = original.OffscreenCanvas
    ;(globalThis as Record<string, unknown>).document = original.document
  })

  it('prefers OffscreenCanvas when present', () => {
    ;(globalThis as Record<string, unknown>).OffscreenCanvas = class {
      constructor(
        public width: number,
        public height: number,
      ) {}
    }
    const canvas = defaultCanvasFactory(4, 3) as unknown as { width: number; height: number }
    expect(canvas.width).toBe(4)
    expect(canvas.height).toBe(3)
  })

  it('falls back to document.createElement when OffscreenCanvas is absent', () => {
    ;(globalThis as Record<string, unknown>).OffscreenCanvas = undefined
    const created: { width: number; height: number } = { width: 0, height: 0 }
    ;(globalThis as Record<string, unknown>).document = {
      createElement: (tag: string) => {
        expect(tag).toBe('canvas')
        return created
      },
    }
    const canvas = defaultCanvasFactory(7, 5) as unknown as { width: number; height: number }
    expect(canvas.width).toBe(7)
    expect(canvas.height).toBe(5)
  })
})
