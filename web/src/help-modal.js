/**
 * Help Modal, renders the active keyboard shortcuts list (sourced
 * from `KeyBindings`) and a short cheat-sheet about debugger
 * navigation.
 */

export class HelpModal {
    constructor(keyBindings) {
        this.keyBindings = keyBindings;
        this.modal = null;
        this.createModal();
    }

    /**
     * Create and inject the help modal into the page
     */
    createModal() {
        // Create modal container
        this.modal = document.createElement('div');
        this.modal.id = 'help-modal';
        this.modal.className = 'help-modal';
        this.modal.innerHTML = this.getModalHTML();

        // Append to body
        document.body.appendChild(this.modal);

        // Setup event listeners
        this.setupEventListeners();
    }

    /**
     * Get modal HTML content
     */
    getModalHTML() {
        const bindings = this.keyBindings.getBindingsByCategory();

        return `
            <div class="help-content">
                <div class="help-header">
                    <h2>⌨️ Keyboard Shortcuts</h2>
                    <button class="help-close-btn" onclick="window.helpModal.close()">&times;</button>
                </div>
                
                <div class="help-body">
                    <div class="help-info-box">
                        <p><strong>Tip:</strong> Press <kbd>F1</kbd> anytime to show/hide this help. Press <kbd>ESC</kbd> to close.</p>
                    </div>
                    
                    ${this.renderCategory('Execution', bindings['Execution'])}
                    ${this.renderCategory('Save States', bindings['Save States'])}
                    ${this.renderCategory('Debugger', bindings['Debugger'])}
                    ${this.renderCategory('View', bindings['View'])}
                    ${this.renderCategory('Performance', bindings['Performance'])}
                    ${this.renderCategory('General', bindings['General'])}
                </div>
                
                <div class="help-footer">
                    <button onclick="window.helpModal.close()">Close (ESC)</button>
                </div>
            </div>
        `;
    }

    /**
     * Render a category of keybindings. A binding whose action the
     * debugger does not have yet (`available: false`) is listed with a
     * mark: its key does nothing here and is left to the browser.
     */
    renderCategory(categoryName, bindings) {
        if (!bindings || bindings.length === 0) return '';

        return `
            <div class="help-section">
                <h3>${categoryName}</h3>
                <table class="keybindings-table">
                    ${bindings.map(binding => `
                        <tr${binding.available === false ? ' class="keybinding-unavailable"' : ''}>
                            <td class="key-combo">${this.renderKeyCombination(binding.key)}</td>
                            <td>${binding.name}${binding.available === false ? ' <span class="keybinding-mark">(not available yet)</span>' : ''}</td>
                        </tr>
                    `).join('')}
                </table>
            </div>
        `;
    }

    /**
     * Render a key combination with <kbd> tags
     */
    renderKeyCombination(keyCombo) {
        const keys = keyCombo.split('+');
        return keys.map(key => {
            // Map key names to display names
            const displayName = this.getKeyDisplayName(key);
            return `<kbd>${displayName}</kbd>`;
        }).join('+');
    }

    /**
     * Get display name for a key
     */
    getKeyDisplayName(key) {
        const displayNames = {
            'Ctrl': 'Ctrl',
            'Shift': 'Shift',
            'Alt': 'Alt',
            'Tab': 'Tab',
            'Escape': 'Esc',
            'Space': 'Space',
            '=': '=',
            '-': '-',
            '0': '0',
        };

        return displayNames[key] || key;
    }

    /**
     * Setup event listeners
     */
    setupEventListeners() {
        // Close on click outside. Escape closes the modal through the
        // keybindings map (`closeModalOrStop`), the one handler of the
        // shortcut keys.
        this.modal.addEventListener('click', (e) => {
            if (e.target === this.modal) {
                this.close();
            }
        });
    }

    /**
     * Show the modal
     */
    show() {
        this.modal.classList.add('active');

        // Keybindings stay active while the modal is open; they pause only
        // while an input, a textarea or a select has the focus.
    }

    /**
     * Close the modal
     */
    close() {
        this.modal.classList.remove('active');
    }

    /**
     * Toggle modal visibility
     */
    toggle() {
        if (this.isVisible()) {
            this.close();
        } else {
            this.show();
        }
    }

    /**
     * Check if modal is visible
     */
    isVisible() {
        return this.modal.classList.contains('active');
    }

    /**
     * Update modal content (if keybindings change)
     */
    update() {
        const content = this.modal.querySelector('.help-content');
        if (content) {
            content.innerHTML = this.getModalHTML();
            this.setupEventListeners();
        }
    }
}

// Export for use in debugger
window.HelpModal = HelpModal;

