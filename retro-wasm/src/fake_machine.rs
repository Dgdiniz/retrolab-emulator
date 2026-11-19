//! What the binding tests' fake cores share: a payload, the machine items
//! no binding under test reaches, and [`FakeMachine`], the machine every
//! binding macro's tests expand on.

use retro_core::audio::{AudioChipInfo, AudioDebugger, ChannelState, ChannelType, HostAudioRing};
use retro_core::cartridge::{
    CartridgeDebugger, CartridgeInfo, ConsoleSystem, MapPlan, MapPlanDebugger, MapTarget, SlotShape,
};
use retro_core::debug::{BreakpointChecks, CpuDebugSpec, DebuggerState};
use retro_core::debugger::DebuggerCore;
use retro_core::input::{GenericInputManager, PadDefaults};
use retro_core::savestate::{PayloadUse, RomIdentity, SavestateFormat, SavestatePayload};
use retro_core::types::{
    AudioInfo, CpuArchitecture, CpuInfo, CpuLocation, DisasmRow, DisasmRowKind, DisplayInfo,
    Framebuffer, InputInfo, InputState, Instruction, PixelFormat, RomInfo, SystemInfo, VideoRegion,
};
use retro_core::video::{
    bytes_from_words, words_from_bytes, ExportError, IndexedImage, LayerData, LayerKind,
    LayerLayout, PaletteData, PixelImage, SpriteData, SpriteTableEntry, TileData,
    TileDecodeRequest, TileSize, TileSource, VideoDebugger, VideoMemoryId, VideoMemoryInfo,
    VideoRegister,
};
use retro_core::{EmulatorCore, StepResult};
use serde::{Deserialize, Serialize};

/// The payload of a fake core, which no test seals.
#[derive(Serialize, Deserialize)]
pub(crate) struct FakeSavestate;

impl SavestatePayload for FakeSavestate {
    retro_core::savestate_codec!();

    const FORMAT: SavestateFormat = SavestateFormat {
        core_id: "fake",
        version: 1,
    };
    type Extension = ();
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
    fn hash_parts(&self) -> Vec<(&'static str, u64)> {
        Vec::new()
    }
}

