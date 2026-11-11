/**
 * Save State Manager - IndexedDB persistence for save states
 * 
 * Handles:
 * - Save state slots (1-10)
 * - Quick save/load
 * - Screenshot thumbnails
 * - Save state browser
 */

export class SaveStateManager {
    constructor() {
        this.dbName = 'retro-emulator-saves';
        this.dbVersion = 2;
        this.db = null;
        this.initialized = false;
        this.maxSlots = 10;
    }

    /**
     * Initialize IndexedDB
     * @returns {Promise<void>}
     */
    async init() {
        if (this.initialized) {
            return;
        }

        return new Promise((resolve, reject) => {
            const request = indexedDB.open(this.dbName, this.dbVersion);

            request.onerror = () => {
                console.error('IndexedDB error:', request.error);
                reject(request.error);
            };

            request.onsuccess = () => {
                this.db = request.result;
                this.initialized = true;
                console.log('SaveStateManager initialized');
                resolve();
            };

            request.onupgradeneeded = (event) => {
                const db = event.target.result;

                // Create save states store
                if (!db.objectStoreNames.contains('saveStates')) {
                    const stateStore = db.createObjectStore('saveStates', {
                        keyPath: 'id',
                        autoIncrement: true
                    });
                    stateStore.createIndex('romHash', 'romHash', { unique: false });
                    stateStore.createIndex('slot', 'slot', { unique: false });
                    stateStore.createIndex('created', 'created', { unique: false });
                    stateStore.createIndex('romHashSlot', ['romHash', 'slot'], { unique: false });
                    console.log('Created save states object store');
                }

                // Also create SRAM store if missing (shared DB)
                if (!db.objectStoreNames.contains('sram')) {
                    const sramStore = db.createObjectStore('sram', { keyPath: 'romHash' });
                    sramStore.createIndex('title', 'title', { unique: false });
                    sramStore.createIndex('lastModified', 'lastModified', { unique: false });
                    console.log('Created SRAM object store');
                }
            };
        });
    }

    /**
     * Save a save state
     * @param {string} romHash - ROM hash
     * @param {string} title - ROM title
     * @param {number} slot - Slot number (1-10, or 0 for quicksave)
     * @param {Uint8Array} stateData - Serialized state data
     * @param {Object} metadata - Additional metadata
     * @param {string} [screenshot] - Base64 PNG screenshot
     * @returns {Promise<number>} Save state ID
     */
    async saveSaveState(romHash, title, slot, stateData, metadata = {}, screenshot = null) {
        if (!this.initialized) {
            await this.init();
        }

        // Delete existing save in this slot for this ROM
        await this.deleteSaveStateBySlot(romHash, slot);

        const transaction = this.db.transaction(['saveStates'], 'readwrite');
        const store = transaction.objectStore('saveStates');

        const record = {
            romHash,
            title,
            slot,
            data: Array.from(stateData), // Convert Uint8Array to Array
            created: new Date().toISOString(),
            frameCount: metadata.frameCount || 0,
            emulatorVersion: metadata.emulatorVersion || 'unknown',
            screenshot: screenshot,
            size: stateData.length
        };

        return new Promise((resolve, reject) => {
            const request = store.add(record);
            request.onsuccess = () => {
                const slotName = slot === 0 ? 'Quick Save' : `Slot ${slot}`;
                console.log(`Save state saved for ${title} in ${slotName} (${stateData.length} bytes)`);
                resolve(request.result);
            };
            request.onerror = () => {
                console.error('Failed to save state:', request.error);
                reject(request.error);
            };
        });
    }

    /**
     * Load a save state from a specific slot
     * @param {string} romHash - ROM hash
     * @param {number} slot - Slot number
     * @returns {Promise<Object|null>} Save state object or null
     */
    async loadSaveState(romHash, slot) {
        if (!this.initialized) {
            await this.init();
        }

        const transaction = this.db.transaction(['saveStates'], 'readonly');
        const store = transaction.objectStore('saveStates');
        const index = store.index('romHashSlot');

        return new Promise((resolve, reject) => {
            const request = index.getAll([romHash, slot]);
            request.onsuccess = () => {
                const states = request.result;
                if (states.length > 0) {
                    // Return most recent if multiple exist
                    const state = states[states.length - 1];
                    const stateData = new Uint8Array(state.data);
                    console.log(`Save state loaded from slot ${slot} (${stateData.length} bytes)`);
                    resolve({
                        id: state.id,
                        romHash: state.romHash,
                        title: state.title,
                        slot: state.slot,
                        data: stateData,
                        created: state.created,
                        frameCount: state.frameCount,
                        screenshot: state.screenshot
                    });
                } else {
                    console.log(`No save state found in slot ${slot}`);
                    resolve(null);
                }
            };
            request.onerror = () => {
                console.error('Failed to load save state:', request.error);
                reject(request.error);
            };
        });
    }

