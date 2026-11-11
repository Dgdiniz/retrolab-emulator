/**
 * Settings Modal UI, modal dialog that exposes the persisted
 * `DebuggerSettings` to the user (layout, appearance, debugger
 * behaviour, panel toggles). Notifies the live UI through the
 * settings store's subscriber list.
 */

export class SettingsModal {
    constructor(settings, debuggerAPI) {
        this.settings = settings;
        this.debuggerAPI = debuggerAPI;
        this.createModal();
        this.attachEventListeners();
    }

    /**
     * Create modal HTML structure
     */
    createModal() {
        const backdrop = document.createElement('div');
        backdrop.className = 'settings-modal-backdrop';
        backdrop.id = 'settings-modal-backdrop';

        backdrop.innerHTML = `
            <div class="settings-modal">
                <!-- Header -->
                <div class="settings-modal-header">
                    <h2>⚙️ Settings</h2>
                    <button class="settings-modal-close" title="Close (Esc)">×</button>
                </div>
                
                <!-- Body -->
                <div class="settings-modal-body">
                    <!-- Sidebar -->
                    <div class="settings-sidebar">
                        <div class="settings-category active" data-category="appearance">Appearance</div>
                        <div class="settings-category" data-category="behavior">Behavior</div>
                        <div class="settings-category" data-category="performance">Performance</div>
                        <div class="settings-category" data-category="debugger">Debugger</div>
                        <div class="settings-category" data-category="keybindings">Keybindings</div>
                    </div>
                    
                    <!-- Content -->
                    <div class="settings-content">
                        <!-- Appearance Section -->
                        <div class="settings-section active" data-section="appearance">
                            <h3>Appearance</h3>
                            
                            <div class="settings-group">
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Theme</label>
                                        <span class="description">Color scheme for the debugger</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <select class="settings-select" data-setting="appearance.theme">
                                            <option value="dark">Dark</option>
                                            <option value="light">Light</option>
                                            <option value="auto">Auto (System)</option>
                                        </select>
                                    </div>
                                </div>
                                
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Font Size</label>
                                        <span class="description">Code font size in pixels</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="range" class="settings-range" 
                                            data-setting="appearance.fontSize" 
                                            min="8" max="32" step="1" />
                                        <span class="settings-range-value">14px</span>
                                    </div>
                                </div>
                                
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Syntax Highlighting</label>
                                        <span class="description">Highlight assembly syntax</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="checkbox" class="settings-checkbox" 
                                            data-setting="appearance.syntaxHighlighting" />
                                    </div>
                                </div>
                                
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Hex Uppercase</label>
                                        <span class="description">Show hex values in uppercase</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="checkbox" class="settings-checkbox" 
                                            data-setting="behavior.hexUppercase" />
                                    </div>
                                </div>
                            </div>
                        </div>
                        
                        <!-- Behavior Section -->
                        <div class="settings-section" data-section="behavior">
                            <h3>Behavior</h3>
                            
                            <div class="settings-group">
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Auto Scroll</label>
                                        <span class="description">Auto-scroll disassembly to PC</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="checkbox" class="settings-checkbox" 
                                            data-setting="behavior.autoScroll" />
                                    </div>
                                </div>
                                
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Highlight PC</label>
                                        <span class="description">Highlight program counter line</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="checkbox" class="settings-checkbox" 
                                            data-setting="behavior.highlightPc" />
                                    </div>
                                </div>
                                
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Show Opcodes</label>
                                        <span class="description">Display instruction opcodes</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="checkbox" class="settings-checkbox" 
                                            data-setting="behavior.showOpcodes" />
                                    </div>
                                </div>
                                
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Auto Update</label>
                                        <span class="description">Update views automatically on step</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="checkbox" class="settings-checkbox" 
                                            data-setting="behavior.autoUpdate" />
                                    </div>
                                </div>
                                
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Confirm Reset</label>
                                        <span class="description">Ask before resetting emulator</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="checkbox" class="settings-checkbox" 
                                            data-setting="behavior.confirmReset" />
                                    </div>
                                </div>
                            </div>
                        </div>
                        
                        <!-- Performance Section -->
                        <div class="settings-section" data-section="performance">
                            <h3>Performance</h3>
                            
                            <div class="settings-group">
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Update Frequency</label>
                                        <span class="description">UI update rate (Hz)</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="range" class="settings-range" 
                                            data-setting="performance.updateFrequency" 
                                            min="10" max="120" step="10" />
                                        <span class="settings-range-value">60Hz</span>
                                    </div>
                                </div>
                                
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Performance Overlay</label>
                                        <span class="description">Show performance metrics</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="checkbox" class="settings-checkbox" 
                                            data-setting="performance.enablePerfOverlay" />
                                    </div>
                                </div>
                            </div>
                        </div>
                        
                        <!-- Debugger Section -->
                        <div class="settings-section" data-section="debugger">
                            <h3>Debugger</h3>
                            
                            <div class="settings-group">
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Break on Start</label>
                                        <span class="description">Pause when ROM loads</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="checkbox" class="settings-checkbox" 
                                            data-setting="debugger.breakOnStart" />
                                    </div>
                                </div>
                                
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Break on Exception</label>
                                        <span class="description">Pause on CPU exceptions</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="checkbox" class="settings-checkbox" 
                                            data-setting="debugger.breakOnException" />
                                    </div>
                                </div>
                                
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Disassembly Lines</label>
                                        <span class="description">Number of lines to show</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="range" class="settings-range" 
                                            data-setting="debugger.disassemblyLines" 
                                            min="20" max="200" step="10" />
                                        <span class="settings-range-value">50</span>
                                    </div>
                                </div>
                                
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Memory Bytes Per Line</label>
                                        <span class="description">Bytes per row in the memory viewer (auto fits the container width)</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <select class="settings-select" data-setting="debugger.memoryBytesPerLine">
                                            <option value="auto">Auto-fit</option>
                                            <option value="8">8</option>
                                            <option value="16">16</option>
                                            <option value="32">32</option>
                                        </select>
                                    </div>
                                </div>

                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Show Old Byte While Editing</label>
                                        <span class="description">Keep the original byte digits visible faintly behind the typed digits</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="checkbox" class="settings-checkbox"
                                            data-setting="debugger.editorShowOldByteOnEdit" />
                                    </div>
                                </div>
                            </div>
                        </div>
                        
                        <!-- Keybindings Section -->
                        <div class="settings-section" data-section="keybindings">
                            <h3>Keybindings</h3>
                            
                            <div class="settings-group">
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label>Enable Keybindings</label>
                                        <span class="description">Enable keyboard shortcuts</span>
                                    </div>
                                    <div class="settings-item-control">
                                        <input type="checkbox" class="settings-checkbox" 
                                            data-setting="keybindings.enabled" />
                                    </div>
                                </div>
                                
                                <div class="settings-item">
                                    <div class="settings-item-label">
                                        <label></label>
                                        <span class="description">Press F1 to view all keybindings</span>
                                    </div>
                                </div>
                            </div>
                        </div>
                    </div>
                </div>
                
                <!-- Footer -->
                <div class="settings-modal-footer">
                    <div class="settings-footer-left">
                        <button class="settings-btn settings-btn-danger" id="settings-reset-all">
                            Reset All
                        </button>
                        <button class="settings-btn settings-btn-secondary" id="settings-export">
                            Export
                        </button>
                        <button class="settings-btn settings-btn-secondary" id="settings-import">
                            Import
                        </button>
                    </div>
                    <div class="settings-footer-right">
                        <button class="settings-btn settings-btn-secondary settings-modal-close">
                            Cancel
                        </button>
                        <button class="settings-btn settings-btn-primary" id="settings-apply">
                            Apply
                        </button>
                    </div>
                </div>
            </div>
        `;

        document.body.appendChild(backdrop);
        this.backdrop = backdrop;
        this.modal = backdrop.querySelector('.settings-modal');
    }