/// The `EmulatorCore` items of the machine (its savestate, resets, clocks,
/// region, audio ring, input and views) that a fake core never reaches.
macro_rules! unreachable_machine_items {
    () => {
        type Savestate = crate::fake_machine::FakeSavestate;
        fn rom_identity(&self) -> Option<retro_core::savestate::RomIdentity<'_>> {
            unreachable!()
        }
        fn savestate_payload(
            &self,
            _purpose: retro_core::savestate::PayloadUse,
        ) -> Self::Savestate {
            unreachable!()
        }
        fn savestate_extension(&self) {}
        fn run_frame_headless(&mut self) {
            unreachable!()
        }
        fn restore_savestate(&mut self, _payload: Self::Savestate, _extension: ()) {
            unreachable!()
        }
        fn power_cycle(&mut self) {
            unreachable!()
        }
        fn frame_count(&self) -> u64 {
            unreachable!()
        }
        fn master_cycles(&self) -> u64 {
            unreachable!()
        }
        fn master_clock_hz(&self) -> u64 {
            unreachable!()
        }
        fn region(&self) -> retro_core::types::VideoRegion {
            unreachable!()
        }
        fn apply_region(&mut self, _region: retro_core::types::VideoRegion) {
            unreachable!()
        }
        fn host_audio(&self) -> &retro_core::audio::HostAudioRing {
            unreachable!()
        }
        fn host_audio_mut(&mut self) -> &mut retro_core::audio::HostAudioRing {
            unreachable!()
        }
        fn input_manager(&self) -> &retro_core::input::GenericInputManager {
            unreachable!()
        }
        fn input_manager_mut(&mut self) -> &mut retro_core::input::GenericInputManager {
            unreachable!()
        }
        fn video_debugger(&mut self) -> &mut dyn retro_core::video::VideoDebugger {
            unreachable!()
        }
        fn audio_chip_count(&self) -> usize {
            unreachable!()
        }
        fn audio_debugger(
            &mut self,
            _chip: usize,
        ) -> Option<&mut dyn retro_core::audio::AudioDebugger> {
            unreachable!()
        }
        fn output_waveform(&self, _samples: u32) -> Vec<f32> {
            unreachable!()
        }
        fn cartridge_debugger(&mut self) -> Box<dyn retro_core::cartridge::CartridgeDebugger + '_> {
            unreachable!()
        }
        fn write_memory_debug(&mut self, _addr: u32, _data: &[u8]) {
            unreachable!()
        }
        fn disassemble_window(
            &mut self,
            _cpu_id: usize,
            _pc: u32,
            _before_count: usize,
            _after_count: usize,
        ) -> Vec<retro_core::types::DisasmRow> {
            unreachable!()
        }
        type Manifest = ();
        fn parse_manifest(_json: &str) -> Result<(), String> {
            unreachable!()
        }
        fn detected_manifest(&self, _rom: &[u8]) {
            unreachable!()
        }
        fn cartridge_manifest_override(&self) -> Option<&()> {
            unreachable!()
        }
        fn set_cartridge_manifest_override(&mut self, _manifest: Option<()>) {
            unreachable!()
        }
        fn last_rom(&self) -> &[u8] {
            unreachable!()
        }
        fn get_cartridge_hash(&self) -> Option<String> {
            unreachable!()
        }
        fn cartridge_info(&self) -> Option<&retro_core::cartridge::CartridgeInfo> {
            unreachable!()
        }
        fn is_cartridge_sram_dirty(&self) -> bool {
            unreachable!()
        }
        fn clear_cartridge_sram_dirty(&mut self) {
            unreachable!()
        }
    };
}
pub(crate) use unreachable_machine_items;

/// The first byte of a ROM [`FakeMachine`] refuses for a chip its build
/// lacks (`"Fake chip"`).
pub(crate) const NEEDS_A_CHIP: u8 = 0xC1;

/// The first byte of a ROM [`FakeMachine`]'s detection refuses: it loads
/// only with a manifest staged.
pub(crate) const SCORES_TOO_LOW: u8 = 0xD7;

/// The size of [`FakeMachine`]'s framebuffer, wider than its picture.
pub(crate) const FRAMEBUFFER_WIDTH: u32 = 8;
pub(crate) const FRAMEBUFFER_HEIGHT: u32 = 4;

/// The size of [`FakeMachine`]'s picture, a corner of its framebuffer.
pub(crate) const PICTURE_WIDTH: u32 = 6;
pub(crate) const PICTURE_HEIGHT: u32 = 3;

/// [`FakeMachine`]'s savestate: the frames it ran.
#[derive(Serialize, Deserialize)]
pub(crate) struct FakeMachineState {
    frames: u64,
}

impl SavestatePayload for FakeMachineState {
    retro_core::savestate_codec!();

    const FORMAT: SavestateFormat = SavestateFormat {
        core_id: "fake",
        version: 1,
    };
    type Extension = ();
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
    fn hash_parts(&self) -> Vec<(&'static str, u64)> {
        vec![("frames", self.frames)]
    }
}

/// A video chip with a small memory, a palette of four colours (words of
/// 15 bits, low byte first), two layers and two sprites, the first linking
/// to the second.
pub(crate) struct FakeVideo {
    vram: Vec<u8>,
    palette: [u16; 4],
    hidden: retro_core::video::LayerMask<2>,
}

/// A sound chip with a name and a number of channels, each of which can be
/// muted; its waveforms are the channel's number.
pub(crate) struct FakeSound {
    name: &'static str,
    muted: Vec<bool>,
}

