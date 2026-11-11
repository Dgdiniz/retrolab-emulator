/**
 * Tracing, wraps `trace-worker.js` (the OPFS-streaming worker) and
 * exposes a small lifecycle (`start` / `stop` / `export` / `clear` /
 * `checkFlush`) plus its own toolbar wiring (record button, stats
 * label, export, clear).
 *
 * The export flow is two-phased so the worker can hand off large traces
 * in chunks without blowing the main thread heap: small files take the
 * `exportReady` path and are decoded + downloaded in one go, while
 * large files step through `exportStarted` -> `exportChunk*` -> final
 * Blob assembly. This file owns the chunk accumulator state because
 * the protocol is internal to the worker boundary.
 */

import type { DebugBridge } from './debug-bridge'
import type { WebCore } from './core-resolver'
import { traceCpuSpec, type TraceCpuSpec } from './trace-ops'

export interface TracingHandle {
  startTracing(): void
  stopTracing(): void
  exportTrace(): void
  clearTrace(): void
  /** Called from the game loop after each frame; flushes the WASM trace ring buffer when full. */
  checkFlush(): void
  /**
   * Re-evaluate which toolbar buttons should be enabled. Called by the
   * bootstrap when `romLoaded` flips so the Record button lights up.
   */
  refreshUi(): void
  dispose(): void
}

interface TracingElements {
  startBtn: HTMLElement | null
  stopBtn: HTMLElement | null
  exportBtn: HTMLElement | null
  clearBtn: HTMLElement | null
  stats: HTMLElement | null
}

function formatNumber(n: number): string {
  return n.toLocaleString()
}

function downloadAsFile(content: string, filename: string, mimeType: string): void {
  const blob = new Blob([content], { type: mimeType })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  document.body.appendChild(a)
  a.click()
  document.body.removeChild(a)
  URL.revokeObjectURL(url)
}

