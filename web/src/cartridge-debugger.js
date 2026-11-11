// Cartridge Debugger, integrates the ROM Library, a per-cartridge
// manifest editor, the memory-map visualiser, and the SRAM hexdump
// into a single sub-tabbed debugger surface.

import { RomLibraryManager } from './rom-library-manager.js';

export class CartridgeDebugger {
    constructor(emulator) {
        this.emulator = emulator;
        this.romLibrary = new RomLibraryManager();
        this.initialized = false;
        this.currentMemoryMapData = null;
    }

    /**
     * Initialize cartridge debugger
     */
    async init() {
        if (this.initialized) return;

        try {
            await this.romLibrary.init();
            this.setupEventListeners();
            this.setupSubtabs();
            this.initManifestEditor();
            this.initialized = true;
            console.log('[Cartridge Debugger] Initialized');
        } catch (error) {
            console.error('[Cartridge Debugger] Initialization failed:', error);
        }
    }

    /**
     * Setup subtabs navigation
     */
    setupSubtabs() {
        const subtabs = document.querySelectorAll('.cartridge-subtab');
        const subtabContents = document.querySelectorAll('.cartridge-subtab-content');

        subtabs.forEach(button => {
            button.addEventListener('click', () => {
                const targetTab = button.dataset.cartTab;

                // Update active state
                subtabs.forEach(btn => btn.classList.remove('active'));
                button.classList.add('active');

                subtabContents.forEach(content => {
                    const tabId = content.id.replace('cart-', '').replace('-tab', '');
                    content.classList.toggle('active', tabId === targetTab);
                });

                // Trigger refresh for specific tabs.
                if (targetTab === 'memmap') {
                    this.renderMemoryMap();
                } else if (targetTab === 'mapper') {
                    this.renderMapperState();
                } else if (targetTab === 'sram') {
                    this.renderSramHexdump();
                }
            });
        });
    }

    /**
     * Setup event listeners
     */
    setupEventListeners() {
        // ROM upload (debug page)
        const uploadZone = document.getElementById('rom-upload-zone');
        const fileInput = document.getElementById('rom-file-input-debug');
        const browseBtn = document.getElementById('rom-browse-btn-debug');

        if (uploadZone && fileInput) {
            // Drag & drop
            uploadZone.addEventListener('dragover', (e) => {
                e.preventDefault();
                uploadZone.classList.add('drag-over');
            });

            uploadZone.addEventListener('dragleave', () => {
                uploadZone.classList.remove('drag-over');
            });

            uploadZone.addEventListener('drop', async (e) => {
                e.preventDefault();
                uploadZone.classList.remove('drag-over');
                const files = Array.from(e.dataTransfer.files);
                await this.handleFiles(files);
            });
        }

        if (browseBtn && fileInput) {
            browseBtn.addEventListener('click', () => fileInput.click());
        }

        if (fileInput) {
            fileInput.addEventListener('change', async (e) => {
                const files = Array.from(e.target.files);
                await this.handleFiles(files);
                fileInput.value = '';
            });
        }

        // Search and filter
        const searchInput = document.getElementById('rom-search-debug');
        if (searchInput) {
            searchInput.addEventListener('input', () => this.refreshRomList());
        }

        const filterSelect = document.getElementById('rom-filter-debug');
        if (filterSelect) {
            filterSelect.addEventListener('change', () => this.refreshRomList());
        }

        // SRAM controls
        const exportBtn = document.getElementById('export-sram-btn');
        if (exportBtn) {
            exportBtn.addEventListener('click', () => this.exportSram());
        }

        const importBtn = document.getElementById('import-sram-btn');
        const sramFileInput = document.getElementById('sram-file-input');
        if (importBtn && sramFileInput) {
            importBtn.addEventListener('click', () => sramFileInput.click());
            sramFileInput.addEventListener('change', async (e) => {
                if (e.target.files[0]) {
                    await this.importSram(e.target.files[0]);
                    sramFileInput.value = '';
                }
            });
        }

        const clearBtn = document.getElementById('clear-sram-btn');
        if (clearBtn) {
            clearBtn.addEventListener('click', () => this.clearSram());
        }
    }