impl AudioDebugger for FakeSound {
    fn chip_info(&self) -> AudioChipInfo {
        AudioChipInfo {
            name: self.name,
            manufacturer: "Nobody",
            channels: self.muted.len() as u32,
            sample_rate: 1000,
            stereo: false,
            channel_types: vec![ChannelType::Tone; self.muted.len()],
        }
    }
    fn get_channel_count(&self) -> u32 {
        self.muted.len() as u32
    }
    fn get_channel_state(&self, channel: u32) -> Option<ChannelState> {
        let muted = *self.muted.get(channel as usize)?;
        Some(ChannelState {
            channel_id: channel,
            enabled: true,
            muted,
            frequency: 0.0,
            frequency_raw: 0,
            period: 0,
            volume: 1.0,
            attenuation: 0,
            pan: 0.0,
            channel_type: ChannelType::Tone,
            envelope: None,
            current_output: 0.0,
            noise_params: None,
        })
    }
    fn mute_channel(&mut self, channel: u32, muted: bool) {
        if let Some(slot) = self.muted.get_mut(channel as usize) {
            *slot = muted;
        }
    }
    fn get_waveform(&self, channel: u32, samples: u32) -> Vec<f32> {
        vec![channel as f32; samples as usize]
    }
    fn get_output_waveform(&self, samples: u32) -> Vec<f32> {
        vec![0.5; samples as usize]
    }
}

impl VideoDebugger for FakeVideo {
    fn get_tile_data(&self, _index: u32) -> Option<TileData> {
        None
    }
    fn get_tile_count(&self) -> u32 {
        0
    }
    fn get_palette_data(&self) -> Vec<PaletteData> {
        vec![PaletteData {
            index: 0,
            colors: self
                .palette
                .iter()
                .map(|&colour| (u32::from(colour) << 8) | 0xFF)
                .collect(),
        }]
    }
    fn get_sprite_data(&self) -> Vec<SpriteData> {
        (0..2)
            .map(|index| SpriteData {
                index,
                x: index as i32 * 8,
                y: -4,
                width: 8,
                height: 16,
                tile_index: 2 * index,
                palette: 1,
                priority: 0,
                flip_h: false,
                flip_v: index == 1,
            })
            .collect()
    }
    fn get_layer_data(&self, layer_id: u32) -> Option<LayerData> {
        (layer_id < 2).then(|| LayerData {
            id: layer_id,
            name: format!("Layer {layer_id}"),
            enabled: !self.hidden.is_hidden(layer_id as usize),
            scroll_x: 0,
            scroll_y: 0,
            width: 32,
            height: 32,
        })
    }
    fn get_layer_count(&self) -> u32 {
        2
    }
    fn toggle_layer(&mut self, layer_id: u32, visible: bool) {
        self.hidden.set_hidden(layer_id as usize, !visible);
    }
    fn get_vram(&self) -> &[u8] {
        &self.vram
    }
    fn get_registers(&self) -> Vec<VideoRegister> {
        Vec::new()
    }
    fn export_sprite_image(&self, index: u32) -> Result<PixelImage, ExportError> {
        if index != 0 {
            return Err(ExportError::OutOfRange);
        }
        Ok(PixelImage {
            width: 1,
            height: 2,
            rgba: vec![1, 2, 3, 255, 4, 5, 6, 0],
        })
    }
    fn decode_tiles(&self, request: &TileDecodeRequest) -> Result<IndexedImage, ExportError> {
        if request.source != TileSource::Vram || request.tile_size != TileSize::Px8 {
            return Err(ExportError::Unsupported);
        }
        Ok(IndexedImage {
            width: 8,
            height: 8 * request.count,
            pixels: vec![request.first_tile as u8; 64 * request.count as usize],
            bpp: request.bpp,
            transparent_index: Some(0),
        })
    }
    fn video_memories(&self) -> Vec<VideoMemoryInfo> {
        vec![
            VideoMemoryInfo {
                id: VideoMemoryId::Vram,
                name: "VRAM",
                size: self.vram.len() as u32,
                address_unit: 1,
                little_endian: true,
            },
            VideoMemoryInfo {
                id: VideoMemoryId::Palette,
                name: "PAL",
                size: 8,
                address_unit: 2,
                little_endian: true,
            },
        ]
    }
    fn read_video_memory(&self, id: VideoMemoryId) -> Option<Vec<u8>> {
        match id {
            VideoMemoryId::Vram => Some(self.vram.clone()),
            VideoMemoryId::Palette => Some(bytes_from_words(&self.palette, true)),
            _ => None,
        }
    }
    fn write_video_memory(&mut self, id: VideoMemoryId, offset: u32, bytes: &[u8]) -> bool {
        match id {
            VideoMemoryId::Vram => {
                let len = self.vram.len();
                for (i, &byte) in bytes.iter().take(len).enumerate() {
                    self.vram[(offset as usize + i) % len] = byte;
                }
            }
            VideoMemoryId::Palette => {
                let words = words_from_bytes(bytes, true);
                for (i, &word) in words.iter().take(4).enumerate() {
                    self.palette[(offset as usize / 2 + i) % 4] = word & 0x7FFF;
                }
            }
            _ => return false,
        }
        true
    }
    fn sprite_table(&self) -> Vec<SpriteTableEntry> {
        self.get_sprite_data()
            .into_iter()
            .map(|sprite| SpriteTableEntry {
                index: sprite.index,
                x: sprite.x,
                y: sprite.y,
                width: sprite.width,
                height: sprite.height,
                tile_index: sprite.tile_index,
                palette: sprite.palette,
                priority: sprite.priority,
                flip_h: sprite.flip_h,
                flip_v: sprite.flip_v,
                next: (sprite.index == 0).then_some(1),
                tile_address: sprite.tile_index * 32,
                large: sprite.index == 1,
            })
            .collect()
    }
    fn layer_layout(&self, layer_id: u32) -> Option<LayerLayout> {
        (layer_id < 2).then(|| LayerLayout {
            id: layer_id,
            kind: LayerKind::TileMap,
            map_base: 16 * layer_id,
            map_width: 4,
            map_height: 2,
            character_base: 0,
            bits_per_pixel: 2,
            cell_width: 8,
            cell_height: 8,
            first_colour: 0,
            scroll_x: 0,
            scroll_y: 0,
        })
    }
}

