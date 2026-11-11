/**
 * Multi-Chip Audio Debugger for Genesis (PSG + YM2612)
 * 
 * Provides tabbed interface for debugging audio:
 * - Channels: Per-channel state for PSG and YM2612
 * - Waveform: Visual waveform viewer with canvas
 * - Spectrum: Spectrum analyzer (future)
 */

export class AudioDebuggerMultiChip {
    constructor(emulator, containerId) {
        this.emulator = emulator;
        this.container = document.getElementById(containerId);

        if (!this.container) {
            console.error(`AudioDebugger: Container '${containerId}' not found`);
            return;
        }

        // State
        this.activeTab = 'channels';
        this.updateInterval = null;
        this.updateRate = 30; // Hz

        // Waveform settings
        this.waveformChip = 'mixed'; // 'mixed', 'psg', 'ym2612'
        this.waveformSamples = 1024;

        // Canvas references
        this.waveformCanvas = null;
        this.waveformCtx = null;
        this.spectrumCanvas = null;
        this.spectrumCtx = null;

        // Channel state cache (to detect changes)
        this.psgChannels = [];
        this.ym2612Channels = [];
    }

    /**
     * Initialize the audio debugger UI
     */
    async init() {
        console.log('Initializing Multi-Chip Audio Debugger...');

        try {
            this.createUI();
            this.setupEventListeners();
            this.startUpdateLoop();
            console.log('Multi-Chip Audio Debugger initialized');
        } catch (err) {
            console.error('Failed to initialize Audio Debugger:', err);
        }
    }

    /**
     * Create the main UI structure
     */
    createUI() {
        this.container.innerHTML = '';
        this.container.className = 'audio-debugger-multi-chip';

        // Create tabs
        const tabs = this.createTabs();
        this.container.appendChild(tabs);

        // Create tab contents
        const tabChannels = this.createChannelsTab();
        const tabWaveform = this.createWaveformTab();
        const tabSpectrum = this.createSpectrumTab();

        this.container.appendChild(tabChannels);
        this.container.appendChild(tabWaveform);
        this.container.appendChild(tabSpectrum);
    }

    /**
     * Create tab buttons
     */
    createTabs() {
        const tabsDiv = document.createElement('div');
        tabsDiv.className = 'audio-tabs';

        const tabs = [
            { id: 'channels', label: '🎵 Channels', title: 'View channel states for PSG and YM2612' },
            { id: 'waveform', label: '📊 Waveform', title: 'Visual waveform viewer' },
            { id: 'spectrum', label: '🎶 Spectrum', title: 'Spectrum analyzer (coming soon)' }
        ];

        tabs.forEach(tab => {
            const btn = document.createElement('button');
            btn.className = 'audio-tab-btn';
            btn.dataset.tab = tab.id;
            btn.textContent = tab.label;
            btn.title = tab.title;

            if (tab.id === this.activeTab) {
                btn.classList.add('active');
            }

            tabsDiv.appendChild(btn);
        });

        return tabsDiv;
    }

    /**
     * Create Channels tab content
     */
    createChannelsTab() {
        const div = document.createElement('div');
        div.id = 'audio-tab-channels';
        div.className = 'audio-tab-content';
        if (this.activeTab === 'channels') {
            div.classList.add('active');
        }

        // PSG Section
        const psgSection = document.createElement('div');
        psgSection.className = 'audio-chip-section';
        psgSection.innerHTML = `
            <h3>🔊 PSG (SN76489)</h3>
            <div id="psg-channels" class="channel-grid"></div>
        `;
        div.appendChild(psgSection);

        // YM2612 Section
        const ymSection = document.createElement('div');
        ymSection.className = 'audio-chip-section';
        ymSection.innerHTML = `
            <h3>🎹 YM2612 (FM Synthesis)</h3>
            <div id="ym2612-channels" class="channel-grid"></div>
            <details class="ym2612-extended">
                <summary>YM2612 Extended Info</summary>
                <div id="ym2612-lfo" class="ym-extended-section"></div>
                <div id="ym2612-dac" class="ym-extended-section"></div>
                <div id="ym2612-timers" class="ym-extended-section"></div>
            </details>
        `;
        div.appendChild(ymSection);

        return div;
    }