    /**
     * Attach event listeners
     */
    attachEventListeners() {
        // Close buttons
        const closeButtons = this.backdrop.querySelectorAll('.settings-modal-close');
        closeButtons.forEach(btn => {
            btn.addEventListener('click', () => this.close());
        });

        // Close on backdrop click. Escape closes the modal through the
        // keybindings map (`closeModalOrStop`), the one handler of the
        // shortcut keys.
        this.backdrop.addEventListener('click', (e) => {
            if (e.target === this.backdrop) {
                this.close();
            }
        });

        // Category switching
        const categories = this.backdrop.querySelectorAll('.settings-category');
        categories.forEach(cat => {
            cat.addEventListener('click', () => {
                const category = cat.dataset.category;
                this.switchCategory(category);
            });
        });

        // Setting controls
        this.attachSettingListeners();

        // Footer buttons
        document.getElementById('settings-reset-all').addEventListener('click', () => {
            if (confirm('Reset all settings to defaults? This cannot be undone.')) {
                this.settings.resetToDefaults();
                this.loadAllSettings();
                this.settings.applyAppearance();
            }
        });

        document.getElementById('settings-export').addEventListener('click', () => {
            this.exportSettings();
        });

        document.getElementById('settings-import').addEventListener('click', () => {
            this.importSettings();
        });

        document.getElementById('settings-apply').addEventListener('click', () => {
            this.close();
        });
    }

