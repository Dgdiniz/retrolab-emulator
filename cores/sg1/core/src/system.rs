//! The Mega Drive machine: the emulator type, its construction, accessors
//! and lifecycle, and its `EmulatorCore` implementation. Its parts are
//! submodules, the same in every core: `step` (the machine step), `frame`
//! (the line timetable and the frame loops), `debug`, `state` (into and
//! out of the savestate), `audio`, `capture` and `descriptors`.

mod audio;
mod capture;
mod debug;
mod descriptors;
mod frame;
mod state;
mod step;

pub use capture::GenesisSnapshotConsole;
pub use debug::InstructionAccessLog;

use crate::bus::AddressBus;
use crate::cartridge::CartridgeManifest;
use crate::cpu68k::M68k;
use crate::debugger::{
    genesis_debugger, GenesisDebugger, TraceEntryBinary, TraceEntryBinaryZ80, MAIN_CPU, SOUND_CPU,
};
use crate::io::devices::genesis_3button::Genesis3ButtonDevice;
use crate::io::devices::genesis_6button::Genesis6ButtonDevice;
use crate::pad_defaults::pad_defaults;
use crate::timing::{GenesisTiming, VideoRegion};
use crate::z80::Z80;
use descriptors::{genesis_button_layout, genesis_cpus};
use frame::LineRun;
use retro_core::capture::{CaptureCore, CaptureRecording};
use retro_core::cpu::Addressable;
use retro_core::debug::{trace_to_text, AccessLog};

use retro_core::*;

/// Main Genesis emulator struct.
///
/// This implements the `EmulatorCore` trait and ties together all
/// the Genesis hardware components.
pub struct GenesisEmulator {
    system_info: SystemInfo,
    debugger: GenesisDebugger,
    input_manager: GenericInputManager,

    // Hardware components
    cpu: M68k,
    z80: Z80,
    bus: AddressBus,

    // Timing and state
    region: VideoRegion,
    frame_count: u64,
    master_cycles: u64,
    /// The master clocks of 68000 time the line has run: zeroed at each
    /// line's start, raised by each step (CPU cycles x 7). The savestate
    /// keeps it.
    step_master_clocks_accumulated: u64,
    /// Fractional-cycle carry for the per-scanline 68K budget in the
    /// `run_frame*` tiers. A scanline is `3420 / 7 = 488.571...` 68K cycles, so
    /// each line spends 488 cycles plus a `4/7` remainder. This holds the
    /// numerator (`0..7`) of that remainder and carries across scanlines AND
    /// frames, so the per-frame budget averages the exact `896,040 / 7` (NTSC) rather
    /// than truncating to `488` and losing ~150 cycles/frame.
    scanline_cycle_carry: u32,
    /// 68K instruction overshoot (in 68K cycles) carried past a scanline
    /// boundary. The last instruction of a line usually runs a few cycles past
    /// the budget; those cycles belong to the next line, whose start reads
    /// it. Written at every line's end; persisting it across frames keeps
    /// one continuous cycle count, as the CPU has, instead of resetting the
    /// sub-frame phase every frame. The savestate keeps the value a frame's
    /// end leaves.
    frame_cycle_overshoot: u32,
    /// Absolute 68K-cycle target for the next DRAM-refresh bus-steal. On the
    /// Mega Drive the 68000 bus is stolen for DRAM refresh ~every `128` CPU
    /// cycles, stalling the CPU `+2` cycles (the period and the penalty
    /// Genesis Plus GX uses, checked once per
    /// instruction). Whenever `master_cycles` reaches this target we charge the
    /// penalty and advance the target by `128`. Carries across frames.
    bus_refresh_target: u64,
    running: bool,

    // Cached framebuffer (updated from VDP)
    cached_framebuffer: Framebuffer,

    /// The bytes the 68000's last instruction accessed, for the watchpoint
    /// check: the bus's word accesses expanded byte by byte, the log kept
    /// from one instruction to the next.
    watch_accesses: AccessLog,

    /// The mixed stereo samples on their way to the host. Capacity about
    /// 4 frames stereo at 44.1 kHz and 60 fps, so the host can tolerate
    /// about 67 ms of jitter without the producer dropping samples or the
    /// consumer starving the worklet. It MUST NOT be cleared while playback
    /// is live: a clear in the middle of the stream races the consumer
    /// worklet and ticks. Drop a frame by not pushing it instead.
    host_audio: HostAudioRing,
    audio_sample_rate: u32,
    /// Drift-free cycle->sample mapper. The integer formula
    /// `cumulative = (num * (cycles - ref) + remainder) / den` carries
    /// the fractional remainder forward across frames, so the long-run
    /// sample rate exactly equals `sample_rate_hz * (cpu_cycles / cpu_clock_hz)`
    /// regardless of how many frames have elapsed. Denominator MUST
    /// be the 68K CPU clock (master clock / 7), since `master_cycles`
    /// counts CPU cycles rather than raw master clocks.
    audio_rate_converter: retro_core::IntegerRateConverter,

    // Audio control: enabled by default, can be disabled via API
    audio_enabled: bool,

    // Pre-allocated audio mixing buffers (avoids 3 Vec allocs per frame)
    psg_sample_buf: Vec<f32>,
    ym_sample_buf: Vec<f32>,
    mix_sample_buf: Vec<f32>,

    /// Mixed waveform ring buffer for visualization (mono, last N samples)
    mixed_waveform_buffer: std::collections::VecDeque<f32>,

    /// Previous step's total CPU cycles (including any VDP wait states).
    /// Used to tick the VDP FIFO drain at the start of the next step.
    last_step_cycles: u32,

