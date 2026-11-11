/**
 * Trace Worker - Handles binary trace streaming to OPFS (Origin Private File System).
 * 
 * This worker receives binary trace chunks from the main thread and writes them
 * to a file in OPFS, allowing for traces of several GB without memory issues.
 * 
 * Message Types:
 * - start: Initialize a new trace file
 * - chunk: Write a binary chunk to the file
 * - stop: Close the file and finalize writing
 * - export: Read the file and send it back for conversion
 * - delete: Delete the trace file
 * - getInfo: Get information about the current trace file
 */

// Bytes per entry of the trace being recorded. It depends on the CPU, so
// the main thread sends it with `start`, taken from that CPU's trace spec;
// zero until a trace starts. The export aligns the file size and every
// chunk boundary to it.
let TRACE_ENTRY_SIZE = 0;

let fileHandle = null;
let writableStream = null;
let totalBytes = 0;
let fileName = 'trace.bin';
let isWriting = false;
let isExporting = false;

/**
 * Get the retrolab/traces/ directory in OPFS.
 */
async function getTracesDir() {
    const root = await navigator.storage.getDirectory();
    const retrolab = await root.getDirectoryHandle('retrolab', { create: true });
    return retrolab.getDirectoryHandle('traces', { create: true });
}

/**
 * Check if OPFS is available.
 */
async function isOpfsAvailable() {
    try {
        const root = await navigator.storage.getDirectory();
        return !!root;
    } catch (e) {
        return false;
    }
}

/**
 * Start a new trace file.
 */
async function startTrace(customFileName) {
    try {
        if (writableStream) {
            // Close existing stream first
            await writableStream.close();
            writableStream = null;
        }

        fileName = customFileName || `trace_${Date.now()}.bin`;
        totalBytes = 0;
        
        const tracesDir = await getTracesDir();
        fileHandle = await tracesDir.getFileHandle(fileName, { create: true });
        writableStream = await fileHandle.createWritable();
        
        self.postMessage({ 
            type: 'started',
            fileName,
            timestamp: Date.now()
        });
    } catch (error) {
        self.postMessage({ 
            type: 'error', 
            message: `Failed to start trace: ${error.message}`,
            operation: 'start'
        });
    }
}

/**
 * Write a binary chunk to the trace file.
 */
async function writeChunk(data) {
    if (!writableStream) {
        self.postMessage({ 
            type: 'error', 
            message: 'No trace file open. Call start first.',
            operation: 'chunk'
        });
        return;
    }

    if (isWriting) {
        // Queue the write - shouldn't happen often with proper flow control
        console.warn('Trace worker: Write already in progress, queuing...');
    }

    try {
        isWriting = true;
        await writableStream.write(data);
        totalBytes += data.byteLength;
        
        self.postMessage({ 
            type: 'progress', 
            bytes: totalBytes,
            chunkSize: data.byteLength
        });
    } catch (error) {
        self.postMessage({ 
            type: 'error', 
            message: `Failed to write chunk: ${error.message}`,
            operation: 'chunk'
        });
    } finally {
        isWriting = false;
    }
}

/**
 * Stop tracing and close the file.
 */
async function stopTrace() {
    try {
        if (writableStream) {
            await writableStream.close();
            writableStream = null;
        }
        
        self.postMessage({ 
            type: 'stopped', 
            totalBytes,
            fileName
        });
    } catch (error) {
        self.postMessage({ 
            type: 'error', 
            message: `Failed to stop trace: ${error.message}`,
            operation: 'stop'
        });
    }
}

/**
 * Export the trace file for download.
 * For large files, this reads in chunks and sends them back.
 * Uses a pull-based approach: sends one chunk at a time, waits for acknowledgment.
 * 
 * IMPORTANT: Chunk size must be a multiple of TRACE_ENTRY_SIZE to avoid 
 * splitting entries across chunks, which would corrupt the trace.
 */
