/**
 * AudioWorklet Processor for Genesis Emulator
 * 
 * Runs in a separate audio thread to process samples from the emulator.
 * Uses a circular buffer to handle timing mismatches between
 * emulator frame rate and audio callback rate.
 */

class GenesisAudioProcessor extends AudioWorkletProcessor {
    constructor() {
        super();

        // Circular buffer - stereo interleaved (L,R,L,R...)
        // ~186ms @ 44.1kHz stereo (enough for ~11 frames @ 60fps)
        // Larger buffer absorbs frame timing jitter and tab-switch pauses
        // without needing to drop samples under normal operation.
        this.bufferSize = 16384;
        this.buffer = new Float32Array(this.bufferSize);
        this.writePosition = 0;
        this.readPosition = 0;
        this.samplesInBuffer = 0;

        // Stats for monitoring
        this.underruns = 0;
        this.drops = 0;
        this.totalSamplesProcessed = 0;

        // Stats go out twice a second on the worklet's own clock
        // (`currentFrame` / `sampleRate` are globals of the
        // AudioWorkletGlobalScope). A count-based cadence would fire only when
        // the processed total landed exactly on a multiple at the end of a
        // 128-frame quantum, once every ~32 s.
        this.statsIntervalFrames = Math.max(128, Math.floor(sampleRate / 2));
        this.lastStatsBucket = -1;

        // Last output samples - used to hold value on underrun instead of
        // jumping to zero, which would create an audible click
        this.lastSampleL = 0;
        this.lastSampleR = 0;

        // Listen for messages from main thread
        this.port.onmessage = this.handleMessage.bind(this);
    }

    handleMessage(event) {
        if (event.data.type === 'samples') {
            this.writeSamples(event.data.samples);
        } else if (event.data.type === 'prime') {
            this.prime(event.data.targetSlots);
        } else if (event.data.type === 'reset') {
            this.reset();
        }
    }

    /**
     * Top the ring up to `targetSlots` with silence so playback starts with a
     * cushion instead of on empty. Producer and consumer run at the same rate,
     * so whatever lead the ring has at the first quantum is the lead it keeps.
     * Only ever adds, so it is safe to call on every resume.
     */
    prime(targetSlots) {
        const target = Math.min(Number(targetSlots) || 0, this.bufferSize);
        let missing = target - this.samplesInBuffer;
        if (missing <= 0) return;
        missing -= missing % 2; // keep L/R pairs aligned
        for (let i = 0; i < missing; i++) {
            this.buffer[this.writePosition] = 0;
            this.writePosition = (this.writePosition + 1) % this.bufferSize;
            this.samplesInBuffer++;
        }
    }

    writeSamples(samples) {
        if (!samples || samples.length === 0) return;

        // If incoming data would overflow, discard oldest data in one batch
        // (advancing the read pointer in bulk) to keep stereo alignment and
        // avoid per-sample branch overhead.
        const spaceAvailable = this.bufferSize - this.samplesInBuffer;
        if (samples.length > spaceAvailable) {
            const excess = samples.length - spaceAvailable;
            // Round up to even count to maintain stereo pair alignment
            const drop = excess + (excess & 1);
            this.readPosition = (this.readPosition + drop) % this.bufferSize;
            this.samplesInBuffer -= drop;
            this.drops += drop;
        }

        // Write all samples into the circular buffer
        for (let i = 0; i < samples.length; i++) {
            this.buffer[this.writePosition] = samples[i];
            this.writePosition = (this.writePosition + 1) % this.bufferSize;
            this.samplesInBuffer++;
        }
    }

    process(inputs, outputs, parameters) {
        const output = outputs[0];

        if (!output || output.length === 0) {
            return true;
        }

        // Genesis outputs stereo interleaved (L, R, L, R, ...)
        const channel0 = output[0]; // Left
        const channel1 = output.length > 1 ? output[1] : null; // Right

        // Fill output from circular buffer (reading stereo pairs)
        for (let i = 0; i < channel0.length; i++) {
            if (this.samplesInBuffer >= 2) {
                // Read stereo pair (L, R)
                this.lastSampleL = this.buffer[this.readPosition];
                this.readPosition = (this.readPosition + 1) % this.bufferSize;
                this.lastSampleR = this.buffer[this.readPosition];
                this.readPosition = (this.readPosition + 1) % this.bufferSize;
                this.samplesInBuffer -= 2;
                this.totalSamplesProcessed++;
            } else {
                // Buffer underrun - hold last sample to avoid click from
                // sudden jump to zero. The held value will naturally decay
                // when new samples arrive.
                this.underruns++;
            }

            channel0[i] = this.lastSampleL;
            if (channel1) {
                channel1[i] = this.lastSampleR;
            }
        }

        // Request more samples if buffer is running low
        // Target: keep buffer at least 25% full (~46ms) for low latency
        if (this.samplesInBuffer < this.bufferSize / 4) {
            this.port.postMessage({
                type: 'needSamples',
                bufferLevel: this.samplesInBuffer,
                bufferSize: this.bufferSize
            });
        }

        // Send stats twice a second, on the worklet clock
        const bucket = Math.floor(currentFrame / this.statsIntervalFrames);
        if (bucket !== this.lastStatsBucket) {
            this.lastStatsBucket = bucket;
            this.port.postMessage({
                type: 'stats',
                underruns: this.underruns,
                drops: this.drops,
                bufferLevel: this.samplesInBuffer,
                bufferSize: this.bufferSize,
                processed: this.totalSamplesProcessed,
                timeSec: currentFrame / sampleRate
            });
        }

        return true; // Keep processor alive
    }

    reset() {
        this.writePosition = 0;
        this.readPosition = 0;
        this.samplesInBuffer = 0;
        this.underruns = 0;
        this.drops = 0;
        this.lastStatsBucket = -1;
        this.buffer.fill(0);
        this.lastSampleL = 0;
        this.lastSampleR = 0;
    }
}

// Register the processor with the Audio Worklet
registerProcessor('genesis-audio-processor', GenesisAudioProcessor);