/// The machine every binding macro's tests expand on: a framebuffer wider
/// than its picture (so a present buffer has a stride to pack away), the
/// host's sound ring, a real input manager with two ports, a small memory,
/// a [`FakeVideo`], a cartridge debugger over an empty plan, a one-CPU
/// debugger, and a ROM rule a test steers ([`NEEDS_A_CHIP`],
/// [`SCORES_TOO_LOW`]). Each frame fills the framebuffer with the frame's
/// number and, with sound on, queues two samples.
pub(crate) struct FakeMachine {
    system_info: SystemInfo,
    framebuffer: Framebuffer,
    frames: u64,
    region: VideoRegion,
    host_audio: HostAudioRing,
    audio_enabled: bool,
    input: GenericInputManager,
    rom: Vec<u8>,
    rom_hash: String,
    cartridge: Option<CartridgeInfo>,
    sram_dirty: bool,
    save_ram: Vec<u8>,
    manifest_override: Option<serde_json::Value>,
    unsupported_chip: Option<&'static str>,
    show_overscan: bool,
    memory: Vec<u8>,
    video: FakeVideo,
    sound: [FakeSound; 2],
    debugger: DebuggerState<1, ()>,
}

impl FakeMachine {
    /// A machine with no ROM loaded.
    pub(crate) fn new() -> Self {
        let system_info = SystemInfo {
            name: "Fake Machine".to_string(),
            short_name: "fake".to_string(),
            manufacturer: "None".to_string(),
            year: 2000,
            cpus: vec![CpuInfo {
                id: 0,
                name: "Fake CPU".to_string(),
                architecture: CpuArchitecture::M68000,
                clock_speed: 1,
                registers: Vec::new(),
                location: CpuLocation::Motherboard,
                address_width: 8,
            }],
            display: DisplayInfo {
                width: PICTURE_WIDTH,
                height: PICTURE_HEIGHT,
                aspect_ratio: 2.0,
                refresh_rate: 60.0,
                pixel_format: PixelFormat::RGBA8888,
            },
            audio: AudioInfo {
                sample_rate: 44100,
                channels: 2,
                bits_per_sample: 16,
            },
            input: InputInfo {
                max_players: 2,
                button_layout: Vec::new(),
            },
        };
        let pads = PadDefaults {
            keyboard: [("KeyZ".to_string(), 4), ("ArrowUp".to_string(), 0)].into(),
            gamepad: [(0, 4), (12, 0)].into(),
        };
        let spec = CpuDebugSpec {
            coverage_size: 0x100,
            address_mask: 0xFF,
            trace_entry_size: 4,
            history: false,
            checks: BreakpointChecks::ALL,
            steppable: true,
            records_coverage: true,
        };
        Self {
            debugger: DebuggerState::new([spec], system_info.clone(), ()),
            system_info,
            framebuffer: Framebuffer::new(FRAMEBUFFER_WIDTH, FRAMEBUFFER_HEIGHT),
            frames: 0,
            region: VideoRegion::Ntsc,
            host_audio: HostAudioRing::with_capacity(8),
            audio_enabled: false,
            input: GenericInputManager::new(2, pads),
            rom: Vec::new(),
            rom_hash: String::new(),
            cartridge: None,
            sram_dirty: false,
            save_ram: Vec::new(),
            manifest_override: None,
            unsupported_chip: None,
            show_overscan: false,
            memory: (0..=255).collect(),
            video: FakeVideo {
                vram: (0..32).collect(),
                palette: [0x0102, 0x0304, 0x0506, 0x0708],
                hidden: Default::default(),
            },
            sound: [
                FakeSound {
                    name: "Main",
                    muted: vec![false; 3],
                },
                FakeSound {
                    name: "Second",
                    muted: vec![false; 2],
                },
            ],
        }
    }

