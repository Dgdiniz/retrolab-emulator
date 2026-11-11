/**
 * Debugger Settings Manager, persistent user preferences for the
 * web debugger UI (layout, theme, panel-specific toggles). Backed by
 * `localStorage`; subscribers are notified after every `set()` so live
 * panels can react without a reload.
 */

export class DebuggerSettings {
    constructor() {
        this.settings = this.getDefaultSettings();
        // Subscribers notified after every `set()` write. Lets panels
        // (memory editor, disassembly) react to live setting changes,
        // e.g. the user toggles "Auto-fit" in the modal and the
        // active editor recomputes its columns without a reload.
        // Listeners receive `(path, value)`; unsubscribe by calling
        // the returned function.
        this.listeners = [];
        this.loadFromStorage();
    }

    /**
     * Get default settings
     */
    getDefaultSettings() {
        return {
            // Layout settings
            layout: {
                disassemblyWidth: '40%',
                memoryViewerVisible: true,
                registersVisible: true,
                consoleVisible: false,
                vdpDebuggerVisible: true,
                audioDebuggerVisible: true,
            },

            // Appearance settings
            appearance: {
                theme: 'dark', // 'light', 'dark', 'auto'
                fontSize: 14,
                monospaceFontFamily: 'Consolas, Monaco, Courier New, monospace',
                syntaxHighlighting: true,
                colorScheme: 'default', // 'default', 'solarized', 'monokai'
                shader: 'none', // Display shader: 'none', 'crt-lottes', 'bicubic', etc.
            },

            // Behavior settings
            behavior: {
                autoScroll: true,
                highlightPc: true,
                showOpcodes: true,
                hexUppercase: true,
                autoUpdate: true, // Update views on step
                confirmReset: true,
                confirmCloseRom: false,
            },

            // Performance settings
            performance: {
                updateFrequency: 60, // Hz
                maxHistorySize: 1000,
                enablePerfOverlay: true,
                perfOverlayVisible: true,
                perfOverlayCollapsed: false,
            },

            // Debugger specific
            debugger: {
                breakOnStart: false,
                breakOnException: true,
                stepMode: 'instruction', // 'instruction', 'line', 'frame'
                disassemblyLines: 50,
                // 'auto' lets the hex editor pick the largest column
                // count from its candidates ladder that fits the
                // container width. Numeric values (8/16/32) lock the
                // layout. Old persisted settings using numeric values
                // continue to work via the storage merge below.
                memoryBytesPerLine: 'auto',
                // Show the original byte digits faintly behind the
                // typed digits while editing, so the user can still
                // see what they're overwriting.
                editorShowOldByteOnEdit: true,
            },

            // Keybindings
            keybindings: {
                enabled: true,
                customBindings: {}, // User custom keybindings
            },

            // Window state
            window: {
                lastRomPath: null,
                recentRoms: [],
                maxRecentRoms: 10,
            },
        };
    }

    /**
     * Get a setting value by path
     * @param {string} path - Dot-separated path (e.g., 'appearance.theme')
     * @returns {any} Setting value
     */
    get(path) {
        const keys = path.split('.');
        let value = this.settings;

        for (const key of keys) {
            if (value === undefined || value === null) {
                return undefined;
            }
            value = value[key];
        }

        return value;
    }

    /**
     * Set a setting value by path
     * @param {string} path - Dot-separated path
     * @param {any} value - New value
     */
    set(path, value) {
        const keys = path.split('.');
        const lastKey = keys.pop();
        let target = this.settings;

        for (const key of keys) {
            if (!(key in target)) {
                target[key] = {};
            }
            target = target[key];
        }

        target[lastKey] = value;
        this.saveToStorage();
        for (const fn of this.listeners) {
            try { fn(path, value); } catch (err) { console.error('[Settings] listener error:', err); }
        }
    }

    /**
     * Subscribe to setting changes. Returns an unsubscribe function.
     * @param {(path: string, value: unknown) => void} fn
     */
    subscribe(fn) {
        if (typeof fn !== 'function') return () => {};
        this.listeners.push(fn);
        return () => {
            const i = this.listeners.indexOf(fn);
            if (i >= 0) this.listeners.splice(i, 1);
        };
    }

    /**
     * Get entire settings object
     */
    getAll() {
        return { ...this.settings };
    }

    /**
     * Update multiple settings at once
     * @param {object} updates - Object with settings to update
     */
    update(updates) {
        this.settings = this.deepMerge(this.settings, updates);
        this.saveToStorage();
    }

    /**
     * Reset to default settings
     */
    resetToDefaults() {
        this.settings = this.getDefaultSettings();
        this.saveToStorage();
    }

    /**
     * Reset a specific category
     * @param {string} category - Category to reset (e.g., 'appearance')
     */
    resetCategory(category) {
        const defaults = this.getDefaultSettings();
        if (category in defaults) {
            this.settings[category] = defaults[category];
            this.saveToStorage();
        }
    }

