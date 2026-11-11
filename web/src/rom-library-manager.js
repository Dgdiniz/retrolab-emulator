// ROM Library Manager, IndexedDB-backed store for user-imported
// ROMs, including metadata, on-disk blob handles, and basic
// CRUD/search helpers.

export class RomLibraryManager {
    constructor() {
        this.dbName = 'retrolab-rom-library';
        this.dbVersion = 1;
        this.db = null;
        this.onRomSelected = null; // Callback when ROM is selected for playing
    }

    /**
     * Initialize IndexedDB database
     */
    async init() {
        return new Promise((resolve, reject) => {
            const request = indexedDB.open(this.dbName, this.dbVersion);

            request.onerror = () => reject(request.error);
            request.onsuccess = () => {
                this.db = request.result;
                console.log('[ROM Library] Database initialized');
                resolve();
            };

            request.onupgradeneeded = (event) => {
                const db = event.target.result;

                // Create ROMs store
                if (!db.objectStoreNames.contains('roms')) {
                    const romsStore = db.createObjectStore('roms', { keyPath: 'hash' });
                    romsStore.createIndex('title', 'title', { unique: false });
                    romsStore.createIndex('system', 'system', { unique: false });
                    romsStore.createIndex('added', 'added', { unique: false });
                    console.log('[ROM Library] Created roms store');
                }

                // Create ROM metadata store (without ROM data for faster queries)
                if (!db.objectStoreNames.contains('romMetadata')) {
                    const metaStore = db.createObjectStore('romMetadata', { keyPath: 'hash' });
                    metaStore.createIndex('title', 'title', { unique: false });
                    metaStore.createIndex('system', 'system', { unique: false });
                    metaStore.createIndex('added', 'added', { unique: false });
                    console.log('[ROM Library] Created romMetadata store');
                }
            };
        });
    }

    /**
     * Add a ROM to the library
     * @param {File} file - ROM file
     * @param {Object} cartridgeInfo - Cartridge info from emulator
     * @returns {Promise<Object>} ROM metadata
     */
    async addRom(file, cartridgeInfo) {
        try {
            // Read file data
            const arrayBuffer = await file.arrayBuffer();
            const romData = new Uint8Array(arrayBuffer);

            // Create ROM entry
            const romEntry = {
                hash: cartridgeInfo.rom_hash,
                title: cartridgeInfo.title || file.name,
                system: cartridgeInfo.system || 'Unknown',
                fileName: file.name,
                fileSize: file.size,
                romSize: cartridgeInfo.rom_size || file.size,
                sramSize: cartridgeInfo.sram_size || 0,
                sramType: cartridgeInfo.sram_type || 'None',
                mapperInfo: cartridgeInfo.mapper_info || null,
                expansionChipInfo: cartridgeInfo.expansion_chip_info || null,
                regions: cartridgeInfo.regions || [],
                headerChecksum: cartridgeInfo.header_checksum,
                calculatedChecksum: cartridgeInfo.calculated_checksum,
                detectionSource: cartridgeInfo.detection_source || 'Header',
                added: new Date().toISOString(),
                lastPlayed: null,
                playCount: 0,
                data: romData,
            };

            // Create metadata (without ROM data)
            const metadata = { ...romEntry };
            delete metadata.data;

            // Save to IndexedDB
            const transaction = this.db.transaction(['roms', 'romMetadata'], 'readwrite');
            const romsStore = transaction.objectStore('roms');
            const metaStore = transaction.objectStore('romMetadata');

            await Promise.all([
                new Promise((resolve, reject) => {
                    const request = romsStore.put(romEntry);
                    request.onsuccess = () => resolve();
                    request.onerror = () => reject(request.error);
                }),
                new Promise((resolve, reject) => {
                    const request = metaStore.put(metadata);
                    request.onsuccess = () => resolve();
                    request.onerror = () => reject(request.error);
                }),
            ]);

            console.log(`[ROM Library] Added ROM: ${romEntry.title}`);
            return metadata;
        } catch (error) {
            console.error('[ROM Library] Failed to add ROM:', error);
            throw error;
        }
    }

    /**
     * Get all ROMs metadata (without ROM data for performance)
     * @returns {Promise<Array>} Array of ROM metadata
     */
    async listRoms() {
        return new Promise((resolve, reject) => {
            const transaction = this.db.transaction(['romMetadata'], 'readonly');
            const store = transaction.objectStore('romMetadata');
            const request = store.getAll();

            request.onsuccess = () => {
                const roms = request.result;
                // Sort by added date (newest first)
                roms.sort((a, b) => new Date(b.added) - new Date(a.added));
                resolve(roms);
            };
            request.onerror = () => reject(request.error);
        });
    }

