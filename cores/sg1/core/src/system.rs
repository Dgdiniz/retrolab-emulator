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
use crate::input_manager::InputManager;
use crate::io::devices::genesis_3button::Genesis3ButtonDevice;
use crate::io::devices::genesis_6button::Genesis6ButtonDevice;
use crate::timing::VideoRegion;
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
    input_manager: InputManager,

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

    /// Bounded ring buffer holding mixed stereo samples ready for the
    /// host. Capacity ~ 4 frames stereo @ 44.1 kHz/60 fps so the host
    /// can tolerate ~67 ms of jitter without the producer dropping
    /// samples or the consumer starving the worklet. The ring MUST
    /// NOT be cleared while playback is live: a mid-stream `clear()`
    /// races the consumer worklet and causes audible ticks. Drop
    /// individual frames by simply not pushing instead.
    audio_ring: retro_core::AudioRingBuffer<f32>,
    /// Scratch buffer used by `EmulatorCore::get_audio_samples()` to
    /// preserve its `&[f32]` return type, drained from the ring on
    /// each call. Kept alive across calls to avoid per-call alloc.
    pending_audio_drain: Vec<f32>,
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
    /// goes on with it. Never part of a savestate; a reset, a savestate load
    /// and a ROM load drop it (`frame::GenesisEmulator::drop_frame`).
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
            input_manager: InputManager::new(),
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
            audio_ring: retro_core::AudioRingBuffer::with_capacity(5880),
            pending_audio_drain: Vec::with_capacity(2048),
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

    /// Stage a cartridge manifest applied on the next [`Self::load_rom`],
    /// overriding heuristic detection. `None` reverts to pure detection.
    /// Does not disturb an already-installed cartridge: reload the ROM to apply it.
    pub fn set_cartridge_manifest_override(&mut self, manifest: Option<CartridgeManifest>) {
        self.cartridge_manifest_override = manifest;
    }

    /// The staged manifest override, if any.
    pub fn cartridge_manifest_override(&self) -> Option<&CartridgeManifest> {
        self.cartridge_manifest_override.as_ref()
    }

    /// The editable `cartridge-manifest-v1` the heuristic detector resolves
    /// for `rom`, with no override applied. Side-effect-free.
    pub fn detected_manifest(&self, rom: &[u8]) -> CartridgeManifest {
        crate::cartridge::detected_manifest(rom)
    }

    /// The raw bytes of the currently loaded ROM (empty when none loaded),
    /// so the host can re-detect the manifest for the live game.
    pub fn last_rom(&self) -> &[u8] {
        &self.last_rom
    }

    /// The installed cartridge's map-plan regions, for the cartridge
    /// debugger's memory-map view. Empty when no cartridge is loaded.
    pub fn cartridge_map_regions(&self) -> Vec<retro_core::cartridge::MapRegionSummary> {
        self.bus
            .cartridge()
            .map(|cart| retro_core::cartridge::describe_map_plan(cart.map_plan()))
            .unwrap_or_default()
    }

    /// Get the current video region (NTSC/PAL).
    pub fn region(&self) -> VideoRegion {
        self.region
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

    /// Set the video region.
    ///
    /// Recreates the audio rate converter with the new region's 68K
    /// CPU clock so the cycles->samples mapping stays accurate across
    /// an NTSC<->PAL switch, and re-anchors it at the current
    /// `master_cycles` so no audible discontinuity is introduced.
    ///
    /// Also pushes the region into the VDP so the system-level scanline
    /// budget (`region.scanlines()`) and the VDP's own timing (line
    /// count, VBLANK window) stay coherent; otherwise a PAL system
    /// would run 313-line frames against an NTSC VDP. For netplay both
    /// peers agree on region in the session handshake and call this
    /// right after `load_rom`, before running any frame.
    pub fn set_region(&mut self, region: VideoRegion) {
        self.region = region;
        // The published refresh rate follows the region: it is what the
        // host paces frames against, so a PAL machine must advertise 49.7 Hz.
        self.system_info.display.refresh_rate = region.frame_rate();
        let mut conv = retro_core::IntegerRateConverter::new(
            self.audio_sample_rate as u64,
            region.cpu_clock(),
        );
        conv.reset(self.master_cycles);
        self.audio_rate_converter = conv;

        // Keep the VDP's region in lockstep with the system region.
        let vdp_region = match region {
            VideoRegion::Ntsc => crate::vdp::timing::VideoRegion::Ntsc,
            VideoRegion::Pal => crate::vdp::timing::VideoRegion::Pal,
        };
        self.bus.vdp_mut().set_region(vdp_region);
    }

    /// Set the video region from a case-insensitive `"ntsc"`/`"pal"`
    /// string. The netplay session handshake carries region as a
    /// string, so this keeps the enum out of the WASM/JS boundary.
    pub fn set_region_str(&mut self, name: &str) -> Result<(), String> {
        let region = match name.trim().to_ascii_lowercase().as_str() {
            "ntsc" => VideoRegion::Ntsc,
            "pal" => VideoRegion::Pal,
            other => {
                return Err(format!(
                    "unknown region '{other}' (expected 'ntsc' or 'pal')"
                ))
            }
        };
        self.set_region(region);
        Ok(())
    }

    /// Current region as the `"ntsc"`/`"pal"` string used by the
    /// netplay handshake and the `getRegion` WASM export.
    pub fn region_str(&self) -> &'static str {
        match self.region {
            VideoRegion::Ntsc => "ntsc",
            VideoRegion::Pal => "pal",
        }
    }

    /// Get the number of frames executed.
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Cold (power-on) reset: rebuild the emulator to the state [`Self::new`]
    /// and `load_rom` give, so work RAM and Z80 RAM start zeroed. Only the
    /// player's controller setup is kept, with nothing it held still pressed.
    ///
    /// The plain `reset()` emulates the Genesis *reset button*, which
    /// preserves RAM on real hardware; this emulates a *power cycle*, which does
    /// not. Netplay needs it: two peers must derive a byte-identical frame-0
    /// state hash, but a host that had been running the game (dirty RAM) and a
    /// freshly-booted guest reset to frame 0 with *different* RAM contents, since
    /// `reset()` leaves RAM untouched. Zeroing it here gives both peers the same
    /// deterministic power-on baseline. The cartridge ROM is preserved, but
    /// battery-backed **SRAM is deliberately blanked** to a fresh image: the
    /// cartridge is rebuilt from the ROM bytes, so both peers start byte-identical
    /// regardless of either one's save file (a save-resume start arrives through
    /// the seeded savestate transfer, not through this cold boot). That makes this
    /// UNSAFE behind a user-facing Reset button: use [`Self::hard_reset`], which
    /// power-cycles the same volatile hardware while keeping the save.
    pub fn power_on_reset(&mut self) {
        // A soft reset (the console's reset *button*, `EmulatorCore::reset`)
        // re-boots the 68000 (PC/SP/SR from the vector table; D0-D7/A0-A6,
        // undefined on real hardware after reset, are zeroed) and clears
        // VRAM/CRAM/VSRAM and the audio chips, but leaves work RAM, Z80 RAM, the
        // USP, the VDP registers other than R0/R1/R12/R15, the I/O ports and the
        // Z80 bank register holding whatever the running game left there. A
        // real *power* cycle clears all of it. Netplay needs the latter: two peers
        // with different pre-session histories (a host mid-game vs a just-joined
        // guest) MUST converge on a byte-identical frame-0 state for the
        // start-commit baseline to match, and they diverge on exactly those
        // subsystems a soft reset preserves.
        //
        // Rebuild the whole machine to the state a freshly-constructed emulator +
        // `load_rom` produces (byte-for-byte what a just-joined guest holds),
        // preserving what is invariant across peers: the ROM image, the
        // cartridge manifest override, and the video region (all three pinned
        // identical by the netplay handshake). Because both peers run this exact
        // logic over identical preserved inputs, the result is identical by
        // construction. Battery-backed SRAM is intentionally reset to a fresh
        // (empty) image: a game with a save resumes through the seeded savestate
        // transfer, NOT this cold boot, so both peers still start byte-identical.
        //
        // The player's input setup survives too, as a console's controllers do:
        // the key mapping, the pads and their mappings and dead zones, the port
        // each feeds, the split keyboard. It is the host's, not the machine's:
        // no savestate holds it, so the peers' machines still match. What any of
        // it holds pressed is let go, so a button held across the cycle (a host
        // that stops forwarding input never releases it) cannot reach the
        // machine that boots.
        let rom = std::mem::take(&mut self.last_rom);
        let manifest = self.cartridge_manifest_override.take();
        let region = self.region;
        let capture_recording = self.capture_recording;
        let mut input_manager = std::mem::take(&mut self.input_manager);
        input_manager.release_all();

        *self = GenesisEmulator::new();
        self.input_manager = input_manager;
        // What a capture records is the host's setting, not the machine's.
        self.set_capture_recording(capture_recording);
        // Must precede `load_rom`, which reads it to build the cartridge.
        self.cartridge_manifest_override = manifest;
        if !rom.is_empty() {
            // These bytes already loaded once, so a re-load cannot fail; swallow
            // the Result rather than propagate: `power_on_reset` is infallible.
            let _ = <Self as EmulatorCore>::load_rom(self, &rom);
        }
        // Re-apply the region *after* load_rom (mirroring the normal load ->
        // set-region order) through the setter, which rebuilds the audio-rate
        // converter for the region's clock and syncs the VDP region; `new()`
        // starts at the NTSC default.
        self.set_region(region);
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
        self.set_region(self.region);
        // The new VDP records what the host asked the old one to.
        self.bus
            .vdp_mut()
            .set_capture_recording(self.capture_recording);

        // Host-visible presentation state derived from the old machine.
        self.cached_framebuffer = Framebuffer::new(320, 224);
        self.system_info.display.width = 320;
        self.system_info.display.height = 224;
        self.mixed_waveform_buffer.clear();
        // A held button belongs to the outgoing session, never the incoming one.
        self.input_manager.clear_all_direct_overrides();
    }

    /// Power-cycle the console: the "turn it off and on again" that the
    /// reset button is not.
    ///
    /// [`EmulatorCore::reset`] emulates the Genesis *reset button*: it
    /// re-boots the CPUs but leaves work RAM, Z80 RAM and most of the VDP
    /// register file holding whatever the running game left there. This
    /// clears all of it while keeping the cartridge,
    /// ROM **and** battery-backed SRAM, plugged in, so a player who hits
    /// "Reset" on a game with a save file does not lose it.
    ///
    /// Distinct from [`Self::power_on_reset`], which rebuilds the whole
    /// emulator (dropping SRAM to a fresh image) because netplay needs two
    /// peers to converge on a byte-identical frame-0 baseline.
    pub fn hard_reset(&mut self) {
        self.rebuild_volatile_hardware();
        self.reset();
        // Re-apply the ROM's controller configuration, which lives on the
        // I/O ports the rebuild replaced.
        let rom = std::mem::take(&mut self.last_rom);
        if !rom.is_empty() {
            self.connect_controllers_for_rom(&rom);
        }
        self.last_rom = rom;
    }

    /// Get the total number of CPU cycles executed.
    pub fn master_cycles(&self) -> u64 {
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

    /// Get reference to the input manager.
    pub fn input_manager(&self) -> &InputManager {
        &self.input_manager
    }

    /// Get mutable reference to the input manager.
    pub fn input_manager_mut(&mut self) -> &mut InputManager {
        &mut self.input_manager
    }

    /// The SHA-256 of the ROM file the last successful
    /// [`EmulatorCore::load_rom`] received, in lowercase hex; `None` before
    /// one. A Mega Drive cartridge's image is the file as given, so this is
    /// also the image's hash, the one a savestate names.
    pub fn get_cartridge_hash(&self) -> Option<String> {
        self.cartridge_hash.clone()
    }

    /// Get cartridge title
    pub fn get_cartridge_title(&self) -> Option<String> {
        self.bus.cartridge().map(|c| c.info().title.clone())
    }

    /// Check if cartridge SRAM is dirty
    pub fn is_cartridge_sram_dirty(&self) -> bool {
        self.bus.cartridge().is_some_and(|c| c.is_sram_dirty())
    }

    /// Clear cartridge SRAM dirty flag
    pub fn clear_cartridge_sram_dirty(&mut self) {
        if let Some(cart) = self.bus.cartridge_mut() {
            cart.clear_sram_dirty();
        }
    }
}