    /**
     * Handle uploaded ROM files
     */
    async handleFiles(files) {
        for (const file of files) {
            const ext = file.name.substring(file.name.lastIndexOf('.')).toLowerCase();
            if (!['.bin', '.gen', '.md', '.smd'].includes(ext)) {
                console.warn(`[Cartridge Debugger] Skipping invalid file: ${file.name}`);
                continue;
            }

            try {
                // Read file
                const arrayBuffer = await file.arrayBuffer();
                const romData = new Uint8Array(arrayBuffer);

                // Load ROM temporarily to get info
                await this.emulator.loadRom(romData);
                const cartridgeInfo = await this.emulator.getCartridgeInfo();

                // Add to library
                await this.romLibrary.addRom(file, cartridgeInfo);

                console.log(`[Cartridge Debugger] Added ROM: ${cartridgeInfo.title || file.name}`);
            } catch (error) {
                console.error(`[Cartridge Debugger] Failed to add ${file.name}:`, error);
            }
        }

        await this.refreshRomList();
        this.updateCartridgeInfo();
    }

    /**
     * Refresh ROM list
     */
    async refreshRomList() {
        const searchQuery = document.getElementById('rom-search-debug')?.value || '';
        const filterValue = document.getElementById('rom-filter-debug')?.value || 'all';

        let roms;
        if (filterValue === 'all') {
            roms = await this.romLibrary.listRoms();
        } else {
            roms = await this.romLibrary.filterBySystem(filterValue);
        }

        if (searchQuery) {
            const query = searchQuery.toLowerCase();
            roms = roms.filter(rom =>
                rom.title.toLowerCase().includes(query) ||
                rom.fileName.toLowerCase().includes(query)
            );
        }

        this.renderRomList(roms);
    }

    /**
     * Render ROM list
     */
    renderRomList(roms) {
        const listContainer = document.getElementById('rom-list-debug');
        if (!listContainer) return;

        if (roms.length === 0) {
            listContainer.innerHTML = '<div class="empty-state">No ROMs found</div>';
            return;
        }

        listContainer.innerHTML = roms.map(rom => `
            <div class="rom-item-compact" data-hash="${rom.hash}">
                <div class="title">${rom.title}</div>
                <div class="info">${RomLibraryManager.formatSize(rom.fileSize)}</div>
                <div class="actions">
                    <button class="load-btn" data-hash="${rom.hash}">Load</button>
                    <button class="delete-btn delete" data-hash="${rom.hash}">Delete</button>
                </div>
            </div>
        `).join('');

        // Add event listeners
        listContainer.querySelectorAll('.load-btn').forEach(btn => {
            btn.addEventListener('click', (e) => {
                e.stopPropagation();
                this.loadRomFromLibrary(btn.dataset.hash);
            });
        });

        listContainer.querySelectorAll('.delete-btn').forEach(btn => {
            btn.addEventListener('click', (e) => {
                e.stopPropagation();
                this.deleteRomFromLibrary(btn.dataset.hash);
            });
        });
    }

    /**
     * Load ROM from library
     */
    async loadRomFromLibrary(hash) {
        try {
            const romData = await this.romLibrary.getRomData(hash);
            await this.emulator.loadRom(romData);

            // Update metadata
            await this.romLibrary.updateRomMetadata(hash, {
                lastPlayed: new Date().toISOString(),
            });

            this.updateCartridgeInfo();
            console.log(`[Cartridge Debugger] Loaded ROM: ${hash.substring(0, 8)}...`);
        } catch (error) {
            console.error('[Cartridge Debugger] Failed to load ROM:', error);
        }
    }

    /**
     * Delete ROM from library
     */
    async deleteRomFromLibrary(hash) {
        if (!confirm('Delete ROM from library?')) return;

        try {
            await this.romLibrary.removeRom(hash);
            await this.refreshRomList();
            console.log(`[Cartridge Debugger] Deleted ROM: ${hash.substring(0, 8)}...`);
        } catch (error) {
            console.error('[Cartridge Debugger] Failed to delete ROM:', error);
        }
    }

