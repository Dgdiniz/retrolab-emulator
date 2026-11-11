/**
 * Generic Audio Channel Panel Component
 * 
 * Works with any audio chip (PSG, YM2612, SNES APU, etc.) by adapting to ChannelType.
 * Data-driven UI that renders different controls based on channel metadata.
 */

export class ChannelPanel {
    constructor(channelId, initialState) {
        this.id = channelId;
        this.state = initialState;
        this.element = this.createElement();
        this.canvas = this.element.querySelector('.waveform-canvas');
        this.ctx = this.canvas ? this.canvas.getContext('2d') : null;
        this.onMuteChange = null; // Callback
    }

    /**
     * Create DOM element based on channel state.
     */
    createElement() {
        const div = document.createElement('div');
        div.className = 'audio-channel';
        div.dataset.channel = this.id;
        div.dataset.type = this.state.channel_type;
        div.dataset.enabled = this.state.enabled;

        div.innerHTML = `
            <div class="channel-header">
                <span class="channel-name">${this.getChannelName()}</span>
                <label class="mute-toggle">
                    <input type="checkbox" ${this.state.muted ? 'checked' : ''}>
                    Mute
                </label>
            </div>
            <div class="channel-body">
                ${this.createParamsHTML()}
                <canvas class="waveform-canvas" width="200" height="60"></canvas>
            </div>
        `;

        // Attach event listeners
        const muteCheckbox = div.querySelector('.mute-toggle input');
        muteCheckbox.addEventListener('change', (e) => {
            if (this.onMuteChange) {
                this.onMuteChange(this.id, e.target.checked);
            }
        });

        return div;
    }

    /**
     * Get channel display name based on type and ID.
     */
    getChannelName() {
        const type = this.state.channel_type;

        switch (type) {
            case 'Tone': return `Tone ${this.id}`;
            case 'Noise': return `Noise`;
            case 'FM': return `FM ${this.id + 1}`;
            case 'PCM': return `PCM ${this.id}`;
            case 'Sample': return `Sample ${this.id}`;
            default: return `Channel ${this.id}`;
        }
    }

    /**
     * Create parameter HTML based on channel type (data-driven).
     */
    createParamsHTML() {
        const type = this.state.channel_type;

        // Tone and FM channels
        if (type === 'Tone' || type === 'FM') {
            let html = `
                <div class="param">
                    <span class="label">Frequency:</span>
                    <span class="value freq-value">-- Hz</span>
                </div>
            `;

            // FM-specific (future)
            if (type === 'FM' && this.state.envelope) {
                html += `
                    <div class="param">
                        <span class="label">Algorithm:</span>
                        <span class="value algo-value">--</span>
                    </div>
                `;
            }

            html += `
                <div class="param">
                    <span class="label">Volume:</span>
                    <span class="value vol-value">--</span>
                </div>
                <div class="volume-bar"><div class="fill" style="width: 0%"></div></div>
            `;

            return html;
        }

        // Noise channels
        if (type === 'Noise') {
            return `
                <div class="param">
                    <span class="label">Mode:</span>
                    <span class="value mode-value">--</span>
                </div>
                <div class="param">
                    <span class="label">Shift Rate:</span>
                    <span class="value rate-value">--</span>
                </div>
                <div class="param">
                    <span class="label">Volume:</span>
                    <span class="value vol-value">--</span>
                </div>
                <div class="volume-bar"><div class="fill" style="width: 0%"></div></div>
            `;
        }

        // PCM/Sample channels (future)
        if (type === 'PCM' || type === 'Sample') {
            return `
                <div class="param">
                    <span class="label">Sample Rate:</span>
                    <span class="value rate-value">-- Hz</span>
                </div>
                <div class="param">
                    <span class="label">Volume:</span>
                    <span class="value vol-value">--</span>
                </div>
                <div class="volume-bar"><div class="fill" style="width: 0%"></div></div>
            `;
        }

        // Generic fallback
        return `
            <div class="param">
                <span class="label">State:</span>
                <span class="value">Active</span>
            </div>
        `;
    }

    /**
     * Update channel state and UI (generic).
     */
    update(state) {
        if (!state) return;

        this.state = state;

        // Update enabled/disabled state
        this.element.dataset.enabled = state.enabled;

        // Update mute checkbox
        const muteCheckbox = this.element.querySelector('.mute-toggle input');
        if (muteCheckbox && muteCheckbox.checked !== state.muted) {
            muteCheckbox.checked = state.muted;
        }

        // Update parameters based on channel type
        const type = state.channel_type;

        if (type === 'Tone' || type === 'FM') {
            this.updateElement('.freq-value', `${Math.round(state.frequency)} Hz`);
            this.updateElement('.vol-value', `Atten: ${state.attenuation}`);
            this.updateVolumeBar(state.volume);
        } else if (type === 'Noise') {
            const noiseParams = state.noise_params;
            if (noiseParams) {
                this.updateElement('.mode-value', noiseParams.mode);
                this.updateElement('.rate-value', noiseParams.shift_rate);
            }
            this.updateElement('.vol-value', `Atten: ${state.attenuation}`);
            this.updateVolumeBar(state.volume);
        } else if (type === 'PCM' || type === 'Sample') {
            this.updateElement('.rate-value', `${state.frequency} Hz`);
            this.updateElement('.vol-value', `Vol: ${Math.round(state.volume * 100)}%`);
            this.updateVolumeBar(state.volume);
        }

        // Draw waveform (generic)
        this.drawWaveform();
    }

    /**
     * Update a specific element's text content.
     */
    updateElement(selector, text) {
        const el = this.element.querySelector(selector);
        if (el) el.textContent = text;
    }

    /**
     * Update volume bar fill.
     */
    updateVolumeBar(volume) {
        const fill = this.element.querySelector('.volume-bar .fill');
        if (fill) {
            const percentage = Math.round(volume * 100);
            fill.style.width = `${percentage}%`;
        }
    }

    /**
     * Draw waveform on canvas (generic).
     */
    drawWaveform(waveformData = null) {
        if (!this.ctx) return;

        const ctx = this.ctx;
        const width = this.canvas.width;
        const height = this.canvas.height;
        const midY = height / 2;

        // Clear
        ctx.fillStyle = '#1a1a1a';
        ctx.fillRect(0, 0, width, height);

        // Draw center line
        ctx.strokeStyle = '#333';
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.moveTo(0, midY);
        ctx.lineTo(width, midY);
        ctx.stroke();

        // If no waveform data provided, just show current output as a line
        if (!waveformData || waveformData.length === 0) {
            const output = this.state.current_output;
            const y = midY - (output * midY);

            ctx.strokeStyle = this.state.enabled && !this.state.muted ? '#00ff88' : '#666';
            ctx.lineWidth = 2;
            ctx.beginPath();
            ctx.moveTo(0, y);
            ctx.lineTo(width, y);
            ctx.stroke();
            return;
        }

        // Draw waveform from data
        const color = this.state.enabled && !this.state.muted ? '#00ff88' : '#666';
        ctx.strokeStyle = color;
        ctx.lineWidth = 1.5;
        ctx.beginPath();

        for (let i = 0; i < waveformData.length; i++) {
            const x = (i / waveformData.length) * width;
            const y = midY - (waveformData[i] * midY);

            if (i === 0) ctx.moveTo(x, y);
            else ctx.lineTo(x, y);
        }

        ctx.stroke();
    }
}

