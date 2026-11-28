//! SNES system integration.
//!
//! Owns the top-level [`SnesEmulator`] and wires it into
//! [`retro_core::EmulatorCore`]. The 5A22 (WDC 65C816) main CPU, the
//! PPU, the DMA/HDMA engine, NMI/IRQ delivery, the SPC700 / S-DSP
//! audio (synthesised at 32 kHz and resampled to the 44.1 kHz host
//! ring here), and the two standard-pad controller ports (auto-joypad
//! read at V-blank + manual `$4016`/`$4017` serial) are all wired
//! through the bus.
//!
//! Its parts are submodules, the same in every core: `step` (the machine
//! step), `frame` (the raster events and the frame loops), `debug`, `state`
//! (into and out of the savestate), `audio`, `capture` and `descriptors`.
//!
//! ## Frame loop and cycle accounting
//!
//! `master_cycles` counts **master clocks** (21.477 MHz NTSC,
//! 21.281 MHz PAL), the same unit the [`crate::scheduler`] uses.
//! Per CPU instruction we charge the real per-access master-clock cost:
//! the bus sums [`crate::bus::SnesBus::cycles_for`] (6/8/12 mclk by
//! region) over every memory access the instruction makes, and
//! `step_cpu_internal` adds the leftover internal cycles at the 6 mclk
//! fast rate. This keeps the 5A22 and the SPC700 (clocked off
//! `master_cycles` via the APU catch-up) in the correct relative phase,
//! which the `$2140` mailbox handshake is sensitive to. A flat
//! `cpu_cycles * 6` would run the CPU ~6-7% too fast and desync that
//! handshake (Chrono Trigger stalls before its Mode-7 intro).

mod audio;
mod capture;
mod debug;
mod descriptors;
mod frame;
mod state;
mod step;

pub use capture::SnesSnapshotConsole;
pub use descriptors::destination_region_str;

use std::collections::VecDeque;

use retro_core::audio::{ChannelMutes, HostAudioRing, SincResampler};
use retro_core::capture::{CaptureCore, CaptureRecording};
use retro_core::cartridge::Cartridge;
use retro_core::cpu::Addressable;
use retro_core::debugger::DebuggerCore;
use retro_core::input::GenericInputManager;
use retro_core::savestate::{PayloadUse, RomIdentity};
#[cfg(any(feature = "chip-superfx", feature = "chip-cx4"))]
use retro_core::CpuArchitecture;
use retro_core::{
    AudioInfo, DisasmRow, DisplayInfo, EmulatorCore, Framebuffer, InputInfo, InputState,
    Instruction, PictureGrid, PixelFormat, RomInfo, StepResult, SystemInfo, VideoRegion,
};

use wdc65816::Wdc65816;

use retro_core::debug::trace_to_text;

use crate::debugger::{
    cartridge_cpu_spec, cpu_state_65816, cpu_state_spc700, snes_debugger, SnesDebugger,
    TraceEntryBinary65816, TraceEntryBinarySpc700, CARTRIDGE_CPU, MAIN_CPU, SOUND_CPU,
};

use crate::bus::{IoAccess, SnesBus, SnesPeekBus};
use crate::cartridge::{
    copier_header_length, detect_preset_with_threshold, detected_manifest, try_parse_header,
    CartridgeBuilder, CartridgeManifest, CoprocessorMode, DetectedPreset, PresetKind, SnesHeader,
    HEADER_CANDIDATES,
};
use crate::controller::{input_state_to_snes_pad, pad_defaults};
use crate::scheduler::{schedule_frame_events, EventKind, TimingProfile};
use retro_core::frame::EventQueue;

use debug::SpcPeekBus;
use descriptors::{secondary_cpu_info, snes_base_cpus, snes_button_layout};

/// Stereo capacity of the host audio ring buffer (interleaved L/R,
/// 44.1 kHz `f32`).
///
/// 8192 samples = 4096 stereo frames ~ 5.5 video frames at 44.1 kHz /
/// 60 fps (one video frame ~ 735 stereo frames). The host pulls each
/// frame, so a few frames of headroom is enough to absorb timing jitter
/// without audibly overflowing.
const AUDIO_RING_CAPACITY: usize = 8192;

/// Worst-case SNES output width in pixels (Mode 5/6 hires).
const MAX_FB_WIDTH: u32 = 512;
/// The framebuffer's height in rows: room for the tallest picture, an
/// overscan frame's 239 lines at the two rows a line a hires frame takes
/// (two fields woven with interlace, `$2133` bit 0, else each line drawn
/// twice): 478 rows.
const MAX_FB_HEIGHT: u32 = 480;
/// Default non-hires width reported via `DisplayInfo` at boot.
const DEFAULT_FB_WIDTH: u32 = 256;
/// Default non-overscan height reported via `DisplayInfo` at boot.
const DEFAULT_FB_HEIGHT: u32 = 224;

/// Fallback window, in CPU instructions, for VBlank NMI delivery. While
/// an NMI is armed at `VBlankStart`, the CPU is vectored as soon as the
/// main thread *observes* the `$4210` flag (see
/// `SnesEmulator::nmi_deliver_armed`); this bound is only reached by
/// games that enable NMI without polling `$4210`, in which case the NMI
/// fires this many instructions into V-blank. Small enough that the
/// added latency is negligible against a multi-thousand-cycle V-blank,
/// large enough to cover the universal `lda $4210 / bpl` vblank-wait in
/// any instruction phase.
const NMI_DELIVERY_MAX_INSNS: u8 = 3;

/// SNES (Super Famicom) emulator.
///
/// Owns the 65C816 CPU, the main [`SnesBus`], the H-event scheduler,
/// the host audio ring + 32 kHz -> 44.1 kHz resampler, and the input
/// manager.
/// Carries parsed cartridge metadata (header, preset, raw size) on
/// the emulator instead of through the [`crate::SnesCartridge`]
/// trait, so the cartridge dispatch path stays small and immutable.
pub struct SnesEmulator {
    system_info: SystemInfo,
    framebuffer: Framebuffer,

    cpu: Wdc65816,
    bus: SnesBus,
    scheduler: EventQueue<EventKind>,
    timing: TimingProfile,

    frame_count: u64,
    master_cycles: u64,
    /// Master clock at the top of the frame currently executing.
    /// Anchors per-frame H/V IRQ scheduling so a mid-frame HTIME /
    /// VTIME / NMITIMEN rewrite re-arms the triggers against the same
    /// frame base the events were originally placed against.
    frame_base_cycle: u64,

    /// Host-rate audio queue (44.1 kHz `f32`, interleaved L/R). Fed each
    /// frame from the APU's native 32 kHz `i16` ring through `resampler`.
    host_audio: HostAudioRing,
    /// 32 kHz (S-DSP) -> 44.1 kHz (host) band-limited resampler
    /// ([`Self::host_resampler`]). The ratio is region-independent, so
    /// this instance lives for the emulator's lifetime and is only
    /// `reset()`.
    resampler: SincResampler,
    /// Scratch for one frame's worth of 32 kHz `i16` samples drained from
    /// the APU before resampling (reused to avoid per-frame allocation).
    apu_drain_buf: Vec<i16>,
    /// Scratch for the resampled 44.1 kHz `f32` samples before they are
    /// pushed into `host_audio`.
    audio_resample_buf: Vec<f32>,
    audio_enabled: bool,