    /**
     * Save settings to localStorage
     */
    saveToStorage() {
        try {
            const json = JSON.stringify(this.settings, null, 2);
            localStorage.setItem('retrolab-debugger-settings', json);
        } catch (err) {
            console.error('Failed to save settings:', err);
        }
    }

    /**
     * Load settings from localStorage (migrates from old sg1 key if needed)
     */
    loadFromStorage() {
        try {
            let stored = localStorage.getItem('retrolab-debugger-settings');
            if (!stored) {
                stored = localStorage.getItem('sg1-debugger-settings');
                if (stored) {
                    localStorage.setItem('retrolab-debugger-settings', stored);
                    localStorage.removeItem('sg1-debugger-settings');
                }
            }
            if (stored) {
                const loaded = JSON.parse(stored);
                // Merge with defaults to handle new settings
                this.settings = this.deepMerge(this.getDefaultSettings(), loaded);
            }
        } catch (err) {
            console.error('Failed to load settings:', err);
            this.settings = this.getDefaultSettings();
        }
    }

    /**
     * Export settings as JSON
     */
    export() {
        return JSON.stringify(this.settings, null, 2);
    }

    /**
     * Import settings from JSON
     * @param {string} json - JSON string
     */
    import(json) {
        try {
            const imported = JSON.parse(json);
            this.settings = this.deepMerge(this.getDefaultSettings(), imported);
            this.saveToStorage();
            return true;
        } catch (err) {
            console.error('Failed to import settings:', err);
            return false;
        }
    }

    /**
     * Add recent ROM to history
     * @param {string} romPath - Path to ROM file
     */
    addRecentRom(romPath) {
        const recent = this.get('window.recentRoms') || [];

        // Remove if already exists
        const index = recent.indexOf(romPath);
        if (index > -1) {
            recent.splice(index, 1);
        }

        // Add to front
        recent.unshift(romPath);

        // Limit to max
        const maxRecent = this.get('window.maxRecentRoms') || 10;
        if (recent.length > maxRecent) {
            recent.length = maxRecent;
        }

        this.set('window.recentRoms', recent);
        this.set('window.lastRomPath', romPath);
    }

    /**
     * Get recent ROMs
     */
    getRecentRoms() {
        return this.get('window.recentRoms') || [];
    }

    /**
     * Clear recent ROMs
     */
    clearRecentRoms() {
        this.set('window.recentRoms', []);
        this.set('window.lastRomPath', null);
    }

    /**
     * Apply appearance settings to DOM
     */
    applyAppearance() {
        const theme = this.get('appearance.theme');
        const fontSize = this.get('appearance.fontSize');
        const fontFamily = this.get('appearance.monospaceFontFamily');

        // Apply theme
        if (theme === 'dark') {
            document.body.classList.add('theme-dark');
            document.body.classList.remove('theme-light');
        } else if (theme === 'light') {
            document.body.classList.add('theme-light');
            document.body.classList.remove('theme-dark');
        } else if (theme === 'auto') {
            // Use system preference
            const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
            document.body.classList.toggle('theme-dark', prefersDark);
            document.body.classList.toggle('theme-light', !prefersDark);
        }

        // Apply fonts
        const monospaceElements = document.querySelectorAll('.disassembly-view, .memory-view, .registers-view, .perf-overlay');
        monospaceElements.forEach(el => {
            el.style.fontSize = `${fontSize}px`;
            el.style.fontFamily = fontFamily;
        });
    }

    /**
     * Deep merge two objects
     * @param {object} target - Target object
     * @param {object} source - Source object
     * @returns {object} Merged object
     */
    deepMerge(target, source) {
        const result = { ...target };

        for (const key in source) {
            if (source[key] && typeof source[key] === 'object' && !Array.isArray(source[key])) {
                result[key] = this.deepMerge(target[key] || {}, source[key]);
            } else {
                result[key] = source[key];
            }
        }

        return result;
    }

    /**
     * Get settings summary (for display)
     */
    getSummary() {
        return {
            theme: this.get('appearance.theme'),
            fontSize: this.get('appearance.fontSize'),
            autoScroll: this.get('behavior.autoScroll'),
            syntaxHighlighting: this.get('appearance.syntaxHighlighting'),
            perfOverlay: this.get('performance.enablePerfOverlay'),
            recentRoms: this.getRecentRoms().length,
        };
    }

    /**
     * Validate settings
     * @returns {boolean} True if valid
     */
    validate() {
        // Basic validation
        const fontSize = this.get('appearance.fontSize');
        if (fontSize < 8 || fontSize > 32) {
            console.warn('Invalid fontSize, resetting to default');
            this.set('appearance.fontSize', 14);
            return false;
        }

        const updateFreq = this.get('performance.updateFrequency');
        if (updateFreq < 1 || updateFreq > 120) {
            console.warn('Invalid updateFrequency, resetting to default');
            this.set('performance.updateFrequency', 60);
            return false;
        }

        return true;
    }
}

// Export for use in debugger
window.DebuggerSettings = DebuggerSettings;

