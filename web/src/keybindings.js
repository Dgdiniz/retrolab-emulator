/**
 * Keyboard Bindings, central registry mapping shortcuts (F5/F10/F11,
 * Ctrl+B, ...) to debugger actions; the help modal renders the current
 * mapping. It is the only handler of the debugger's shortcut keys, so a
 * key does what the help modal lists and nothing else.
 */

export class KeyBindings {
    constructor(debuggerApi) {
        this.debugger = debuggerApi;
        this.bindings = this.getDefaultBindings();
        this.enabled = true;
        this.setupEventListeners();
    }

    /**
     * Get default keybindings. Each key appears once: a key repeated in
     * this object literal silently replaces the earlier entry.
     *
     * A binding marked `available: false` names an action the debugger
     * does not have yet (its API method only logs). The help modal lists
     * it as not available yet, and the dispatcher leaves its key to the
     * browser: no action, no `preventDefault`. `available` can also be a
     * function, asked each time: the focus keys (Ctrl+1 to Ctrl+5) ask
     * whether the page has what they bring forward, which depends on the
     * core (the SNES page hides the VDP and Audio tabs).
     */
    getDefaultBindings() {
        return {
            // Execution Control
            'F9': {
                name: 'Run/Pause',
                action: () => this.debugger.toggleRunPause(),
                category: 'Execution',
            },
            'F10': {
                name: 'Step Over',
                action: () => this.debugger.stepOver(),
                category: 'Execution',
            },
            'Shift+F11': {
                name: 'Step Out',
                available: false,
                action: () => this.debugger.stepOut(),
                category: 'Execution',
            },
            'Ctrl+R': {
                name: 'Reset',
                action: () => this.debugger.reset(),
                category: 'Execution',
            },
            'Ctrl+Shift+R': {
                name: 'Hot Reload ROM',
                action: () => this.debugger.hotReloadRom(),
                category: 'Execution',
            },
            'Ctrl+Shift+Backspace': {
                name: 'Force Reset (recreate the emulator)',
                action: () => this.debugger.forceReset(),
                category: 'Execution',
            },

            // Save States
            'F5': {
                name: 'Quick Save (Slot 1)',
                action: () => this.debugger.quickSave(1),
                category: 'Save States',
            },
            'F6': {
                name: 'Save to Slot 2',
                action: () => this.debugger.saveToSlot(2),
                category: 'Save States',
            },
            'F7': {
                name: 'Save to Slot 3',
                action: () => this.debugger.saveToSlot(3),
                category: 'Save States',
            },
            'F8': {
                name: 'Save to Slot 4',
                action: () => this.debugger.saveToSlot(4),
                category: 'Save States',
            },
            'Shift+F5': {
                name: 'Quick Load (Slot 1)',
                action: () => this.debugger.quickLoad(1),
                category: 'Save States',
            },
            'Shift+F6': {
                name: 'Load from Slot 2',
                action: () => this.debugger.loadFromSlot(2),
                category: 'Save States',
            },
            'Shift+F7': {
                name: 'Load from Slot 3',
                action: () => this.debugger.loadFromSlot(3),
                category: 'Save States',
            },
            'Shift+F8': {
                name: 'Load from Slot 4',
                action: () => this.debugger.loadFromSlot(4),
                category: 'Save States',
            },

            // Debugger Control
            'Ctrl+B': {
                name: 'Toggle Breakpoint',
                available: false,
                action: () => this.debugger.toggleBreakpoint(),
                category: 'Debugger',
            },
            'Ctrl+G': {
                name: 'Go to Address',
                action: () => this.debugger.showGoToDialog(),
                category: 'Debugger',
            },
            'Ctrl+F': {
                name: 'Find',
                available: false,
                action: () => this.debugger.showFindDialog(),
                category: 'Debugger',
            },
            'Ctrl+H': {
                name: 'Find and Replace',
                available: false,
                action: () => this.debugger.showFindReplaceDialog(),
                category: 'Debugger',
            },
            'F3': {
                name: 'Find Next',
                available: false,
                action: () => this.debugger.findNext(),
                category: 'Debugger',
            },
            'Shift+F3': {
                name: 'Find Previous',
                available: false,
                action: () => this.debugger.findPrevious(),
                category: 'Debugger',
            },

            // View Control
            'Ctrl+1': {
                name: 'Focus Disassembly',
                available: () => this.debugger.canFocusTab('disassembly'),
                action: () => this.debugger.focusTab('disassembly'),
                category: 'View',
            },
            'Ctrl+2': {
                name: 'Focus Memory',
                available: () => this.debugger.canFocusTab('memory'),
                action: () => this.debugger.focusTab('memory'),
                category: 'View',
            },
            'Ctrl+3': {
                name: 'Focus VDP',
                available: () => this.debugger.canFocusTab('vdp'),
                action: () => this.debugger.focusTab('vdp'),
                category: 'View',
            },
            'Ctrl+4': {
                name: 'Focus Audio',
                available: () => this.debugger.canFocusTab('audio'),
                action: () => this.debugger.focusTab('audio'),
                category: 'View',
            },
            'Ctrl+5': {
                name: 'Focus Cartridge',
                available: () => this.debugger.canFocusTab('cartridge'),
                action: () => this.debugger.focusTab('cartridge'),
                category: 'View',
            },
            'Ctrl+Tab': {
                name: 'Next Tab',
                available: false,
                action: () => this.debugger.nextTab(),
                category: 'View',
            },
            'Ctrl+Shift+Tab': {
                name: 'Previous Tab',
                available: false,
                action: () => this.debugger.previousTab(),
                category: 'View',
            },

            // Performance
            'Tab': {
                name: 'Fast Forward (hold)',
                available: false,
                down: () => this.debugger.setFastForward(true),
                up: () => this.debugger.setFastForward(false),
                category: 'Performance',
            },
            'Ctrl+=': {
                name: 'Speed Up',
                available: false,
                action: () => this.debugger.increaseSpeed(),
                category: 'Performance',
            },
            'Ctrl+-': {
                name: 'Speed Down',
                available: false,
                action: () => this.debugger.decreaseSpeed(),
                category: 'Performance',
            },
            'Ctrl+0': {
                name: 'Normal Speed',
                available: false,
                action: () => this.debugger.normalSpeed(),
                category: 'Performance',
            },

            // General
            'F1': {
                name: 'Help',
                action: () => this.debugger.showHelp(),
                category: 'General',
            },
            'Ctrl+,': {
                name: 'Settings',
                action: () => this.debugger.showSettings(),
                category: 'General',
            },
            'Ctrl+O': {
                name: 'Open ROM',
                action: () => this.debugger.openRomDialog(),
                category: 'General',
            },
            'Ctrl+S': {
                name: 'Screenshot',
                available: false,
                action: () => this.debugger.takeScreenshot(),
                category: 'General',
            },
            'Escape': {
                name: 'Close Modal, or Stop the Emulator',
                action: () => this.debugger.closeModalOrStop(),
                category: 'General',
            },
            'F11': {
                name: 'Toggle Fullscreen',
                action: () => this.debugger.toggleFullscreen(),
                category: 'General',
            },
        };
    }