    input_manager: GenericInputManager,

    header: Option<SnesHeader>,
    preset_kind: Option<PresetKind>,
    rom_size: usize,
    /// The SHA-256 of the ROM file the last successful
    /// [`EmulatorCore::load_rom`] received, in lowercase hex, its copier
    /// header included ([`Self::get_cartridge_hash`]); `None` before one.
    /// The savestate envelope names the image's hash instead, the
    /// cartridge's (`CartridgeInfo::rom_hash`).
    cartridge_hash: Option<String>,

    /// The raw bytes of the most recently loaded ROM, retained so
    /// [`EmulatorCore::power_cycle`] can re-run [`EmulatorCore::load_rom`]
    /// to reconstruct a byte-identical power-on state (the netplay cold
    /// boot) without the host re-supplying them. Empty until a ROM is
    /// loaded.
    last_rom: Vec<u8>,

    /// Coprocessor firmware (e.g. an 8192-byte DSP-1 dump) handed to the
    /// cartridge builder on the next [`EmulatorCore::load_rom`]. Set via
    /// [`Self::set_coprocessor_firmware`]; `None` leaves a DSP-1 cart on
    /// the firmware-free HLE, and so does a build without the `nec-lle`
    /// feature. Retained across `reset` (the ROM stays loaded) but
    /// cleared by a fresh `load_rom` that omits it.
    coprocessor_firmware: Option<Vec<u8>>,
    /// How a detected coprocessor is emulated on the next `load_rom`.
    coprocessor_mode: CoprocessorMode,
    /// The chip the last `load_rom` refused for want of its module (a
    /// build without that chip's feature), by display name; `None` after
    /// a load that was not refused for that. The WASM bindings read it to
    /// raise a typed error the platform maps to another core.
    last_unsupported_chip: Option<&'static str>,

    /// User-supplied cartridge manifest applied on the next
    /// [`EmulatorCore::load_rom`], overriding the builder's heuristic
    /// detection (preset, SRAM size, FastROM, custom slots/chips). Set
    /// via [`Self::set_cartridge_manifest_override`]; `None` means "use
    /// pure auto-detection". SHA-256 verification is skipped for these
    /// because a hand-edited manifest can't always recompute the
    /// stripped-ROM digest. Retained across `reset` (the ROM stays
    /// loaded) but cleared by a fresh `load_rom` only if the host
    /// clears it first.
    cartridge_manifest_override: Option<CartridgeManifest>,

    /// Host display preference: when `true`, expose the 239 lines an
    /// overscan frame draws; when `false` (default), crop to the 224-line
    /// NTSC TV window. Stored here (not just on the PPU) so it survives the
    /// wholesale bus rebuild in [`EmulatorCore::load_rom`], `reset`
    /// re-applies it to the live PPU. It is a UI toggle, never emulated
    /// state, so it is intentionally outside savestate.
    show_overscan: bool,

    /// What the host asked the core to record while frames run, for a
    /// capture to read ([`CaptureRecording`]). Kept here, as `show_overscan`
    /// is, so it survives the bus rebuild in [`EmulatorCore::load_rom`]:
    /// `reset` applies it again. A host setting, never part of a savestate.
    capture_recording: CaptureRecording,

    /// Breakpoints, coverage, tracing, and cached CPU-state snapshots.
    /// Drives execution-tier selection in [`Self::run_frame`].
    debugger: SnesDebugger,

    /// Mirror of `debugger.has_watchpoints_or_events()`, refreshed each
    /// debug entry: whether the bus records the CPU's accesses, the APU
    /// its S-DSP register writes and the PPU its palette and tile events,
    /// and whether the debugger's tiers check them.
    debug_armed: bool,

    /// VBlank NMI *delivery* state, modelling the 5A22's NMI latency in a
    /// phase-independent way. The `$4210` NMI flag is latched the instant
    /// VBlank begins (so a `LDA $4210 / BPL` poll can observe it), but the
    /// CPU is not vectored immediately: a typical handler's first act is
    /// to read `$4210` and clear the flag, which, if it ran first, would
    /// preempt the main thread's poll before it ever saw bit 7 and leave a
    /// vblank-synced routine spinning forever (the Chrono Trigger intro
    /// black screen). When this is armed at `VBlankStart`,
    /// `step_cpu_internal` defers raising the CPU's `nmi_pending` until the
    /// main thread has *observed* the flag via a `$4210` read
    /// (`SystemRegs::take_nmi_flag_observed`) or `nmi_deliver_countdown`
    /// expires. Because the NMI handler preserves A/X/Y, vectoring right
    /// after the poll's read is correct: the poll's `BPL` tests the
    /// restored accumulator and exits the loop. This is robust to where
    /// the in-flight instruction landed relative to VBlank, unlike a
    /// fixed instruction-count window, which works only for whichever
    /// phase a given scene happens to hit.
    nmi_deliver_armed: bool,
    /// Fallback window (in CPU instructions) bounding how long delivery
    /// waits for the main thread to observe `$4210` before vectoring the
    /// NMI anyway. Decremented each step while `nmi_deliver_armed`; only
    /// reached by NMI-driven games that don't poll `$4210`. 0 =
    /// idle/expired. See [`Self::nmi_deliver_armed`] and
    /// [`NMI_DELIVERY_MAX_INSNS`].
    nmi_deliver_countdown: u8,

    /// Master cycles of the H/V IRQ events the scheduler holds, earliest
    /// first. Rebuilt whenever the events are armed and popped as each
    /// fires, so the next IRQ is at hand before every instruction (see
    /// [`SnesBus::set_next_irq_cycle`]).
    irq_cycles: VecDeque<u64>,
    /// Master cycle of the IRQ a `$4211` read saw and acknowledged inside
    /// the instruction during which it rose, before its event fired.
    /// Transient: set after that instruction and spent when the event
    /// drains, right after it.
    irq_read_early: Option<u64>,

    /// When `false`, the frame loop suppresses the host-facing
    /// presentation steps that do not affect deterministic machine
    /// state: the framebuffer post-process (`finalize_frame`) and the
    /// 32 kHz -> 44.1 kHz host-audio drain. Set to `false` for the
    /// duration of a headless frame (netplay
    /// rollback/catch-up) so re-simulating a span is cheaper while
    /// leaving `state_hash` byte-identical to a presented run. Always
    /// `true` on the normal play path.
    present: bool,