    /**
     * Create Waveform tab content
     */
    createWaveformTab() {
        const div = document.createElement('div');
        div.id = 'audio-tab-waveform';
        div.className = 'audio-tab-content';
        if (this.activeTab === 'waveform') {
            div.classList.add('active');
        }

        // Controls
        const controls = document.createElement('div');
        controls.className = 'waveform-controls';
        controls.innerHTML = `
            <label>
                Chip:
                <select id="waveform-chip-select">
                    <option value="mixed">Mixed (PSG + YM2612)</option>
                    <option value="psg">PSG Only</option>
                </select>
            </label>
            <label>
                Samples: <span id="waveform-samples-value">1024</span>
                <input type="range" id="waveform-samples" min="256" max="4096" value="1024" step="256">
            </label>
        `;
        div.appendChild(controls);

        // Canvas
        const canvas = document.createElement('canvas');
        canvas.id = 'waveform-canvas';
        canvas.width = 800;
        canvas.height = 300;
        div.appendChild(canvas);

        this.waveformCanvas = canvas;
        this.waveformCtx = canvas.getContext('2d');

        return div;
    }

    /**
     * Create Spectrum tab content
     */
    createSpectrumTab() {
        const div = document.createElement('div');
        div.id = 'audio-tab-spectrum';
        div.className = 'audio-tab-content';
        if (this.activeTab === 'spectrum') {
            div.classList.add('active');
        }

        const info = document.createElement('div');
        info.className = 'spectrum-info';
        info.innerHTML = `
            <p><strong>Spectrum Analyzer (FFT)</strong></p>
            <p>Coming soon... Requires Web Audio API AnalyserNode integration</p>
            <p style="color: #888; font-size: 12px;">This will show frequency spectrum of the audio output</p>
        `;
        div.appendChild(info);

        // Canvas (for future)
        const canvas = document.createElement('canvas');
        canvas.id = 'spectrum-canvas';
        canvas.width = 800;
        canvas.height = 400;
        canvas.style.display = 'none';
        div.appendChild(canvas);

        this.spectrumCanvas = canvas;
        this.spectrumCtx = canvas.getContext('2d');

        return div;
    }

    /**
     * Setup event listeners
     */
    setupEventListeners() {
        // Tab switching
        const tabButtons = this.container.querySelectorAll('.audio-tab-btn');
        tabButtons.forEach(btn => {
            btn.addEventListener('click', () => {
                this.switchTab(btn.dataset.tab);
            });
        });

        // Waveform controls
        const chipSelect = this.container.querySelector('#waveform-chip-select');
        if (chipSelect) {
            chipSelect.addEventListener('change', (e) => {
                this.waveformChip = e.target.value;
            });
        }

        const samplesSlider = this.container.querySelector('#waveform-samples');
        const samplesValue = this.container.querySelector('#waveform-samples-value');
        if (samplesSlider && samplesValue) {
            samplesSlider.addEventListener('input', (e) => {
                this.waveformSamples = parseInt(e.target.value);
                samplesValue.textContent = this.waveformSamples;
            });
        }
    }

    /**
     * Switch active tab
     */
    switchTab(tabId) {
        this.activeTab = tabId;

        // Update tab buttons
        const tabButtons = this.container.querySelectorAll('.audio-tab-btn');
        tabButtons.forEach(btn => {
            btn.classList.toggle('active', btn.dataset.tab === tabId);
        });

        // Update tab content
        const tabContents = this.container.querySelectorAll('.audio-tab-content');
        tabContents.forEach(content => {
            content.classList.toggle('active', content.id === `audio-tab-${tabId}`);
        });
    }

    /**
     * Main update loop
     */
    update() {
        try {
            if (this.activeTab === 'channels') {
                this.updateChannels();
            } else if (this.activeTab === 'waveform') {
                this.updateWaveform();
            } else if (this.activeTab === 'spectrum') {
                this.updateSpectrum();
            }
        } catch (err) {
            console.error('Audio debugger update error:', err);
        }
    }

    /**
     * Update Channels tab
     */
    updateChannels() {
        this.updatePsgChannels();
        this.updateYm2612Channels();
        this.updateYm2612Extended();
    }

    /**
     * Update PSG channels
     */
    updatePsgChannels() {
        const container = this.container.querySelector('#psg-channels');
        if (!container) return;

        // Get channel count
        const channelCount = this.emulator.getAudioChannelCount();

        // Clear if first time
        if (container.children.length === 0) {
            for (let i = 0; i < channelCount; i++) {
                const channelDiv = this.createChannelCard('psg', i);
                container.appendChild(channelDiv);
            }
        }

        // Update each channel
        for (let i = 0; i < channelCount; i++) {
            try {
                const state = this.emulator.getAudioChannelState(i);
                if (state) {
                    this.updateChannelCard('psg', i, state);
                }
            } catch (err) {
                console.error(`Error getting PSG channel ${i} state:`, err);
            }
        }
    }

