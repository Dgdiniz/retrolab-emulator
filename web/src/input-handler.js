/**
 * InputHandler - Manages keyboard and gamepad input for the emulator
 * 
 * Handles:
 * - Keyboard events (keydown/keyup)
 * - Gamepad API polling (60 Hz)
 * - Gamepad connection/disconnection
 * - Input provider management
 */

export class InputHandler {
    constructor(emulator) {
        this.emulator = emulator;
        this.gamepadIndex = null;
        this.keyboardEnabled = true;
        this.gamepadEnabled = true;
        this.pollInterval = null;
        this.lastGamepadState = null;

        // Track which keys are currently pressed (for preventing repeats)
        this.keysPressed = new Set();

        console.log('InputHandler created');
    }

    /**
     * Initialize input handling
     */
    init() {
        // Keyboard events
        window.addEventListener('keydown', (e) => this.handleKeyDown(e));
        window.addEventListener('keyup', (e) => this.handleKeyUp(e));

        // Gamepad events
        window.addEventListener('gamepadconnected', (e) => this.handleGamepadConnected(e));
        window.addEventListener('gamepaddisconnected', (e) => this.handleGamepadDisconnected(e));

        // Start polling gamepads
        this.startGamepadPolling();

        // Check for already connected gamepads
        this.checkExistingGamepads();

        console.log('InputHandler initialized');
    }

    /**
     * Handle key down event
     */
    handleKeyDown(event) {
        if (!this.keyboardEnabled) return;

        // Prevent key repeat
        if (this.keysPressed.has(event.code)) {
            event.preventDefault();
            return;
        }

        this.keysPressed.add(event.code);

        // Prevent default for game keys
        const gameKeys = [
            'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight',
            'Enter', 'Space', 'KeyA', 'KeyS', 'KeyD',
            'KeyQ', 'KeyW', 'KeyE', 'ShiftLeft', 'ShiftRight'
        ];

        if (gameKeys.includes(event.code)) {
            event.preventDefault();
        }

        // Send to emulator
        try {
            this.emulator.keyDown(event.code);
        } catch (err) {
            console.error('Error sending keyDown:', err);
        }
    }

    /**
     * Handle key up event
     */
    handleKeyUp(event) {
        if (!this.keyboardEnabled) return;

        this.keysPressed.delete(event.code);

        // Send to emulator
        try {
            this.emulator.keyUp(event.code);
        } catch (err) {
            console.error('Error sending keyUp:', err);
        }
    }

    /**
     * Handle gamepad connected event
     */
    handleGamepadConnected(event) {
        console.log('Gamepad connected:', event.gamepad.id, 'index:', event.gamepad.index);

        // Use first connected gamepad
        if (this.gamepadIndex === null) {
            this.gamepadIndex = event.gamepad.index;
            this.lastGamepadState = {
                buttons: [],
                axes: []
            };
            console.log('Using gamepad index:', this.gamepadIndex);
        }
    }

    /**
     * Handle gamepad disconnected event
     */
    handleGamepadDisconnected(event) {
        console.log('Gamepad disconnected:', event.gamepad.id);

        if (this.gamepadIndex === event.gamepad.index) {
            this.gamepadIndex = null;
            this.lastGamepadState = null;
            console.log('Active gamepad disconnected');
        }
    }

    /**
     * Check for already connected gamepads
     */
    checkExistingGamepads() {
        const gamepads = navigator.getGamepads();
        for (let i = 0; i < gamepads.length; i++) {
            if (gamepads[i]) {
                this.handleGamepadConnected({ gamepad: gamepads[i] });
                break;
            }
        }
    }

    /**
     * Start polling gamepad state
     */
    startGamepadPolling() {
        // Poll at 60 Hz
        this.pollInterval = setInterval(() => {
            this.pollGamepad();
        }, 1000 / 60);
    }

    /**
     * Poll current gamepad state
     */
    pollGamepad() {
        if (!this.gamepadEnabled || this.gamepadIndex === null) {
            return;
        }

        const gamepads = navigator.getGamepads();
        const gamepad = gamepads[this.gamepadIndex];

        if (!gamepad) {
            return;
        }

        const gamepadId = `gamepad-${gamepad.index}`;

        try {
            // Poll buttons (only send changes)
            for (let i = 0; i < gamepad.buttons.length; i++) {
                const pressed = gamepad.buttons[i].pressed;
                const lastPressed = this.lastGamepadState.buttons[i] || false;

                if (pressed !== lastPressed) {
                    this.emulator.gamepadButton(gamepadId, i, pressed);
                    this.lastGamepadState.buttons[i] = pressed;
                }
            }

            // Poll axes (send all, let emulator handle deadzone)
            for (let i = 0; i < gamepad.axes.length; i++) {
                const value = gamepad.axes[i];
                this.emulator.gamepadAxis(gamepadId, i, value);
                this.lastGamepadState.axes[i] = value;
            }
        } catch (err) {
            console.error('Error polling gamepad:', err);
        }
    }

    /**
     * Enable/disable keyboard input
     */
    setKeyboardEnabled(enabled) {
        this.keyboardEnabled = enabled;

        // Clear pressed keys when disabling
        if (!enabled) {
            this.keysPressed.clear();
        }
    }

    /**
     * Enable/disable gamepad input
     */
    setGamepadEnabled(enabled) {
        this.gamepadEnabled = enabled;
    }

    /**
     * Get current gamepad info
     */
    getGamepadInfo() {
        if (this.gamepadIndex === null) {
            return null;
        }

        const gamepads = navigator.getGamepads();
        const gamepad = gamepads[this.gamepadIndex];

        if (!gamepad) {
            return null;
        }

        return {
            id: gamepad.id,
            index: gamepad.index,
            buttons: gamepad.buttons.length,
            axes: gamepad.axes.length,
            connected: gamepad.connected
        };
    }

    /**
     * Cleanup
     */
    destroy() {
        if (this.pollInterval) {
            clearInterval(this.pollInterval);
            this.pollInterval = null;
        }

        this.keysPressed.clear();
        this.lastGamepadState = null;

        console.log('InputHandler destroyed');
    }
}