    /// Set for the length of a
    /// [`run_to_capture_boundary`](CaptureCore::run_to_capture_boundary) call:
    /// the frame loops then stop in front of the frame's `VBlankStart`.
    capture_boundary_requested: bool,
    /// The core is stopped at a capture boundary: the frame's active
    /// display is drawn and its picture finished, and its `VBlankStart`,
    /// still queued, has started nothing of the vertical blank. The next
    /// frame call, step or savestate goes on from there.
    ///
    /// Never part of a savestate. That a frame is in progress is already
    /// in the saved scheduler (see [`Self::frame_in_progress`]); this only
    /// says that its picture is finished, so that the queued
    /// `VBlankStart` does not finish it a second time.
    held_at_capture_boundary: bool,
    /// The cycle the drain that stopped at the capture boundary ran to:
    /// the frame's `VBlankStart` and the events after it that were due by
    /// then are the rest of that drain's batch (see
    /// [`Self::release_capture_boundary`]). Meaningful only while
    /// `held_at_capture_boundary`.
    capture_boundary_cycle: u64,
}

impl Default for SnesEmulator {
    fn default() -> Self {
        Self::new()
    }
}

impl SnesEmulator {
    /// Build a fresh emulator with no ROM loaded. Defaults to NTSC
    /// timing; [`load_rom`](Self::load_rom) re-selects the profile
    /// from the cartridge destination byte.
    pub fn new() -> Self {
        let system_info = SystemInfo {
            name: "Super Nintendo Entertainment System".to_string(),
            short_name: "SNES".to_string(),
            manufacturer: "Nintendo".to_string(),
            year: 1990,
            cpus: snes_base_cpus(),
            display: DisplayInfo {
                width: DEFAULT_FB_WIDTH,
                height: DEFAULT_FB_HEIGHT,
                aspect_ratio: 8.0 / 7.0,
                refresh_rate: TimingProfile::NTSC.refresh_rate(),
                pixel_format: PixelFormat::RGBA8888,
                grid: PictureGrid::ONE,
            },
            audio: AudioInfo {
                sample_rate: 44_100,
                channels: 2,
                bits_per_sample: 32,
            },
            input: InputInfo {
                max_players: 2,
                button_layout: snes_button_layout(),
            },
        };

        let timing = TimingProfile::NTSC;
        // Allocate the framebuffer at the SNES worst-case resolution
        // (512x480 = hires + overscan + interlace). The active region,
        // exposed via `get_display_info`, shrinks dynamically to match
        // the live PPU bgmode + overscan + interlace bits, so games
        // that stay in a narrow mode never see the extra pixels but a
        // hot-swap into Mode 5/6 (or interlace) doesn't need a
        // reallocation.
        let framebuffer = Framebuffer::new(MAX_FB_WIDTH, MAX_FB_HEIGHT);

        let debugger = snes_debugger(system_info.clone());

        Self {
            system_info,
            framebuffer,
            cpu: Wdc65816::new(),
            bus: SnesBus::new(),
            scheduler: EventQueue::new(),
            timing,
            frame_count: 0,
            master_cycles: 0,
            frame_base_cycle: 0,
            host_audio: HostAudioRing::with_capacity(AUDIO_RING_CAPACITY),
            resampler: Self::host_resampler(),
            apu_drain_buf: Vec::new(),
            audio_resample_buf: Vec::new(),
            audio_enabled: false,
            input_manager: GenericInputManager::new(2, pad_defaults()),
            header: None,
            preset_kind: None,
            rom_size: 0,
            cartridge_hash: None,
            last_rom: Vec::new(),
            coprocessor_firmware: None,
            coprocessor_mode: CoprocessorMode::Auto,
            last_unsupported_chip: None,
            cartridge_manifest_override: None,
            show_overscan: false,
            capture_recording: CaptureRecording::default(),
            debugger,
            debug_armed: false,
            nmi_deliver_armed: false,
            nmi_deliver_countdown: 0,
            irq_cycles: VecDeque::new(),
            irq_read_early: None,
            present: true,
            capture_boundary_requested: false,
            held_at_capture_boundary: false,
            capture_boundary_cycle: 0,
        }
    }

    /// Stage coprocessor firmware (e.g. an 8192-byte DSP-1 dump) for the
    /// next [`EmulatorCore::load_rom`]. Pass `None` to clear it (a
    /// detected DSP-1 cart then runs on the firmware-free HLE). Byte order
    /// is auto-detected by the firmware loader. The firmware is used only
    /// by a build with the `nec-lle` feature ([`crate::NEC_LLE_BUILT`]).
    pub fn set_coprocessor_firmware(&mut self, firmware: Option<Vec<u8>>) {
        self.coprocessor_firmware = firmware;
    }

    /// Choose how a detected coprocessor is emulated on the next
    /// `load_rom` ([`CoprocessorMode::Auto`] by default).
    pub fn set_coprocessor_mode(&mut self, mode: CoprocessorMode) {
        self.coprocessor_mode = mode;
    }

    /// Start (or stop) capturing the host<->coprocessor register stream
    /// (`(is_write, offset, byte)` per DR/SR access). Enabling clears any
    /// previously captured log. Off by default. It lets a real game's
    /// command stream be captured, replayed through the firmware-free HLE
    /// backend and compared with what the LLE answers.
    pub fn enable_coprocessor_trace(&mut self, on: bool) {
        self.bus.enable_coprocessor_trace(on);
    }

    /// Drain and return the captured coprocessor register stream, leaving
    /// capture disabled. Empty when capture was never enabled.
    pub fn take_coprocessor_trace(&mut self) -> Vec<(bool, u32, u8)> {
        self.bus.take_coprocessor_trace()
    }

    /// Start capturing the synchronisation-register I/O trace, retaining
    /// the most recent `cap` accesses (see [`IoAccess`]). Off by default;
    /// used by `ppu-dump --io-trace` to find the spin loop a stalled game
    /// is parked in and the register condition it polls. `cap` of 0 is
    /// clamped to 1.
    pub fn enable_io_trace(&mut self, cap: usize) {
        self.bus.enable_io_trace(cap);
    }

    /// Drain and return the captured I/O access ring buffer (oldest
    /// first), leaving capture disabled. Empty when never enabled.
    pub fn take_io_trace(&mut self) -> Vec<IoAccess> {
        self.bus.take_io_trace()
    }

    /// Start capturing the instruction-PC ring buffer (most recent
    /// `cap` program counters). Off by default; used by `ppu-dump
    /// --pc-trace` to find the spin loop the main thread is parked in
    /// when a stall touches no watched MMIO register.
    pub fn enable_pc_trace(&mut self, cap: usize) {
        self.bus.enable_pc_trace(cap);
    }

    /// Drain and return the captured PC ring buffer (oldest first),
    /// leaving capture disabled. Empty when never enabled.
    pub fn take_pc_trace(&mut self) -> Vec<u32> {
        self.bus.take_pc_trace()
    }