    /// Z80 maskable interrupt pending flag.
    /// Set true when VBLANK occurs (same signal as 68K level 6 IRQ).
    /// The Z80 checks this via check_interrupts() before each step.
    /// Cleared when the Z80 takes the interrupt or when VBLANK ends; a VDP
    /// status read does not clear it.
    z80_irq_pending: bool,

    /// The Z80's and the YM2612's counters an earlier step of a host's kept
    /// apart from the frame's lines. The savestate's layout holds them, so
    /// they are written and read back with it; nothing else changes them.
    step_z80_target: u32,
    step_z80_executed: u32,
    step_z80_credit_numer: u32,
    step_ym_cycles_clocked: u32,

    /// Staged cartridge manifest applied on the next [`Self::load_rom`],
    /// overriding heuristic detection. Set from the host via
    /// [`Self::set_cartridge_manifest_override`]; `None` means "use pure
    /// detection". Editing it does not disturb an already-loaded cartridge.
    cartridge_manifest_override: Option<CartridgeManifest>,

    /// The raw bytes of the most recently loaded ROM, kept so
    /// [`Self::detected_manifest`] can re-run the heuristic detector for
    /// the *currently loaded* game without the host re-supplying them.
    last_rom: Vec<u8>,

    /// The SHA-256 of the ROM file the last successful [`EmulatorCore::load_rom`]
    /// received, in lowercase hex ([`Self::get_cartridge_hash`]); `None`
    /// before one.
    cartridge_hash: Option<String>,

    /// Whether the stepping paths keep each 68000 instruction's bus
    /// accesses in `instruction_access_log`. Debugger state, never part of
    /// a savestate.
    instruction_access_logging: bool,

    /// The bus accesses of the last instruction stepped while
    /// `instruction_access_logging` is on.
    instruction_access_log: InstructionAccessLog,

    /// A frame is in progress: a frame call that stopped part of the way
    /// through, or a host step, left it, and the next frame call or step
    /// goes on with it. A reset and a ROM load drop it
    /// (`frame::GenesisEmulator::drop_frame`); a savestate holds it in the
    /// extension after its payload, and a load brings back the state's.
    frame_open: bool,
    /// The line the frame in progress is on, and whether that line's start
    /// has run.
    frame_line: u32,
    line_started: bool,
    /// The line's counters between runs of steps.
    line: LineRun,
    /// The frame stands in front of its first line of vertical blanking,
    /// held there for a capture.
    held_at_capture_boundary: bool,
    /// A capture asked the frame to stop at that boundary.
    capture_boundary_requested: bool,
    /// Whether a frame's end presents (the host's picture and sound): off
    /// for a headless frame.
    present: bool,
    /// An enabled breakpoint watches memory or an event: the watchpoints,
    /// the access log and the event checks run. Read when the debugger is
    /// armed for a frame call or a step.
    debug_armed: bool,
    /// The Z80 is traced, read when the debugger is armed.
    z80_trace_armed: bool,
    /// The first word of the instruction about to run, as the 68000 trace
    /// entry read it, for the call stack's start to take rather than read
    /// it again (one read through the cartridge a traced instruction).
    opcode_read: std::cell::Cell<Option<(u32, u16)>>,
    /// The cycles of the interrupt the step in progress took before its
    /// instruction, which its total and its trace entry count.
    interrupt_cycles: u32,

    /// What the host asked the video chip to record while frames run, for a
    /// capture ([`CaptureCore::set_capture_recording`]). A host setting,
    /// never serialized, applied again whenever the video chip is replaced.
    capture_recording: CaptureRecording,
}