    /**
     * List all save states for a ROM
     * @param {string} romHash - ROM hash
     * @returns {Promise<Array>} Array of save state info
     */
    async listSaveStates(romHash) {
        if (!this.initialized) {
            await this.init();
        }

        const transaction = this.db.transaction(['saveStates'], 'readonly');
        const store = transaction.objectStore('saveStates');
        const index = store.index('romHash');

        return new Promise((resolve, reject) => {
            const request = index.getAll(romHash);
            request.onsuccess = () => {
                const states = request.result.map(state => ({
                    id: state.id,
                    slot: state.slot,
                    created: state.created,
                    frameCount: state.frameCount,
                    size: state.size,
                    screenshot: state.screenshot
                }));

                // Sort by slot
                states.sort((a, b) => a.slot - b.slot);
                resolve(states);
            };
            request.onerror = () => {
                console.error('Failed to list save states:', request.error);
                reject(request.error);
            };
        });
    }

    /**
     * Delete a save state by ID
     * @param {number} id - Save state ID
     * @returns {Promise<void>}
     */
    async deleteSaveState(id) {
        if (!this.initialized) {
            await this.init();
        }

        const transaction = this.db.transaction(['saveStates'], 'readwrite');
        const store = transaction.objectStore('saveStates');

        return new Promise((resolve, reject) => {
            const request = store.delete(id);
            request.onsuccess = () => {
                console.log(`Save state ${id} deleted`);
                resolve();
            };
            request.onerror = () => {
                console.error('Failed to delete save state:', request.error);
                reject(request.error);
            };
        });
    }

    /**
     * Delete a save state by slot
     * @param {string} romHash - ROM hash
     * @param {number} slot - Slot number
     * @returns {Promise<void>}
     */
    async deleteSaveStateBySlot(romHash, slot) {
        if (!this.initialized) {
            await this.init();
        }

        const transaction = this.db.transaction(['saveStates'], 'readwrite');
        const store = transaction.objectStore('saveStates');
        const index = store.index('romHashSlot');

        return new Promise((resolve, reject) => {
            const request = index.getAll([romHash, slot]);
            request.onsuccess = () => {
                const states = request.result;

                // Delete all states in this slot
                const deletePromises = states.map(state => {
                    return new Promise((res, rej) => {
                        const delRequest = store.delete(state.id);
                        delRequest.onsuccess = () => res();
                        delRequest.onerror = () => rej(delRequest.error);
                    });
                });

                Promise.all(deletePromises).then(() => {
                    console.log(`Deleted ${states.length} save state(s) from slot ${slot}`);
                    resolve();
                }).catch(reject);
            };
            request.onerror = () => {
                console.error('Failed to delete save state by slot:', request.error);
                reject(request.error);
            };
        });
    }

    /**
     * Delete all save states for a ROM
     * @param {string} romHash - ROM hash
     * @returns {Promise<number>} Number of deleted states
     */
    async deleteAllSaveStates(romHash) {
        if (!this.initialized) {
            await this.init();
        }

        const transaction = this.db.transaction(['saveStates'], 'readwrite');
        const store = transaction.objectStore('saveStates');
        const index = store.index('romHash');

        return new Promise((resolve, reject) => {
            const request = index.getAll(romHash);
            request.onsuccess = () => {
                const states = request.result;

                const deletePromises = states.map(state => {
                    return new Promise((res, rej) => {
                        const delRequest = store.delete(state.id);
                        delRequest.onsuccess = () => res();
                        delRequest.onerror = () => rej(delRequest.error);
                    });
                });

                Promise.all(deletePromises).then(() => {
                    console.log(`Deleted ${states.length} save state(s) for ROM`);
                    resolve(states.length);
                }).catch(reject);
            };
            request.onerror = () => {
                console.error('Failed to delete all save states:', request.error);
                reject(request.error);
            };
        });
    }

    /**
     * Get total save state storage size
     * @returns {Promise<number>} Size in bytes
     */
    async getTotalSize() {
        if (!this.initialized) {
            await this.init();
        }

        const transaction = this.db.transaction(['saveStates'], 'readonly');
        const store = transaction.objectStore('saveStates');

        return new Promise((resolve, reject) => {
            const request = store.getAll();
            request.onsuccess = () => {
                const total = request.result.reduce((sum, state) => sum + state.size, 0);
                resolve(total);
            };
            request.onerror = () => {
                console.error('Failed to calculate total size:', request.error);
                reject(request.error);
            };
        });
    }

    /**
     * Export save state to file
     * @param {number} id - Save state ID
     * @param {string} filename - Output filename (without extension)
     * @returns {Promise<void>}
     */
    async exportSaveState(id, filename) {
        if (!this.initialized) {
            await this.init();
        }

        const transaction = this.db.transaction(['saveStates'], 'readonly');
        const store = transaction.objectStore('saveStates');

        return new Promise((resolve, reject) => {
            const request = store.get(id);
            request.onsuccess = () => {
                if (!request.result) {
                    reject(new Error('Save state not found'));
                    return;
                }

                const state = request.result;
                const stateData = new Uint8Array(state.data);

                // Create blob and download
                const blob = new Blob([stateData], { type: 'application/octet-stream' });
                const url = URL.createObjectURL(blob);

                const a = document.createElement('a');
                a.href = url;
                a.download = `${filename}.state`;
                a.click();

                URL.revokeObjectURL(url);
                console.log(`Save state exported to ${filename}.state`);
                resolve();
            };
            request.onerror = () => {
                console.error('Failed to export save state:', request.error);
                reject(request.error);
            };
        });
    }
}

// Create singleton instance
export const saveStateManager = new SaveStateManager();