    /**
     * Update YM2612 channels
     */
    updateYm2612Channels() {
        const container = this.container.querySelector('#ym2612-channels');
        if (!container) return;

        const channelCount = this.emulator.getYm2612ChannelCount();

        // Clear if first time
        if (container.children.length === 0) {
            for (let i = 0; i < channelCount; i++) {
                const channelDiv = this.createChannelCard('ym2612', i);
                container.appendChild(channelDiv);
            }
        }

        // Update each channel
        for (let i = 0; i < channelCount; i++) {
            try {
                const state = this.emulator.getYm2612ChannelState(i);
                if (state) {
                    this.updateChannelCard('ym2612', i, state);
                }
            } catch (err) {
                console.error(`Error getting YM2612 channel ${i} state:`, err);
            }
        }
    }

    /**
     * Update YM2612 extended info (LFO, DAC, Timers)
     */
    updateYm2612Extended() {
        try {
            const lfoState = this.emulator.getYm2612LfoState();
            const dacState = this.emulator.getYm2612DacState();
            const timerAState = this.emulator.getYm2612TimerAState();
            const timerBState = this.emulator.getYm2612TimerBState();

            // Update LFO
            const lfoDiv = this.container.querySelector('#ym2612-lfo');
            if (lfoDiv && lfoState) {
                lfoDiv.innerHTML = `
                    <strong>LFO:</strong>
                    Enabled: ${lfoState.enabled ? '✅' : '❌'},
                    Freq: ${lfoState.frequency || 'N/A'}
                `;
            }

            // Update DAC
            const dacDiv = this.container.querySelector('#ym2612-dac');
            if (dacDiv && dacState) {
                dacDiv.innerHTML = `
                    <strong>DAC:</strong>
                    Enabled: ${dacState.enabled ? '✅' : '❌'},
                    Value: ${dacState.value || 0}
                `;
            }

            // Update Timers
            const timersDiv = this.container.querySelector('#ym2612-timers');
            if (timersDiv && timerAState && timerBState) {
                timersDiv.innerHTML = `
                    <strong>Timers:</strong>
                    A: ${timerAState.enabled ? '✅' : '❌'} (${timerAState.value || 0}),
                    B: ${timerBState.enabled ? '✅' : '❌'} (${timerBState.value || 0})
                `;
            }
        } catch (err) {
            console.error('Error updating YM2612 extended info:', err);
        }
    }

    /**
     * Create a channel card
     */
    createChannelCard(chip, channel) {
        const div = document.createElement('div');
        div.className = 'audio-channel';
        div.dataset.chip = chip;
        div.dataset.channel = channel;
        div.id = `${chip}-ch${channel}`;

        // Will be populated by updateChannelCard
        return div;
    }

    /**
     * Update a channel card with current state
     */
    updateChannelCard(chip, channel, state) {
        const div = this.container.querySelector(`#${chip}-ch${channel}`);
        if (!div) return;

        const isActive = state.enabled && !state.muted;
        div.classList.toggle('active', isActive);

        // Format channel type
        let typeStr = state.channel_type || 'Unknown';
        if (typeof typeStr === 'object') {
            typeStr = typeStr.variant || Object.keys(typeStr)[0] || 'Unknown';
        }

        div.innerHTML = `
            <div class="channel-header">
                <span class="channel-id">${chip.toUpperCase()} CH${channel}</span>
                <button class="mute-btn" data-chip="${chip}" data-channel="${channel}">
                    ${state.muted ? '🔇' : '🔊'}
                </button>
            </div>
            <div class="channel-info">
                <div class="channel-row">
                    <span class="label">Type:</span>
                    <span class="value">${typeStr}</span>
                </div>
                <div class="channel-row">
                    <span class="label">Freq:</span>
                    <span class="value">${state.frequency ? state.frequency.toFixed(2) + ' Hz' : 'N/A'}</span>
                </div>
                <div class="channel-row">
                    <span class="label">Vol:</span>
                    <span class="value">${state.volume !== undefined ? (state.volume * 100).toFixed(0) + '%' : 'N/A'}</span>
                </div>
                ${state.envelope ? `
                <div class="channel-row">
                    <span class="label">Env:</span>
                    <span class="value">${state.envelope.current_phase || 'Off'}</span>
                </div>
                ` : ''}
            </div>
        `;

        // Re-attach mute button handler
        const muteBtn = div.querySelector('.mute-btn');
        if (muteBtn) {
            muteBtn.onclick = () => this.handleMuteToggle(chip, channel, !state.muted);
        }
    }