    /// Run a frame: the framebuffer filled with the frame's number and, with
    /// sound on, two samples queued.
    fn frame(&mut self) {
        self.frames += 1;
        self.framebuffer.data.fill(self.frames as u8);
        if self.audio_enabled {
            self.host_audio.push_slice(&[0.25, -0.25]);
        }
    }

    /// Mark the battery save written, as a game's write would.
    pub(crate) fn write_save(&mut self) {
        self.sram_dirty = true;
    }
}

impl EmulatorCore for FakeMachine {
    type Savestate = FakeMachineState;
    type Manifest = serde_json::Value;

    fn system_info(&self) -> &SystemInfo {
        &self.system_info
    }
    fn rom_identity(&self) -> Option<RomIdentity<'_>> {
        (!self.rom.is_empty()).then(|| RomIdentity {
            sha256: &self.rom_hash,
            title: "FAKE",
        })
    }
    fn savestate_payload(&self, _purpose: PayloadUse) -> FakeMachineState {
        FakeMachineState {
            frames: self.frames,
        }
    }
    fn savestate_extension(&self) {}
    fn restore_savestate(&mut self, payload: FakeMachineState, _extension: ()) {
        self.frames = payload.frames;
        self.framebuffer.data.fill(0xEE);
    }
    fn reset(&mut self) {
        self.framebuffer.data.fill(0);
    }
    fn power_cycle(&mut self) {
        self.frames = 0;
        self.framebuffer.data.fill(0);
        self.save_ram.fill(0);
    }
    fn frame_count(&self) -> u64 {
        self.frames
    }
    fn master_cycles(&self) -> u64 {
        self.frames * 1000
    }
    fn master_clock_hz(&self) -> u64 {
        60_000
    }
    fn region(&self) -> VideoRegion {
        self.region
    }
    fn apply_region(&mut self, region: VideoRegion) {
        self.region = region;
    }
    fn host_audio(&self) -> &HostAudioRing {
        &self.host_audio
    }
    fn host_audio_mut(&mut self) -> &mut HostAudioRing {
        &mut self.host_audio
    }
    fn input_manager(&self) -> &GenericInputManager {
        &self.input
    }
    fn input_manager_mut(&mut self) -> &mut GenericInputManager {
        &mut self.input
    }
    fn video_debugger(&mut self) -> &mut dyn VideoDebugger {
        &mut self.video
    }
    fn audio_chip_count(&self) -> usize {
        self.sound.len()
    }
    fn audio_debugger(&mut self, chip: usize) -> Option<&mut dyn AudioDebugger> {
        self.sound
            .get_mut(chip)
            .map(|sound| sound as &mut dyn AudioDebugger)
    }
    fn output_waveform(&self, samples: u32) -> Vec<f32> {
        vec![0.25; samples as usize]
    }
    fn cartridge_debugger(&mut self) -> Box<dyn CartridgeDebugger + '_> {
        Box::new(MapPlanDebugger::new(
            MapPlan::<FakeSlot>::default(),
            self.rom.len() as u32,
            self.save_ram.len() as u32,
            None,
            Vec::new(),
            self.debugger.breakpoints_mut(),
        ))
    }
    fn write_memory_debug(&mut self, addr: u32, data: &[u8]) {
        for (i, byte) in data.iter().enumerate() {
            let at = (addr as usize + i) % self.memory.len();
            self.memory[at] = *byte;
        }
    }
    fn disassemble_window(
        &mut self,
        _cpu_id: usize,
        pc: u32,
        before_count: usize,
        after_count: usize,
    ) -> Vec<DisasmRow> {
        (0..(before_count + 1 + after_count) as u32)
            .map(|i| DisasmRow {
                address: (pc + i).wrapping_sub(before_count as u32),
                kind: DisasmRowKind::Code,
                bytes: vec![0xEA],
                mnemonic: "nop".to_string(),
                operands: String::new(),
                comment: None,
            })
            .collect()
    }
    fn step_cpu(&mut self, _cpu_id: usize) -> StepResult {
        unreachable!()
    }
    fn focused_cpu(&self) -> usize {
        0
    }
    fn set_focused_cpu(&mut self, _cpu_id: usize) {}
    fn run_frame(&mut self) {
        self.frame();
    }
    fn run_frame_headless(&mut self) {
        self.frame();
    }
    fn run_until_vblank(&mut self) {
        self.frame();
    }
    fn load_rom(&mut self, data: &[u8]) -> Result<(), String> {
        self.unsupported_chip = None;
        match data.first() {
            None => return Err("empty file".to_string()),
            Some(&NEEDS_A_CHIP) => {
                self.unsupported_chip = Some("Fake chip");
                return Err("this build has no module for the fake chip".to_string());
            }
            Some(&SCORES_TOO_LOW) if self.manifest_override.is_none() => {
                return Err("no mapping: detector score below threshold".to_string());
            }
            Some(_) => {}
        }
        self.rom = data.to_vec();
        self.rom_hash = format!("{:016x}", retro_core::hash::fnv1a64(data));
        let mut info = CartridgeInfo::new(ConsoleSystem::Genesis, data);
        info.title = "FAKE".to_string();
        self.cartridge = Some(info);
        self.save_ram = vec![0; 4];
        self.power_cycle();
        Ok(())
    }
    fn get_rom_info(&self) -> Option<RomInfo> {
        self.cartridge.as_ref().map(|info| RomInfo {
            title: info.title.clone(),
            region: "ntsc".to_string(),
            size: self.rom.len(),
            checksum: 0,
            mapper: None,
            title_confidence: "high".to_string(),
        })
    }
    fn get_framebuffer(&self) -> &Framebuffer {
        &self.framebuffer
    }
    fn get_display_info(&self) -> &DisplayInfo {
        &self.system_info.display
    }
    fn get_audio_info(&self) -> &AudioInfo {
        &self.system_info.audio
    }
    fn set_audio_enabled(&mut self, enabled: bool) {
        self.audio_enabled = enabled;
    }
    fn is_audio_enabled(&self) -> bool {
        self.audio_enabled
    }
    fn set_input(&mut self, player: u8, input: InputState) {
        self.input
            .set_direct_override(usize::from(player), input.buttons);
    }
    fn get_input_info(&self) -> &InputInfo {
        &self.system_info.input
    }
    fn get_save_ram(&self) -> Option<&[u8]> {
        (!self.save_ram.is_empty()).then_some(&self.save_ram[..])
    }
    fn set_save_ram(&mut self, data: &[u8]) {
        self.save_ram = data.to_vec();
    }
    fn debugger(&mut self) -> &mut dyn DebuggerCore {
        &mut self.debugger
    }
    fn debugger_ref(&self) -> &dyn DebuggerCore {
        &self.debugger
    }
    fn refresh_debugger_state(&mut self) {}
    fn read_memory_debug(&mut self, addr: u32, size: u32) -> Vec<u8> {
        (0..size)
            .map(|i| self.memory[(addr + i) as usize % self.memory.len()])
            .collect()
    }
    fn disassemble_at(&mut self, addr: u32, count: u32) -> Vec<Instruction> {
        self.disassemble_cpu(0, addr, count)
    }
    fn disassemble_cpu(&mut self, cpu_id: usize, addr: u32, count: u32) -> Vec<Instruction> {
        (0..count)
            .map(|i| Instruction {
                address: addr + i,
                bytes: vec![0xEA],
                mnemonic: "nop".to_string(),
                operands: format!("cpu{cpu_id}"),
                comment: None,
            })
            .collect()
    }

    fn parse_manifest(json: &str) -> Result<serde_json::Value, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }
    fn detected_manifest(&self, rom: &[u8]) -> serde_json::Value {
        serde_json::json!({ "preset": "plain", "rom_size": rom.len() })
    }
    fn cartridge_manifest_override(&self) -> Option<&serde_json::Value> {
        self.manifest_override.as_ref()
    }
    fn set_cartridge_manifest_override(&mut self, manifest: Option<serde_json::Value>) {
        self.manifest_override = manifest;
    }
    fn last_rom(&self) -> &[u8] {
        &self.rom
    }
    fn get_cartridge_hash(&self) -> Option<String> {
        (!self.rom.is_empty()).then(|| self.rom_hash.clone())
    }
    fn cartridge_info(&self) -> Option<&CartridgeInfo> {
        self.cartridge.as_ref()
    }
    fn is_cartridge_sram_dirty(&self) -> bool {
        self.sram_dirty
    }
    fn clear_cartridge_sram_dirty(&mut self) {
        self.sram_dirty = false;
    }
    fn last_unsupported_chip(&self) -> Option<&'static str> {
        self.unsupported_chip
    }
    fn set_show_overscan(&mut self, show: bool) {
        self.show_overscan = show;
    }
    fn show_overscan_enabled(&self) -> bool {
        self.show_overscan
    }
}

/// The one slot shape of [`FakeMachine`]'s empty map plan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FakeSlot;

impl SlotShape for FakeSlot {
    type Target = MapTarget;
    fn contains(&self, _addr: u32) -> bool {
        false
    }
    fn linear_offset(&self, addr: u32) -> u32 {
        addr
    }
    fn mask(&self) -> Option<u32> {
        None
    }
    fn target(&self) -> MapTarget {
        MapTarget::OpenBus
    }
    fn bounds(&self) -> (u32, u32) {
        (0, 0)
    }
    fn size(&self) -> usize {
        0
    }
}
