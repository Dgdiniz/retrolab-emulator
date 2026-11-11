/**
 * Performance Overlay, floating widget that surfaces live emulator
 * metrics (FPS, frame time, audio buffer health, CPU/PPU/APU costs)
 * on top of the canvas.
 */

export class PerformanceOverlay {
    constructor(emulator) {
        this.emulator = emulator;
        this.overlay = null;
        this.visible = true;
        this.collapsed = false;

        // Performance metrics
        this.fps = 0;
        this.frameTime = 0;
        this.cpuUsage = 0;
        this.pc68k = 0;
        this.pcZ80 = 0;

        // FPS calculation
        this.frameCount = 0;
        this.lastFpsTime = performance.now();
        this.frameTimes = [];
        this.maxFrameTimes = 60; // Keep last 60 frames

        // Update frequency
        this.updateInterval = 100; // ms
        this.lastUpdateTime = 0;

        this.createOverlay();
        this.loadPreferences();
    }

    /**
     * Create and inject overlay into DOM
     */
    createOverlay() {
        this.overlay = document.createElement('div');
        this.overlay.className = 'perf-overlay';
        this.overlay.innerHTML = `
            <div class="perf-overlay-header">
                <span class="perf-overlay-title">Performance</span>
                <button class="perf-toggle-btn" title="Toggle minimize"></button>
            </div>
            <div class="perf-item">
                <span class="perf-label">FPS:</span>
                <span class="perf-value" id="perf-fps">0</span>
            </div>
            <div class="perf-item">
                <span class="perf-label">Frame Time:</span>
                <span class="perf-value" id="perf-frametime">0.0ms</span>
            </div>
            <div class="perf-item">
                <span class="perf-label">CPU Usage:</span>
                <span class="perf-value" id="perf-cpu">0%</span>
            </div>
            <div class="perf-separator"></div>
            <div class="perf-item">
                <span class="perf-label">68000 PC:</span>
                <span class="perf-value mono" id="perf-pc68k">------</span>
            </div>
            <div class="perf-item">
                <span class="perf-label">Z80 PC:</span>
                <span class="perf-value mono" id="perf-pcz80">----</span>
            </div>
        `;

        document.body.appendChild(this.overlay);

        // Setup toggle button
        const toggleBtn = this.overlay.querySelector('.perf-toggle-btn');
        toggleBtn.addEventListener('click', () => this.toggleCollapsed());

        // Setup double-click to hide
        this.overlay.addEventListener('dblclick', (e) => {
            if (e.target.classList.contains('perf-overlay-title')) {
                this.toggleVisibility();
            }
        });
    }

    /**
     * Update performance metrics
     */
    update() {
        const now = performance.now();

        // Throttle updates
        if (now - this.lastUpdateTime < this.updateInterval) {
            return;
        }
        this.lastUpdateTime = now;

        try {
            // Update FPS
            this.updateFPS(now);

            // Update frame time
            this.updateFrameTime();

            // Update CPU usage (estimated)
            this.updateCPUUsage();

            // Update PC values
            this.updatePCValues();

            // Update DOM
            this.updateDOM();

        } catch (err) {
            console.error('Performance overlay update error:', err);
        }
    }

    /**
     * Update FPS counter
     */
    updateFPS(now) {
        this.frameCount++;

        const elapsed = now - this.lastFpsTime;
        if (elapsed >= 1000) {
            this.fps = Math.round((this.frameCount * 1000) / elapsed);
            this.frameCount = 0;
            this.lastFpsTime = now;
        }
    }

    /**
     * Update frame time
     */
    updateFrameTime() {
        if (this.frameTimes.length > 0) {
            const sum = this.frameTimes.reduce((a, b) => a + b, 0);
            this.frameTime = sum / this.frameTimes.length;
        }
    }

    /**
     * Update CPU usage (estimated from frame time)
     */
    updateCPUUsage() {
        // Estimate CPU usage based on frame time
        // 16.67ms = 100% of frame budget at 60 FPS
        const frameBudget = 16.67;
        this.cpuUsage = Math.min(100, Math.round((this.frameTime / frameBudget) * 100));
    }

    /**
     * Update PC values from emulator
     */
    updatePCValues() {
        try {
            if (this.emulator) {
                // Get 68000 PC
                const state68k = this.emulator.get_68k_state();
                if (state68k) {
                    this.pc68k = state68k.pc || 0;
                }

                // Get Z80 PC
                const stateZ80 = this.emulator.get_z80_state();
                if (stateZ80) {
                    this.pcZ80 = stateZ80.pc || 0;
                }
            }
        } catch (err) {
            // Silently ignore errors (ROM not loaded, etc)
        }
    }

