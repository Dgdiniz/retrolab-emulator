import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { traceCpuSpecForKind } from '@debugger-core/trace-spec-core'
import type { CpuKind } from '@debugger-core/cpu-flags-core'

// The worker runs here against an in-memory stand-in for the origin
// private file system: one traces directory whose files are the chunks
// written to them, read back as a Blob.
function fakeOpfs() {
  const files = new Map<string, BlobPart[]>()
  const fileHandle = (name: string) => ({
    async createWritable() {
      const parts: BlobPart[] = []
      files.set(name, parts)
      return {
        async write(data: Uint8Array) {
          parts.push(new Uint8Array(data))
        },
        async close() {},
      }
    },
    async getFile() {
      return new Blob(files.get(name) ?? [])
    },
  })
  const traces = {
    async getFileHandle(name: string) {
      return fileHandle(name)
    },
  }
  return {
    async getDirectoryHandle() {
      return {
        async getDirectoryHandle() {
          return traces
        },
      }
    },
  }
}

// Bytes numbered 0..n-1 (mod 251), so a chunk that starts or ends in the
// wrong place shows up in the reassembled file.
function numberedBytes(n: number): Uint8Array {
  const out = new Uint8Array(n)
  for (let i = 0; i < n; i++) out[i] = i % 251
  return out
}

let posted: any[]
let workerSelf: { postMessage(message: unknown): void; onmessage?: (e: { data: unknown }) => Promise<void> }

async function loadWorker(): Promise<(message: Record<string, unknown>) => Promise<void>> {
  posted = []
  workerSelf = { postMessage: (message) => posted.push(message) }
  const root = fakeOpfs()
  vi.stubGlobal('self', workerSelf)
  vi.stubGlobal('navigator', { storage: { getDirectory: async () => root } })
  vi.resetModules()
  // The worker is a script with no exports: evaluating it installs
  // `self.onmessage`.
  await import('../src/trace-worker.js' as string)
  return async (message) => {
    await workerSelf.onmessage!({ data: message })
  }
}

// Record `entries` entries of `entrySize` bytes, stop, then run the export
// the way the page does: `export`, then one `exportNextChunk` per chunk the
// worker announces. Returns every data message the worker sent back.
async function recordAndExport(entrySize: number, entries: number, chunkSize?: number) {
  const send = await loadWorker()
  const recorded = numberedBytes(entrySize * entries)
  await send({ type: 'start', fileName: 'trace_test.bin', entrySize })
  await send({ type: 'chunk', data: recorded })
  await send({ type: 'stop' })
  await send({ type: 'export', chunkSize })

  const started = posted.find((m) => m.type === 'exportStarted')
  if (started) {
    for (let i = 0; i < started.totalChunks; i++) {
      await send({
        type: 'exportNextChunk',
        chunkIndex: i,
        chunkSize: started.chunkSize,
        totalChunks: started.totalChunks,
        totalSize: started.totalSize,
      })
    }
  }
  const pieces = posted.filter((m) => m.type === 'exportChunk' || m.type === 'exportReady')
  return { recorded, started, pieces }
}

describe('trace worker export', () => {
  beforeEach(() => {
    vi.spyOn(console, 'log').mockImplementation(() => {})
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    vi.spyOn(console, 'error').mockImplementation(() => {})
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  // The page asks for 64 MiB chunks; a small chunk size runs the same
  // arithmetic over a few hundred bytes.
  const kinds: CpuKind[] = ['w65816', 'spc700', 'm68k', 'z80']
  for (const kind of kinds) {
    it(`cuts a ${kind} trace into chunks of whole entries of its own size`, async () => {
      const { entrySize } = traceCpuSpecForKind(kind)
      const { recorded, started, pieces } = await recordAndExport(entrySize, 25, 200)

      expect(started).toBeDefined()
      expect(started.chunkSize % entrySize).toBe(0)
      expect(pieces.length).toBe(started.totalChunks)
      // The page converts each chunk on its own and refuses one with
      // leftover bytes: every chunk must hold whole entries.
      for (const piece of pieces) {
        expect(piece.data.byteLength % entrySize).toBe(0)
      }
      const joined = new Uint8Array(await new Blob(pieces.map((p) => p.data)).arrayBuffer())
      expect(joined).toEqual(recorded)
    })
  }

  it('exports one 65816 entry, which is shorter than a 68000 entry, instead of calling the file empty', async () => {
    const { entrySize } = traceCpuSpecForKind('w65816')
    expect(entrySize).toBeLessThan(traceCpuSpecForKind('m68k').entrySize)
    const { recorded, pieces } = await recordAndExport(entrySize, 1)

    expect(posted.find((m) => m.type === 'error')).toBeUndefined()
    expect(pieces.length).toBe(1)
    expect(pieces[0].type).toBe('exportReady')
    expect(new Uint8Array(pieces[0].data)).toEqual(recorded)
  })

  it('refuses to start a trace without an entry size', async () => {
    const send = await loadWorker()
    await send({ type: 'start', fileName: 'trace_test.bin' })
    expect(posted.find((m) => m.type === 'started')).toBeUndefined()
    expect(posted.find((m) => m.type === 'error' && m.operation === 'start')).toBeDefined()
  })

  it('refuses an export before any trace has started', async () => {
    const send = await loadWorker()
    await send({ type: 'export' })
    expect(posted.find((m) => m.type === 'exportReady' || m.type === 'exportStarted')).toBeUndefined()
    expect(posted.find((m) => m.type === 'error' && m.operation === 'export')).toBeDefined()
  })

  it('logs the first bytes of a chunk without decoding them as a 68000 entry', async () => {
    const log = vi.mocked(console.log)
    const { pieces } = await recordAndExport(traceCpuSpecForKind('w65816').entrySize, 25, 200)
    expect(pieces.length).toBeGreaterThan(1)
    const lines = log.mock.calls.map((args) => args.map(String).join(' '))
    expect(lines.some((line) => line.includes('First 16 bytes'))).toBe(true)
    // PC and SR are the first two fields of a 68000 entry only.
    expect(lines.filter((line) => /\bPC=|\bSR=/.test(line))).toEqual([])
  })
})