    /// Start watching a single WRAM cell (most recent `cap` reads/writes,
    /// each stamped with the PC + raster). `addr` may be any alias of the
    /// cell (`$7E/$7F` direct or the low-8 KiB system-bank mirror). Off by
    /// default; used by `ppu-dump --watch-addr` to find the exact PC that
    /// touches a game-state variable (e.g. a stalled cutscene counter).
    pub fn enable_addr_watch(&mut self, addr: u32, cap: usize) {
        self.bus.enable_addr_watch(addr, cap);
    }

    /// Drain and return the captured address-watch ring buffer (oldest
    /// first), leaving the watch disabled. Empty when never enabled.
    pub fn take_addr_watch(&mut self) -> Vec<IoAccess> {
        self.bus.take_addr_watch()
    }

    /// Borrow the main address-space dispatcher.
    pub fn bus(&self) -> &SnesBus {
        &self.bus
    }

    /// Mutable borrow of the main address-space dispatcher.
    pub fn bus_mut(&mut self) -> &mut SnesBus {
        &mut self.bus
    }

    /// Borrow the 65C816 CPU. Used by integration tests and the CLI
    /// to inspect register state.
    pub fn cpu(&self) -> &Wdc65816 {
        &self.cpu
    }

    /// Mutable borrow of the 65C816 CPU.
    pub fn cpu_mut(&mut self) -> &mut Wdc65816 {
        &mut self.cpu
    }

    /// Borrow the event queue. Tests use this to check pending events.
    pub fn scheduler(&self) -> &EventQueue<EventKind> {
        &self.scheduler
    }

    /// Timing profile currently active (NTSC or PAL).
    pub fn timing(&self) -> TimingProfile {
        self.timing
    }

    /// Parsed SNES header for the currently loaded ROM, or `None`
    /// if no ROM has been loaded.
    pub fn header(&self) -> Option<&SnesHeader> {
        self.header.as_ref()
    }

    /// The preset the installed cartridge was built as: a staged
    /// manifest's when one was staged at its load, else the one the header
    /// gives. `None` before a load.
    pub fn preset_kind(&self) -> Option<PresetKind> {
        self.preset_kind
    }

    /// Borrow the PPU. Useful for the debugger and for tests that
    /// want to inspect VRAM / OAM / CGRAM / register state without
    /// going through the bus.
    pub fn ppu(&self) -> &crate::ppu::Ppu {
        self.bus.ppu()
    }

    /// Sample both controller ports from the input manager and push the
    /// SNES-pad bitmask into the bus's [`StandardPad`]s.
    ///
    /// Called from `set_input` (so manual-serial games and register-level
    /// tests see fresh state immediately) and from `VBlankStart` (so the
    /// auto-joypad read samples at the hardware-faithful point). The
    /// merged port state already folds in any `set_direct_override` and
    /// assigned-provider input.
    ///
    /// [`StandardPad`]: crate::controller::StandardPad
    fn refresh_controllers_from_input(&mut self) {
        for port in 0..2 {
            let mask = self.input_manager.get_port_state(port);
            self.bus
                .set_controller_buttons(port, input_state_to_snes_pad(mask));
        }
    }

    /// Synchronise the public `DisplayInfo` with the live PPU
    /// state. The PPU's `active_display_size` returns the current
    /// (width, height) based on the bgmode + overscan bit, and its
    /// `picture_grid` how that picture lies over the dots and lines; we
    /// mirror both into [`Self::system_info`] so the trait-bound
    /// `get_display_info(&self)` stays accurate without needing
    /// interior mutability. Called at VBlankStart, on `reset`, and
    /// inside `load_rom`.
    fn sync_display_info(&mut self) {
        let (w, h) = self.bus.ppu().active_display_size();
        self.system_info.display.width = w;
        self.system_info.display.height = h;
        self.system_info.display.grid = self.bus.ppu().picture_grid();
        // The refresh rate follows the timing profile, which `load_rom`
        // (header destination), `set_region` (the netplay handshake) and a
        // savestate restore can all change, every one of them ends here.
        self.system_info.display.refresh_rate = self.timing.refresh_rate();
    }
}

impl EmulatorCore for SnesEmulator {
    type Savestate = crate::savestate::SnesSaveState;

    fn system_info(&self) -> &SystemInfo {
        &self.system_info
    }