impl GenesisEmulator {
    /// Create a new Genesis emulator instance.
    pub fn new() -> Self {
        let system_info = SystemInfo {
            name: "Sega Genesis".to_string(),
            short_name: "genesis".to_string(),
            manufacturer: "Sega".to_string(),
            year: 1988,
            cpus: genesis_cpus(),
            display: DisplayInfo {
                width: 320,
                height: 224,
                aspect_ratio: 4.0 / 3.0,
                refresh_rate: VideoRegion::default().frame_rate(),
                pixel_format: PixelFormat::RGBA8888,
                grid: PictureGrid::ONE,
            },
            audio: AudioInfo {
                sample_rate: 44100,
                channels: 2,
                bits_per_sample: 16,
            },
            input: InputInfo {
                max_players: 2,
                button_layout: genesis_button_layout(),
            },
        };

        // Create bus and connect 3-button controllers to ports 0 and 1
        let mut bus = AddressBus::new();
        bus.io_mut()
            .port_mut(0)
            .unwrap()
            .connect_device(Box::new(Genesis3ButtonDevice::new()));
        bus.io_mut()
            .port_mut(1)
            .unwrap()
            .connect_device(Box::new(Genesis3ButtonDevice::new()));

        Self {
            debugger: genesis_debugger(system_info.clone()),
            input_manager: GenericInputManager::new(2, pad_defaults()),
            system_info,
            cpu: M68k::new(),
            z80: Z80::new(),
            bus,
            region: VideoRegion::default(),
            frame_count: 0,
            // MAME shows totalcycles=0x22 (34) after hard reset, before first instruction
            // This represents the reset sequence cycles. We match this for trace compatibility.
            master_cycles: 34,
            // Convert initial CPU cycles to master clocks: 34 * 7 = 238 master clocks
            // This ensures scanline tracking starts at the correct position within the scanline
            step_master_clocks_accumulated: 34 * 7,
            scanline_cycle_carry: 0,
            frame_cycle_overshoot: 0,
            // First refresh bus-steal lands at the 128-cycle boundary after the
            // 34-cycle reset sequence.
            bus_refresh_target: 128,
            running: false,
            cached_framebuffer: Framebuffer::new(320, 224),
            watch_accesses: AccessLog::new(),

            // ~4 frames stereo @ 44.1 kHz/60 fps; bounded, overflow
            // drops oldest samples (host already drained them in the
            // previous frame in the typical case).
            host_audio: HostAudioRing::with_capacity(5880),
            audio_sample_rate: 44100,
            // The converter's denominator is the **68K CPU clock**
            // (master_clock / 7 ~ 7.67 MHz NTSC), because
            // `self.master_cycles` is a 68K CPU-cycle counter, NOT
            // master clocks. Using master_clock here would produce
            // ~1/7 of the expected sample count per frame and the
            // converter would silently underrun the host.
            audio_rate_converter: retro_core::IntegerRateConverter::new(
                44_100,
                VideoRegion::default().cpu_clock(),
            ),

            // Audio enabled by default
            audio_enabled: true,

            // Pre-allocated audio mixing buffers (~735 samples/frame at 44.1kHz/60fps)
            psg_sample_buf: vec![0.0; 1024],
            ym_sample_buf: vec![0.0; 2048],
            mix_sample_buf: vec![0.0; 2048],
            mixed_waveform_buffer: std::collections::VecDeque::with_capacity(2048),

            // Previous step's total cycles for FIFO drain timing
            last_step_cycles: 0,
            z80_irq_pending: false,
            step_z80_target: 0,
            step_z80_executed: 0,
            step_z80_credit_numer: 0,
            step_ym_cycles_clocked: 0,
            cartridge_manifest_override: None,
            last_rom: Vec::new(),
            cartridge_hash: None,
            instruction_access_logging: false,
            instruction_access_log: InstructionAccessLog::default(),
            frame_open: false,
            frame_line: 0,
            line_started: false,
            line: LineRun::default(),
            held_at_capture_boundary: false,
            capture_boundary_requested: false,
            present: true,
            debug_armed: false,
            z80_trace_armed: false,
            opcode_read: std::cell::Cell::new(None),
            interrupt_cycles: 0,
            capture_recording: CaptureRecording::default(),
        }
    }

    /// Get VDP reference for debugging.
    pub fn vdp(&self) -> &crate::vdp::Vdp {
        self.bus.vdp()
    }

    /// Get mutable VDP reference for debugging/testing.
    pub fn vdp_mut(&mut self) -> &mut crate::vdp::Vdp {
        self.bus.vdp_mut()
    }

    /// Set controller port button state directly (for CLI input injection).
    /// `state` is a bitmask: bit 0=UP, 1=DOWN, 2=LEFT, 3=RIGHT, 4=B, 5=C, 6=A, 7=START.
    pub fn set_port_button_state(&mut self, port: usize, state: u32) {
        if let Some(p) = self.bus.io_mut().port_mut(port) {
            p.set_device_button_state(state);
        }
    }

    /// True when the cartridge header's I/O-support field ($190-$19F)
    /// contains `'6'` (6-button pad).
    pub fn rom_advertises_six_button(rom: &[u8]) -> bool {
        if rom.len() < 0x1A0 {
            return false;
        }
        rom[0x190..0x1A0].contains(&b'6')
    }

    /// Install 3- or 6-button pads on ports 0/1 from the ROM header.
    pub fn connect_controllers_for_rom(&mut self, rom: &[u8]) {
        self.connect_controllers(Self::rom_advertises_six_button(rom));
    }

    /// Install 3- or 6-button pads on both DE-9 ports.
    pub fn connect_controllers(&mut self, six_button: bool) {
        for port in 0..2 {
            if let Some(p) = self.bus.io_mut().port_mut(port) {
                if six_button {
                    p.connect_device(Box::new(Genesis6ButtonDevice::new()));
                } else {
                    p.connect_device(Box::new(Genesis3ButtonDevice::new()));
                }
            }
        }
    }

    /// Get PSG reference.
    pub fn psg(&self) -> &crate::psg::Psg {
        self.bus.psg()
    }

    /// Get mutable PSG reference.
    pub fn psg_mut(&mut self) -> &mut crate::psg::Psg {
        self.bus.psg_mut()
    }

    // === YM2612 Audio Chip Access ===

    /// Get immutable YM2612 reference.
    pub fn ym2612(&self) -> &crate::ym2612::Ym2612 {
        self.bus.ym2612()
    }

    /// Get mutable YM2612 reference.
    pub fn ym2612_mut(&mut self) -> &mut crate::ym2612::Ym2612 {
        self.bus.ym2612_mut()
    }