export function setupTracing(bridge: DebugBridge, core: WebCore): TracingHandle {
  const elements: TracingElements = {
    startBtn: document.getElementById('trace-start-btn'),
    stopBtn: document.getElementById('trace-stop-btn'),
    exportBtn: document.getElementById('trace-export-btn'),
    clearBtn: document.getElementById('trace-clear-btn'),
    stats: document.getElementById('trace-stats'),
  }

  let worker: Worker | null = null
  let active = false
  let opfsAvailable = false
  let cpuId = 0
  // Trace bindings + entry sizes for the CPU being recorded. Resolved
  // afresh at record-start (the focus selector can move between runs)
  // and held through export so the post-stop convert path uses the same
  // CPU's entry size + converter. The worker gets the same entry size
  // with `start`, so the export chunks it cuts hold whole entries of
  // this CPU.
  let spec: TraceCpuSpec = traceCpuSpec(core, cpuId)

  // Export accumulator state, only relevant during a multi-chunk
  // export. Reset by `handleExportStarted` and torn down on success or
  // error so a follow-up export starts from a clean slate.
  let exportChunks: string[] = []
  let exportTotalChunks = 0
  let exportInProgress = false
  let exportCurrentChunkIndex = 0
  let exportErrorShown = false
  let exportChunkSize = 0
  let exportTotalSize = 0

  try {
    worker = new Worker(new URL('./trace-worker.js', import.meta.url), { type: 'module' })
    worker.onmessage = handleWorkerMessage
    worker.onerror = (e) => {
      console.error('Trace worker error:', e)
      opfsAvailable = false
      updateUI()
    }
    console.log('Trace worker initialized')
  } catch (err) {
    console.warn('Failed to create trace worker:', err)
    opfsAvailable = false
  }

  function setActive(value: boolean): void {
    active = value
  }

  function handleWorkerMessage(e: MessageEvent): void {
    const { type, bytes, data, totalBytes, fileName, opfsAvailable: opfs, message } = e.data

    switch (type) {
      case 'ready':
        opfsAvailable = opfs
        console.log(`Trace worker ready. OPFS available: ${opfs}`)
        updateUI()
        break

      case 'started':
        console.log(`Trace started: ${fileName}`)
        updateUI()
        break

      case 'progress': {
        const entries = Math.floor(bytes / spec.statsEntrySize)
        if (elements.stats) {
          elements.stats.textContent = `${formatNumber(entries)} ${spec.uiLabel} entries`
        }
        break
      }

      case 'stopped':
        console.log(`Trace stopped: ${totalBytes} bytes total`)
        updateUI()
        break

      case 'exportReady': {
        const emulator = bridge.getEmulator()
        if (!emulator) return
        try {
          const text = emulator[spec.convertChunkToText](new Uint8Array(data))
          downloadAsFile(text, `trace_${spec.fileLabel}.txt`, 'text/plain')
          console.log('Trace exported successfully')
        } catch (err) {
          console.error('Failed to convert trace:', err)
          alert('Failed to export trace: ' + (err as Error).message)
        }
        break
      }

      case 'exportStarted':
        handleExportStarted(e.data)
        break

      case 'exportChunk':
        void handleExportChunk(e.data)
        break

      case 'deleted':
        console.log('Trace file deleted')
        if (elements.stats) elements.stats.textContent = '0 entries'
        updateUI()
        break

      case 'error':
        console.error('Trace worker error:', message)
        if (exportInProgress && !exportErrorShown) {
          exportErrorShown = true
          exportInProgress = false
          exportChunks = []
          alert('Export failed: ' + message)
        }
        break
    }
  }

  function handleExportStarted(data: any): void {
    const { totalChunks, totalSize, chunkSize } = data
    exportChunks = []
    exportTotalChunks = totalChunks
    exportCurrentChunkIndex = 0
    exportInProgress = true
    exportErrorShown = false
    exportChunkSize = chunkSize
    exportTotalSize = totalSize

    console.log(`Export started: ${totalChunks} chunks, ${(totalSize / (1024 * 1024)).toFixed(1)} MB total`)
    requestNextChunk()
  }

  function requestNextChunk(): void {
    if (!worker) return
    if (exportCurrentChunkIndex >= exportTotalChunks) {
      console.error('requestNextChunk called but all chunks received')
      return
    }
    console.log(`Requesting chunk ${exportCurrentChunkIndex + 1}/${exportTotalChunks}`)
    worker.postMessage({
      type: 'exportNextChunk',
      chunkIndex: exportCurrentChunkIndex,
      chunkSize: exportChunkSize,
      totalChunks: exportTotalChunks,
      totalSize: exportTotalSize,
    })
  }

  async function handleExportChunk(chunkData: any): Promise<void> {
    const { data, chunkIndex, totalChunks, isComplete } = chunkData
    const emulator = bridge.getEmulator()
    if (!emulator) return

    try {
      const TRACE_ENTRY_SIZE = spec.entrySize
      const numEntries = Math.floor(data.byteLength / TRACE_ENTRY_SIZE)
      const remainder = data.byteLength % TRACE_ENTRY_SIZE

      console.log(`Processing export chunk ${chunkIndex + 1}/${totalChunks}:`)
      console.log(`  - Size: ${data.byteLength} bytes (${(data.byteLength / (1024 * 1024)).toFixed(1)} MB)`)
      console.log(`  - Entries: ${numEntries} complete ${remainder > 0 ? `+ ${remainder} extra bytes (ERROR!)` : ''}`)

      if (remainder > 0 && !isComplete) {
        console.error(`ERROR: Chunk ${chunkIndex + 1} has ${remainder} extra bytes that don't form a complete entry!`)
        throw new Error(`Chunk alignment error: ${remainder} extra bytes`)
      }

      if (elements.stats) {
        elements.stats.textContent = `Converting chunk ${chunkIndex + 1}/${totalChunks} (${numEntries} entries)...`
      }

      // Yield to the event loop so the stats label can repaint before
      // we synchronously convert a multi-MB chunk.
      await new Promise<void>((resolve) => setTimeout(resolve, 0))

      const startTime = performance.now()
      const text = emulator[spec.convertChunkToText](new Uint8Array(data))
      const elapsed = performance.now() - startTime

      exportChunks.push(text)
      exportCurrentChunkIndex++

      console.log(`  - Converted in ${(elapsed / 1000).toFixed(1)}s: ${(text.length / (1024 * 1024)).toFixed(1)} MB text`)

      if (isComplete) {
        console.log(`All ${exportChunks.length} chunks received, combining...`)
        if (elements.stats) elements.stats.textContent = 'Creating download file...'

        await new Promise<void>((resolve) => setTimeout(resolve, 0))

        // Concatenate via Blob rather than `''.join` to keep the intermediate string allocations off the V8 heap for >100 MB traces.
        const blob = new Blob(exportChunks, { type: 'text/plain' })
        const url = URL.createObjectURL(blob)
        const a = document.createElement('a')
        a.href = url
        a.download = `trace_${spec.fileLabel}.txt`
        document.body.appendChild(a)
        a.click()
        document.body.removeChild(a)
        URL.revokeObjectURL(url)

        exportChunks = []
        exportInProgress = false
        console.log('Trace export completed successfully')
        updateUI()
      } else {
        requestNextChunk()
      }
    } catch (err) {
      console.error(`Failed to convert chunk ${chunkIndex}:`, err)
      console.error('Error details:', (err as Error).stack)
      if (!exportErrorShown) {
        exportErrorShown = true
        alert('Failed to export trace: ' + (err as Error).message)
      }
      exportChunks = []
      exportInProgress = false
      updateUI()
    }
  }

  function startTracing(): void {
    const emulator = bridge.getEmulator()
    if (!emulator || !worker || !bridge.isRomLoaded()) {
      console.warn('Cannot start tracing: emulator, worker, or ROM not ready')
      return
    }

    cpuId = bridge.getFocusedCpu()
    spec = traceCpuSpec(core, cpuId)

    worker.postMessage({
      type: 'start',
      fileName: `trace_${spec.fileLabel}_${Date.now()}.bin`,
      entrySize: spec.entrySize,
    })

    emulator[spec.setEnabled](true)
    setActive(true)
    updateUI()
    console.log(`Tracing started for ${spec.uiLabel} (cpu_id=${cpuId})`)
  }

  function stopTracing(): void {
    const emulator = bridge.getEmulator()
    if (!emulator) return

    emulator[spec.setEnabled](false)

    if (emulator[spec.needsFlush]()) {
      const flushData = emulator[spec.getFlushData]()
      if (flushData.length > 0 && worker) {
        worker.postMessage({ type: 'chunk', data: flushData }, [flushData.buffer])
      }
      emulator[spec.clearFlushed]()
    }

    if (worker) worker.postMessage({ type: 'stop' })
    setActive(false)
    updateUI()
    console.log(`Tracing stopped for cpu_id=${cpuId}`)
  }

  function exportTrace(): void {
    if (!worker) {
      console.warn('Trace worker not available')
      return
    }
    if (exportInProgress) {
      console.warn('Export already in progress, ignoring request')
      return
    }
    console.log('Requesting trace export from worker...')
    worker.postMessage({ type: 'export' })
  }

  function clearTrace(): void {
    if (!worker) return
    if (active) stopTracing()
    worker.postMessage({ type: 'delete' })
  }

  function checkFlush(): void {
    if (!active || !worker) return
    const emulator = bridge.getEmulator()
    if (!emulator) return

    try {
      if (emulator[spec.needsFlush]()) {
        const flushData = emulator[spec.getFlushData]()
        if (flushData.length > 0) {
          worker.postMessage({ type: 'chunk', data: flushData }, [flushData.buffer])
          emulator[spec.clearFlushed]()
        }
      }
    } catch (err) {
      console.error('Error flushing trace buffer:', err)
    }
  }

  function updateUI(): void {
    if (!elements.startBtn) return
    const romLoaded = bridge.isRomLoaded()

    const canRecord = romLoaded && opfsAvailable && !active
    const canStop = active
    const canExport = !active && opfsAvailable
    const canClear = !active && opfsAvailable

    ;(elements.startBtn as HTMLButtonElement).disabled = !canRecord
    if (elements.stopBtn) (elements.stopBtn as HTMLButtonElement).disabled = !canStop
    if (elements.exportBtn) (elements.exportBtn as HTMLButtonElement).disabled = !canExport
    if (elements.clearBtn) (elements.clearBtn as HTMLButtonElement).disabled = !canClear

    if (active) {
      elements.startBtn.classList.add('recording')
      elements.stats?.classList.add('recording')
      elements.startBtn.textContent = `⏺ ${spec.uiLabel}`
    } else {
      elements.startBtn.classList.remove('recording')
      elements.stats?.classList.remove('recording')
      elements.startBtn.textContent = '⏺ Record'
    }

    const emulator = bridge.getEmulator()
    if (emulator && active && elements.stats) {
      try {
        const stats = emulator.getTraceStats()
        if (stats) {
          elements.stats.textContent = `${formatNumber(stats.total_entries)} entries`
        }
      } catch {
        // emulator may be mid-frame; skip the stats refresh
      }
    }
  }

  // Wire the toolbar buttons here so `main.ts` doesn't have to know
  // about them, `setupTracing` returns a handle for the few cases
  // (game loop, debugger-api) that need to drive the lifecycle.
  if (elements.startBtn) {
    elements.startBtn.addEventListener('click', () => startTracing())
  }
  if (elements.stopBtn) {
    elements.stopBtn.addEventListener('click', () => stopTracing())
  }
  if (elements.exportBtn) {
    elements.exportBtn.addEventListener('click', () => exportTrace())
  }
  if (elements.clearBtn) {
    elements.clearBtn.addEventListener('click', () => {
      if (confirm('Delete the trace file? This cannot be undone.')) {
        clearTrace()
      }
    })
  }

  return {
    startTracing,
    stopTracing,
    exportTrace,
    clearTrace,
    checkFlush,
    refreshUi: updateUI,
    dispose() {
      if (worker) {
        worker.terminate()
        worker = null
      }
    },
  }
}
