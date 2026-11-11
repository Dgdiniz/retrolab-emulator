/**
 * SRAM Manager - IndexedDB persistence for SRAM data
 * 
 * Handles:
 * - Automatic SRAM saving with debouncing
 * - IndexedDB storage and retrieval
 * - Export/Import .srm files
 * - ROM library management
 */

export class SRAMManager {
    constructor() {
        this.dbName = 'retro-emulator-saves';
        this.dbVersion = 2;
        this.db = null;
        this.initialized = false;
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
                console.log('IndexedDB initialized');
                resolve();
            };

            request.onupgradeneeded = (event) => {
                const db = event.target.result;

                // Create SRAM store
                if (!db.objectStoreNames.contains('sram')) {
                    const sramStore = db.createObjectStore('sram', { keyPath: 'romHash' });
                    sramStore.createIndex('title', 'title', { unique: false });
                    sramStore.createIndex('lastModified', 'lastModified', { unique: false });
                    console.log('Created SRAM object store');
                }

                // Also create saveStates store if missing (shared DB)
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
            };
        });
    }

    /**
     * Save SRAM data for a ROM
     * @param {string} romHash - ROM hash (SHA-256)
     * @param {string} title - ROM title
     * @param {Uint8Array} sramData - SRAM data
     * @returns {Promise<void>}
     */
    async saveSRAM(romHash, title, sramData) {
        if (!this.initialized) {
            await this.init();
        }

        const transaction = this.db.transaction(['sram'], 'readwrite');
        const store = transaction.objectStore('sram');

        const record = {
            romHash,
            title,
            data: Array.from(sramData), // Convert Uint8Array to Array for storage
            lastModified: new Date().toISOString(),
            size: sramData.length
        };

        return new Promise((resolve, reject) => {
            const request = store.put(record);
            request.onsuccess = () => {
                console.log(`SRAM saved for ${title} (${sramData.length} bytes)`);
                resolve();
            };
            request.onerror = () => {
                console.error('Failed to save SRAM:', request.error);
                reject(request.error);
            };
        });
    }

    /**
     * Load SRAM data for a ROM
     * @param {string} romHash - ROM hash
     * @returns {Promise<Uint8Array|null>}
     */
    async loadSRAM(romHash) {
        if (!this.initialized) {
            await this.init();
        }

        const transaction = this.db.transaction(['sram'], 'readonly');
        const store = transaction.objectStore('sram');

        return new Promise((resolve, reject) => {
            const request = store.get(romHash);
            request.onsuccess = () => {
                if (request.result) {
                    const data = new Uint8Array(request.result.data);
                    console.log(`SRAM loaded for ${request.result.title} (${data.length} bytes)`);
                    resolve(data);
                } else {
                    console.log(`No SRAM found for hash ${romHash}`);
                    resolve(null);
                }
            };
            request.onerror = () => {
                console.error('Failed to load SRAM:', request.error);
                reject(request.error);
            };
        });
    }

    /**
     * Export SRAM to .srm file
     * @param {string} romHash - ROM hash
     * @param {string} filename - Output filename (without extension)
     * @returns {Promise<void>}
     */
    async exportSRAM(romHash, filename) {
        const sramData = await this.loadSRAM(romHash);
        if (!sramData) {
            throw new Error('No SRAM data found');
        }

        // Create blob and download
        const blob = new Blob([sramData], { type: 'application/octet-stream' });
        const url = URL.createObjectURL(blob);

        const a = document.createElement('a');
        a.href = url;
        a.download = `${filename}.srm`;
        a.click();

        URL.revokeObjectURL(url);
        console.log(`SRAM exported to ${filename}.srm`);
    }

    /**
     * Import SRAM from .srm file
     * @param {string} romHash - ROM hash
     * @param {string} title - ROM title
     * @param {File} file - .srm file
     * @returns {Promise<void>}
     */
    async importSRAM(romHash, title, file) {
        const arrayBuffer = await file.arrayBuffer();
        const uint8Array = new Uint8Array(arrayBuffer);

        await this.saveSRAM(romHash, title, uint8Array);
        console.log(`SRAM imported from ${file.name}`);
    }

    /**
     * Delete SRAM for a ROM
     * @param {string} romHash - ROM hash
     * @returns {Promise<void>}
     */
    async deleteSRAM(romHash) {
        if (!this.initialized) {
            await this.init();
        }

        const transaction = this.db.transaction(['sram'], 'readwrite');
        const store = transaction.objectStore('sram');

        return new Promise((resolve, reject) => {
            const request = store.delete(romHash);
            request.onsuccess = () => {
                console.log(`SRAM deleted for hash ${romHash}`);
                resolve();
            };
            request.onerror = () => {
                console.error('Failed to delete SRAM:', request.error);
                reject(request.error);
            };
        });
    }

    /**
     * List all saved SRAMs
     * @returns {Promise<Array>}
     */
    async listSRAMs() {
        if (!this.initialized) {
            await this.init();
        }

        const transaction = this.db.transaction(['sram'], 'readonly');
        const store = transaction.objectStore('sram');

        return new Promise((resolve, reject) => {
            const request = store.getAll();
            request.onsuccess = () => {
                const srams = request.result.map(record => ({
                    romHash: record.romHash,
                    title: record.title,
                    lastModified: record.lastModified,
                    size: record.size
                }));
                resolve(srams);
            };
            request.onerror = () => {
                console.error('Failed to list SRAMs:', request.error);
                reject(request.error);
            };
        });
    }

    /**
     * Get total SRAM storage size
     * @returns {Promise<number>} Size in bytes
     */
    async getTotalSize() {
        const srams = await this.listSRAMs();
        return srams.reduce((total, sram) => total + sram.size, 0);
    }
}

// Create singleton instance
export const sramManager = new SRAMManager();