    /// Rebuild every volatile subsystem to its power-on state, keeping the
    /// cartridge plugged in.
    ///
    /// The address bus owns work RAM, Z80 RAM, the VDP (registers **and**
    /// VRAM/CRAM/VSRAM), the PSG, the YM2612, the I/O ports and the Z80
    /// bus/reset/bank latches, among them everything [`EmulatorCore::reset`]
    /// deliberately leaves alone because a real Genesis *reset button* does
    /// not clear it (work RAM, Z80 RAM, most VDP registers, the I/O ports,
    /// the Z80 bank register). Replacing the bus wholesale is therefore the
    /// console's power switch. The cartridge is unplugged first and plugged
    /// straight back in so its ROM and battery-backed SRAM survive, exactly
    /// as they do across a real power cycle.
    ///
    /// Callers must run [`EmulatorCore::reset`] afterwards to boot the CPUs
    /// off the (now freshly mapped) vector table.
    fn rebuild_volatile_hardware(&mut self) {
        let cartridge = self.bus.take_cartridge();

        let mut bus = AddressBus::with_region(self.region);
        bus.io_mut()
            .port_mut(0)
            .unwrap()
            .connect_device(Box::new(Genesis3ButtonDevice::new()));
        bus.io_mut()
            .port_mut(1)
            .unwrap()
            .connect_device(Box::new(Genesis3ButtonDevice::new()));
        self.bus = bus;

        if let Some(cartridge) = cartridge {
            self.bus.set_cartridge(cartridge);
        }

        // `AddressBus::with_region` only sizes the audio chip clocks; the VDP
        // is constructed at its NTSC default, so re-apply the system region to
        // keep its scanline timing in lockstep (a no-op on NTSC).
        self.apply_region(self.region);
        // The new chips render as the audio switch says.
        self.bus
            .ym2612_mut()
            .set_rendering_enabled(self.audio_enabled);
        self.bus.psg_mut().set_rendering_enabled(self.audio_enabled);
        // The new VDP records what the host asked the old one to.
        self.bus
            .vdp_mut()
            .set_capture_recording(self.capture_recording);

        // Host-visible presentation state derived from the old machine.
        self.cached_framebuffer = Framebuffer::new(320, 224);
        self.system_info.display.width = 320;
        self.system_info.display.height = 224;
        self.system_info.display.grid = self.bus.vdp().picture_grid();
        self.mixed_waveform_buffer.clear();
        // A held button belongs to the outgoing session, never the incoming one.
        self.input_manager.clear_all_direct_overrides();
    }

    /// The 68000 cycles run since the last reset or load: the master clocks
    /// [`EmulatorCore::master_cycles`] counts, divided by seven.
    pub fn cpu_cycles(&self) -> u64 {
        self.master_cycles
    }

    /// Get reference to the CPU.
    pub fn cpu(&self) -> &M68k {
        &self.cpu
    }

    /// Get mutable reference to the CPU.
    pub fn cpu_mut(&mut self) -> &mut M68k {
        &mut self.cpu
    }

    /// Get reference to the Z80 CPU.
    pub fn z80(&self) -> &Z80 {
        &self.z80
    }

    /// Get mutable reference to the Z80 CPU.
    pub fn z80_mut(&mut self) -> &mut Z80 {
        &mut self.z80
    }

    /// Check if Z80 is in reset (reads state from bus, which is set by 68K writes to 0xA11200).
    pub fn z80_is_reset(&self) -> bool {
        self.bus.is_z80_reset()
    }

    /// Set Z80 reset state. In real hardware this is controlled by 68K writes
    /// to 0xA11200. This method provides a programmatic interface for tests
    /// and initialization.
    pub fn set_z80_reset(&mut self, reset: bool) {
        // Write to bus register 0xA11200: bit 0 = 0 means hold reset, 1 means release
        let value = if reset { 0x00 } else { 0x01 };
        self.bus.write(0xA11200, value);
        if reset {
            self.z80.pc = 0x0000;
            self.z80.halted = false;
        }
    }

    /// Check if 68K has requested Z80 bus (reads state from bus, set by 68K writes to 0xA11100).
    pub fn z80_bus_requested(&self) -> bool {
        self.bus.is_z80_bus_requested()
    }

    /// Set Z80 bus request state. In real hardware this is controlled by 68K
    /// writes to 0xA11100. This method provides a programmatic interface for
    /// tests and initialization.
    pub fn set_z80_bus_request(&mut self, request: bool) {
        let value = if request { 0x01 } else { 0x00 };
        self.bus.write(0xA11100, value);
    }

    /// Get reference to the bus.
    pub fn bus(&self) -> &AddressBus {
        &self.bus
    }

    /// Get mutable reference to the bus.
    pub fn bus_mut(&mut self) -> &mut AddressBus {
        &mut self.bus
    }
}

impl GenesisEmulator {
    /// Update the cached framebuffer from the VDP.
    fn update_cached_framebuffer(&mut self) {
        let vdp_fb = self.bus.get_vdp_framebuffer();
        let display_width = self.bus.get_vdp_display_width();
        // The rows this frame drew: 224 or 240 lines, two rows each in
        // interlace mode 2.
        let display_height = self.bus.vdp().picture_height();

        // VDP framebuffer is always 320x480 internally, but display_width can be 256 or 320
        // and the frame can draw 224, 240, 448 or 480 rows. We need to handle this mismatch
        // carefully
        let vdp_internal_width = crate::vdp::FRAMEBUFFER_WIDTH;
        let vdp_internal_height = crate::vdp::FRAMEBUFFER_ROWS;
        let expected_vdp_size = (vdp_internal_width * vdp_internal_height * 4) as usize;

        // Resize cached framebuffer to match display dimensions if needed
        if self.cached_framebuffer.width != display_width
            || self.cached_framebuffer.height != display_height
        {
            self.cached_framebuffer = Framebuffer::new(display_width, display_height);
        }

        // The display info gives the picture this framebuffer holds: the
        // mode's width, the rows the frame drew, and how it lies over the
        // dots and lines.
        self.system_info.display.width = display_width;
        self.system_info.display.height = display_height;
        self.system_info.display.grid = self.bus.vdp().picture_grid();

        // Safety check: ensure VDP framebuffer has expected size
        if vdp_fb.len() != expected_vdp_size {
            // VDP framebuffer has unexpected size, skip copy
            eprintln!(
                "Warning: VDP framebuffer size mismatch: {} vs expected {}",
                vdp_fb.len(),
                expected_vdp_size
            );
            return;
        }

        // Copy each row's visible pixels: all 320 in the 40-cell mode, the
        // first 256 in the 32-cell mode, down to the last row both hold.
        // A line at a time in both modes: the C library hands a copy as
        // long as the whole frame to the processor's string move (`rep
        // movsb`), which an AMD Zen 4 runs about fifteen times slower when
        // the destination sits 8 bytes past the source modulo 4 KiB, and
        // where the allocator puts the two buffers changes with the build
        // and the game. A copy as short as a line goes to a vector loop,
        // as fast at any distance.
        let src_stride = (vdp_internal_width * 4) as usize;
        let dst_stride = (display_width * 4) as usize;
        for (dst, src) in self
            .cached_framebuffer
            .data
            .chunks_exact_mut(dst_stride)
            .zip(vdp_fb.chunks_exact(src_stride))
        {
            dst.copy_from_slice(&src[..dst_stride]);
        }
    }
}