    /**
     * Update cartridge info display
     */
    async updateCartridgeInfo() {
        try {
            const info = await this.emulator.getCartridgeInfo();

            document.getElementById('cart-title').textContent = info.title || '-';
            document.getElementById('cart-system').textContent = info.system || '-';
            document.getElementById('cart-rom-size').textContent = RomLibraryManager.formatSize(info.rom_size || 0);
            document.getElementById('cart-sram-size').textContent = info.sram_size ? RomLibraryManager.formatSize(info.sram_size) : 'None';
            document.getElementById('cart-mapper').textContent = info.mapper_info?.name || 'None';
            document.getElementById('cart-chip').textContent = info.expansion_chip_info?.name || 'None';
            document.getElementById('cart-regions').textContent = (info.regions || []).join(', ') || '-';
            document.getElementById('cart-detection').textContent = info.detection_source || '-';

            const checksumMatch = info.header_checksum === info.calculated_checksum;
            const checksumText = checksumMatch ?
                `✓ Match (0x${(info.header_checksum || 0).toString(16)})` :
                `⚠️ Mismatch (header: 0x${(info.header_checksum || 0).toString(16)}, calc: 0x${(info.calculated_checksum || 0).toString(16)})`;
            document.getElementById('cart-checksum').textContent = checksumText;

            const hash = await this.emulator.getCartridgeHash();
            document.getElementById('cart-hash').textContent = hash || '-';

            // Update mapper state if visible
            if (document.getElementById('cart-mapper-tab').classList.contains('active')) {
                this.updateMapperState();
            }
        } catch (error) {
            console.error('[Cartridge Debugger] Failed to update info:', error);
        }
    }

    /**
     * Update mapper state display
     */
    async updateMapperState() {
        try {
            const mapperState = await this.emulator.getMapperState();

            // TODO: show the state through renderMapperState() instead of these placeholders
            document.getElementById('mapper-registers').innerHTML = '<div class="empty-state">Mapper state display coming soon</div>';
            document.getElementById('bank-configuration').innerHTML = '<div class="empty-state">Bank configuration display coming soon</div>';
        } catch (error) {
            console.log('[Cartridge Debugger] Mapper state not available');
        }
    }

    /**
     * Render SRAM hexdump
     */
    async renderSramHexdump() {
        const container = document.getElementById('sram-hexdump');
        if (!container) return;

        try {
            const sram = await this.emulator.getSaveRam();

            if (!sram || sram.length === 0) {
                container.innerHTML = '<div class="empty-state">No SRAM available</div>';
                return;
            }

            let html = '';
            const bytesPerLine = 16;

            for (let i = 0; i < sram.length; i += bytesPerLine) {
                const addr = i.toString(16).padStart(6, '0').toUpperCase();
                const bytes = [];
                const ascii = [];

                for (let j = 0; j < bytesPerLine && (i + j) < sram.length; j++) {
                    const byte = sram[i + j];
                    bytes.push(byte.toString(16).padStart(2, '0').toUpperCase());
                    ascii.push(byte >= 32 && byte <= 126 ? String.fromCharCode(byte) : '.');
                }

                html += `
                    <div class="hexdump-line">
                        <div class="hexdump-address">${addr}</div>
                        <div class="hexdump-bytes">${bytes.join(' ')}</div>
                        <div class="hexdump-ascii">${ascii.join('')}</div>
                    </div>
                `;
            }

            container.innerHTML = html;
        } catch (error) {
            container.innerHTML = '<div class="empty-state">Failed to load SRAM</div>';
            console.error('[Cartridge Debugger] Failed to render SRAM:', error);
        }
    }

