/**
 * InputDebugger - Visual debugging interface for input system
 * 
 * Features:
 * - Controller visual (SVG with real-time button highlights)
 * - Button state list
 * - I/O register viewer
 * - Input history
 * - Port assignments with provider selection
 */

export class InputDebugger {
    constructor(emulator) {
        this.emulator = emulator;
        this.updateInterval = null;
        this.updateRate = 30; // Hz (30 FPS for UI updates)

        console.log('InputDebugger created');
    }

    /**
     * Initialize debugger UI
     */
    init() {
        this.setupControllerViewer();
        this.setupPortAssignmentControls();
        this.setupPortInfo();
        this.setupInputHistory();
        this.startUpdateLoop();

        console.log('InputDebugger initialized');
    }

    /**
     * Setup controller viewer with SVG
     */
    setupControllerViewer() {
        const port1Visual = document.getElementById('port1-visual');
        const port2Visual = document.getElementById('port2-visual');

        if (port1Visual) {
            port1Visual.innerHTML = this.createControllerSVG('port1');
        }

        if (port2Visual) {
            port2Visual.innerHTML = this.createControllerSVG('port2');
        }
    }

    /**
     * Create Genesis 6-button controller SVG - accurate layout
     */
    createControllerSVG(portId) {
        return `
        <svg width="380" height="180" viewBox="0 0 380 180" class="controller-svg">
            <!-- Controller body -->
            <ellipse cx="190" cy="95" rx="185" ry="80" fill="#1a1a1a" stroke="#333" stroke-width="2"/>
            <ellipse cx="190" cy="95" rx="180" ry="76" fill="#222" stroke="none"/>
            
            <!-- Left grip -->
            <ellipse cx="55" cy="110" rx="50" ry="55" fill="#1a1a1a" stroke="#333" stroke-width="2"/>
            <ellipse cx="55" cy="110" rx="47" ry="52" fill="#222" stroke="none"/>
            
            <!-- Right grip -->
            <ellipse cx="325" cy="110" rx="50" ry="55" fill="#1a1a1a" stroke="#333" stroke-width="2"/>
            <ellipse cx="325" cy="110" rx="47" ry="52" fill="#222" stroke="none"/>
            
            <!-- D-Pad background circle -->
            <circle cx="90" cy="90" r="38" fill="#1a1a1a" stroke="#333" stroke-width="1"/>
            
            <!-- D-Pad -->
            <g id="${portId}-dpad" transform="translate(90, 90)">
                <!-- D-Pad cross shape -->
                <rect x="-10" y="-30" width="20" height="60" rx="3" fill="#2a2a2a" stroke="#444" stroke-width="1"/>
                <rect x="-30" y="-10" width="60" height="20" rx="3" fill="#2a2a2a" stroke="#444" stroke-width="1"/>
                
                <!-- Clickable areas -->
                <rect id="${portId}-btn-up" x="-10" y="-30" width="20" height="20" rx="3" class="controller-btn dpad-btn" fill-opacity="0"/>
                <text x="0" y="-16" text-anchor="middle" fill="#666" font-size="12" pointer-events="none">▲</text>
                
                <rect id="${portId}-btn-down" x="-10" y="10" width="20" height="20" rx="3" class="controller-btn dpad-btn" fill-opacity="0"/>
                <text x="0" y="24" text-anchor="middle" fill="#666" font-size="12" pointer-events="none">▼</text>
                
                <rect id="${portId}-btn-left" x="-30" y="-10" width="20" height="20" rx="3" class="controller-btn dpad-btn" fill-opacity="0"/>
                <text x="-20" y="4" text-anchor="middle" fill="#666" font-size="12" pointer-events="none">◀</text>
                
                <rect id="${portId}-btn-right" x="10" y="-10" width="20" height="20" rx="3" class="controller-btn dpad-btn" fill-opacity="0"/>
                <text x="20" y="4" text-anchor="middle" fill="#666" font-size="12" pointer-events="none">▶</text>
            </g>
            
            <!-- Start / Mode buttons -->
            <g id="${portId}-start-mode" transform="translate(190, 100)">
                <rect id="${portId}-btn-start" x="-28" y="-6" width="26" height="12" rx="6" class="controller-btn start-btn"/>
                <text x="-15" y="3" text-anchor="middle" fill="#888" font-size="7" font-weight="bold" pointer-events="none">START</text>
                
                <rect id="${portId}-btn-mode" x="2" y="-6" width="26" height="12" rx="6" class="controller-btn start-btn"/>
                <text x="15" y="3" text-anchor="middle" fill="#888" font-size="7" font-weight="bold" pointer-events="none">MODE</text>
            </g>
            
            <!-- Face Buttons - Bottom row: A, B, C (horizontal, slight arc) -->
            <g id="${portId}-face-buttons">
                <circle id="${portId}-btn-a" cx="260" cy="100" r="16" class="controller-btn face-btn"/>
                <text x="260" y="105" text-anchor="middle" fill="#888" font-size="13" font-weight="bold" pointer-events="none">A</text>
                
                <circle id="${portId}-btn-b" cx="296" cy="96" r="16" class="controller-btn face-btn"/>
                <text x="296" y="101" text-anchor="middle" fill="#888" font-size="13" font-weight="bold" pointer-events="none">B</text>
                
                <circle id="${portId}-btn-c" cx="332" cy="92" r="16" class="controller-btn face-btn"/>
                <text x="332" y="97" text-anchor="middle" fill="#888" font-size="13" font-weight="bold" pointer-events="none">C</text>
            </g>
            
            <!-- Extra Buttons - Top row: X, Y, Z (smaller, above A/B/C) -->
            <g id="${portId}-extra-buttons">
                <circle id="${portId}-btn-x" cx="260" cy="66" r="12" class="controller-btn extra-btn"/>
                <text x="260" y="70" text-anchor="middle" fill="#888" font-size="10" font-weight="bold" pointer-events="none">X</text>
                
                <circle id="${portId}-btn-y" cx="296" cy="62" r="12" class="controller-btn extra-btn"/>
                <text x="296" y="66" text-anchor="middle" fill="#888" font-size="10" font-weight="bold" pointer-events="none">Y</text>
                
                <circle id="${portId}-btn-z" cx="332" cy="58" r="12" class="controller-btn extra-btn"/>
                <text x="332" y="62" text-anchor="middle" fill="#888" font-size="10" font-weight="bold" pointer-events="none">Z</text>
            </g>
            
            <!-- Labels -->
            <text x="190" y="28" text-anchor="middle" fill="#444" font-size="9" font-family="Arial, sans-serif" letter-spacing="4">MEGA DRIVE</text>
        </svg>
        `;
    }