    /**
     * Setup event listeners
     */
    setupEventListeners() {
        document.addEventListener('keydown', (e) => this.handleKeyDown(e));
        document.addEventListener('keyup', (e) => this.handleKeyUp(e));
    }

    /**
     * Handle keydown event
     */
    handleKeyDown(e) {
        if (!this.enabled) return;

        // Don't intercept keys when typing in input fields
        if (this.isInputElement(e.target)) {
            // Except for Escape
            if (e.key === 'Escape') {
                e.target.blur();
                this.handleKeyPress(e);
            }
            return;
        }

        this.handleKeyPress(e);
    }

    /**
     * Handle keyup event
     */
    handleKeyUp(e) {
        if (!this.enabled) return;
        if (this.isInputElement(e.target)) return;

        const key = this.getKeyString(e);
        const binding = this.bindings[key];

        if (binding && this.isAvailable(binding) && binding.up) {
            e.preventDefault();
            try {
                binding.up();
            } catch (error) {
                console.error(`Error executing key up handler for ${key}:`, error);
            }
        }
    }

    /**
     * Handle key press (common logic for keydown)
     */
    handleKeyPress(e) {
        const key = this.getKeyString(e);
        const binding = this.bindings[key];

        // A binding not available yet leaves the key to the browser.
        if (binding && this.isAvailable(binding)) {
            e.preventDefault();

            try {
                if (binding.action) {
                    binding.action();
                } else if (binding.down) {
                    binding.down();
                }
            } catch (error) {
                console.error(`Error executing keybinding for ${key}:`, error);
                // Show error to user
                this.debugger.showError(`Keybinding error (${key}): ${error.message}`);
            }
        }
    }

    /**
     * Whether a binding's action exists here: `available` is absent (it
     * does), a flag, or a function asked now.
     */
    isAvailable(binding) {
        const available = binding.available;
        return typeof available === 'function' ? available() !== false : available !== false;
    }

    /**
     * Get key string from event (e.g., "Ctrl+Shift+F5")
     */
    getKeyString(e) {
        const parts = [];

        if (e.ctrlKey) parts.push('Ctrl');
        if (e.shiftKey) parts.push('Shift');
        if (e.altKey) parts.push('Alt');

        // Normalize key name
        let key = e.key;
        if (key === ' ') key = 'Space';
        if (key === '+') key = '='; // Ctrl++ is actually Ctrl+=
        // A letter's case follows Shift and Caps Lock (Ctrl+R arrives as
        // 'r'); the map spells every letter in upper case.
        if (/^[a-z]$/.test(key)) key = key.toUpperCase();

        parts.push(key);

        return parts.join('+');
    }

    /**
     * Check if target is an input element
     */
    isInputElement(element) {
        const tagName = element.tagName.toLowerCase();
        return (
            tagName === 'input' ||
            tagName === 'textarea' ||
            tagName === 'select' ||
            element.isContentEditable
        );
    }

    /**
     * Enable keybindings
     */
    enable() {
        this.enabled = true;
    }

    /**
     * Disable keybindings (useful for modals)
     */
    disable() {
        this.enabled = false;
    }

    /**
     * Get all bindings grouped by category
     */
    getBindingsByCategory() {
        const categories = {};

        for (const [key, binding] of Object.entries(this.bindings)) {
            const category = binding.category || 'Other';
            if (!categories[category]) {
                categories[category] = [];
            }
            categories[category].push({
                key,
                name: binding.name,
                available: this.isAvailable(binding),
            });
        }

        return categories;
    }

    /**
     * Add custom keybinding
     */
    addBinding(key, action, name = 'Custom', category = 'Custom') {
        this.bindings[key] = {
            name,
            action,
            category,
        };
    }

    /**
     * Remove keybinding
     */
    removeBinding(key) {
        delete this.bindings[key];
    }

    /**
     * Clear all keybindings
     */
    clearBindings() {
        this.bindings = {};
    }

    /**
     * Reset to default keybindings
     */
    resetToDefaults() {
        this.bindings = this.getDefaultBindings();
    }
}

// Export for use in debugger
window.KeyBindings = KeyBindings;