    /**
     * Export SRAM to .srm file
     */
    async exportSram() {
        try {
            const sram = await this.emulator.getSaveRam();
            if (!sram || sram.length === 0) {
                alert('No SRAM to export');
                return;
            }

            const hash = await this.emulator.getCartridgeHash();
            const filename = `${hash.substring(0, 8)}.srm`;

            const blob = new Blob([sram], { type: 'application/octet-stream' });
            const url = URL.createObjectURL(blob);
            const a = document.createElement('a');
            a.href = url;
            a.download = filename;
            a.click();
            URL.revokeObjectURL(url);

            console.log(`[Cartridge Debugger] Exported SRAM: ${filename}`);
        } catch (error) {
            console.error('[Cartridge Debugger] Failed to export SRAM:', error);
            alert('Failed to export SRAM');
        }
    }

    /**
     * Import SRAM from .srm file
     */
    async importSram(file) {
        try {
            const arrayBuffer = await file.arrayBuffer();
            const sramData = new Uint8Array(arrayBuffer);

            await this.emulator.setSaveRam(sramData);
            this.renderSramHexdump();

            console.log(`[Cartridge Debugger] Imported SRAM: ${file.name}`);
        } catch (error) {
            console.error('[Cartridge Debugger] Failed to import SRAM:', error);
            alert('Failed to import SRAM');
        }
    }

    /**
     * Clear SRAM
     */
    async clearSram() {
        if (!confirm('Clear all SRAM data?')) return;

        try {
            const sram = await this.emulator.getSaveRam();
            if (sram && sram.length > 0) {
                const cleared = new Uint8Array(sram.length).fill(0xFF);
                await this.emulator.setSaveRam(cleared);
                this.renderSramHexdump();
                console.log('[Cartridge Debugger] Cleared SRAM');
            }
        } catch (error) {
            console.error('[Cartridge Debugger] Failed to clear SRAM:', error);
        }
    }

    /**
     * Initialize the per-cartridge manifest editor UI (mapper type,
     * SRAM size, region overrides) and wire its load/save buttons.
     */
    initManifestEditor() {
        const hasSramCheckbox = document.getElementById('manifest-has-sram');
        const sramConfig = document.getElementById('sram-config');
        const loadBtn = document.getElementById('load-manifest-btn');
        const saveBtn = document.getElementById('save-manifest-btn');
        const applyBtn = document.getElementById('apply-manifest-btn');
        const resetBtn = document.getElementById('reset-manifest-btn');
        const manifestFileInput = document.getElementById('manifest-file-input');

        // Toggle SRAM config visibility
        hasSramCheckbox.addEventListener('change', () => {
            sramConfig.style.display = hasSramCheckbox.checked ? 'block' : 'none';
        });

        // Update JSON preview when form changes
        const formInputs = document.querySelectorAll('#manifest-form input, #manifest-form select');
        formInputs.forEach(input => {
            input.addEventListener('change', () => this.updateManifestPreview());
            input.addEventListener('input', () => this.updateManifestPreview());
        });

        // Load manifest from JSON file
        loadBtn.addEventListener('click', () => {
            manifestFileInput.click();
        });

        manifestFileInput.addEventListener('change', async (e) => {
            const file = e.target.files[0];
            if (file) {
                try {
                    const text = await file.text();
                    const manifest = JSON.parse(text);
                    this.loadManifestIntoForm(manifest);
                    console.log('[Manifest Editor] Loaded manifest from file');
                } catch (error) {
                    alert('Failed to load manifest: ' + error.message);
                }
            }
        });

        // Export manifest to JSON file
        saveBtn.addEventListener('click', () => {
            const manifest = this.getManifestFromForm();
            const json = JSON.stringify(manifest, null, 2);
            const blob = new Blob([json], { type: 'application/json' });
            const url = URL.createObjectURL(blob);
            const a = document.createElement('a');
            a.href = url;
            a.download = `${manifest.title || 'rom'}-manifest.json`;
            a.click();
            URL.revokeObjectURL(url);
            console.log('[Manifest Editor] Exported manifest');
        });

        // Apply manifest to currently loaded ROM
        applyBtn.addEventListener('click', async () => {
            const manifest = this.getManifestFromForm();
            console.log('[Manifest Editor] Applying manifest:', manifest);
            alert('Manifest apply functionality requires emulator reload.\nFor now, save the manifest and load it with the ROM.');
            // TODO: Implement manifest application via WASM
        });

        // Reset to auto-detect
        resetBtn.addEventListener('click', () => {
            if (confirm('Reset manifest to auto-detected values?')) {
                this.loadAutoDetectedManifest();
            }
        });
    }

