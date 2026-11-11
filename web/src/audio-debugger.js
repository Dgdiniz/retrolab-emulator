/**
 * Generic Audio Debugger Component
 * 
 * Data-driven audio debugging UI that works with any audio chip.
 * Automatically adapts to chip capabilities by reading metadata.
 */

import { ChannelPanel } from './audio-channel.js';

export class AudioDebugger {
    constructor(emulator, containerId) {
        this.emulator = emulator;
        this.container = document.getElementById(containerId);
        this.chipInfo = null;
        this.channels = [];
        this.updateInterval = null;
        this.updateRate = 30; // Hz
        this.mixedCanvas = null;
        this.mixedCtx = null;
        this.enabled = false;
    }

    /**
     * Initialize audio debugger (data-driven).
     */
    async init() {
        if (!this.container) {
            console.error('Audio debugger container not found');
            return;
        }

        try {
            // Get chip metadata (generic)
            this.chipInfo = this.emulator.getAudioChipInfo();
            console.log('Audio chip detected:', this.chipInfo);

            // Create UI based on chip metadata
            this.createUI();

            console.log('Audio debugger initialized');
        } catch (err) {
            console.error('Failed to initialize audio debugger:', err);
        }
    }

    /**
     * Create UI structure (data-driven).
     */
    createUI() {
        this.container.innerHTML = '';
        this.container.className = 'audio-debugger';

        // Header with chip info
        const header = this.createHeader();
        this.container.appendChild(header);

        // Channels grid
        const grid = document.createElement('div');
        grid.className = 'channels-grid';
        grid.id = 'audio-channels-grid';
        this.container.appendChild(grid);

        // Create channel panels (data-driven)
        const channelCount = this.emulator.getAudioChannelCount();
        for (let i = 0; i < channelCount; i++) {
            try {
                const state = this.emulator.getAudioChannelState(i);
                if (state) {
                    const panel = new ChannelPanel(i, state);
                    panel.onMuteChange = (ch, muted) => this.handleMuteChange(ch, muted);
                    this.channels.push(panel);
                    grid.appendChild(panel.element);
                }
            } catch (err) {
                console.error(`Failed to create channel ${i} panel:`, err);
            }
        }

        // Mixed waveform viewer
        const mixedSection = this.createMixedWaveform();
        this.container.appendChild(mixedSection);
    }

    /**
     * Create header with chip metadata.
     */
    createHeader() {
        const header = document.createElement('div');
        header.className = 'audio-debugger-header';

        const name = document.createElement('span');
        name.className = 'chip-name';
        name.textContent = this.chipInfo.name;

        const stats = document.createElement('span');
        stats.className = 'chip-stats';
        stats.textContent = `${this.chipInfo.channels} channels • ${this.chipInfo.sample_rate} Hz • ${this.chipInfo.manufacturer}`;

        header.appendChild(name);
        header.appendChild(stats);

        return header;
    }

    /**
     * Create mixed waveform section.
     */
    createMixedWaveform() {
        const section = document.createElement('div');
        section.className = 'mixed-waveform';

        const title = document.createElement('h3');
        title.textContent = 'Mixed Output';

        this.mixedCanvas = document.createElement('canvas');
        this.mixedCanvas.className = 'mixed-canvas';
        this.mixedCanvas.width = 800;
        this.mixedCanvas.height = 120;
        this.mixedCtx = this.mixedCanvas.getContext('2d');

        section.appendChild(title);
        section.appendChild(this.mixedCanvas);

        return section;
    }

    /**
     * Handle mute checkbox change.
     */
    handleMuteChange(channel, muted) {
        try {
            this.emulator.muteAudioChannel(channel, muted);
            console.log(`Channel ${channel} ${muted ? 'muted' : 'unmuted'}`);
        } catch (err) {
            console.error(`Failed to mute channel ${channel}:`, err);
        }
    }

    /**
     * Start update loop.
     */
    start() {
        if (this.updateInterval) return;

        this.enabled = true;
        this.updateInterval = setInterval(() => {
            this.update();
        }, 1000 / this.updateRate);

        console.log(`Audio debugger started (${this.updateRate} Hz)`);
    }

    /**
     * Stop update loop.
     */
    stop() {
        if (!this.updateInterval) return;

        clearInterval(this.updateInterval);
        this.updateInterval = null;
        this.enabled = false;

        console.log('Audio debugger stopped');
    }