async function exportTrace(chunkSize = 64 * 1024 * 1024) { // 64MB chunks
    // Prevent multiple simultaneous exports
    if (isExporting) {
        console.warn('[Trace Worker] Export already in progress, ignoring request');
        self.postMessage({ 
            type: 'error', 
            message: 'Export already in progress',
            operation: 'export'
        });
        return;
    }
    
    isExporting = true;
    
    try {
        if (TRACE_ENTRY_SIZE === 0) {
            // Without the entry size of the CPU recorded the file cannot be
            // cut at entry boundaries.
            self.postMessage({
                type: 'error',
                message: 'No trace has been recorded since the page loaded',
                operation: 'export'
            });
            isExporting = false;
            return;
        }

        if (!fileHandle) {
            const tracesDir = await getTracesDir();
            fileHandle = await tracesDir.getFileHandle(fileName, { create: false });
        }

        const file = await fileHandle.getFile();
        let fileSize = file.size;
        
        // Verify file is a multiple of TRACE_ENTRY_SIZE, truncate if not
        const remainder = fileSize % TRACE_ENTRY_SIZE;
        if (remainder !== 0) {
            console.warn(`[Trace Worker] File size ${fileSize} is not a multiple of ${TRACE_ENTRY_SIZE}`);
            console.warn(`[Trace Worker] Truncating ${remainder} extra bytes to align to entry boundaries`);
            fileSize -= remainder;
        }
        
        console.log(`[Trace Worker] Exporting file: ${fileName}, size: ${fileSize} bytes (${(fileSize / (1024 * 1024)).toFixed(1)} MB)`);
        
        if (fileSize === 0) {
            self.postMessage({ 
                type: 'error', 
                message: 'Trace file is empty',
                operation: 'export'
            });
            isExporting = false;
            return;
        }

        // For smaller files, send all at once
        if (fileSize <= chunkSize) {
            console.log(`[Trace Worker] File fits in single chunk, reading entire file...`);
            const arrayBuffer = await file.arrayBuffer();
            console.log(`[Trace Worker] Read ${arrayBuffer.byteLength} bytes, sending as exportReady`);
            self.postMessage({ 
                type: 'exportReady', 
                data: arrayBuffer,
                totalSize: fileSize,
                isComplete: true
            }, [arrayBuffer]);
            isExporting = false;
            return;
        }

        // For larger files, align chunk size to trace entry boundaries
        const entriesPerChunk = Math.floor(chunkSize / TRACE_ENTRY_SIZE);
        const alignedChunkSize = entriesPerChunk * TRACE_ENTRY_SIZE;
        
        console.log(`[Trace Worker] Aligning chunks: ${entriesPerChunk} entries = ${alignedChunkSize} bytes per chunk`);
        
        // Store file info and send metadata
        const totalChunks = Math.ceil(fileSize / alignedChunkSize);
        console.log(`[Trace Worker] File requires ${totalChunks} chunks`);
        console.log(`[Trace Worker] Sending metadata, main thread will request chunks...`);
        
        // Send metadata first
        self.postMessage({ 
            type: 'exportStarted',
            totalChunks,
            totalSize: fileSize,
            chunkSize: alignedChunkSize
        });
        
        // Note: We don't send any data yet. Main thread will request chunks one by one.
        isExporting = false; // Allow subsequent chunk requests
        
    } catch (error) {
        console.error(`[Trace Worker] Export failed:`, error);
        self.postMessage({ 
            type: 'error', 
            message: `Failed to export trace: ${error.message}`,
            operation: 'export'
        });
        isExporting = false;
    }
}

/**
 * Export a specific chunk by index.
 * Ensures chunks are aligned to trace entry boundaries.
 */
async function exportChunk(chunkIndex, chunkSize, totalChunks, totalSize) {
    try {
        console.log(`[Trace Worker] Exporting chunk ${chunkIndex + 1}/${totalChunks}`);
        
        if (!fileHandle) {
            const tracesDir = await getTracesDir();
            fileHandle = await tracesDir.getFileHandle(fileName, { create: false });
        }

        // Re-get the file handle each time to avoid stale references
        const file = await fileHandle.getFile();
        const offset = chunkIndex * chunkSize;
        const chunkEnd = Math.min(offset + chunkSize, totalSize);
        
        console.log(`[Trace Worker] Reading from offset ${offset} to ${chunkEnd} (size: ${chunkEnd - offset})`);
        
        // Verify alignment with trace entries
        if (offset % TRACE_ENTRY_SIZE !== 0) {
            console.error(`[Trace Worker] ERROR: Chunk offset ${offset} is not aligned to ${TRACE_ENTRY_SIZE} byte boundaries!`);
        }
        
        const chunk = file.slice(offset, chunkEnd);
        const arrayBuffer = await chunk.arrayBuffer();
        const isLastChunk = chunkEnd >= totalSize;
        
        // Verify chunk size is multiple of entry size (except last chunk)
        if (!isLastChunk && arrayBuffer.byteLength % TRACE_ENTRY_SIZE !== 0) {
            console.error(`[Trace Worker] ERROR: Chunk size ${arrayBuffer.byteLength} is not a multiple of ${TRACE_ENTRY_SIZE}!`);
        }
        
        const numEntries = Math.floor(arrayBuffer.byteLength / TRACE_ENTRY_SIZE);
        
        // Log the first 16 bytes for debugging. They are not decoded: the
        // layout of an entry depends on the CPU traced.
        const firstBytes = new Uint8Array(arrayBuffer, 0, Math.min(16, arrayBuffer.byteLength));
        const firstBytesHex = Array.from(firstBytes).map(b => b.toString(16).padStart(2, '0')).join(' ');
        console.log(`[Trace Worker] First 16 bytes: ${firstBytesHex}`);
        
        console.log(`[Trace Worker] Chunk ${chunkIndex + 1}/${totalChunks}: ${arrayBuffer.byteLength} bytes (${numEntries} entries), isComplete: ${isLastChunk}`);
        
        self.postMessage({ 
            type: 'exportChunk',
            data: arrayBuffer,
            chunkIndex,
            totalChunks,
            offset,
            totalSize,
            isComplete: isLastChunk
        }, [arrayBuffer]);
        
    } catch (error) {
        console.error(`[Trace Worker] Failed to export chunk ${chunkIndex}:`, error);
        self.postMessage({ 
            type: 'error', 
            message: `Failed to export chunk ${chunkIndex}: ${error.message}`,
            operation: 'exportChunk'
        });
    }
}