    /**
     * Handle mute toggle
     */
    handleMuteToggle(chip, channel, mute) {
        try {
            if (chip === 'psg') {
                this.emulator.muteAudioChannel(channel, mute);
            } else if (chip === 'ym2612') {
                this.emulator.muteYm2612Channel(channel, mute);
            }
        } catch (err) {
            console.error(`Error toggling mute for ${chip} CH${channel}:`, err);
        }
    }

    /**
     * Update Waveform tab
     */
    updateWaveform() {
        if (!this.waveformCanvas || !this.waveformCtx) return;

        const ctx = this.waveformCtx;
        const canvas = this.waveformCanvas;
        const width = canvas.width;
        const height = canvas.height;
        const centerY = height / 2;

        // Clear canvas
        ctx.fillStyle = '#000';
        ctx.fillRect(0, 0, width, height);

        // Get waveform data
        let waveform = [];
        try {
            if (this.waveformChip === 'mixed') {
                waveform = this.emulator.getMixedWaveform(this.waveformSamples);
            } else if (this.waveformChip === 'psg') {
                // Get PSG output waveform
                waveform = this.emulator.getMixedWaveform(this.waveformSamples); // TODO: PSG-only waveform
            }
        } catch (err) {
            console.error('Error getting waveform:', err);
            return;
        }

        if (!waveform || waveform.length === 0) {
            // Draw "No data" message
            ctx.fillStyle = '#888';
            ctx.font = '14px monospace';
            ctx.textAlign = 'center';
            ctx.fillText('No waveform data available', width / 2, centerY);
            return;
        }

        // Draw grid lines
        ctx.strokeStyle = '#222';
        ctx.lineWidth = 1;
        ctx.setLineDash([2, 4]);
        for (let i = 0; i <= 4; i++) {
            const y = (height / 4) * i;
            ctx.beginPath();
            ctx.moveTo(0, y);
            ctx.lineTo(width, y);
            ctx.stroke();
        }
        ctx.setLineDash([]);

        // Draw center line
        ctx.strokeStyle = '#444';
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.moveTo(0, centerY);
        ctx.lineTo(width, centerY);
        ctx.stroke();

        // Draw waveform
        ctx.strokeStyle = '#0f0';
        ctx.lineWidth = 2;
        ctx.beginPath();

        for (let i = 0; i < width; i++) {
            const idx = Math.floor((i / width) * waveform.length);
            const sample = waveform[idx] || 0;
            const y = centerY - (sample * centerY * 0.9); // 90% height

            if (i === 0) {
                ctx.moveTo(i, y);
            } else {
                ctx.lineTo(i, y);
            }
        }

        ctx.stroke();

        // Draw labels
        ctx.fillStyle = '#888';
        ctx.font = '10px monospace';
        ctx.textAlign = 'left';
        ctx.fillText('+1.0', 5, 15);
        ctx.fillText('0.0', 5, centerY);
        ctx.fillText('-1.0', 5, height - 5);
        ctx.textAlign = 'right';
        ctx.fillText(`${waveform.length} samples`, width - 5, height - 5);
    }

    /**
     * Update Spectrum tab
     */
    updateSpectrum() {
        // TODO: Implement spectrum analyzer (FFT)
        // Requires Web Audio API AnalyserNode integration
    }

    /**
     * Start update loop
     */
    startUpdateLoop() {
        if (this.updateInterval) return;

        this.updateInterval = setInterval(() => {
            // Skip update if emulator is busy (running) to avoid aliasing errors
            // Only update when paused or stepping
            try {
                const mode = this.emulator.getExecutionMode();
                if (mode === 'Running') {
                    return; // Skip update during continuous execution
                }
            } catch (err) {
                return; // Emulator busy, skip update
            }

            this.update();
        }, 1000 / this.updateRate);
    }

    /**
     * Stop update loop
     */
    stopUpdateLoop() {
        if (this.updateInterval) {
            clearInterval(this.updateInterval);
            this.updateInterval = null;
        }
    }

    /**
     * Cleanup
     */
    destroy() {
        this.stopUpdateLoop();
        if (this.container) {
            this.container.innerHTML = '';
        }
    }
}