impl GenesisEmulator {
    /// Update the cached framebuffer from the VDP.
    fn update_cached_framebuffer(&mut self) {
        let vdp_fb = self.bus.get_vdp_framebuffer();
        let display_width = self.bus.get_vdp_display_width();
        let display_height = self.bus.get_vdp_display_height();

        // VDP framebuffer is always 320x224 internally, but display_width can be 256 or 320
        // We need to handle this mismatch carefully
        let vdp_internal_width = 320u32;
        let vdp_internal_height = 224u32;
        let expected_vdp_size = (vdp_internal_width * vdp_internal_height * 4) as usize;

        // Resize cached framebuffer to match display dimensions if needed
        if self.cached_framebuffer.width != display_width
            || self.cached_framebuffer.height != display_height
        {
            self.cached_framebuffer = Framebuffer::new(display_width, display_height);
        }

        // The display info gives the picture this framebuffer holds: the
        // mode's width, and its height cut to the lines the VDP draws.
        self.system_info.display.width = display_width;
        self.system_info.display.height = self.bus.vdp().drawn_height();

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

        // Copy each line's visible pixels: all 320 in the 40-cell mode, the
        // first 256 in the 32-cell mode, down to the last line both hold.
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
    fn system_info(&self) -> &SystemInfo {
        &self.system_info
    }

    fn reset(&mut self) {
        // Reset debugger state first (ensures clean execution mode). The
        // calls the call stack holds belong to the machine the reset ends.
        self.debugger.set_execution_mode(ExecutionMode::Paused);
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
        self.audio_ring.clear();
        self.audio_ring.reset_counters();
        self.audio_rate_converter.reset(self.master_cycles);

        // Leave the reset state in the debugger, so a host that reads it
        // before the first step or frame (the platform opens its debugger
        // right after a load) shows the reset vector instead of nothing.
        // It reads the registers only: nothing here runs or changes state.
        self.refresh_debugger_state();
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

        // Print ROM info (debug)
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
        eprintln!("Loaded ROM: {}", info.title);
        eprintln!("  System: {:?}", info.system);
        eprintln!("  ROM Size: {} bytes", info.rom_size);
        if info.sram_size > 0 {
            eprintln!("  SRAM: {} bytes ({:?})", info.sram_size, info.sram_type);
        }
        if let Some(ref mapper_info) = info.mapper_info {
            eprintln!("  Mapper: {:?}", mapper_info.mapper_id);
        }
        if let Some(ref chip_info) = info.expansion_chip_info {
            eprintln!("  Expansion Chip: {:?}", chip_info.chip_type);
        }

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
        let same_rom = self
            .bus
            .cartridge()
            .is_some_and(|installed| installed.info().rom_hash == new_hash);

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
        self.set_region(header_region);

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

    fn get_framebuffer(&self) -> &Framebuffer {
        &self.cached_framebuffer
    }

    /// The size of the picture [`EmulatorCore::get_framebuffer`] holds: the
    /// mode's width, 256 or 320, and its height cut to the lines the VDP
    /// draws ([`crate::vdp::Vdp::drawn_height`]). It follows the framebuffer,
    /// refreshed with it each frame shown.
    fn get_display_info(&self) -> &DisplayInfo {
        &self.system_info.display
    }

    fn get_audio_samples(&mut self) -> &[f32] {
        // Drain the ring into the scratch buffer so we can satisfy
        // the trait's `&[f32]` contract without per-call allocation.
        // Hosts that can take an explicit count should call
        // `drain_audio_into(&mut [f32]) -> usize` on the concrete
        // type to skip the resize.
        let n = self.audio_ring.available();
        self.pending_audio_drain.clear();
        self.pending_audio_drain.resize(n, 0.0);
        self.audio_ring.drain_into(&mut self.pending_audio_drain);
        &self.pending_audio_drain
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
            self.audio_ring.clear();
        }
    }

    fn is_audio_enabled(&self) -> bool {
        self.audio_enabled
    }

    fn set_input(&mut self, player: u8, input: InputState) {
        // Route the bitmask through `InputManager` as a "direct override"
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

    /// The machine's state, where a frame ends. A frame in progress (a
    /// frame call that stopped, a host step, a capture boundary) has no
    /// place in a savestate, which cannot say where to resume it, and
    /// finishing it needs `&mut self`: while one is in progress this returns
    /// nothing. [`CaptureCore::save_state_at_frame_end`] finishes the frame
    /// first, and is what every host calls.
    fn save_state(&self) -> Vec<u8> {
        use crate::savestate::*;
        use retro_core::savestate::{
            serialize_compressed, SaveStateEnvelope, RETRO_SAVESTATE_ENVELOPE_VERSION,
        };

        // The savestate holds no frame part of the way through.
        if self.frame_open {
            return Vec::new();
        }

        let (game_title, rom_hash) = self
            .bus
            .cartridge()
            .map(|c| (c.info().title.clone(), c.info().rom_hash.clone()))
            .unwrap_or_default();

        let state = self.build_save_state(SaveStateMetadata::now(game_title, rom_hash.clone(), 0));

        // Wrap the SG1-specific snapshot in the cross-core envelope.
        // `created_at` mirrors chrono's RFC3339 timestamp in
        // `state.metadata.timestamp` but encoded as a plain unix
        // seconds counter for tooling that doesn't parse RFC3339.
        let envelope = SaveStateEnvelope {
            envelope_version: RETRO_SAVESTATE_ENVELOPE_VERSION,
            core_id: "sg1".to_string(),
            core_version: SAVE_STATE_VERSION,
            rom_hash,
            created_at: chrono::Utc::now().timestamp().max(0) as u64,
            payload: state,
        };

        serialize_compressed(&envelope, 3).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        self.load_state_with_rom_check(data, retro_core::savestate::RomCheck::SameRom)
            .map_err(|e| e.to_string())
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