/**
 * Delete the trace file.
 */
async function deleteTrace() {
    try {
        if (writableStream) {
            await writableStream.close();
            writableStream = null;
        }
        
        const tracesDir = await getTracesDir();
        await tracesDir.removeEntry(fileName);
        fileHandle = null;
        totalBytes = 0;
        
        self.postMessage({ type: 'deleted', fileName });
    } catch (error) {
        // File might not exist, which is fine
        if (error.name !== 'NotFoundError') {
            self.postMessage({ 
                type: 'error', 
                message: `Failed to delete trace: ${error.message}`,
                operation: 'delete'
            });
        } else {
            self.postMessage({ type: 'deleted', fileName });
        }
    }
}

/**
 * Get information about the current trace file.
 */
async function getTraceInfo() {
    try {
        const opfsAvailable = await isOpfsAvailable();
        
        let fileInfo = null;
        if (fileHandle) {
            try {
                const file = await fileHandle.getFile();
                fileInfo = {
                    name: file.name,
                    size: file.size,
                    lastModified: file.lastModified
                };
            } catch (e) {
                // File handle might be stale
                fileHandle = null;
            }
        }
        
        self.postMessage({ 
            type: 'info',
            opfsAvailable,
            isRecording: !!writableStream,
            totalBytesWritten: totalBytes,
            currentFile: fileInfo
        });
    } catch (error) {
        self.postMessage({ 
            type: 'error', 
            message: `Failed to get trace info: ${error.message}`,
            operation: 'getInfo'
        });
    }
}

/**
 * List all trace files in OPFS.
 */
async function listTraceFiles() {
    try {
        const tracesDir = await getTracesDir();
        const files = [];
        
        for await (const [name, handle] of tracesDir.entries()) {
            if (handle.kind === 'file' && name.endsWith('.bin')) {
                try {
                    const file = await handle.getFile();
                    files.push({
                        name,
                        size: file.size,
                        lastModified: file.lastModified
                    });
                } catch (e) {
                    // Skip files we can't access
                }
            }
        }
        
        self.postMessage({ type: 'fileList', files });
    } catch (error) {
        self.postMessage({ 
            type: 'error', 
            message: `Failed to list files: ${error.message}`,
            operation: 'list'
        });
    }
}

/**
 * Message handler.
 */
self.onmessage = async (e) => {
    const { type, data, fileName: customFileName, chunkSize, chunkIndex, totalChunks, totalSize, entrySize } = e.data;
    
    switch (type) {
        case 'start':
            if (!Number.isInteger(entrySize) || entrySize <= 0) {
                self.postMessage({
                    type: 'error',
                    message: `Cannot start a trace without its entry size (got ${entrySize})`,
                    operation: 'start'
                });
                break;
            }
            TRACE_ENTRY_SIZE = entrySize;
            await startTrace(customFileName);
            break;
            
        case 'chunk':
            await writeChunk(data);
            break;
            
        case 'stop':
            await stopTrace();
            break;
            
        case 'export':
            await exportTrace(chunkSize);
            break;
            
        case 'exportNextChunk':
            await exportChunk(chunkIndex, chunkSize, totalChunks, totalSize);
            break;
            
        case 'delete':
            await deleteTrace();
            break;
            
        case 'getInfo':
            await getTraceInfo();
            break;
            
        case 'list':
            await listTraceFiles();
            break;
            
        default:
            self.postMessage({ 
                type: 'error', 
                message: `Unknown message type: ${type}`,
                operation: type
            });
    }
};

// Initial check for OPFS availability
isOpfsAvailable().then(available => {
    self.postMessage({ 
        type: 'ready',
        opfsAvailable: available
    });
});