impl Default for GenesisEmulator {
    fn default() -> Self {
        Self::new()
    }
}

impl EmulatorCore for GenesisEmulator {
    type Savestate = crate::savestate::SystemSaveState;

    fn system_info(&self) -> &SystemInfo {
        &self.system_info
    }

    fn rom_identity(&self) -> Option<retro_core::savestate::RomIdentity<'_>> {
        self.bus.cartridge().map(|cart| {
            let info = cart.info();
            retro_core::savestate::RomIdentity {
                sha256: &info.rom_hash,
                title: &info.title,
            }
        })
    }

    /// The machine's payload: a savestate's stamped with the wall clock,
    /// the game's title and its hash; the state hash's with neither.
    fn savestate_payload(
        &self,
        purpose: retro_core::savestate::PayloadUse,
    ) -> crate::savestate::SystemSaveState {
        use crate::savestate::SaveStateMetadata;
        let metadata = match purpose {
            retro_core::savestate::PayloadUse::Savestate => {
                let (title, hash) = self
                    .bus
                    .cartridge()
                    .map(|c| (c.info().title.clone(), c.info().rom_hash.clone()))
                    .unwrap_or_default();
                SaveStateMetadata::now(title, hash, 0)
            }
            retro_core::savestate::PayloadUse::StateHash => SaveStateMetadata {
                game_title: String::new(),
                rom_hash: String::new(),
                timestamp: String::new(),
                slot: 0,
                emulator_version: String::new(),
            },
        };
        self.build_save_state(metadata)
    }

    fn savestate_extension(&self) -> crate::savestate::GenesisExtension {
        self.capture_extension()
    }

    fn restore_savestate(
        &mut self,
        payload: crate::savestate::SystemSaveState,
        extension: crate::savestate::GenesisExtension,
    ) {
        self.restore_payload(payload);
        self.restore_extension(extension);
    }

    /// The wall clock, in seconds since 1970: the envelope's twin of the
    /// payload's RFC 3339 stamp, for tools that do not parse one.
    fn savestate_created_at(&self) -> u64 {
        chrono::Utc::now().timestamp().max(0) as u64
    }

    /// The frame held at the capture boundary, finished on the play tier
    /// with the debugger disarmed for its length
    /// (`retro_core::frame::finish_held_frame`); a frame stopped anywhere
    /// else is saved where it stands, with the extension after the payload.
    fn finish_held_frame(&mut self) -> bool {
        retro_core::frame::finish_held_frame(self)
    }

    fn reset(&mut self) {
        // The calls the call stack holds belong to the machine the reset
        // ends.
        self.debugger.console_mut().clear_instruction_boundaries();
        self.debugger.machine_replaced();
        // The machine starts over: a frame in progress is dropped, not
        // finished.
        self.drop_frame();

        // Reset 68000 CPU
        self.cpu.reset(&mut self.bus);

        // Reset Z80 CPU
        self.z80 = Z80::new();
        // Put Z80 in reset state (write 0 to 0xA11200) and release bus (write 0 to 0xA11100)
        self.bus.write(0xA11200, 0x00); // Hold Z80 in reset
        self.bus.write(0xA11100, 0x00); // Release bus request

        // Reset VDP
        self.bus.vdp_mut().reset();

        // Reset PSG
        self.bus.psg_mut().reset();

        // Reset YM2612
        self.bus.ym2612_mut().reset();

        // Reset the cartridge: clear latches/SRAM-enable and re-anchor every
        // coprocessor's clock to the region master clock.
        let cart_mclk = self.region.master_clock();
        if let Some(cart) = self.bus.cartridge_mut() {
            cart.reset();
            cart.reset_chip(cart_mclk);
        }

        // Reset counters
        self.frame_count = 0;
        // MAME shows totalcycles=0x22 (34) after hard reset, before first instruction
        // This represents the reset sequence cycles. We match this for trace compatibility.
        self.master_cycles = 34;
        // Convert initial CPU cycles to master clocks: 34 * 7 = 238 master clocks
        self.step_master_clocks_accumulated = 34 * 7;
        self.scanline_cycle_carry = 0;
        self.frame_cycle_overshoot = 0;
        self.bus_refresh_target = 128;
        self.last_step_cycles = 0;
        self.z80_irq_pending = false;
        self.running = true;

        // Drop any queued samples (this is a real reset, not
        // mid-stream: the host is expected to drain after) and
        // re-anchor the rate converter at the new master_cycles
        // baseline (34, set above).
        self.host_audio.clear();
        self.host_audio.reset_counters();
        self.audio_rate_converter.reset(self.master_cycles);

        // Leave the reset state in the debugger, so a host that reads it
        // before the first step or frame (the platform opens its debugger
        // right after a load) shows the reset vector instead of nothing.
        // It reads the registers only: nothing here runs or changes state.
        self.refresh_debugger_state();
    }

    /// The power switch, which the reset button is not: [`Self::reset`]
    /// re-boots the CPUs but leaves work RAM, Z80 RAM, the 68000's user
    /// stack pointer, most of the VDP's registers, the I/O ports and the
    /// Z80's bank register as the game left them; this builds all of them
    /// again. A new 68000 and Z80; a new bus in the machine's region, with
    /// every chip on it; the cartridge built again from its ROM, its save
    /// RAM blank; the pads the ROM's header asks for; then the machine's
    /// region, not the one the header names. The debugger, the input, the
    /// audio switch and what a capture records are the host's and are
    /// kept; the coverage, which the same ROM keeps true, too.
    fn power_cycle(&mut self) {
        let region = self.region;
        self.cpu = M68k::new();
        self.z80 = Z80::new();
        drop(self.bus.take_cartridge());
        self.rebuild_volatile_hardware();
        let rom = std::mem::take(&mut self.last_rom);
        if rom.is_empty() {
            self.reset();
        } else {
            // These bytes loaded once, so loading them again cannot fail;
            // the load sees the ROM it loaded last, so it builds nothing
            // more and keeps the coverage.
            let _ = <Self as EmulatorCore>::load_rom(self, &rom);
        }
        self.apply_region(region);
    }

    fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// The 68000's cycles times seven: the master clocks (the field counts
    /// 68000 cycles, which the line budget and the trace are kept in).
    fn master_cycles(&self) -> u64 {
        self.master_cycles * 7
    }

    fn master_clock_hz(&self) -> u64 {
        self.region.master_clock()
    }

    fn region(&self) -> VideoRegion {
        self.region
    }

    /// The machine retimed for `region`: the refresh rate the host paces
    /// frames against, the audio converter (whose denominator is the
    /// region's 68000 clock) anchored again at the present cycle so no
    /// sample is lost or doubled, the VDP's line count and blanking, and
    /// the sound chips' clocks (`AddressBus::set_sound_clocks`; a bus
    /// builds its chips on the clocks of the region it is built in, and a
    /// ROM's load builds it in the region the machine had before). A ROM's
    /// load, a region change's power cycle and a state's load all come
    /// here.
    fn apply_region(&mut self, region: VideoRegion) {
        self.region = region;
        self.system_info.display.refresh_rate = region.frame_rate();
        let mut converter = retro_core::IntegerRateConverter::new(
            self.audio_sample_rate as u64,
            region.cpu_clock(),
        );
        converter.reset(self.master_cycles);
        self.audio_rate_converter = converter;
        self.bus.vdp_mut().set_region(region);
        self.bus.set_sound_clocks(region);
    }

    fn host_audio(&self) -> &HostAudioRing {
        &self.host_audio
    }

    fn host_audio_mut(&mut self) -> &mut HostAudioRing {
        &mut self.host_audio
    }

    fn input_manager(&self) -> &GenericInputManager {
        &self.input_manager
    }

    fn input_manager_mut(&mut self) -> &mut GenericInputManager {
        &mut self.input_manager
    }

    fn video_debugger(&mut self) -> &mut dyn retro_core::video::VideoDebugger {
        self.bus.vdp_mut()
    }

    /// The YM2612 (0) and the PSG (1).
    fn audio_chip_count(&self) -> usize {
        2
    }

    fn audio_debugger(&mut self, chip: usize) -> Option<&mut dyn retro_core::audio::AudioDebugger> {
        match chip {
            0 => Some(self.bus.ym2612_mut()),
            1 => Some(self.bus.psg_mut()),
            _ => None,
        }
    }

    /// The PSG and the YM2612 mixed.
    fn output_waveform(&self, samples: u32) -> Vec<f32> {
        self.mixed_waveform(samples)
    }

    fn cartridge_debugger(&mut self) -> Box<dyn retro_core::cartridge::CartridgeDebugger + '_> {
        Box::new(self.map_plan_debugger())
    }

    /// A byte at a time through the bus's CPU path: a write to an I/O or
    /// VDP port acts as a CPU write would.
    fn write_memory_debug(&mut self, addr: u32, data: &[u8]) {
        for (i, &byte) in data.iter().enumerate() {
            self.bus.write(addr.wrapping_add(i as u32), byte);
        }
    }

    /// Bitmap-aware disassembly around `pc` (0 the 68000, 1 the Z80), over
    /// [`retro_core::disassemble_window`].
    fn disassemble_window(
        &mut self,
        cpu_id: usize,
        pc: u32,
        before_count: usize,
        after_count: usize,
    ) -> Vec<DisasmRow> {
        let mut driver = debug::GenesisDisasmDriver { emu: self, cpu_id };
        retro_core::disassemble_window(&mut driver, pc, before_count, after_count)
    }

    fn step_cpu(&mut self, cpu_id: usize) -> StepResult {
        match cpu_id {
            // The step a host asks for on the 68000, one step of the frame
            // loop: what is due before it handled, the step with its
            // coverage, trace entry, call stack and history, what it made
            // due handled and the frame closed at its end, then the cached
            // register snapshots refreshed.
            MAIN_CPU => retro_core::frame::host_step(self),
            SOUND_CPU => self.step_z80(),
            _ => StepResult {
                cycles: 0,
                breakpoint_hit: false,
                instruction_executed: false,
            },
        }
    }

    fn focused_cpu(&self) -> usize {
        self.debugger.focused_cpu()
    }

    fn set_focused_cpu(&mut self, cpu_id: usize) {
        self.debugger.set_focused_cpu(cpu_id);
    }

    fn disassemble_cpu(&mut self, cpu_id: usize, addr: u32, count: u32) -> Vec<Instruction> {
        match cpu_id {
            MAIN_CPU => Self::disassemble_at(self, addr, count),
            SOUND_CPU => Self::disassemble_z80_at(self, addr, count),
            _ => Vec::new(),
        }
    }

    /// A frame call on the loop the debugger's state selects: the play
    /// tier when nothing is instrumented, the debugger's tier while it
    /// runs with breakpoints or traces, and its paused or stepping tier
    /// otherwise (`retro_core::frame::run_frame_on`). A frame in progress
    /// goes on from where it stopped.
    fn run_frame(&mut self) {
        let tier = self.debugger.execution_tier();
        retro_core::frame::run_frame_on(self, tier);
    }

    fn run_frame_headless(&mut self) {
        retro_core::frame::run_frame_headless(self);
    }

    /// Run to the start of vertical blanking and stop there: the capture
    /// boundary ([`CaptureCore::run_to_capture_boundary`]), on the loop
    /// [`EmulatorCore::run_frame`] picks.
    fn run_until_vblank(&mut self) {
        self.run_to_capture_boundary();
    }

    fn load_rom(&mut self, data: &[u8]) -> Result<(), String> {
        use crate::cartridge::{build_genesis_cartridge, build_with_manifest};

        // Build cartridge from a staged manifest override when present, else
        // via heuristic detection (auto-detects mapper/chip type, SVP before
        // the mapper match). The override's `preset` / `sram_size` / chips
        // drive the installed plan; hand-edited overrides need no SHA-256
        // gate here (the manifest is trusted metadata).
        let cartridge = match &self.cartridge_manifest_override {
            Some(manifest) => build_with_manifest(data.to_vec(), manifest)?,
            None => build_genesis_cartridge(data.to_vec())?,
        };

        // Remember the raw bytes so `detected_manifest` can re-detect the
        // live game's mapping for the editor.
        self.last_rom = data.to_vec();

        let info = cartridge.info();
        // A cartridge sold only in PAL markets ran on a 50 Hz console and
        // its game logic (music tempo, timers, physics) assumes it; running
        // it at 60 Hz plays it 20 % fast. Multi-region and worldwide
        // releases stay NTSC: the 60 Hz build is what they shipped for
        // outside Europe. Applied after the reset below; an explicit region
        // set later (the netplay handshake) still wins, and `power_on_reset`
        // re-applies that override on top of this load.
        let header_region = if !info.regions.is_empty()
            && info
                .regions
                .iter()
                .all(|r| matches!(r, retro_core::cartridge::Region::Europe))
        {
            VideoRegion::Pal
        } else {
            VideoRegion::Ntsc
        };

        // Verify checksum (warn if mismatch)
        if let (Some(header_ck), Some(calc_ck)) = (info.header_checksum, info.calculated_checksum) {
            if header_ck != calc_ck {
                eprintln!("WARNING: ROM checksum mismatch!");
                eprintln!("  Expected: 0x{:04X}", header_ck);
                eprintln!("  Calculated: 0x{:04X}", calc_ck);
            }
        }

        // Decide whether to wipe the per-CPU coverage bitmaps. They
        // describe *which addresses are real instruction starts* for
        // the loaded ROM, so loading a different cartridge invalidates
        // every entry. Hot-reload of the same bytes (Bass build with
        // no-op output, or external load_rom of identical data) keeps
        // the bitmap so the disassembly window stays populated.
        let new_hash = info.rom_hash.clone();
        let same_rom = self.cartridge_hash.as_deref() == Some(new_hash.as_str());

        if !same_rom {
            self.debugger.clear_all_coverage();

            // Swapping in a *different* cartridge is a power cycle, not a
            // press of the reset button: on real hardware you eject the cart
            // with the console off. `reset()` alone would leave the outgoing
            // game's work RAM, Z80 RAM, VDP register file (plane/sprite-table
            // base addresses!), I/O ports and bus latches in place, and the
            // incoming game would boot on top of them, which is exactly how a
            // title that does not rewrite every VDP register comes up with
            // corrupt graphics after another game has run. Rebuild the
            // volatile hardware first; the cartridge is re-plugged with its
            // SRAM intact.
            self.rebuild_volatile_hardware();
        }

        // Load cartridge into bus
        self.bus.set_cartridge(cartridge);
        // The file's hash: the image is the file on this console.
        self.cartridge_hash = Some(new_hash);

        // Reset system with new ROM
        self.reset();
        self.apply_region(header_region);

        // The ROM's I/O-support field (header $190-$19F) names a "6" when
        // the game expects 6-button pads. Contra Hard Corps
        // advertises `J6` and its attract demo forks if it only ever sees
        // 3-button TH replies.
        self.connect_controllers_for_rom(data);

        Ok(())
    }

    fn get_rom_info(&self) -> Option<RomInfo> {
        self.bus.cartridge().map(|cart| {
            let info = cart.info();
            let confidence = if info.title.is_empty() {
                "none"
            } else if info.title.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                "high"
            } else {
                "low"
            };
            RomInfo {
                title: info.title.clone(),
                region: info
                    .regions
                    .iter()
                    .map(|r| format!("{:?}", r))
                    .collect::<Vec<_>>()
                    .join(", "),
                size: info.rom_size,
                checksum: info.header_checksum.unwrap_or(0) as u32,
                mapper: info
                    .mapper_info
                    .as_ref()
                    .map(|m| format!("{:?}", m.mapper_id)),
                title_confidence: confidence.to_string(),
            }
        })
    }

    type Manifest = CartridgeManifest;

    fn parse_manifest(json: &str) -> Result<CartridgeManifest, String> {
        crate::cartridge::parse_manifest(json).map_err(|e| e.to_string())
    }

    /// The editable `cartridge-manifest-v1` the heuristic detector resolves
    /// for `rom`, with no override applied. Side-effect-free.
    fn detected_manifest(&self, rom: &[u8]) -> CartridgeManifest {
        crate::cartridge::detected_manifest(rom)
    }

    fn cartridge_manifest_override(&self) -> Option<&CartridgeManifest> {
        self.cartridge_manifest_override.as_ref()
    }

    /// Does not disturb an already-installed cartridge: reload the ROM to
    /// apply it.
    fn set_cartridge_manifest_override(&mut self, manifest: Option<CartridgeManifest>) {
        self.cartridge_manifest_override = manifest;
    }

    fn last_rom(&self) -> &[u8] {
        &self.last_rom
    }

    /// A Mega Drive cartridge's image is the file as given, so this is also
    /// the image's hash, the one a savestate names.
    fn get_cartridge_hash(&self) -> Option<String> {
        self.cartridge_hash.clone()
    }

    fn cartridge_info(&self) -> Option<&retro_core::cartridge::CartridgeInfo> {
        self.bus.cartridge().map(|cart| cart.info())
    }

    fn is_cartridge_sram_dirty(&self) -> bool {
        self.bus.cartridge().is_some_and(|c| c.is_sram_dirty())
    }

    fn clear_cartridge_sram_dirty(&mut self) {
        if let Some(cart) = self.bus.cartridge_mut() {
            cart.clear_sram_dirty();
        }
    }

    fn get_framebuffer(&self) -> &Framebuffer {
        &self.cached_framebuffer
    }

    /// The size of the picture [`EmulatorCore::get_framebuffer`] holds: the
    /// mode's width, 256 or 320, and its height cut to the rows the VDP
    /// draws ([`crate::vdp::Vdp::picture_height`]: the lines drawn, two rows
    /// each in interlace mode 2), and its grid. It follows the framebuffer,
    /// refreshed with it each frame shown.
    fn get_display_info(&self) -> &DisplayInfo {
        &self.system_info.display
    }

    fn get_audio_info(&self) -> &AudioInfo {
        &self.system_info.audio
    }

    fn set_audio_enabled(&mut self, enabled: bool) {
        self.audio_enabled = enabled;
        // The chips render incrementally (before each register write); gate
        // that catch-up synthesis too, so disabling audio saves its cost.
        self.bus.ym2612_mut().set_rendering_enabled(enabled);
        self.bus.psg_mut().set_rendering_enabled(enabled);
        if !enabled {
            self.host_audio.clear();
        }
    }

    fn is_audio_enabled(&self) -> bool {
        self.audio_enabled
    }

    fn set_input(&mut self, player: u8, input: InputState) {
        // Route the bitmask through the input manager as a "direct override"
        // that OR-merges with whatever provider (keyboard / gamepad) is
        // assigned to the port. This is the path the JS frontend uses for
        // virtual-input sources like the mobile touch overlay: they get
        // to inject buttons without having to swap the keyboard out of
        // port 0 or auto-claim port 1 (which would silently bump them to
        // player 2).
        //
        // Frontends that don't want this layered behaviour can keep using
        // `keyDown` / `gamepadButton` exclusively; `set_input(_, 0)` clears
        // the override so the port reverts to provider-only state.
        self.input_manager
            .set_direct_override(player as usize, input.buttons);
    }

    fn get_input_info(&self) -> &InputInfo {
        &self.system_info.input
    }

    fn get_save_ram(&self) -> Option<&[u8]> {
        self.bus
            .cartridge()
            .map(|c| c.sram())
            .filter(|s| !s.is_empty())
    }

    fn set_save_ram(&mut self, data: &[u8]) {
        if let Some(cart) = self.bus.cartridge_mut() {
            cart.restore_sram(data);
        }
    }

    fn debugger(&mut self) -> &mut dyn DebuggerCore {
        &mut self.debugger
    }

    fn debugger_ref(&self) -> &dyn DebuggerCore {
        &self.debugger
    }

    fn refresh_debugger_state(&mut self) {
        let states = self.cpu_states();
        self.debugger.set_cpu_states(states);
    }

    /// The 68000's entries disassembled from memory as it is now, the Z80's
    /// from the bytes each entry holds.
    fn trace_to_text(&mut self, cpu_id: usize, data: &[u8]) -> Option<String> {
        match cpu_id {
            MAIN_CPU => Some(trace_to_text::<TraceEntryBinary, Self>(data, self)),
            SOUND_CPU => Some(trace_to_text::<TraceEntryBinaryZ80, ()>(data, &mut ())),
            _ => None,
        }
    }

    fn read_memory_debug(&mut self, addr: u32, size: u32) -> Vec<u8> {
        Self::read_memory_debug(self, addr, size)
    }

    fn disassemble_at(&mut self, addr: u32, count: u32) -> Vec<Instruction> {
        Self::disassemble_at(self, addr, count)
    }
}

#[cfg(test)]
mod tests;