    fn rom_identity(&self) -> Option<RomIdentity<'_>> {
        self.bus.cartridge().map(|cart| {
            let info = cart.info();
            RomIdentity {
                sha256: &info.rom_hash,
                title: &info.title,
            }
        })
    }

    /// The machine's payload, the same for a save and the state hash: it
    /// carries no wall clock and no host metadata (the envelope names the
    /// ROM, and its `created_at` stays 0).
    fn savestate_payload(&self, _purpose: PayloadUse) -> crate::savestate::SnesSaveState {
        self.capture_save_state()
    }

    fn savestate_extension(&self) -> crate::savestate::SnesExtension {
        self.capture_extension()
    }

    fn restore_savestate(
        &mut self,
        payload: crate::savestate::SnesSaveState,
        extension: crate::savestate::SnesExtension,
    ) {
        self.restore_payload(payload);
        self.restore_extension(extension);
    }

    /// The frame held at the capture boundary, finished on the play tier
    /// with the debugger disarmed for its length
    /// (`retro_core::frame::finish_held_frame`); a frame stopped anywhere
    /// else is saved where it stands, its queue and its base with it.
    fn finish_held_frame(&mut self) -> bool {
        retro_core::frame::finish_held_frame(self)
    }

    fn reset(&mut self) {
        // The reset line. The 5A22's registers (FastROM off, the WRAM
        // port's address 0, nothing a register write left for the next
        // instruction boundary), its eight DMA channels, and the PPU's
        // registers and counters go back to their power-on values, and the
        // sprite table is cleared; work RAM, VRAM, CGRAM and the battery
        // save keep what the game left there.
        self.bus.system_regs_mut().reset();
        self.bus.apply_memsel(false);
        self.bus.reset_wram_port();
        self.bus.take_pending_nmi();
        self.bus.take_pending_irq_reschedule();
        self.bus.take_pending_general_dma();
        self.bus.dma_mut().reset();
        self.bus.ppu_mut().reset();
        self.bus.set_raster_snapshot(0, 0);
        self.cpu.reset_with_bus(&mut self.bus);
        self.frame_count = 0;
        self.master_cycles = 0;
        self.frame_base_cycle = 0;
        self.nmi_deliver_armed = false;
        self.nmi_deliver_countdown = 0;
        self.irq_read_early = None;
        // A reset, and the ROM load that ends in one, leaves no frame
        // stopped at a capture boundary.
        self.held_at_capture_boundary = false;
        self.capture_boundary_requested = false;
        self.scheduler.reset(0);
        // Frame 0 schedules off the live overscan bit (off at the
        // reset/power-on state); subsequent frames re-read it in
        // `end_of_frame_bookkeeping`.
        schedule_frame_events(
            &mut self.scheduler,
            self.timing,
            0,
            self.bus.ppu().overscan(),
        );
        self.arm_irq_events(0, 0);
        self.host_audio.clear();
        self.resampler.reset();
        self.apu_drain_buf.clear();
        self.audio_resample_buf.clear();
        // Clear both controller ports' strobe / shift-register state.
        self.bus.reset_controllers();
        // Reboot the SPC700 off the IPL vector and rebind the APU's
        // rate converters to this console's region clock, clearing the
        // cached catch-up cycle so it tracks `master_cycles` from 0.
        self.bus.reset_apu(self.timing.master_clock_hz as u64);
        // Re-anchor a cartridge coprocessor's lazy-catch-up clock to this
        // region so it tracks `master_cycles` from 0 (no-op without one).
        self.bus
            .reset_coprocessor(self.timing.master_clock_hz as u64);
        // RGBA black (every pixel 0,0,0,0). The renderer writes
        // alpha = 0xFF wherever it touches a scanline, so any
        // visible region after reset starts as the SNES default
        // backdrop (forced blank -> black with brightness 0) and
        // the un-touched tail is fully transparent. Tests rely on
        // both halves.
        self.framebuffer.data.fill(0);
        // `load_rom` rebuilds the bus wholesale, so the fresh PPU lost
        // the host's overscan preference, re-apply it before syncing
        // the display dimensions (a plain reset re-applies the same
        // value, a no-op).
        self.bus.ppu_mut().set_show_overscan(self.show_overscan);
        // The same for what the host asked to record; a line table kept
        // across the reset would describe a frame from before it, and so
        // would the live recorders.
        self.bus.set_capture_recording(self.capture_recording);
        let ppu = self.bus.ppu_mut();
        ppu.drop_recorded_frame();
        ppu.forget_recorded_writes();
        ppu.set_recording_frame(1);
        // The PPU's registers are back at BG mode 0 (not hires) with
        // overscan off: the public display dimensions go back to the
        // default 256x224 view.
        self.sync_display_info();
        // The calls the debugger saw belonged to the machine before the
        // reset; the history stays, it records what ran.
        self.debugger.machine_replaced();
        // Refresh the cached CPU-state snapshot so a debugger reading
        // `get_cpu_states()` right after load/reset sees the real
        // reset-vector PC + registers instead of the construction
        // defaults. Without this the disassembly view anchors at PC 0
        // until the first single-step.
        self.refresh_debugger_state();
    }

    /// The power switch, which the reset button is not: [`Self::reset`]
    /// re-boots the 65816 off the reset vector, puts the registers of the
    /// 5A22, its DMA channels and the PPU back to their power-on values and
    /// clears the sprite table, but leaves work RAM, the PPU's VRAM and
    /// CGRAM, and the APU's memory holding what the game left there. This
    /// builds them again: a new CPU, and the ROM loaded again, which builds
    /// a new bus (work RAM, the PPU and its memories, the APU) and reads
    /// the coprocessor's firmware and mode and the manifest override again;
    /// the battery SRAM a fresh image. Then the region the machine was in,
    /// which the header's may not be. With no ROM loaded, the reset button
    /// and the work RAM cleared. The debugger, the input, the audio switch
    /// and the host's settings are kept; the coverage too, the image being
    /// the same.
    fn power_cycle(&mut self) {
        let region = self.region();
        self.cpu = Wdc65816::new();
        if self.last_rom.is_empty() {
            EmulatorCore::reset(self);
            self.bus.clear_wram();
        } else {
            let rom = std::mem::take(&mut self.last_rom);
            // These bytes loaded once, so loading them again cannot fail.
            let _ = <Self as EmulatorCore>::load_rom(self, &rom);
        }
        if self.region() != region {
            self.apply_region(region);
            // The reset arms frame 0 on the region's line count and binds
            // the APU's and the coprocessor's clocks to its master clock.
            EmulatorCore::reset(self);
        }
    }

    fn frame_count(&self) -> u64 {
        self.frame_count
    }

    fn master_cycles(&self) -> u64 {
        self.master_cycles
    }

    fn master_clock_hz(&self) -> u64 {
        self.timing.master_clock_hz as u64
    }

    fn region(&self) -> VideoRegion {
        self.timing.region()
    }

    /// The region's timing profile, the PAL bit `$213F` (STAT78) reports
    /// and the refresh rate the host paces frames against. The frame's
    /// events, the APU's and the coprocessor's clocks follow it at the
    /// next reset, which a power cycle and a ROM's load end with; a
    /// savestate carries the profile itself.
    fn apply_region(&mut self, region: VideoRegion) {
        self.timing = TimingProfile::for_region(region);
        self.bus
            .system_regs_mut()
            .set_timing_is_pal(region == VideoRegion::Pal);
        self.sync_display_info();
    }

    fn host_audio(&self) -> &HostAudioRing {
        &self.host_audio
    }

    fn host_audio_mut(&mut self) -> &mut HostAudioRing {
        &mut self.host_audio
    }

    /// The samples a headless frame left in the APU's own ring: a headless
    /// frame runs the DSP but skips the drain, so after a rollback's frames
    /// they describe sound the player already heard in its mispredicted
    /// form. The host ring is left alone: it holds sound presented and
    /// still on its way to the speaker.
    fn discard_pending_audio(&mut self) {
        self.bus.apu_mut().discard_pending_audio();
    }

    fn input_manager(&self) -> &GenericInputManager {
        &self.input_manager
    }

    fn input_manager_mut(&mut self) -> &mut GenericInputManager {
        &mut self.input_manager
    }

    /// The PPU: VRAM, the palette, the sprites, the tiles, the layers and
    /// their toggles.
    fn video_debugger(&mut self) -> &mut dyn retro_core::video::VideoDebugger {
        self.bus.ppu_mut()
    }

    /// The S-DSP, the one sound chip.
    fn audio_chip_count(&self) -> usize {
        1
    }

    /// The S-DSP: its eight voices, their mutes and waveforms.
    fn audio_debugger(&mut self, chip: usize) -> Option<&mut dyn retro_core::audio::AudioDebugger> {
        match chip {
            0 => Some(self.bus.apu_mut()),
            _ => None,
        }
    }

    /// The S-DSP's output, its two channels mixed.
    fn output_waveform(&self, samples: u32) -> Vec<f32> {
        retro_core::audio::AudioDebugger::get_output_waveform(self.bus.apu(), samples)
    }

    fn cartridge_debugger(&mut self) -> Box<dyn retro_core::cartridge::CartridgeDebugger + '_> {
        Box::new(self.map_plan_debugger())
    }

    /// A byte at a time through the bus's CPU path, so a write to an MMIO
    /// register has the side effect a CPU store would (a `$420B` write
    /// starts a DMA, as a game's would); plain WRAM and SRAM edits behave
    /// as the memory editor expects. A write to a PPU register is not one
    /// the frame made: until the next frame's line 1, a repaint and a layer
    /// render draw from the registers as they stand, which show it, rather
    /// than from the line table. What the PPU's data ports take from it is
    /// put down to the debugger.
    fn write_memory_debug(&mut self, addr: u32, data: &[u8]) {
        self.bus.begin_debugger_writes();
        for (i, &byte) in data.iter().enumerate() {
            let at = addr.wrapping_add(i as u32);
            if crate::bus::is_ppu_register_address(at) {
                self.bus.ppu_mut().note_registers_edited();
            }
            self.bus.write(at, byte);
        }
        self.bus.end_debugger_writes();
    }

    /// Bitmap-aware disassembly around `pc` on CPU `cpu_id` (0 the 65C816,
    /// 1 the SPC700, 2 the cartridge's CPU on an SA-1, Super FX or Cx4
    /// cart), over [`retro_core::disassemble_window`]. The SPC700 is
    /// decoded against a side-effect-free view of its memory, so a window
    /// never perturbs the running audio program.
    fn disassemble_window(
        &mut self,
        cpu_id: usize,
        pc: u32,
        before_count: usize,
        after_count: usize,
    ) -> Vec<DisasmRow> {
        let mut driver = debug::SnesDisasmDriver { emu: self, cpu_id };
        retro_core::disassemble_window(&mut driver, pc, before_count, after_count)
    }

    fn step_cpu(&mut self, cpu_id: usize) -> StepResult {
        match cpu_id {
            MAIN_CPU => {
                // The step a host asks for on the 5A22, one step of the
                // frame loop: what is due before it handled, the step with
                // its coverage, trace entry, call stack and history, what
                // it made due handled and the frame closed at its
                // boundary, then the cached register snapshots refreshed.
                retro_core::frame::host_step(self)
            }
            CARTRIDGE_CPU => {
                // The slot-2 CPU: the SA-1's second 65816, the GSU or the Cx4.
                // The chip owns the step and the catch-up anchor (the SA-1 also
                // its deferred-wake, IRQ/NMI sampling and the timer tick), so we
                // just delegate and refresh the cached register snapshots.
                // Reports "no instruction" when no such chip is installed or it
                // has nothing to run (an SA-1 held in reset or paused, a halted
                // GSU or Cx4).
                let result = self
                    .bus
                    .cartridge_mut()
                    .and_then(|c| c.step_secondary_cpu())
                    .unwrap_or(StepResult {
                        cycles: 0,
                        breakpoint_hit: false,
                        instruction_executed: false,
                    });
                self.refresh_debugger_state();
                result
            }
            // The APU runs via lazy catch-up (mailbox access + the
            // per-frame `FrameEnd` hook), not as a debugger-driven
            // single-step target. Stepping a non-5A22 CPU id reports
            // "no instruction executed" so the debugger UI doesn't
            // deadlock waiting for progress.
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

    /// A frame call on the loop the debugger's state selects: the play
    /// tier when nothing is instrumented, the debugger's tier while it
    /// runs with breakpoints or traces, and its paused or stepping tier
    /// otherwise (`retro_core::frame::run_frame_on`, which arms the
    /// debugger's recorders and the second CPUs' trace capture first).
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
        self.last_unsupported_chip = None;
        // Coprocessor firmware + mode + an optional cartridge manifest
        // are configured ahead of time via
        // [`SnesEmulator::set_coprocessor_firmware`] /
        // [`SnesEmulator::set_coprocessor_mode`] /
        // [`SnesEmulator::set_cartridge_manifest_override`] (the
        // `EmulatorCore` trait signature can't carry them). The builder
        // owns DSP-1 detection + mapping; here we only thread the
        // firmware/mode/manifest in.
        let mut builder = CartridgeBuilder::new(data).with_coprocessor_mode(self.coprocessor_mode);
        if let Some(fw) = &self.coprocessor_firmware {
            builder = builder.with_coprocessor_firmware(fw.clone());
        }
        if let Some(manifest) = &self.cartridge_manifest_override {
            // Hand-edited overrides can't always recompute the
            // stripped-ROM digest, so skip the SHA-256 gate; the
            // manifest's `preset` / `sram_size` / `fastrom` still drive
            // the installed plan (builder precedence: manifest > header).
            builder = builder.with_manifest(manifest.clone()).skip_sha256_check();
        }
        let (cart, preset) = match builder.build_reporting_preset() {
            Ok(built) => built,
            Err(e) => {
                if let crate::cartridge::BuildError::UnsupportedChip { chip } = &e {
                    self.last_unsupported_chip = Some(chip);
                }
                return Err(e.to_string());
            }
        };

        // Best-effort header pull for region / metadata. The builder
        // already validated the ROM, but it doesn't expose the
        // header on the trait, we re-run detection (and fall back
        // to a plain offset sweep for manifest-driven layouts that
        // the detector would have rejected). The preset is the one the
        // cartridge was built as, a staged manifest's when one is
        // staged, never this detection's.
        self.header = match detect_preset_with_threshold(data, i32::MIN) {
            Some(DetectedPreset { header, .. }) => Some(header),
            None => HEADER_CANDIDATES
                .iter()
                .find_map(|&offset| try_parse_header(data, offset)),
        };
        self.preset_kind = Some(preset);
        self.rom_size = data.len();
        // Another game's coverage says nothing of this one's code: it is
        // cleared, while loading the same image again keeps it. The image
        // is the file less its copier header (the cartridge's hash, which
        // the savestate envelope names too).
        let same_image = self
            .bus
            .cartridge()
            .is_some_and(|installed| installed.info().rom_hash == cart.info().rom_hash);
        if !same_image {
            self.debugger.clear_all_coverage();
        }
        // The file's hash is the image's unless a copier header comes
        // first, so most loads hash the bytes once.
        self.cartridge_hash = Some(if copier_header_length(data.len()) == 0 {
            cart.info().rom_hash.clone()
        } else {
            retro_core::cartridge::rom_sha256(data)
        });
        // Retain the raw bytes so `power_cycle` can reconstruct a fresh
        // power-on machine (netplay cold boot) by re-running `load_rom`.
        self.last_rom = data.to_vec();

        let region = self
            .header
            .as_ref()
            .map(|h| TimingProfile::from_destination(h.destination).region())
            .unwrap_or(VideoRegion::Ntsc);
        // The host resampler runs at a fixed 32 kHz -> 44.1 kHz ratio that
        // is independent of the console region, so it needs no rebuild
        // here; the APU's own region-dependent 32 kHz clock is rebound in
        // `reset()` via `reset_apu(master_clock_hz)`.

        // `SnesBus::install_cartridge` is first-wins per page,
        // pages already owned by a previously installed cart stay
        // bound to that cart. To support hot-swapping ROMs (used by
        // the host whenever the user picks a different game without
        // tearing the emulator down), replace the bus wholesale with
        // a fresh instance so every page resets to `OpenBus` before
        // the new cart claims its slots.
        // The new bus's APU keeps the channels the host muted, the stems
        // and the sound log the host takes, and the key-on latch.
        let mutes = ChannelMutes::of(self);
        let stems = self.bus.apu_mut().take_stem_state();
        let log = self.bus.apu_mut().take_sound_log_state();
        let latch = self.bus.apu_mut().take_key_on_latch();
        self.bus = SnesBus::new();
        mutes.apply(self);
        self.bus.apu_mut().restore_stem_state(stems);
        self.bus.apu_mut().restore_sound_log_state(log);
        self.bus.apu_mut().restore_key_on_latch(latch);
        self.bus.install_cartridge(cart);
        // The header's region: the timing profile, and the PAL bit STAT78
        // (`$213F` bit 4) reports.
        self.apply_region(region);

        // Surface the ROM / SRAM sizes through the debugger's memory-region
        // map now that the cartridge is installed.
        let sram_size = self.bus.cartridge_sram().map(|s| s.len()).unwrap_or(0) as u32;
        self.debugger
            .console_mut()
            .set_memory_sizes(self.rom_size as u32, sram_size);

        // A cartridge that hosts a steppable secondary CPU (the SA-1's second
        // 65816, the Super FX's GSU or the Cx4) surfaces as a third debugger
        // CPU slot; a plain cart, or a DSP-1 / S-DD1 / SPC7110 cart, stays a
        // two-CPU console. Rebuild the roster (which clamps `set_focused_cpu`)
        // from the base on every load so a hot-swap back to a plain ROM drops
        // the extra slot, and mirror it into the debugger (whose copy backs
        // `get_cpu_info`). The chip that hosts the CPU names it, whatever
        // preset the cartridge was built as: a manifest can put a Super FX
        // on a LoROM board.
        let cartridge_cpu_chip = self
            .bus
            .cartridge()
            .and_then(|c| c.chips().secondary_cpu_chip())
            .map(|chip| chip.info().chip_type);
        let mut cpus = snes_base_cpus();
        if let Some(chip) = cartridge_cpu_chip {
            cpus.push(secondary_cpu_info(chip));
        }
        // The cartridge's CPU decides what slot 2 traces: the GSU's or the
        // Cx4's entries, or nothing (the SA-1, a cartridge with no CPU). A
        // different CPU rebuilds the slot; the same one keeps it.
        let cartridge_cpu = cpus.get(CARTRIDGE_CPU).map(|cpu| cpu.architecture);
        self.debugger
            .configure_cpu(CARTRIDGE_CPU, cartridge_cpu_spec(cartridge_cpu));
        self.system_info.cpus = cpus;
        self.debugger.set_system_info(self.system_info.clone());

        self.reset();
        Ok(())
    }

    /// The header's title, region and checksum, the file's size, and the
    /// mapper of the cartridge built ([`CartridgeInfo::mapper_info`], which
    /// the Mega Drive's reads too): a staged manifest's preset when one is
    /// staged.
    ///
    /// [`CartridgeInfo::mapper_info`]: retro_core::cartridge::CartridgeInfo::mapper_info
    fn get_rom_info(&self) -> Option<RomInfo> {
        let header = self.header.as_ref()?;
        Some(RomInfo {
            title: header.title.clone(),
            region: destination_region_str(header.destination).to_string(),
            size: self.rom_size,
            checksum: header.checksum as u32,
            mapper: self
                .bus
                .cartridge()
                .and_then(|cart| cart.info().mapper_info.as_ref())
                .map(|mapper| mapper.name.clone()),
            title_confidence: "high".to_string(),
        })
    }

    type Manifest = CartridgeManifest;

    fn parse_manifest(json: &str) -> Result<CartridgeManifest, String> {
        crate::cartridge::parse_manifest(json).map_err(|e| e.to_string())
    }

    /// The `cartridge-manifest-v1` the builder's heuristics resolve for
    /// `rom`, with no override applied. Lets the host show the detected
    /// mapping before the user edits it. Pure, does not touch the
    /// loaded cartridge or any staged override.
    fn detected_manifest(&self, rom: &[u8]) -> CartridgeManifest {
        detected_manifest(rom)
    }

    /// Lets a host distinguish "the user picked a mapping" from "detection
    /// is in charge", e.g. to decide whether a low-confidence ROM should
    /// fall back to its vector-implied mapping or surface the failure.
    fn cartridge_manifest_override(&self) -> Option<&CartridgeManifest> {
        self.cartridge_manifest_override.as_ref()
    }

    /// The override does not affect the *currently* installed cartridge:
    /// the host must reload the ROM for it to take effect (mirroring the
    /// firmware / mode staging contract).
    fn set_cartridge_manifest_override(&mut self, manifest: Option<CartridgeManifest>) {
        self.cartridge_manifest_override = manifest;
    }

    fn last_rom(&self) -> &[u8] {
        &self.last_rom
    }

    /// Its copier header included.
    fn get_cartridge_hash(&self) -> Option<String> {
        self.cartridge_hash.clone()
    }

    fn cartridge_info(&self) -> Option<&retro_core::cartridge::CartridgeInfo> {
        self.bus.cartridge().map(|cart| cart.info())
    }

    fn is_cartridge_sram_dirty(&self) -> bool {
        self.bus
            .cartridge()
            .is_some_and(|cart| cart.is_sram_dirty())
    }

    fn clear_cartridge_sram_dirty(&mut self) {
        if let Some(cart) = self.bus.cartridge_mut() {
            cart.clear_sram_dirty();
        }
    }

    /// `"SA-1"`, `"Super FX"` or `"Cx4"`; see [`crate::unsupported_chip`].
    fn last_unsupported_chip(&self) -> Option<&'static str> {
        self.last_unsupported_chip
    }

    /// When `false` (default) overscan frames are cropped to the 224-line
    /// NTSC TV window (matching bsnes-plus / snes9x default output); when
    /// `true` the 239 lines an overscan frame draws are exposed. Where the
    /// line table holds the frame the framebuffer shows, that frame is
    /// drawn again from it with the new setting, and the display follows.
    /// Otherwise, while the overscan bit (`$2133` bit 2) is clear, as it is
    /// after a load or a reset, the setting changes neither the display's
    /// size nor its grid, and the PPU takes it at once; while the bit is
    /// set the PPU takes it when the next frame opens
    /// (`take_overscan_setting`), so the framebuffer, the display's size
    /// and grid and the sprites' boxes keep the frame held until then. It
    /// survives reset / savestate restore.
    fn set_show_overscan(&mut self, show: bool) {
        self.show_overscan = show;
        if self.bus.ppu().show_overscan_enabled() == show {
            return;
        }
        if self.draws_from_line_states() {
            // The line table holds every line of the frame and the video
            // memory each read, so the frame comes out as it was drawn,
            // cropped to the TV window or not as the new setting asks; the
            // drawing puts back the state it moves.
            self.bus.ppu_mut().set_show_overscan(show);
            self.repaint_held_frame();
            self.sync_display_info();
        } else if !self.bus.ppu().overscan() {
            self.bus.ppu_mut().set_show_overscan(show);
        }
    }

    fn show_overscan_enabled(&self) -> bool {
        self.show_overscan
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

    /// The sound switch. Off, the frames' sound stays out of the host's
    /// ring; the APU runs on and its ring keeps the S-DSP's latest samples
    /// for the debugger's output waveform. On again, what that ring holds
    /// is dropped, so the next frame's end queues that frame's sound, not
    /// the sound made while the switch was off.
    fn set_audio_enabled(&mut self, enabled: bool) {
        if enabled && !self.audio_enabled {
            self.bus.apu_mut().discard_pending_audio();
        }
        self.audio_enabled = enabled;
    }

    fn is_audio_enabled(&self) -> bool {
        self.audio_enabled
    }

    fn set_input(&mut self, player: u8, input: InputState) {
        self.input_manager
            .set_direct_override(player as usize, input.buttons);
        // Map the merged port state into the bus pads immediately so
        // manual-serial reads (and register-level tests) observe the
        // new buttons without waiting for the next V-blank sample.
        self.refresh_controllers_from_input();
    }

    fn get_input_info(&self) -> &InputInfo {
        &self.system_info.input
    }

    fn get_save_ram(&self) -> Option<&[u8]> {
        self.bus.cartridge_sram()
    }

    fn set_save_ram(&mut self, data: &[u8]) {
        self.bus.restore_cartridge_sram(data);
    }

    fn debugger(&mut self) -> &mut dyn DebuggerCore {
        &mut self.debugger
    }

    fn debugger_ref(&self) -> &dyn DebuggerCore {
        &self.debugger
    }

    /// Refresh the debugger's cached register snapshots for every CPU
    /// (65C816 from the main CPU, SPC700 from the APU, and, on an SA-1,
    /// Super FX or Cx4 cart, the chip's CPU as slot 2).
    fn refresh_debugger_state(&mut self) {
        let mut states = vec![
            cpu_state_65816(&self.cpu),
            cpu_state_spc700(self.bus.apu().spc700()),
        ];
        // Each chip's snapshot stamps `cpu_id = 0` (the SA-1's comes from the
        // shared 65816 helper); rewrite it to the real slot index.
        if let Some(mut cartridge) = self.bus.cartridge().and_then(|c| c.secondary_cpu_state()) {
            cartridge.cpu_id = CARTRIDGE_CPU;
            states.push(cartridge);
        }
        self.debugger.set_cpu_states(states);
    }

    /// The text of trace entries of CPU `cpu_id` in the formats of
    /// bsnes-plus: the 65C816's, the SPC700's, and on slot 2 the GSU's or
    /// the Cx4's, as the cartridge loaded says. `None` for a CPU with no
    /// trace.
    fn trace_to_text(&mut self, cpu_id: usize, data: &[u8]) -> Option<String> {
        match cpu_id {
            MAIN_CPU => Some(trace_to_text::<TraceEntryBinary65816, ()>(data, &mut ())),
            SOUND_CPU => Some(trace_to_text::<TraceEntryBinarySpc700, ()>(data, &mut ())),
            CARTRIDGE_CPU => match self
                .system_info
                .cpus
                .get(CARTRIDGE_CPU)
                .map(|cpu| cpu.architecture)
            {
                #[cfg(feature = "chip-superfx")]
                Some(CpuArchitecture::SuperFx) => Some(trace_to_text::<
                    crate::debugger::TraceEntryBinaryGsu,
                    (),
                >(data, &mut ())),
                #[cfg(feature = "chip-cx4")]
                Some(CpuArchitecture::Hg51b) => Some(trace_to_text::<
                    crate::debugger::TraceEntryBinaryCx4,
                    (),
                >(data, &mut ())),
                _ => None,
            },
            _ => None,
        }
    }

    /// Reads memory as the CPU sees it, through the bus's side-effect-free
    /// peek: work RAM, ROM and the cartridge's RAM give their bytes, a
    /// coprocessor's ROM or RAM window the bytes behind it, and a register
    /// (the PPU's, the APU's ports, the 5A22's, a chip's) 0 or the
    /// open-bus value, since reading it could change it. Nothing moves:
    /// no latch, no clock, no coprocessor.
    fn read_memory_debug(&mut self, addr: u32, size: u32) -> Vec<u8> {
        (0..size)
            .map(|i| self.bus.peek(addr.wrapping_add(i)))
            .collect()
    }

    fn disassemble_at(&mut self, addr: u32, count: u32) -> Vec<Instruction> {
        self.disassemble_cpu(0, addr, count)
    }

    fn disassemble_cpu(&mut self, cpu_id: usize, addr: u32, count: u32) -> Vec<Instruction> {
        // Forward-decode `count` instructions from `addr`. The 65C816
        // reads through the bus's side-effect-free peek with the current
        // M/X/E flags (immediate operand width depends on them); the
        // SPC700 uses a side-effect-free view of its memory and the live
        // `PSW.P` direct-page select.
        match cpu_id {
            0 => {
                let state = wdc65816::disasm::DisasmState {
                    emulation: self.cpu.emulation,
                    flag_m: self.cpu.p & 0x20 != 0,
                    flag_x: self.cpu.p & 0x10 != 0,
                };
                let mut out = Vec::with_capacity(count as usize);
                let mut cursor = addr & 0x00FF_FFFF;
                for _ in 0..count {
                    let res = wdc65816::disasm::disassemble_one(
                        &mut SnesPeekBus(&self.bus),
                        cursor,
                        &state,
                    );
                    let len = res.bytes_consumed.max(1);
                    out.push(Instruction {
                        address: cursor,
                        bytes: res.bytes,
                        mnemonic: res.mnemonic,
                        operands: res.operands,
                        comment: res.comment,
                    });
                    // Advance within the bank; instruction fetch wraps the
                    // 16-bit offset and keeps the program bank fixed.
                    let bank = cursor & 0x00FF_0000;
                    let next = (cursor as u16).wrapping_add(len as u16);
                    cursor = bank | next as u32;
                }
                out
            }
            1 => {
                let state = spc700::disasm::DisasmState {
                    p: self.bus.apu().spc700().psw & 0x20 != 0,
                };
                let mut out = Vec::with_capacity(count as usize);
                let mut cursor = (addr & 0xFFFF) as u16;
                for _ in 0..count {
                    let mut bus = SpcPeekBus {
                        apu: self.bus.apu(),
                    };
                    let res = spc700::disasm::disassemble_one(&mut bus, cursor as u32, &state);
                    let len = res.bytes_consumed.max(1);
                    out.push(Instruction {
                        address: cursor as u32,
                        bytes: res.bytes,
                        mnemonic: res.mnemonic,
                        operands: res.operands,
                        comment: res.comment,
                    });
                    cursor = cursor.wrapping_add(len as u16);
                }
                out
            }
            // The slot-2 CPU (the SA-1's second 65816, the GSU or the Cx4). The
            // chip decodes against a side-effect-free view of its own memory
            // (for the SA-1, ROM via the Super MMC + I-RAM + BW-RAM, MMIO
            // read-inert), so a window never perturbs it.
            2 => self
                .bus
                .cartridge()
                .map(|c| c.disassemble_secondary(addr, count))
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