    /**
     * Update DOM elements
     */
    updateDOM() {
        // FPS
        const fpsEl = document.getElementById('perf-fps');
        if (fpsEl) {
            fpsEl.textContent = this.fps;
            fpsEl.className = 'perf-value ' + this.getFPSClass(this.fps);
        }

        // Frame time
        const frameTimeEl = document.getElementById('perf-frametime');
        if (frameTimeEl) {
            frameTimeEl.textContent = this.frameTime.toFixed(1) + 'ms';
            frameTimeEl.className = 'perf-value ' + this.getFrameTimeClass(this.frameTime);
        }

        // CPU usage
        const cpuEl = document.getElementById('perf-cpu');
        if (cpuEl) {
            cpuEl.textContent = this.cpuUsage + '%';
            cpuEl.className = 'perf-value ' + this.getCPUClass(this.cpuUsage);
        }

        // 68000 PC
        const pc68kEl = document.getElementById('perf-pc68k');
        if (pc68kEl) {
            pc68kEl.textContent = this.pc68k.toString(16).padStart(6, '0').toUpperCase();
        }

        // Z80 PC
        const pcZ80El = document.getElementById('perf-pcz80');
        if (pcZ80El) {
            pcZ80El.textContent = this.pcZ80.toString(16).padStart(4, '0').toUpperCase();
        }
    }

    /**
     * Record frame time
     */
    recordFrameTime(time) {
        this.frameTimes.push(time);
        if (this.frameTimes.length > this.maxFrameTimes) {
            this.frameTimes.shift();
        }
    }

    /**
     * Get FPS CSS class
     */
    getFPSClass(fps) {
        if (fps >= 55) return 'fps-excellent';
        if (fps >= 45) return 'fps-good';
        if (fps >= 30) return 'fps-fair';
        return 'fps-poor';
    }

    /**
     * Get frame time CSS class
     */
    getFrameTimeClass(ms) {
        if (ms <= 16.67) return 'frametime-good';
        if (ms <= 33) return 'frametime-fair';
        return 'frametime-poor';
    }

    /**
     * Get CPU usage CSS class
     */
    getCPUClass(percent) {
        if (percent < 50) return 'cpu-low';
        if (percent < 80) return 'cpu-medium';
        return 'cpu-high';
    }

    /**
     * Toggle collapsed state
     */
    toggleCollapsed() {
        this.collapsed = !this.collapsed;
        if (this.collapsed) {
            this.overlay.classList.add('collapsed');
        } else {
            this.overlay.classList.remove('collapsed');
        }
        this.savePreferences();
    }

    /**
     * Toggle visibility
     */
    toggleVisibility() {
        this.visible = !this.visible;
        if (this.visible) {
            this.show();
        } else {
            this.hide();
        }
        this.savePreferences();
    }

    /**
     * Show overlay
     */
    show() {
        this.visible = true;
        this.overlay.classList.remove('hidden');
    }

    /**
     * Hide overlay
     */
    hide() {
        this.visible = false;
        this.overlay.classList.add('hidden');
    }

    /**
     * Check if visible
     */
    isVisible() {
        return this.visible;
    }

    /**
     * Save preferences to localStorage
     */
    savePreferences() {
        const prefs = {
            visible: this.visible,
            collapsed: this.collapsed,
        };
        localStorage.setItem('retrolab-perf-overlay-prefs', JSON.stringify(prefs));
    }

    /**
     * Load preferences from localStorage (migrates from old sg1 key if needed)
     */
    loadPreferences() {
        try {
            let stored = localStorage.getItem('retrolab-perf-overlay-prefs');
            if (!stored) {
                stored = localStorage.getItem('sg1-perf-overlay-prefs');
                if (stored) {
                    localStorage.setItem('retrolab-perf-overlay-prefs', stored);
                    localStorage.removeItem('sg1-perf-overlay-prefs');
                }
            }
            if (stored) {
                const prefs = JSON.parse(stored);
                this.visible = prefs.visible !== false; // Default to true
                this.collapsed = prefs.collapsed || false;

                if (!this.visible) {
                    this.hide();
                }
                if (this.collapsed) {
                    this.overlay.classList.add('collapsed');
                }
            }
        } catch (err) {
            console.error('Failed to load performance overlay preferences:', err);
        }
    }
}

// Export for use in debugger
window.PerformanceOverlay = PerformanceOverlay;