    /**
     * Get manifest from form inputs
     */
    getManifestFromForm() {
        const manifest = {
            title: document.getElementById('manifest-title').value || 'Unknown',
            regions: [],
            mapper: document.getElementById('manifest-mapper').value,
            chip: document.getElementById('manifest-chip').value,
            eeprom: document.getElementById('manifest-eeprom').value,
        };

        // Regions
        if (document.getElementById('region-j').checked) manifest.regions.push('J');
        if (document.getElementById('region-u').checked) manifest.regions.push('U');
        if (document.getElementById('region-e').checked) manifest.regions.push('E');

        // SRAM
        if (document.getElementById('manifest-has-sram').checked) {
            manifest.sram = {
                start: document.getElementById('manifest-sram-start').value || '0x200000',
                end: document.getElementById('manifest-sram-end').value || '0x200FFF',
                odd_only: document.getElementById('manifest-sram-odd-only').checked,
            };
        }

        return manifest;
    }

    /**
     * Load manifest into form
     */
    loadManifestIntoForm(manifest) {
        document.getElementById('manifest-title').value = manifest.title || '';
        document.getElementById('manifest-mapper').value = manifest.mapper || 'None';
        document.getElementById('manifest-chip').value = manifest.chip || 'None';
        document.getElementById('manifest-eeprom').value = manifest.eeprom || 'None';

        // Regions
        document.getElementById('region-j').checked = manifest.regions?.includes('J') || false;
        document.getElementById('region-u').checked = manifest.regions?.includes('U') || false;
        document.getElementById('region-e').checked = manifest.regions?.includes('E') || false;

        // SRAM
        const hasSram = !!manifest.sram;
        document.getElementById('manifest-has-sram').checked = hasSram;
        document.getElementById('sram-config').style.display = hasSram ? 'block' : 'none';

        if (hasSram) {
            document.getElementById('manifest-sram-start').value = manifest.sram.start || '0x200000';
            document.getElementById('manifest-sram-end').value = manifest.sram.end || '0x200FFF';
            document.getElementById('manifest-sram-odd-only').checked = manifest.sram.odd_only || false;
        }

        this.updateManifestPreview();
    }

    /**
     * Load auto-detected manifest (from current ROM)
     */
    async loadAutoDetectedManifest() {
        try {
            const info = await this.emulator.getCartridgeInfo();

            const manifest = {
                title: info.title || 'Unknown',
                regions: info.regions || [],
                mapper: info.mapper?.type || 'None',
                chip: info.expansion_chip?.type || 'None',
                eeprom: 'None', // Not exposed in current WASM API
            };

            if (info.sram_size > 0) {
                manifest.sram = {
                    start: '0x200000', // Default Genesis SRAM location
                    end: `0x${(0x200000 + info.sram_size - 1).toString(16).toUpperCase()}`,
                    odd_only: false,
                };
            }

            document.getElementById('manifest-detection-source').textContent = info.detection_source || 'Header';
            this.loadManifestIntoForm(manifest);

            console.log('[Manifest Editor] Loaded auto-detected manifest');
        } catch (error) {
            console.error('[Manifest Editor] Failed to load auto-detected manifest:', error);
        }
    }

    /**
     * Update JSON preview
     */
    updateManifestPreview() {
        const manifest = this.getManifestFromForm();
        const json = JSON.stringify(manifest, null, 2);
        document.getElementById('manifest-json-preview').textContent = json;
    }