    /**
     * Setup port assignment dropdown controls
     */
    setupPortAssignmentControls() {
        // Port 1 dropdown
        const select1 = document.getElementById('port1-provider-select');
        if (select1) {
            select1.addEventListener('change', (e) => {
                try {
                    this.emulator.assignProviderToPort(e.target.value, 0);
                } catch (err) {
                    console.error('Error assigning provider to port 1:', err);
                }
            });
        }

        // Port 2 dropdown
        const select2 = document.getElementById('port2-provider-select');
        if (select2) {
            select2.addEventListener('change', (e) => {
                try {
                    this.emulator.assignProviderToPort(e.target.value, 1);
                } catch (err) {
                    console.error('Error assigning provider to port 2:', err);
                }
            });
        }

        // Swap button
        const swapBtn = document.getElementById('swap-ports-btn');
        if (swapBtn) {
            swapBtn.addEventListener('click', () => {
                try {
                    this.emulator.swapPortAssignments();
                    this.updateProviderDropdowns();
                } catch (err) {
                    console.error('Error swapping ports:', err);
                }
            });
        }
    }

    /**
     * Update provider dropdown options
     */
    updateProviderDropdowns() {
        try {
            const providers = this.emulator.getInputProviders();
            const select1 = document.getElementById('port1-provider-select');
            const select2 = document.getElementById('port2-provider-select');

            if (!select1 || !select2 || !providers) return;

            // Get current assignments
            const assignment0 = this.emulator.getPortAssignment(0);
            const assignment1 = this.emulator.getPortAssignment(1);

            // Rebuild options for both selects
            for (const [select, currentAssignment] of [[select1, assignment0], [select2, assignment1]]) {
                const currentValue = select.value;
                select.innerHTML = '<option value="">(none)</option>';

                providers.forEach(provider => {
                    const option = document.createElement('option');
                    option.value = provider.id;
                    const typeLabel = provider.provider_type === 'Keyboard' ? '⌨️' : '🎮';
                    option.textContent = `${typeLabel} ${provider.name || provider.id}`;
                    select.appendChild(option);
                });

                // Restore selection based on actual assignment from emulator
                if (currentAssignment) {
                    select.value = currentAssignment;
                } else {
                    select.value = '';
                }
            }
        } catch (err) {
            // Silently ignore
        }
    }

    /**
     * Setup port info section
     */
    setupPortInfo() {
        this.updatePortInfo();
    }

    /**
     * Setup input history section
     */
    setupInputHistory() {
        this.updateInputHistory();
    }

    /**
     * Main update loop
     */
    update() {
        this.updatePortStates();
        this.updatePortInfo();
        this.updateInputHistory();
    }