    /**
     * Update all channels and waveforms (generic).
     */
    update() {
        if (!this.enabled) return;

        try {
            // Update each channel panel
            for (const panel of this.channels) {
                const state = this.emulator.getAudioChannelState(panel.id);
                if (state) {
                    panel.update(state);
                }
            }

            // Update mixed waveform
            this.updateMixedWaveform();
        } catch (err) {
            // Silently fail to avoid spam (emulator might be paused)
        }
    }

    /**
     * Update mixed waveform visualization.
     */
    updateMixedWaveform() {
        if (!this.mixedCtx) return;

        try {
            // Get mixed waveform data (generic)
            const data = this.emulator.getMixedWaveform(800); // Match canvas width

            if (!data || data.length === 0) {
                this.drawSilence();
                return;
            }

            this.drawMixedWaveform(data);
        } catch (err) {
            // Silently fail
        }
    }

    /**
     * Draw mixed waveform on canvas.
     */
    drawMixedWaveform(data) {
        const ctx = this.mixedCtx;
        const width = this.mixedCanvas.width;
        const height = this.mixedCanvas.height;
        const midY = height / 2;

        // Clear
        ctx.fillStyle = '#1a1a1a';
        ctx.fillRect(0, 0, width, height);

        // Draw waveform
        ctx.strokeStyle = '#00a0e9';
        ctx.lineWidth = 1.5;
        ctx.beginPath();

        for (let i = 0; i < data.length; i++) {
            const x = (i / data.length) * width;
            const y = midY - (data[i] * midY);

            if (i === 0) ctx.moveTo(x, y);
            else ctx.lineTo(x, y);
        }

        ctx.stroke();

        // Draw center line
        ctx.strokeStyle = '#333';
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.moveTo(0, midY);
        ctx.lineTo(width, midY);
        ctx.stroke();

        // Draw level indicators
        this.drawLevelIndicators(data);
    }

    /**
     * Draw silence indicator.
     */
    drawSilence() {
        const ctx = this.mixedCtx;
        const width = this.mixedCanvas.width;
        const height = this.mixedCanvas.height;
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

        // Draw "Silence" text
        ctx.fillStyle = '#666';
        ctx.font = '14px monospace';
        ctx.textAlign = 'center';
        ctx.fillText('Silence', width / 2, midY);
    }

    /**
     * Draw level indicators (peak meters).
     */
    drawLevelIndicators(data) {
        const ctx = this.mixedCtx;
        const width = this.mixedCanvas.width;
        const height = this.mixedCanvas.height;

        // Calculate peak level
        let peak = 0;
        for (let i = 0; i < data.length; i++) {
            peak = Math.max(peak, Math.abs(data[i]));
        }

        // Draw peak level bar (right side)
        const barWidth = 20;
        const barHeight = height - 10;
        const barX = width - barWidth - 5;
        const barY = 5;

        // Background
        ctx.fillStyle = '#333';
        ctx.fillRect(barX, barY, barWidth, barHeight);

        // Peak fill
        const fillHeight = peak * barHeight;
        const gradient = ctx.createLinearGradient(barX, barY + barHeight, barX, barY);
        gradient.addColorStop(0, '#00ff88');
        gradient.addColorStop(0.7, '#00a0e9');
        gradient.addColorStop(1, '#ff4444');

        ctx.fillStyle = gradient;
        ctx.fillRect(barX, barY + barHeight - fillHeight, barWidth, fillHeight);

        // Peak text
        ctx.fillStyle = '#fff';
        ctx.font = '10px monospace';
        ctx.textAlign = 'center';
        const peakText = `${Math.round(peak * 100)}%`;
        ctx.fillText(peakText, barX + barWidth / 2, barY - 2);
    }

    /**
     * Toggle visibility of audio debugger.
     */
    toggle() {
        if (this.container.style.display === 'none') {
            this.container.style.display = 'block';
            this.start();
        } else {
            this.container.style.display = 'none';
            this.stop();
        }
    }

    /**
     * Destroy audio debugger and cleanup resources.
     */
    destroy() {
        this.stop();
        this.channels = [];
        if (this.container) {
            this.container.innerHTML = '';
        }
    }
}