    /**
     * Render the cartridge memory-map visualiser onto the
     * `#memory-map-canvas` element, colouring each region (ROM banks,
     * SRAM, mapper-mapped windows) for at-a-glance inspection.
     */
    async renderMemoryMap() {
        const canvas = document.getElementById('memory-map-canvas');
        if (!canvas) return;

        const ctx = canvas.getContext('2d');
        const width = canvas.width;
        const height = canvas.height;

        // Clear canvas
        ctx.fillStyle = '#1e1e1e';
        ctx.fillRect(0, 0, width, height);

        try {
            // Get memory map from emulator
            const memoryMap = await this.emulator.getMemoryMap();

            if (!memoryMap || !memoryMap.regions) {
                this.renderMemoryMapError(ctx, width, height, 'No memory map data');
                return;
            }

            // Draw memory regions
            const totalAddressSpace = 0x1000000; // 16MB (Genesis address space)
            const regions = memoryMap.regions;

            // Draw each region
            regions.forEach(region => {
                const startRatio = region.start / totalAddressSpace;
                const endRatio = region.end / totalAddressSpace;
                const startY = startRatio * height;
                const regionHeight = (endRatio - startRatio) * height;

                // Draw region bar
                ctx.fillStyle = region.color || '#4CAF50';
                ctx.fillRect(50, startY, width - 200, regionHeight);

                // Draw region border
                ctx.strokeStyle = '#ffffff';
                ctx.lineWidth = 1;
                ctx.strokeRect(50, startY, width - 200, regionHeight);

                // Draw start address
                ctx.fillStyle = '#ffffff';
                ctx.font = '12px monospace';
                ctx.textAlign = 'right';
                ctx.fillText(`0x${region.start.toString(16).toUpperCase().padStart(6, '0')}`, 45, startY + 12);

                // Draw region type/description
                if (regionHeight > 20) {
                    ctx.textAlign = 'left';
                    ctx.fillStyle = '#ffffff';
                    ctx.font = 'bold 13px sans-serif';
                    const textY = startY + regionHeight / 2 + 5;
                    ctx.fillText(region.description, width - 190, textY);
                }
            });

            // Draw legend
            this.renderMemoryMapLegend(regions);

            console.log('[Memory Map] Rendered successfully');
        } catch (error) {
            console.error('[Memory Map] Render error:', error);
            this.renderMemoryMapError(ctx, width, height, error.message);
        }
    }

    /**
     * Render memory map error message
     */
    renderMemoryMapError(ctx, width, height, message) {
        ctx.fillStyle = '#ffffff';
        ctx.font = '14px sans-serif';
        ctx.textAlign = 'center';
        ctx.fillText(message || 'Error loading memory map', width / 2, height / 2);
    }

    /**
     * Render memory map legend
     */
    renderMemoryMapLegend(regions) {
        const legendDiv = document.getElementById('memory-map-legend');
        if (!legendDiv) return;

        legendDiv.innerHTML = '<h5>Memory Regions</h5>';

        const legendList = document.createElement('div');
        legendList.className = 'legend-list';

        regions.forEach(region => {
            const item = document.createElement('div');
            item.className = 'legend-item';

            const colorBox = document.createElement('span');
            colorBox.className = 'legend-color';
            colorBox.style.backgroundColor = region.color || '#4CAF50';

            const label = document.createElement('span');
            label.className = 'legend-label';
            label.textContent = `${region.type}: ${region.description}`;

            item.appendChild(colorBox);
            item.appendChild(label);
            legendList.appendChild(item);
        });

        legendDiv.appendChild(legendList);
    }

    /**
     * Render the active mapper's runtime state (bank registers,
     * mirroring, control bits) in the cartridge debugger.
     */
    async renderMapperState() {
        try {
            const mapperState = await this.emulator.getMapperState();

            if (!mapperState) {
                this.renderMapperEmpty();
                return;
            }

            // Render registers
            this.renderMapperRegisters(mapperState.registers);

            // Render bank configuration
            this.renderBankConfiguration(mapperState.banks);

            console.log('[Mapper State] Rendered successfully');
        } catch (error) {
            console.error('[Mapper State] Render error:', error);
            this.renderMapperEmpty();
        }
    }