    /**
     * Update controller button states
     */
    updatePortStates() {
        try {
            for (let port = 0; port < 2; port++) {
                const portId = `port${port + 1}`;
                const state = this.emulator.getPortState(port);

                // Button mapping (matches Genesis button indices)
                const buttons = {
                    'up': (state & (1 << 0)) !== 0,
                    'down': (state & (1 << 1)) !== 0,
                    'left': (state & (1 << 2)) !== 0,
                    'right': (state & (1 << 3)) !== 0,
                    'b': (state & (1 << 4)) !== 0,
                    'c': (state & (1 << 5)) !== 0,
                    'a': (state & (1 << 6)) !== 0,
                    'start': (state & (1 << 7)) !== 0,
                    'z': (state & (1 << 8)) !== 0,
                    'y': (state & (1 << 9)) !== 0,
                    'x': (state & (1 << 10)) !== 0,
                    'mode': (state & (1 << 11)) !== 0,
                };

                // Update SVG button states
                for (const [button, pressed] of Object.entries(buttons)) {
                    const btnElement = document.getElementById(`${portId}-btn-${button}`);
                    if (btnElement) {
                        if (pressed) {
                            btnElement.classList.add('pressed');
                        } else {
                            btnElement.classList.remove('pressed');
                        }
                    }
                }

                // Update button list
                this.updateButtonList(portId, buttons, state);
            }
        } catch (err) {
            // Silently ignore errors during update
        }
    }

    /**
     * Update button list text
     */
    updateButtonList(portId, buttons, rawState) {
        const buttonList = document.getElementById(`${portId}-buttons`);
        if (!buttonList) return;

        const pressedButtons = Object.entries(buttons)
            .filter(([_, pressed]) => pressed)
            .map(([button, _]) => button.toUpperCase());

        if (pressedButtons.length > 0) {
            buttonList.textContent = `Pressed: ${pressedButtons.join(', ')}`;
            buttonList.classList.add('has-input');
        } else {
            buttonList.textContent = 'No input';
            buttonList.classList.remove('has-input');
        }

        // Show raw state
        const rawStateElement = document.getElementById(`${portId}-raw-state`);
        if (rawStateElement) {
            rawStateElement.textContent = `Raw: 0x${rawState.toString(16).padStart(4, '0').toUpperCase()}`;
        }
    }

    /**
     * Update port info (providers, assignments)
     */
    updatePortInfo() {
        try {
            const providers = this.emulator.getInputProviders();

            // Update provider list
            const providerList = document.getElementById('provider-list');
            if (providerList && providers) {
                providerList.innerHTML = '';

                providers.forEach(provider => {
                    const item = document.createElement('div');
                    item.className = 'provider-item';
                    const typeLabel = provider.provider_type === 'Keyboard' ? '⌨️' : '🎮';
                    item.innerHTML = `
                        <span class="provider-type">${typeLabel} ${provider.provider_type}</span>
                        <span class="provider-name">${provider.name || provider.id}</span>
                        <span class="provider-id">${provider.id}</span>
                    `;
                    providerList.appendChild(item);
                });
            }

            // Update provider dropdowns (only when providers change)
            this.updateProviderDropdowns();

            // Update port assignments display
            for (let port = 0; port < 2; port++) {
                const assignment = this.emulator.getPortAssignment(port);
                const assignmentElement = document.getElementById(`port${port + 1}-assignment`);

                if (assignmentElement) {
                    if (assignment) {
                        assignmentElement.textContent = `Assigned: ${assignment}`;
                        assignmentElement.classList.add('assigned');
                    } else {
                        assignmentElement.textContent = 'No provider assigned';
                        assignmentElement.classList.remove('assigned');
                    }
                }
            }
        } catch (err) {
            // Silently ignore errors
        }
    }

    /**
     * Update input history
     */
    updateInputHistory() {
        try {
            const history = this.emulator.getInputHistory(10);
            const historyList = document.getElementById('input-history-list');

            if (!historyList || !history || history.length === 0) {
                return;
            }

            historyList.innerHTML = '';

            history.forEach(snapshot => {
                const item = document.createElement('div');
                item.className = 'history-item';

                const port0 = snapshot.port_states[0];
                const port1 = snapshot.port_states[1];

                item.innerHTML = `
                    <span class="history-frame">Frame ${snapshot.frame}</span>
                    <span class="history-port">P1: 0x${port0.toString(16).padStart(4, '0')}</span>
                    <span class="history-port">P2: 0x${port1.toString(16).padStart(4, '0')}</span>
                `;

                historyList.appendChild(item);
            });
        } catch (err) {
            // Silently ignore errors
        }
    }

    /**
     * Start update loop
     */
    startUpdateLoop() {
        this.updateInterval = setInterval(() => {
            // Skip update if emulator is busy (running) to avoid aliasing errors
            try {
                const mode = this.emulator.getExecutionMode();
                if (mode === 'Running') {
                    return; // Skip during continuous execution
                }
            } catch (err) {
                return; // Emulator busy, skip
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
        console.log('InputDebugger destroyed');
    }
}

