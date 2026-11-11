/**
 * Audio Manager for Genesis Emulator
 * 
 * Manages Web Audio API AudioContext and AudioWorklet integration.
 * Handles sample buffering and synchronization between emulator and audio output.
 */

export class AudioManager {
    constructor() {
        this.audioContext = null;
        this.audioWorklet = null;
        this.emulator = null;
        this.isRunning = false;
        this.isInitialized = false;

        // Stats
        this.underruns = 0;
        this.drops = 0;
        this.lastBufferLevel = 0;
    }

    /**
     * Initialize audio system (requires user gesture).
     * 
     * @param {Object} emulator - The WASM emulator instance
     * @returns {Promise<void>}
     */
    async init(emulator) {
        if (this.isInitialized) {
            console.warn('Audio already initialized');
            return;
        }

        this.emulator = emulator;

        try {
            // Create AudioContext (Safari might need webkitAudioContext)
            this.audioContext = new (window.AudioContext || window.webkitAudioContext)({
                sampleRate: 44100,
                latencyHint: 'interactive' // Balance between latency and glitch prevention
            });

            console.log(`AudioContext created: ${this.audioContext.sampleRate}Hz, state=${this.audioContext.state}`);

            // Load AudioWorklet module. We serve the file from `public/` so
            // Vite (and any future bundler) never transforms it, the
            // AudioWorkletGlobalScope is extremely restricted (no
            // `document`, no `import.meta.env`, etc.) and Vite-injected
            // wrappers would silently break the worklet. Resolving from
            // `import.meta.env.BASE_URL` keeps it correct in both dev
            // (served from `/`) and the GitHub Pages deploy (under
            // `/retrolab-emulator/`).
            try {
                const workletUrl = new URL(
                    import.meta.env.BASE_URL + 'audio-worklet-processor.js',
                    location.origin
                );
                await this.audioContext.audioWorklet.addModule(workletUrl);
                console.log('AudioWorklet module loaded successfully');
            } catch (err) {
                console.error('Failed to load AudioWorklet module:', err);
                throw new Error('AudioWorklet not supported or module failed to load');
            }

            // Create worklet node with stereo output (2 channels)
            this.audioWorklet = new AudioWorkletNode(
                this.audioContext,
                'genesis-audio-processor',
                { outputChannelCount: [2] }
            );

            // Connect to audio destination (speakers)
            this.audioWorklet.connect(this.audioContext.destination);

            // Handle messages from AudioWorklet
            this.audioWorklet.port.onmessage = (event) => {
                this.handleWorkletMessage(event.data);
            };

            this.isInitialized = true;

            console.log('Audio initialized successfully');
            console.log(`Base latency: ${(this.audioContext.baseLatency * 1000).toFixed(2)}ms`);
            console.log(`Output latency: ${(this.audioContext.outputLatency * 1000).toFixed(2)}ms`);

        } catch (err) {
            console.error('Failed to initialize audio:', err);
            throw err;
        }
    }

    /**
     * Handle messages from AudioWorklet processor.
     */
    handleWorkletMessage(data) {
        if (data.type === 'needSamples') {
            // AudioWorklet needs more samples
            this.sendSamples();
            this.lastBufferLevel = data.bufferLevel || 0;
        } else if (data.type === 'stats') {
            // Periodic stats from AudioWorklet
            this.underruns = data.underruns || 0;
            this.drops = data.drops || 0;
            this.lastBufferLevel = data.bufferLevel || 0;

            // Log if there are underruns
            if (this.underruns > 0 && this.underruns % 100 === 0) {
                console.warn(`Audio underruns: ${this.underruns}`);
            }
        }
    }

    /**
     * Send audio samples from emulator to AudioWorklet.
     *
     * Prefers `getAudioSamplesN(4096)` so each drain only pulls the
     * worklet's `bufferSize / 4` "needSamples" lookahead instead of
     * emptying the producer ring. `4096` matches the AudioWorklet's
     * `bufferSize / 4` trigger so refills slot in evenly. The
     * `getAudioSamples()` fallback exists for older WASM builds that
     * predate `getAudioSamplesN`; both APIs drain destructively so
     * the host must NOT call `clearAudioBuffer()` between drains.
     */
    sendSamples() {
        if (!this.emulator || !this.isRunning || !this.audioWorklet) {
            return;
        }

        try {
            const samples = this.emulator.getAudioSamplesN
                ? this.emulator.getAudioSamplesN(4096)
                : this.emulator.getAudioSamples();

            if (samples && samples.length > 0) {
                this.audioWorklet.port.postMessage({
                    type: 'samples',
                    samples: samples
                });
            }
        } catch (err) {
            console.error('Failed to send audio samples:', err);
        }
    }

    /**
     * Start audio playback.
     */
    start() {
        if (!this.isInitialized) {
            console.warn('Audio not initialized yet');
            return;
        }

        // Resume AudioContext if suspended
        if (this.audioContext.state === 'suspended') {
            this.audioContext.resume().then(() => {
                console.log('AudioContext resumed');
            });
        }

        this.isRunning = true;

        // Prime the audio buffer with initial samples
        this.sendSamples();
    }

    /**
     * Stop audio playback.
     */
    stop() {
        this.isRunning = false;

        // Suspend AudioContext to save power
        if (this.audioContext && this.audioContext.state === 'running') {
            this.audioContext.suspend().then(() => {
                console.log('AudioContext suspended');
            });
        }
    }

    /**
     * Reset audio buffer.
     */
    reset() {
        if (this.audioWorklet) {
            this.audioWorklet.port.postMessage({ type: 'reset' });
        }
        this.underruns = 0;
        this.drops = 0;
        this.lastBufferLevel = 0;
    }

    /**
     * Get total audio latency in milliseconds.
     * 
     * @returns {number} Total latency (base + output) in ms
     */
    getLatency() {
        if (!this.audioContext) return 0;

        const baseLatency = this.audioContext.baseLatency || 0;
        const outputLatency = this.audioContext.outputLatency || 0;

        return (baseLatency + outputLatency) * 1000; // Convert to ms
    }

    /**
     * Get audio buffer fill level (0.0 to 1.0).
     * 
     * @returns {number} Buffer fill percentage
     */
    getBufferFillLevel() {
        return Math.min(1.0, this.lastBufferLevel / 8192); // Match worklet buffer size
    }

    /** Ring slots the worklet overwrote (drop-oldest) since it started. */
    getDrops() {
        return this.drops;
    }

    getUnderruns() {
        return this.underruns;
    }

    /**
     * Get AudioContext state.
     * 
     * @returns {string} 'running', 'suspended', or 'closed'
     */
    getState() {
        return this.audioContext ? this.audioContext.state : 'closed';
    }

    /**
     * Clean up audio resources.
     */
    destroy() {
        this.stop();

        if (this.audioWorklet) {
            this.audioWorklet.disconnect();
            this.audioWorklet = null;
        }

        if (this.audioContext) {
            this.audioContext.close();
            this.audioContext = null;
        }

        this.isInitialized = false;
    }
}