    /**
     * Render mapper registers
     */
    renderMapperRegisters(registers) {
        const container = document.getElementById('mapper-registers');
        if (!container) return;

        if (!registers || registers.length === 0) {
            container.innerHTML = '<p class="info-message">No mapper registers</p>';
            return;
        }

        let html = '<table class="register-table"><thead><tr>';
        html += '<th>Address</th><th>Name</th><th>Value</th><th>Description</th>';
        html += '</tr></thead><tbody>';

        registers.forEach(reg => {
            html += '<tr>';
            html += `<td class="mono">0x${reg.address.toString(16).toUpperCase().padStart(6, '0')}</td>`;
            html += `<td>${reg.name}</td>`;
            html += `<td class="mono">0x${reg.value.toString(16).toUpperCase().padStart(2, '0')}</td>`;
            html += `<td class="desc">${reg.description}</td>`;
            html += '</tr>';
        });

        html += '</tbody></table>';
        container.innerHTML = html;
    }

    /**
     * Render bank configuration table
     */
    renderBankConfiguration(banks) {
        const container = document.getElementById('bank-configuration');
        if (!container) return;

        if (!banks || banks.length === 0) {
            container.innerHTML = '<p class="info-message">No bank configuration</p>';
            return;
        }

        let html = '<table class="bank-table"><thead><tr>';
        html += '<th>Bank</th><th>CPU Address</th><th>ROM Offset</th><th>Size</th>';
        html += '</tr></thead><tbody>';

        banks.forEach(bank => {
            html += '<tr>';
            html += `<td>${bank.bank_number}</td>`;
            html += `<td class="mono">0x${bank.cpu_address_start.toString(16).toUpperCase().padStart(6, '0')} - 0x${bank.cpu_address_end.toString(16).toUpperCase().padStart(6, '0')}</td>`;
            html += `<td class="mono">0x${bank.physical_offset.toString(16).toUpperCase().padStart(6, '0')}</td>`;
            html += `<td>${this.formatSize(bank.size)}</td>`;
            html += '</tr>';
        });

        html += '</tbody></table>';
        container.innerHTML = html;
    }

    /**
     * Render empty mapper state
     */
    renderMapperEmpty() {
        const registersContainer = document.getElementById('mapper-registers');
        const banksContainer = document.getElementById('bank-configuration');

        if (registersContainer) {
            registersContainer.innerHTML = '<p class="info-message">No mapper detected</p>';
        }

        if (banksContainer) {
            banksContainer.innerHTML = '<p class="info-message">No bank configuration</p>';
        }
    }

    /**
     * Render the expansion-chip state (SVP, J-Cart, lock-on, etc.)
     * for cartridges that ship extra on-board hardware.
     */
    async renderChipState() {
        try {
            const chipState = await this.emulator.getChipState();

            if (!chipState) {
                this.renderChipEmpty();
                return;
            }

            // Render chip info and registers
            const container = document.getElementById('chip-info');
            if (container) {
                let html = `<h5>${chipState.chip_type}</h5>`;
                html += `<p>Cycle Count: ${chipState.cycle_count || 0}</p>`;

                if (chipState.registers && chipState.registers.length > 0) {
                    html += '<table class="register-table"><thead><tr>';
                    html += '<th>Register</th><th>Value</th></tr></thead><tbody>';

                    chipState.registers.forEach(reg => {
                        html += '<tr>';
                        html += `<td>${reg.name}</td>`;
                        html += `<td class="mono">0x${reg.value.toString(16).toUpperCase()}</td>`;
                        html += '</tr>';
                    });

                    html += '</tbody></table>';
                }

                container.innerHTML = html;
            }

            console.log('[Chip State] Rendered successfully');
        } catch (error) {
            console.error('[Chip State] Render error:', error);
            this.renderChipEmpty();
        }
    }

    /**
     * Render empty chip state
     */
    renderChipEmpty() {
        const container = document.getElementById('chip-info');
        if (container) {
            container.innerHTML = '<p class="info-message">No expansion chip detected</p>';
        }
    }

    /**
     * Format size helper
     */
    formatSize(bytes) {
        if (bytes >= 1024 * 1024) {
            return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
        } else if (bytes >= 1024) {
            return `${(bytes / 1024).toFixed(2)} KB`;
        } else {
            return `${bytes} bytes`;
        }
    }
}