    /**
     * Attach listeners to setting controls
     */
    attachSettingListeners() {
        // Checkboxes
        const checkboxes = this.backdrop.querySelectorAll('.settings-checkbox');
        checkboxes.forEach(cb => {
            cb.addEventListener('change', () => {
                const path = cb.dataset.setting;
                this.settings.set(path, cb.checked);
                if (path.startsWith('appearance.')) {
                    this.settings.applyAppearance();
                }
            });
        });

        // Selects
        const selects = this.backdrop.querySelectorAll('.settings-select');
        selects.forEach(sel => {
            sel.addEventListener('change', () => {
                const path = sel.dataset.setting;
                const value = sel.value;
                const parsedValue = !isNaN(value) && value !== '' ? parseInt(value) : value;
                this.settings.set(path, parsedValue);
                if (path.startsWith('appearance.')) {
                    this.settings.applyAppearance();
                }
            });
        });

        // Ranges
        const ranges = this.backdrop.querySelectorAll('.settings-range');
        ranges.forEach(range => {
            const valueSpan = range.nextElementSibling;

            range.addEventListener('input', () => {
                const path = range.dataset.setting;
                const value = parseInt(range.value);

                // Update display
                if (path === 'appearance.fontSize') {
                    valueSpan.textContent = `${value}px`;
                } else if (path === 'performance.updateFrequency') {
                    valueSpan.textContent = `${value}Hz`;
                } else {
                    valueSpan.textContent = value;
                }

                this.settings.set(path, value);
                if (path.startsWith('appearance.')) {
                    this.settings.applyAppearance();
                }
            });
        });
    }

    /**
     * Switch category
     */
    switchCategory(category) {
        // Update sidebar
        const categories = this.backdrop.querySelectorAll('.settings-category');
        categories.forEach(cat => {
            cat.classList.toggle('active', cat.dataset.category === category);
        });

        // Update content
        const sections = this.backdrop.querySelectorAll('.settings-section');
        sections.forEach(sec => {
            sec.classList.toggle('active', sec.dataset.section === category);
        });
    }

    /**
     * Load all settings into UI
     */
    loadAllSettings() {
        // Checkboxes
        const checkboxes = this.backdrop.querySelectorAll('.settings-checkbox');
        checkboxes.forEach(cb => {
            const path = cb.dataset.setting;
            cb.checked = this.settings.get(path);
        });

        // Selects
        const selects = this.backdrop.querySelectorAll('.settings-select');
        selects.forEach(sel => {
            const path = sel.dataset.setting;
            sel.value = this.settings.get(path);
        });

        // Ranges
        const ranges = this.backdrop.querySelectorAll('.settings-range');
        ranges.forEach(range => {
            const path = range.dataset.setting;
            const value = this.settings.get(path);
            range.value = value;

            const valueSpan = range.nextElementSibling;
            if (path === 'appearance.fontSize') {
                valueSpan.textContent = `${value}px`;
            } else if (path === 'performance.updateFrequency') {
                valueSpan.textContent = `${value}Hz`;
            } else {
                valueSpan.textContent = value;
            }
        });
    }

    /**
     * Export settings
     */
    exportSettings() {
        const json = this.settings.export();
        const blob = new Blob([json], { type: 'application/json' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = 'retrolab-debugger-settings.json';
        a.click();
        URL.revokeObjectURL(url);
    }

    /**
     * Import settings
     */
    importSettings() {
        const input = document.createElement('input');
        input.type = 'file';
        input.accept = '.json';
        input.onchange = (e) => {
            const file = e.target.files[0];
            if (file) {
                const reader = new FileReader();
                reader.onload = (event) => {
                    const success = this.settings.import(event.target.result);
                    if (success) {
                        this.loadAllSettings();
                        this.settings.applyAppearance();
                        alert('Settings imported successfully!');
                    } else {
                        alert('Failed to import settings. Invalid format.');
                    }
                };
                reader.readAsText(file);
            }
        };
        input.click();
    }

    /**
     * Open modal
     */
    open() {
        this.loadAllSettings();
        this.backdrop.classList.add('visible');
    }

    /**
     * Close modal
     */
    close() {
        this.backdrop.classList.remove('visible');
    }

    /**
     * Toggle modal
     */
    toggle() {
        if (this.isOpen()) {
            this.close();
        } else {
            this.open();
        }
    }

    /**
     * Check if modal is open
     */
    isOpen() {
        return this.backdrop.classList.contains('visible');
    }
}