    /**
     * Get ROM data by hash
     * @param {string} hash - ROM hash
     * @returns {Promise<Uint8Array>} ROM data
     */
    async getRomData(hash) {
        return new Promise((resolve, reject) => {
            const transaction = this.db.transaction(['roms'], 'readonly');
            const store = transaction.objectStore('roms');
            const request = store.get(hash);

            request.onsuccess = () => {
                if (request.result) {
                    resolve(request.result.data);
                } else {
                    reject(new Error('ROM not found'));
                }
            };
            request.onerror = () => reject(request.error);
        });
    }

    /**
     * Remove ROM from library
     * @param {string} hash - ROM hash
     */
    async removeRom(hash) {
        const transaction = this.db.transaction(['roms', 'romMetadata'], 'readwrite');
        const romsStore = transaction.objectStore('roms');
        const metaStore = transaction.objectStore('romMetadata');

        await Promise.all([
            new Promise((resolve, reject) => {
                const request = romsStore.delete(hash);
                request.onsuccess = () => resolve();
                request.onerror = () => reject(request.error);
            }),
            new Promise((resolve, reject) => {
                const request = metaStore.delete(hash);
                request.onsuccess = () => resolve();
                request.onerror = () => reject(request.error);
            }),
        ]);

        console.log(`[ROM Library] Removed ROM: ${hash}`);
    }

    /**
     * Update ROM metadata (e.g., last played, play count)
     * @param {string} hash - ROM hash
     * @param {Object} updates - Fields to update
     */
    async updateRomMetadata(hash, updates) {
        const transaction = this.db.transaction(['romMetadata'], 'readwrite');
        const store = transaction.objectStore('romMetadata');

        return new Promise((resolve, reject) => {
            const getRequest = store.get(hash);

            getRequest.onsuccess = () => {
                const metadata = getRequest.result;
                if (!metadata) {
                    reject(new Error('ROM not found'));
                    return;
                }

                // Apply updates
                Object.assign(metadata, updates);

                const putRequest = store.put(metadata);
                putRequest.onsuccess = () => resolve(metadata);
                putRequest.onerror = () => reject(putRequest.error);
            };

            getRequest.onerror = () => reject(getRequest.error);
        });
    }

    /**
     * Search ROMs by title
     * @param {string} query - Search query
     * @returns {Promise<Array>} Matching ROMs
     */
    async searchRoms(query) {
        const allRoms = await this.listRoms();
        const lowerQuery = query.toLowerCase();

        return allRoms.filter((rom) =>
            rom.title.toLowerCase().includes(lowerQuery) ||
            rom.fileName.toLowerCase().includes(lowerQuery)
        );
    }

    /**
     * Filter ROMs by system
     * @param {string} system - System name (e.g., 'Genesis', 'NES')
     * @returns {Promise<Array>} Filtered ROMs
     */
    async filterBySystem(system) {
        if (system === 'all') {
            return this.listRoms();
        }

        return new Promise((resolve, reject) => {
            const transaction = this.db.transaction(['romMetadata'], 'readonly');
            const store = transaction.objectStore('romMetadata');
            const index = store.index('system');
            const request = index.getAll(system);

            request.onsuccess = () => {
                const roms = request.result;
                roms.sort((a, b) => new Date(b.added) - new Date(a.added));
                resolve(roms);
            };
            request.onerror = () => reject(request.error);
        });
    }

    /**
     * Calculate storage usage
     * @returns {Promise<Object>} Storage stats
     */
    async getStorageStats() {
        const allRoms = await this.listRoms();

        const stats = {
            totalRoms: allRoms.length,
            totalSize: 0,
            bySystem: {},
        };

        for (const rom of allRoms) {
            stats.totalSize += rom.fileSize;

            if (!stats.bySystem[rom.system]) {
                stats.bySystem[rom.system] = { count: 0, size: 0 };
            }

            stats.bySystem[rom.system].count++;
            stats.bySystem[rom.system].size += rom.fileSize;
        }

        return stats;
    }

    /**
     * Format file size for display
     * @param {number} bytes - File size in bytes
     * @returns {string} Formatted size
     */
    static formatSize(bytes) {
        if (bytes < 1024) return `${bytes} B`;
        if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
        return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    }

    /**
     * Format date for display
     * @param {string} isoDate - ISO date string
     * @returns {string} Formatted date
     */
    static formatDate(isoDate) {
        const date = new Date(isoDate);
        const now = new Date();
        const diffMs = now - date;
        const diffDays = Math.floor(diffMs / (1000 * 60 * 60 * 24));

        if (diffDays === 0) return 'Today';
        if (diffDays === 1) return 'Yesterday';
        if (diffDays < 7) return `${diffDays} days ago`;
        if (diffDays < 30) return `${Math.floor(diffDays / 7)} weeks ago`;
        if (diffDays < 365) return `${Math.floor(diffDays / 30)} months ago`;
        return date.toLocaleDateString();
    }
}

