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

use std::collections::VecDeque;

use retro_core::audio::AudioDebugger;
use retro_core::audio::{AudioRingBuffer, LinearResampler};
use retro_core::cpu::Addressable;
use retro_core::debugger::DebuggerCore;
use retro_core::input::{GenericInputManager, KeyboardInputProvider};
use retro_core::video::VideoDebugger;
use retro_core::*;
use wdc65816::instructions::FLAG_I;
use wdc65816::Wdc65816;

use crate::debugger::{
    cpu_state_65816, cpu_state_spc700, SnesDebugger, SnesEvent, TraceEntryBinary65816,
};
use crate::trace_format::{effective_address, EffRegs};

use crate::bus::{BusDebugEvent, IoAccess, SnesBus};
use crate::cartridge::{
    detect_preset_with_threshold, detected_manifest, rom_sha256, try_parse_header,
    CartridgeBuilder, CartridgeManifest, CoprocessorMode, DetectedPreset, PresetKind, SnesHeader,
    HEADER_CANDIDATES,
};
use crate::controller::{
    default_gamepad_mapping, default_keyboard_mapping, input_state_to_snes_pad,
};
use crate::irq::schedule_irq_events;
use crate::scheduler::{schedule_frame_events, EventKind, HEventScheduler, TimingProfile};

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
/// Worst-case SNES output height in pixels: 240 visible lines with
/// overscan, doubled to 480 by Mode 5/6 interlace (`$2133` bit 0),
/// which the renderer weaves into a single 512x480 frame.
const MAX_FB_HEIGHT: u32 = 480;
/// Default non-hires width reported via `DisplayInfo` at boot.
const DEFAULT_FB_WIDTH: u32 = 256;
/// Default non-overscan height reported via `DisplayInfo` at boot.
const DEFAULT_FB_HEIGHT: u32 = 224;

/// Master clocks the auto-joypad read holds the HVBJOY busy flag
/// (`$4212` bit 0) after it starts at V-blank. Hardware spends roughly
/// the first few scanlines of V-blank shifting the 16 bits; we latch
/// the snapshot at once (see `SnesBus::auto_joypad_read`) and clear the
/// busy flag this many cycles later so a polling game still observes
/// the window. ~3.9 scanlines at 1364 mclks/line.
const AUTO_JOYPAD_CYCLES: u64 = 5360;

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
    scheduler: HEventScheduler,
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
    audio_ring: AudioRingBuffer<f32>,
    /// 32 kHz (S-DSP) -> 44.1 kHz (host) linear resampler. The ratio is
    /// region-independent, so this instance lives for the emulator's
    /// lifetime and is only `reset()`.
    resampler: LinearResampler,
    /// Scratch for one frame's worth of 32 kHz `i16` samples drained from
    /// the APU before resampling (reused to avoid per-frame allocation).
    apu_drain_buf: Vec<i16>,
    /// Scratch for the resampled 44.1 kHz `f32` samples before they are
    /// pushed into `audio_ring`.
    audio_resample_buf: Vec<f32>,
    audio_pending_drain: Vec<f32>,
    audio_enabled: bool,

    input_manager: GenericInputManager,

    focused_cpu: usize,

    header: Option<SnesHeader>,
    preset_kind: Option<PresetKind>,
    rom_size: usize,
    /// SHA-256 (lowercase hex) of the loaded ROM, post-SMC-strip, the
    /// manifest-convention digest. Embedded in every savestate envelope
    /// and checked on load so a state can't be applied to the wrong
    /// ROM. Empty until a ROM is loaded.
    rom_hash: String,

    /// The raw bytes of the most recently loaded ROM, retained so
    /// [`Self::power_on_reset`] can re-run [`EmulatorCore::load_rom`] to
    /// reconstruct a byte-identical power-on state (the netplay cold boot)
    /// without the host re-supplying them. Empty until a ROM is loaded.
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

    /// Host display preference: when `true`, expose the full 240-line
    /// overscan frame; when `false` (default), crop to the 224-line NTSC
    /// TV window. Stored here (not just on the PPU) so it survives the
    /// wholesale bus rebuild in [`EmulatorCore::load_rom`], `reset`
    /// re-applies it to the live PPU. It is a UI toggle, never emulated
    /// state, so it is intentionally outside savestate.
    show_overscan: bool,

    /// Breakpoints, coverage, tracing, and cached CPU-state snapshots.
    /// Drives execution-tier selection in [`Self::run_frame`].
    debugger: SnesDebugger,

    /// Mirror of `debugger.has_watchpoints_or_events()`, refreshed each
    /// debug entry. Gates the inline event checks in `handle_event_at` and
    /// `step_cpu_internal` so the fast path stays branch-free.
    debug_armed: bool,
    /// Set by an armed event/watchpoint hit mid-frame; the instrumented
    /// frame loop honours it by pausing and returning. Reset at the top
    /// of every instrumented frame.
    pending_break: bool,
    /// Reused buffer for draining the bus's per-instruction debug events
    /// without reallocating each step.
    debug_event_scratch: Vec<BusDebugEvent>,

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
    /// duration of [`Self::run_frame_headless`] (netplay
    /// rollback/catch-up) so re-simulating a span is cheaper while
    /// leaving `state_hash` byte-identical to a presented run. Always
    /// `true` on the normal play path.
    present: bool,
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

        let debugger = SnesDebugger::new(system_info.clone());

        Self {
            system_info,
            framebuffer,
            cpu: Wdc65816::new(),
            bus: SnesBus::new(),
            scheduler: HEventScheduler::new(timing),
            timing,
            frame_count: 0,
            master_cycles: 0,
            frame_base_cycle: 0,
            audio_ring: AudioRingBuffer::<f32>::with_capacity(AUDIO_RING_CAPACITY),
            resampler: LinearResampler::new(32_000, 44_100),
            apu_drain_buf: Vec::new(),
            audio_resample_buf: Vec::new(),
            audio_pending_drain: Vec::new(),
            audio_enabled: false,
            input_manager: default_input_manager(),
            focused_cpu: 0,
            header: None,
            preset_kind: None,
            rom_size: 0,
            rom_hash: String::new(),
            last_rom: Vec::new(),
            coprocessor_firmware: None,
            coprocessor_mode: CoprocessorMode::Auto,
            last_unsupported_chip: None,
            cartridge_manifest_override: None,
            show_overscan: false,
            debugger,
            debug_armed: false,
            pending_break: false,
            nmi_deliver_armed: false,
            nmi_deliver_countdown: 0,
            irq_cycles: VecDeque::new(),
            irq_read_early: None,
            debug_event_scratch: Vec::new(),
            present: true,
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

    /// The chip the last [`EmulatorCore::load_rom`] refused because this
    /// build has no module for it (`"SA-1"`, `"Super FX"`, `"Cx4"`), or
    /// `None`. See [`crate::unsupported_chip`].
    pub fn last_unsupported_chip(&self) -> Option<&'static str> {
        self.last_unsupported_chip
    }

    /// Host display preference: when `false` (default) overscan frames
    /// are cropped to the 224-line NTSC TV window (matching bsnes-plus /
    /// snes9x default output); when `true` the full 240-line frame is
    /// exposed. Applies immediately and survives reset / savestate
    /// restore, it is a UI toggle, not emulated state.
    pub fn set_show_overscan(&mut self, show: bool) {
        self.show_overscan = show;
        self.bus.ppu_mut().set_show_overscan(show);
        self.sync_display_info();
    }

    /// Whether the full overscan frame is exposed (vs cropped to 224).
    pub fn show_overscan_enabled(&self) -> bool {
        self.show_overscan
    }

    /// Stage a cartridge manifest applied on the next
    /// [`EmulatorCore::load_rom`], overriding heuristic detection. Pass
    /// `None` to revert to pure auto-detection. The override does not
    /// affect the *currently* installed cartridge, the host must
    /// reload the ROM for it to take effect (mirroring the firmware /
    /// mode staging contract).
    pub fn set_cartridge_manifest_override(&mut self, manifest: Option<CartridgeManifest>) {
        self.cartridge_manifest_override = manifest;
    }

    /// The manifest currently staged via
    /// [`Self::set_cartridge_manifest_override`], or `None` when the
    /// next `load_rom` will run pure auto-detection. Lets a host
    /// distinguish "the user picked a mapping" from "detection is in
    /// charge", e.g. to decide whether a low-confidence ROM should
    /// fall back to its vector-implied mapping or surface the failure.
    pub fn cartridge_manifest_override(&self) -> Option<&CartridgeManifest> {
        self.cartridge_manifest_override.as_ref()
    }

    /// The `cartridge-manifest-v1` the builder's heuristics resolve for
    /// `rom`, with no override applied. Lets the host show the detected
    /// mapping before the user edits it. Pure, does not touch the
    /// loaded cartridge or any staged override.
    pub fn detected_manifest(&self, rom: &[u8]) -> CartridgeManifest {
        detected_manifest(rom)
    }

    /// Human-readable summary of the installed coprocessor (e.g.
    /// `"DSP-1B (LLE)"`), or `None` for a plain cartridge. Used by the
    /// CLI `info` command.
    pub fn coprocessor_label(&self) -> Option<&str> {
        self.bus.cartridge_ref()?.coprocessor_label()
    }

    /// Count of CPU accesses to the coprocessor's DR/SR window since the
    /// last reset (`0` when no coprocessor is installed or the game has
    /// not touched it yet). A positive value proves the running game is
    /// actively driving the chip.
    pub fn coprocessor_accesses(&self) -> u64 {
        self.bus
            .cartridge_ref()
            .map(|c| c.coprocessor_accesses())
            .unwrap_or(0)
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

    /// Live coprocessor chip state (DSP-1 uPD7725 registers + data RAM),
    /// or `None` for a plain cartridge. Read-only path for the CLI `info`
    /// command and WASM/frontend debugger, does not borrow the
    /// breakpoint list like [`Self::cartridge_debugger`] does. Reports the
    /// primary (lowest-id) chip; see [`Self::coprocessors_debug_state`] for
    /// the full set.
    pub fn coprocessor_debug_state(&self) -> Option<retro_core::cartridge::ExpansionChipState> {
        self.bus.cartridge_ref()?.coprocessor_debug_state()
    }

    /// Live state of every installed coprocessor, keyed by `chip_id`. Empty
    /// for a plain cartridge. Read-only path for the CLI `info` command and
    /// WASM/frontend debugger, enumerates a multi-chip cartridge instead of
    /// assuming a single coprocessor.
    pub fn coprocessors_debug_state(&self) -> Vec<(u8, retro_core::cartridge::ExpansionChipState)> {
        self.bus
            .cartridge_ref()
            .map(|c| c.coprocessors_debug_state())
            .unwrap_or_default()
    }

    /// Human-readable label of every installed coprocessor, keyed by
    /// `chip_id` (e.g. `[(1, "DSP-1B (LLE)")]`). Empty for a plain
    /// cartridge. Used by the CLI `info` command to enumerate the chips.
    pub fn coprocessor_labels(&self) -> Vec<(u8, String)> {
        self.bus
            .cartridge_ref()
            .map(|c| c.coprocessor_labels())
            .unwrap_or_default()
    }

    /// Borrow the main address-space dispatcher.
    pub fn bus(&self) -> &SnesBus {
        &self.bus
    }

    /// Mutable borrow of the main address-space dispatcher.
    pub fn bus_mut(&mut self) -> &mut SnesBus {
        &mut self.bus
    }

    /// Times the host fell behind and the host audio ring dropped its
    /// oldest samples. Mirrors SG1's `audio_ring_overflows()` WASM export
    /// for the audio-backpressure telemetry overlay and the no-drift test.
    pub fn audio_overflows(&self) -> u64 {
        self.audio_ring.overflow_count()
    }

    /// Times the host pulled more samples than were queued (the ring ran
    /// dry). Mirrors SG1's `audio_ring_underruns()`.
    pub fn audio_underruns(&self) -> u64 {
        self.audio_ring.underrun_count()
    }

    /// Drop audio that has been generated but not yet handed to the host.
    ///
    /// Called after a rollback re-simulation: those frames' samples sit in
    /// the APU's own ring (a headless frame runs the DSP but skips the drain)
    /// and describe audio the player already heard in its mispredicted form.
    /// The host ring is deliberately left alone, it holds audio that *was*
    /// presented and is still on its way to the speaker.
    pub fn discard_pending_audio(&mut self) {
        self.bus.apu_mut().discard_pending_audio();
    }

    /// Interleaved L/R `f32` samples currently queued in the host audio
    /// ring (a value of `1470` means 735 stereo pairs ready). Lets a host
    /// worklet right-size a [`Self::drain_audio_into`] pull. Mirrors SG1's
    /// `audio_available()`.
    pub fn audio_available(&self) -> usize {
        self.audio_ring.available()
    }

    /// Drain up to `dst.len()` queued host samples into `dst`, returning
    /// how many were written. Destructive: each sample is yielded once.
    /// Mirrors SG1's `drain_audio_into()` so the WASM `getAudioSamplesN`
    /// export pulls a bounded, host-controlled window without over-draining
    /// the ring (unlike [`EmulatorCore::get_audio_samples`], which empties
    /// it). Size `dst` to `audio_available()` to avoid spurious underruns.
    pub fn drain_audio_into(&mut self, dst: &mut [f32]) -> usize {
        self.audio_ring.drain_into(dst)
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

    /// Borrow the generic input manager (providers, port assignments,
    /// key/gamepad mappings, history). Mirrors SG1's `input_manager()` so
    /// the WASM layer can expose the same keyboard/gamepad/provider
    /// surface across cores.
    pub fn input_manager(&self) -> &GenericInputManager {
        &self.input_manager
    }

    /// Mutable borrow of the input manager. Mirrors SG1's
    /// `input_manager_mut()`; used by the WASM `keyDown`/`keyUp`/`gamepad*`
    /// and remap exports.
    pub fn input_manager_mut(&mut self) -> &mut GenericInputManager {
        &mut self.input_manager
    }

    /// Borrow the scheduler. Tests use this to check pending events
    /// and the current scanline position.
    pub fn scheduler(&self) -> &HEventScheduler {
        &self.scheduler
    }

    /// Timing profile currently active (NTSC or PAL).
    pub fn timing(&self) -> TimingProfile {
        self.timing
    }

    /// Set the video region from a case-insensitive `"ntsc"` / `"pal"`
    /// string, reconfiguring the timing profile, the event scheduler,
    /// the APU + coprocessor rate converters, and the `$213F` STAT78
    /// PAL/NTSC bit, then re-priming the machine via [`Self::reset`].
    ///
    /// The netplay session handshake carries region as a string, so this
    /// keeps the [`TimingProfile`] enum off the WASM/JS boundary. Call it
    /// right after [`EmulatorCore::load_rom`] and before running any
    /// frames: it resets the machine (so both peers agree on region
    /// before frame 0). Without an explicit call the region defaults to
    /// the cartridge header's destination byte (see `load_rom`).
    pub fn set_region_str(&mut self, name: &str) -> Result<(), String> {
        let profile = match name.trim().to_ascii_lowercase().as_str() {
            "ntsc" => TimingProfile::NTSC,
            "pal" => TimingProfile::PAL,
            other => {
                return Err(format!(
                    "unknown region '{other}' (expected 'ntsc' or 'pal')"
                ))
            }
        };
        self.timing = profile;
        // `reset()` re-anchors the scheduler cursor but keeps whatever
        // timing the scheduler was built with, so rebuild it on the new
        // profile here; the per-frame event cycles are then computed
        // against the new scanline count.
        self.scheduler = HEventScheduler::new(profile);
        // STAT78 (`$213F` bit 4) must report the new region.
        self.bus
            .system_regs_mut()
            .set_timing_is_pal(profile.scanlines_per_frame == 312);
        // reset() re-arms frame-0 events and rebinds the APU +
        // coprocessor rate converters to `self.timing.master_clock_hz`.
        self.reset();
        Ok(())
    }

    /// Current region as the `"ntsc"` / `"pal"` string used by the
    /// netplay handshake and the `getRegion` WASM export. PAL profiles
    /// run 312 scanlines per frame, NTSC 262.
    pub fn region_str(&self) -> &'static str {
        if self.timing.scanlines_per_frame == 312 {
            "pal"
        } else {
            "ntsc"
        }
    }

    /// Number of frames produced since the last `reset()` /
    /// `load_rom()`. Used by tests and the host UI for FPS reporting.
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Build the full deterministic machine snapshot shared by
    /// [`EmulatorCore::save_state`] (which wraps it in the cross-core
    /// envelope) and [`Self::state_hash`] (which hashes it). The payload
    /// carries no wall-clock or host metadata (the envelope owns
    /// `created_at` and `rom_hash`), so it is a pure function of
    /// deterministic machine state.
    fn capture_save_state(&self) -> crate::savestate::SnesSaveState {
        use crate::savestate::{Cpu65816State, SnesSaveState, SNES_SAVESTATE_VERSION};
        SnesSaveState {
            version: SNES_SAVESTATE_VERSION,
            cpu: Cpu65816State::capture(&self.cpu),
            bus: self.bus.to_savestate(),
            scheduler: self.scheduler.to_savestate(),
            master_cycles: self.master_cycles,
            frame_base_cycle: self.frame_base_cycle,
            frame_count: self.frame_count,
            timing: crate::scheduler::timing_to_state(&self.timing),
            focused_cpu: self.focused_cpu as u8,
            audio_enabled: self.audio_enabled,
        }
    }

    /// Load a savestate, refusing one captured against another ROM unless
    /// `check` is `RomCheck::AnyRom`. [`EmulatorCore::load_state`] is this
    /// with `RomCheck::SameRom` and its refusal as text; a host that asked
    /// the player first loads a state made on another build of the game
    /// (a translation in progress, say) with `RomCheck::AnyRom`. The
    /// refusal says whether the ROM was the reason.
    pub fn load_state_with_rom_check(
        &mut self,
        data: &[u8],
        check: retro_core::savestate::RomCheck,
    ) -> Result<(), retro_core::savestate::LoadStateError> {
        use crate::savestate::{SnesSaveState, SNES_SAVESTATE_VERSION};
        use retro_core::savestate::{check_rom_hash, deserialize_compressed, LoadStateError};

        let env = deserialize_compressed::<SnesSaveState>(data)?;
        // Refuse a state captured against a different ROM unless the host
        // asked for any ROM (an empty hash in the state is a wildcard,
        // matching the envelope contract).
        check_rom_hash(&env, &self.rom_hash, check)?;
        // Guard the envelope's own version too, so a tampered wrapper
        // can't smuggle a future schema past the payload check below.
        if env.core_version > SNES_SAVESTATE_VERSION {
            return Err(LoadStateError::Unreadable(format!(
                "SNES save state core_version {} is newer than supported version {}",
                env.core_version, SNES_SAVESTATE_VERSION
            )));
        }
        let payload = env.payload;
        // Reject a newer payload schema and validate the big-memory
        // lengths before mutating any live state.
        payload.validate()?;

        // Restore only volatile state. The cartridge, its map plan, and
        // the page/speed tables are deliberately preserved, the ROM is
        // already loaded, and `SnesBus::from_savestate` re-syncs the
        // FastROM speeds via `apply_memsel`.
        payload.cpu.restore(&mut self.cpu);
        self.timing = crate::scheduler::timing_from_state(&payload.timing);
        self.bus
            .from_savestate(&payload.bus, self.timing.master_clock_hz as u64);
        self.scheduler.from_savestate(&payload.scheduler);
        self.collect_irq_cycles();
        self.irq_read_early = None;
        self.master_cycles = payload.master_cycles;
        self.frame_base_cycle = payload.frame_base_cycle;
        self.frame_count = payload.frame_count;
        // Saves are taken at frame boundaries, where NMI delivery has
        // already resolved (it arms and fires within a frame), so it is
        // not part of the wire format, just clear the transient state.
        // `SystemRegs::nmi_flag_observed` is likewise transient and
        // re-derived the next vblank.
        self.nmi_deliver_armed = false;
        self.nmi_deliver_countdown = 0;
        self.focused_cpu =
            (payload.focused_cpu as usize).min(self.system_info.cpus.len().saturating_sub(1));
        self.audio_enabled = payload.audio_enabled;

        // Host-side audio is not in the wire format: clear the ring and
        // reset the resampler so playback resumes from the restored APU
        // without replaying stale samples.
        self.audio_ring.clear();
        self.resampler.reset();
        self.apu_drain_buf.clear();
        self.audio_resample_buf.clear();
        self.audio_pending_drain.clear();

        // The restored PPU register file may imply a different active
        // display size (hires / overscan); resync the cached DisplayInfo
        // so the next host poll matches.
        self.sync_display_info();
        Ok(())
    }

    /// Cold (power-on) reset: reload the current ROM into a fresh CPU and bus,
    /// which zeroes the 128 KiB work RAM; with no ROM loaded, run the ordinary
    /// [`EmulatorCore::reset`] and then zero the work RAM.
    ///
    /// The plain `reset()` preserves WRAM (SNES *reset-button* semantics); this
    /// emulates a *power cycle*, which clears it. Netplay needs it: two peers must
    /// derive a byte-identical frame-0 state hash, but a host that had been running
    /// the game (dirty WRAM) and a freshly-booted guest reset to frame 0 with
    /// *different* WRAM, since `reset()` leaves it untouched. Zeroing it gives both
    /// peers the same deterministic power-on baseline. The cartridge ROM is kept,
    /// but battery SRAM starts as a fresh image, as after any load of the ROM.
    pub fn power_on_reset(&mut self) {
        // A soft reset (the console's reset *button*, `EmulatorCore::reset`) is
        // faithful to hardware: it re-boots the 65816 off the reset vector but
        // leaves the CPU registers, the PPU's VRAM/CGRAM/OAM, and the APU holding
        // whatever the running game left there. A real *power* cycle clears all of
        // it. Netplay needs the latter: two peers with different pre-session
        // histories (a host mid-game vs a just-joined guest) MUST converge on a
        // byte-identical frame-0 state, because the session starts only once
        // both peers report the same state hash.
        //
        // `load_rom` already replaces the bus wholesale (fresh PPU/VRAM/CGRAM/OAM,
        // APU, and WRAM) and re-reads the retained coprocessor firmware/mode +
        // cartridge manifest override + re-derives the region from the ROM header,
        // so re-running it after recreating the CPU (a plain reset leaves its
        // registers holding the game's values) reconstructs exactly
        // what a just-loaded guest holds. Because both peers run this identical
        // logic over the same ROM, the result is identical by construction.
        // Battery-backed SRAM is intentionally a fresh (empty) image: a game with
        // a save resumes through the seeded savestate transfer, NOT this cold boot.
        self.cpu = Wdc65816::new();
        if !self.last_rom.is_empty() {
            let rom = std::mem::take(&mut self.last_rom);
            // These bytes already loaded once, so a re-load cannot fail; swallow
            // the Result rather than propagate: `power_on_reset` is infallible.
            let _ = <Self as EmulatorCore>::load_rom(self, &rom);
        } else {
            // No ROM ever loaded: fall back to the soft reset + WRAM wipe so a
            // bare-machine cold boot still zeroes volatile RAM.
            EmulatorCore::reset(self);
            self.bus.clear_wram();
        }
    }

    /// Deterministic FNV-1a-64 hash of the full machine state, for
    /// netplay desync detection and the determinism harness. Hashes the
    /// exact payload [`EmulatorCore::save_state`] serializes and
    /// [`EmulatorCore::load_state`] restores (raw bincode, no
    /// compression), so a save/load round-trip is hash-preserving by
    /// construction, two emulators in the same logical state hash equal,
    /// and [`Self::run_frame_headless`] leaves the same hash as a
    /// presented frame.
    pub fn state_hash(&self) -> u64 {
        let bytes = bincode::serialize(&self.capture_save_state()).unwrap_or_default();
        retro_core::fnv1a64(&bytes)
    }

    /// Per-subsystem FNV-1a-64 hashes of the canonical savestate payload,
    /// for netplay desync triage: when two instances (or a save/load
    /// round-trip) disagree on [`Self::state_hash`], this pinpoints which
    /// subsystem's serialized state diverged. `frame_count` is reported
    /// verbatim (not hashed) so a frame-alignment mismatch is obvious.
    pub fn state_hash_parts(&self) -> Vec<(&'static str, u64)> {
        let s = self.capture_save_state();
        let h = |bytes: Vec<u8>| retro_core::fnv1a64(&bytes);
        vec![
            ("cpu", h(bincode::serialize(&s.cpu).unwrap_or_default())),
            (
                "wram",
                h(bincode::serialize(&s.bus.wram).unwrap_or_default()),
            ),
            ("ppu", h(bincode::serialize(&s.bus.ppu).unwrap_or_default())),
            ("apu", h(bincode::serialize(&s.bus.apu).unwrap_or_default())),
            ("dma", h(bincode::serialize(&s.bus.dma).unwrap_or_default())),
            (
                "system_regs",
                h(bincode::serialize(&s.bus.system_regs).unwrap_or_default()),
            ),
            (
                "controllers",
                h(bincode::serialize(&s.bus.controllers).unwrap_or_default()),
            ),
            (
                "sram",
                h(bincode::serialize(&s.bus.sram).unwrap_or_default()),
            ),
            (
                "coprocessors",
                h(bincode::serialize(&s.bus.coprocessors).unwrap_or_default()),
            ),
            ("bus", h(bincode::serialize(&s.bus).unwrap_or_default())),
            (
                "scheduler",
                h(bincode::serialize(&s.scheduler).unwrap_or_default()),
            ),
            (
                "timing",
                h(bincode::serialize(&s.timing).unwrap_or_default()),
            ),
            ("frame_count", s.frame_count),
        ]
    }

    /// Master clocks accumulated since the last `reset()` /
    /// `load_rom()`.
    pub fn master_cycles(&self) -> u64 {
        self.master_cycles
    }

    /// Parsed SNES header for the currently loaded ROM, or `None`
    /// if no ROM has been loaded.
    pub fn header(&self) -> Option<&SnesHeader> {
        self.header.as_ref()
    }

    /// Detected/forced mapper kind for the currently loaded ROM.
    pub fn preset_kind(&self) -> Option<PresetKind> {
        self.preset_kind
    }

    /// Borrow the PPU. Useful for the debugger and for tests that
    /// want to inspect VRAM / OAM / CGRAM / register state without
    /// going through the bus.
    pub fn ppu(&self) -> &crate::ppu::Ppu {
        self.bus.ppu()
    }

    /// Mutable [`VideoDebugger`] view of the PPU (VRAM / palette /
    /// sprites / tiles / layers + `toggle_layer`).
    ///
    /// Mirrors the SG1 precedent: `DebuggerCore::video_debugger()`
    /// returns `None` because the debugger struct doesn't own the PPU,
    /// so the host inspects video state through this emulator accessor
    /// instead.
    pub fn video_debugger(&mut self) -> &mut dyn VideoDebugger {
        self.bus.ppu_mut()
    }

    /// Mutable [`AudioDebugger`] view of the S-DSP (8 voices, per-voice
    /// mute, output/per-voice waveforms).
    ///
    /// As with [`Self::video_debugger`], `DebuggerCore::audio_debugger()`
    /// returns `None` (the debugger doesn't own the APU); inspection
    /// goes through this emulator accessor instead.
    pub fn audio_debugger(&mut self) -> &mut dyn AudioDebugger {
        self.bus.apu_mut()
    }

    /// Live Mode 7 matrix as signed `[A, B, C, D, X, Y]`, for the Mode 7 viewer.
    pub fn get_mode7_matrix(&self) -> [i16; 6] {
        self.bus.ppu().get_mode7_matrix()
    }

    /// 256-colour CGRAM rendered as a 256x256 RGBA8888 swatch grid.
    pub fn export_palette_image(&self) -> Vec<u8> {
        self.bus.ppu().export_palette_image()
    }

    /// A 16x16 tile sheet (128x128 RGBA) decoded from the chosen VRAM
    /// character base with the given 16-colour sub-palette.
    pub fn export_tile_image(
        &self,
        source: crate::ppu::TileSetSource,
        palette: u8,
    ) -> crate::ppu::ViewerImage {
        self.bus.ppu().export_tile_image(source, palette)
    }

    /// Background `bg` (`0..=3`) tilemap rendered to an RGBA image.
    pub fn export_tilemap_image(&self, bg: usize) -> crate::ppu::ViewerImage {
        self.bus.ppu().export_tilemap_image(bg)
    }

    /// All 128 OAM sprites flattened into [`crate::ppu::SpriteDescriptor`]s.
    pub fn export_oam_sprites(&self) -> Vec<crate::ppu::SpriteDescriptor> {
        self.bus.ppu().export_oam_sprites()
    }

    /// Per-background addressing + format snapshot ([`crate::ppu::BgLayout`])
    /// for the host's raw-layer (full-map) assembler.
    pub fn bg_layout(&self, bg: usize) -> crate::ppu::BgLayout {
        self.bus.ppu().bg_layout(bg)
    }

    /// Where the renderer finds the sprites' tiles, and the two sprite
    /// sizes ([`crate::ppu::SpriteTileSettings`]), for a host that reads
    /// a sprite's tiles the way the renderer does.
    pub fn sprite_tile_settings(&self) -> crate::ppu::SpriteTileSettings {
        self.bus.ppu().sprite_tile_settings()
    }

    /// Re-render the held frame from the present PPU state (VRAM / CGRAM /
    /// OAM / registers + the debugger layer mask) into the framebuffer,
    /// without advancing emulation.
    ///
    /// Mirrors the live per-frame sequence: walk every visible scanline
    /// through [`crate::ppu::Ppu::render_scanline`] (starting at line 1 so
    /// the per-frame reset fires and the `in_active_scanline` gate drops
    /// the lines past the active region) then run
    /// [`crate::ppu::Ppu::finalize_frame`] for the hires/overscan
    /// normalisation. Only meaningful while paused, a running core
    /// repaints on its own next frame, but a paused one never would, so a
    /// host that toggles a layer calls this to see the change at once.
    pub fn repaint_held_frame(&mut self) {
        // 240 is the overscan ceiling; `in_active_scanline` clips the
        // tail to the live 224/240-line window per the overscan bit.
        for scanline in 1..240u16 {
            self.bus
                .ppu_mut()
                .render_scanline(scanline, &mut self.framebuffer);
        }
        self.bus.ppu().finalize_frame(&mut self.framebuffer);
    }

    /// A [`crate::cartridge::SnesCartridgeDebugger`] bound to the live
    /// breakpoint list: memory-map / ROM / SRAM / mapper inspection from
    /// the installed [`crate::cartridge::MapPlan`], with ROM/SRAM
    /// breakpoints delegating into the emulator's own breakpoint list.
    pub fn cartridge_debugger(&mut self) -> crate::cartridge::SnesCartridgeDebugger<'_> {
        let plan = self.bus.cartridge_plan().cloned().unwrap_or_default();
        let rom_size = self.rom_size as u32;
        let sram_size = self.bus.cartridge_sram().map(<[u8]>::len).unwrap_or(0) as u32;
        let preset_kind = self.preset_kind;
        // Snapshot every coprocessor's state up front (owned), so the
        // immutable cartridge borrow ends before we borrow the breakpoint
        // list mutably below.
        let chip_states = self
            .bus
            .cartridge_ref()
            .map(|c| c.coprocessors_debug_state())
            .unwrap_or_default();
        crate::cartridge::SnesCartridgeDebugger::new(
            plan,
            rom_size,
            sram_size,
            preset_kind,
            chip_states,
            self.debugger.breakpoints_mut(),
        )
    }

    /// `true` iff `cpu_id` has executed an instruction from `addr`
    /// (coverage bitmap). `cpu_id` 0 = 65C816 (24-bit), 1 = SPC700
    /// (16-bit); any other id reads as uncovered. Mirrors SG1's
    /// `is_covered()` so the disassembly panel can grey out cold regions.
    pub fn is_covered(&self, cpu_id: usize, addr: u32) -> bool {
        match cpu_id {
            0 => self.debugger.is_covered_65816(addr),
            1 => self.debugger.is_covered_spc700(addr),
            _ => false,
        }
    }

    /// `(total_addressable_bytes, covered_bytes)` for `cpu_id`'s coverage
    /// bitmap: 65C816 spans the full 24-bit space (`0x100_0000`), the
    /// SPC700 its 16-bit space (`0x1_0000`). Mirrors the pair SG1's
    /// `coverage_stats()` returns so the host can render a percentage.
    pub fn coverage_stats(&self, cpu_id: usize) -> (u64, u64) {
        match cpu_id {
            0 => (0x0100_0000, u64::from(self.debugger.coverage_count_65816())),
            1 => (
                0x0001_0000,
                u64::from(self.debugger.coverage_count_spc700()),
            ),
            _ => (0, 0),
        }
    }

    /// Write `data` to the 65C816 address space starting at `addr` for
    /// debugging. This drives the live bus, so writes to register space take
    /// effect with their normal side effects (matching what a CPU store would
    /// do); plain WRAM/SRAM edits behave as the memory editor expects.
    /// [`EmulatorCore::read_memory_debug`] reads through the same bus.
    pub fn write_memory_debug(&mut self, addr: u32, data: &[u8]) {
        for (i, &byte) in data.iter().enumerate() {
            self.bus.write(addr.wrapping_add(i as u32), byte);
        }
    }

    /// Run one 65C816 instruction, returning the master clocks it
    /// consumed (the sum of each memory access's 6/8/12 mclk cost plus
    /// internal cycles at the 6 mclk fast rate, plus any general-DMA
    /// stall).
    fn step_cpu_internal(&mut self) -> u64 {
        // Mirror the level-triggered IRQ line into the CPU before the
        // instruction. The scheduler's `Irq*` handlers raise the line;
        // a `$4211` (TIMEUP) read inside the previous instruction
        // lowered it again. The CPU's own `I` flag decides whether a
        // raised line is actually serviced. A burst-CPU coprocessor (the
        // SA-1, the Super FX or the Cx4) ORs its own S-CPU-directed IRQ
        // line in on top, gated so only such a cart pays for the call.
        self.cpu.irq_pending = self.bus.system_regs().irq_line();
        if self.bus.cart_runs_burst_cpu() {
            self.cpu.irq_pending |= self.bus.cart_has_irq();
        }

        // VBlank NMI delivery is decided *after* the instruction runs (see
        // below), so a `$4210` poll the instruction performs is observed
        // before the NMI is vectored.

        // Diagnostic traces: stamp the PC of the instruction about to
        // run so any watched synchronisation-register access it makes is
        // attributed to it, and feed the PC ring buffer. Gated on a
        // trace being enabled, so the fast path never computes or stores
        // it.
        if self.bus.diag_trace_enabled() {
            let pc24 = ((self.cpu.pbr as u32) << 16) | self.cpu.pc as u32;
            self.bus.set_trace_pc(pc24);
        }

        // Charge the instruction its real per-access master-cycle cost.
        // The bus accumulates 6/8/12 mclk for each memory access the CPU
        // makes (via `cycles_for`); the remaining `cpu_cycles - accesses`
        // are internal (non-bus) cycles, each 6 mclk on hardware. A flat
        // `cpu_cycles * 6` would assume every cycle is a FastROM access
        // and run the CPU ~6-7% too fast in master-cycle terms, enough to
        // put the 5A22 and the SPC700 in the wrong relative phase and
        // desync the `$2140` mailbox handshake. `saturating_sub`
        // guards the unexpected case of fewer reported cycles than
        // accesses.
        //
        // A `$4211` read inside the instruction sees an IRQ that rises
        // before it, so the bus is told where the next one falls.
        self.bus
            .set_next_irq_cycle(self.irq_cycles.front().copied());
        self.bus.begin_cpu_instruction();
        let cpu_cycles = self.cpu.step(&mut self.bus);
        let (access_mclks, accesses) = self.bus.take_cpu_instruction_cost();
        if let Some(at) = self.bus.take_irq_read_early() {
            self.irq_read_early = Some(at);
        }
        let internal = (cpu_cycles as u64).saturating_sub(accesses as u64);
        let mut delta = access_mclks + internal * 6;
        // General DMA is deferred to the instruction boundary: a `$420B`
        // write only flagged a pending channel mask. Run the whole block
        // now and fold the stolen master-cycles into this step's cost so
        // the scheduler timeline and the frame budget both observe the
        // CPU stall.
        let dma_mask = self.bus.take_pending_general_dma();
        if dma_mask != 0 {
            delta += self.bus.run_general_dma(dma_mask);
            // A general-DMA block just ran: match a `DmaStart` event
            // breakpoint (only armed when one exists).
            if self.debug_armed && self.debugger.check_event(SnesEvent::DmaStart) {
                self.pending_break = true;
            }
        }
        // A `$4200` NMI-enable 0->1 edge while the `$4210` flag is still
        // latched re-triggers NMI. The bus flagged it because it cannot
        // reach the CPU from inside the store that set it.
        if self.bus.take_pending_nmi() {
            self.cpu.nmi_pending = true;
        }
        // VBlank NMI delivery (poll-aware): once armed at `VBlankStart`,
        // hold off vectoring until the main thread has *observed* the
        // `$4210` flag via a read, or the fallback window expires. This
        // lets a `lda $4210 / bpl` vblank-wait see bit 7 before the
        // handler's own `$4210` ack clears it, regardless of which
        // instruction was in flight when VBlank began. The handler
        // preserves A/X/Y, so vectoring right after the poll's read is
        // correct, its `BPL` tests the restored accumulator and exits.
        if self.nmi_deliver_armed {
            let observed = self.bus.system_regs_mut().take_nmi_flag_observed();
            self.nmi_deliver_countdown = self.nmi_deliver_countdown.saturating_sub(1);
            if observed || self.nmi_deliver_countdown == 0 {
                self.cpu.nmi_pending = true;
                self.nmi_deliver_armed = false;
            }
        }
        // A `$4200`/`$4207-$420A` write changed the IRQ configuration:
        // drop the stale H/V IRQ events and re-arm them from the current
        // frame base, keeping only the still-future triggers.
        if self.bus.take_pending_irq_reschedule() {
            self.reschedule_irq_events();
        }
        self.master_cycles += delta;
        // SystemRegs::tick takes u32 and saturates internally to 255,
        // a single instruction's master-cycle cost never approaches u32
        // anyway.
        self.bus
            .system_regs_mut()
            .tick(delta.min(u32::MAX as u64) as u32);
        // Refresh the bus's raster snapshot so the next PPU access
        // (OAM/CGRAM active-scan gate, $2137 SLHV latch, WRIO
        // falling-edge latch) sees an up-to-1-instruction-stale
        // `(h_dot, v_scanline)`. Decoded against `frame_base_cycle`
        // (see `raster_position`) so the `$213D` value a game latches
        // agrees with the line its scheduled NMI/IRQ events fire on;
        // the bus expects the pair in `(h, v)` order.
        let (v, h) = self.raster_position(self.master_cycles);
        self.bus.set_raster_snapshot(h, v);
        // The APU is caught up lazily on a mailbox access; hand it the
        // instruction boundary it should advance to, mirroring the
        // raster snapshot above. Staleness is bounded by one
        // instruction.
        self.bus.set_apu_cycle(self.master_cycles);
        // A burst-driven coprocessor CPU (the SA-1's second 65816, the Super FX's
        // GSU or the Cx4) must advance every instruction, not just at `FrameEnd`,
        // so it keeps step with the S-CPU across code that never touches shared
        // memory. Gated so plain / DSP-1 / S-DD1 / SPC7110 carts keep their exact
        // current cadence. The chip's S-CPU-directed IRQ line is ORed into
        // `cpu.irq_pending` at the head of the next instruction (see
        // `step_cpu_internal`).
        if self.bus.cart_runs_burst_cpu() {
            self.bus.catch_up_coprocessor(self.master_cycles);
        }
        delta
    }

    /// Decompose a master cycle into the `(v_scanline, h_dot)` raster
    /// position **relative to the current frame base**.
    ///
    /// The scheduler arms every per-frame event, `VBlankStart`/NMI,
    /// `RenderPos`, `HBlankStart`, the H/V IRQ triggers, at
    /// `frame_base_cycle + line * cycles_per_scanline + dot * 4`. The
    /// CPU-visible raster counters (`$213D` OPVCT via the `$2137` SLHV
    /// latch, plus the trace V/H columns) MUST be decoded against the
    /// same `frame_base_cycle` anchor. `frame_base_cycle` re-anchors to
    /// the live `master_cycles` at each frame boundary and is *not* a
    /// multiple of `cycles_per_frame`, it carries the overshoot of the
    /// last instruction or DMA stall that crossed the boundary. Decoding
    /// the vcounter from an absolute `master_cycles % cycles_per_frame`
    /// grid therefore drifts away from the event timeline by that
    /// accumulated overshoot, so the scanline a game reads from `$213D`
    /// disagrees with the line its NMI/IRQ actually fired on. A
    /// vblank-paced raster routine (e.g. Chrono Trigger's per-scanline
    /// queue walk) then sees the wrong vcounter, takes the wrong branch,
    /// and the `RenderPos` handler writes the framebuffer at the wrong
    /// row. Anchoring here keeps the readout and the event timeline on
    /// one clock, as on the hardware, where both come from one counter.
    #[inline]
    fn raster_position(&self, master_cycle: u64) -> (u16, u16) {
        let cps = self.timing.cycles_per_scanline as u64;
        let scanlines = self.timing.scanlines_per_frame as u64;
        let rel = master_cycle.saturating_sub(self.frame_base_cycle);
        let v = ((rel / cps) % scanlines) as u16;
        let h = ((rel % cps) / 4) as u16;
        (v, h)
    }

    /// Test-only convenience wrapper that dispatches an event at the live
    /// `master_cycles`. The frame loops call [`Self::handle_event_at`]
    /// directly with each event's true scheduled `at_cycle`; unit tests use
    /// this shorthand when the scheduled cycle is irrelevant to the case.
    #[cfg(test)]
    fn handle_event(&mut self, kind: EventKind) {
        self.handle_event_at(self.master_cycles, kind);
    }

    /// Per-event dispatcher. Each variant drives the subsystem that
    /// owns it (PPU rendering, the HVBJOY flags, the DMA/HDMA engine,
    /// NMI/IRQ delivery, the APU catch-up):
    ///
    /// - `HBlankStart`: raise the HVBJOY HBlank flag ($4212 bit 6) at
    ///   dot 274; `RenderPos` lowers it when the active display resumes.
    /// - `RenderPos`: render the current scanline. The scheduler
    ///   fires this at dot 22 of lines 1..=240; the PPU drops the
    ///   lines past the active region.
    /// - `VBlankStart`: raise NMI through SystemRegs when NMITIMEN
    ///   bit 7 is set; flip the HVBJOY VBlank flag; sample the
    ///   controllers and run the auto-joypad read when NMITIMEN bit 0
    ///   is set (scheduling `AutoJoypadDone` to clear the busy flag).
    /// - `VBlankEnd`: clear the RDNMI/HVBJOY VBlank flags at the top of
    ///   the frame (V:0), not on the last scanline, so a poll that waits
    ///   for VBlank to end sees the hardware-correct edge.
    /// - `AutoJoypadDone`: lower the HVBJOY auto-joypad busy flag.
    /// - `HdmaInit`: reload every HDMA-enabled channel's
    ///   table pointer + line counter at the top of the frame.
    /// - `HdmaTrigger`: drive a single line of HDMA per
    ///   active channel through the DMA engine.
    /// - `IrqV / IrqH / IrqHV`: raise the 65C816's IRQ line. The events
    ///   are armed for each frame and re-armed after an IRQ register
    ///   write, not here.
    /// - `FrameEnd`: catch the APU up to the frame boundary so the
    ///   SPC700 and sample clock advance even across a frame with no
    ///   mailbox traffic. (`ApuCatchup` is defined but unscheduled,
    ///   port access plus this hook already bound staleness to one
    ///   frame.)
    ///
    /// Raster-dependent behaviour (`RenderPos` scanline, `HdmaInit` /
    /// `HdmaTrigger` active-scan gate snapshot) is decoded from `at_cycle`,
    /// the cycle the event was scheduled for, not the live
    /// `master_cycles`. The two agree to within one instruction on the hot
    /// path; they diverge only when a general DMA (or any single step) folds
    /// many scanlines into one jump, deferring a batch of events. Cycle
    /// *accounting* (HDMA stolen cycles, APU/IRQ bookkeeping) still uses
    /// `master_cycles`: only the raster attribution reads `at_cycle`.
    fn handle_event_at(&mut self, at_cycle: u64, kind: EventKind) {
        match kind {
            EventKind::RenderPos => {
                // The line that owns this RenderPos, decoded against
                // `frame_base_cycle` (the same anchor the event was
                // armed on) from the event's scheduled cycle, so a
                // RenderPos deferred behind a long mid-screen DMA still
                // renders its own scanline instead of the late line the
                // post-DMA `master_cycles` points at.
                // The PPU itself drops any out-of-range scanline via
                // `Ppu::render_scanline`'s in_active_scanline gate,
                // overscan toggles flip the gate without a reschedule.
                let (scanline, _h_dot) = self.raster_position(at_cycle);
                // RenderPos lands at dot 22, the active display has
                // resumed, so the line is no longer in horizontal blank.
                self.bus.system_regs_mut().set_hblank(false);
                self.bus
                    .ppu_mut()
                    .render_scanline(scanline, &mut self.framebuffer);
                if self.debug_armed && self.debugger.check_event(SnesEvent::Scanline(scanline)) {
                    self.pending_break = true;
                }
            }
            EventKind::HBlankStart => {
                // Dot 274: raise the HVBJOY HBlank flag ($4212 bit 6).
                // Cleared at the next RenderPos (active display resumes).
                // Games that gate work on HBlank (Chrono Trigger spins
                // on `lda $4212 / bit #$40 / beq` during its NMI handler)
                // depend on this edge to make progress.
                self.bus.system_regs_mut().set_hblank(true);
            }
            EventKind::HdmaInit => {
                // HDMA runs at a fixed raster dot (init at dot 6, per-line
                // at dot 276, both in H-blank), independent of where the
                // CPU last updated the cached raster snapshot. Re-anchor it
                // from the event's scheduled cycle so any PPU-register write
                // the transfer performs gates its active-scan CGRAM/OAM
                // redirect against the true H dot, even when a long general
                // DMA deferred this trigger and the live `master_cycles`
                // has since walked deep into active display. Without this a
                // transfer firing while the snapshot sits inside dots
                // 22..274 is wrongly redirected to the internal address
                // (colour 0), which would corrupt Yoshi's Island's per-line
                // water palette into the backdrop.
                let (v, h) = self.raster_position(at_cycle);
                self.bus.set_raster_snapshot(h, v);
                // Top-of-frame HDMA setup: reload table pointers and
                // line counters for every HDMA-enabled channel. The
                // stolen cycles fold into the frame budget like a CPU
                // stall.
                let stolen = self.bus.hdma_init_frame();
                self.master_cycles += stolen;
                self.bus
                    .system_regs_mut()
                    .tick(stolen.min(u32::MAX as u64) as u32);
                if self.debug_armed && self.debugger.check_event(SnesEvent::HdmaStart) {
                    self.pending_break = true;
                }
            }
            EventKind::HdmaTrigger => {
                // Re-anchor the raster snapshot to this trigger's own
                // scheduled dot (276, in H-blank) so per-line HDMA writes to
                // CGRAM/OAM gate the active-scan redirect against the real H
                // dot rather than the live `master_cycles`, which a long
                // general DMA can push a batch of deferred triggers deep into
                // active display. See the HdmaInit arm above for the Yoshi's
                // Island failure this cures.
                let (v, h) = self.raster_position(at_cycle);
                self.bus.set_raster_snapshot(h, v);
                // Per-scanline HDMA: one transfer pattern per active
                // channel, then advance the line counters.
                let stolen = self.bus.hdma_run_line();
                self.master_cycles += stolen;
                self.bus
                    .system_regs_mut()
                    .tick(stolen.min(u32::MAX as u64) as u32);
            }
            EventKind::VBlankStart => {
                // Interlace field toggle, alternates once per frame
                // even when the interlace bit itself is off, which
                // mirrors how real silicon drives `$213F` bit 7.
                self.bus.ppu_mut().toggle_field();
                // Entering VBlank re-anchors the OAM address pointer to
                // its base (a side effect of sprite eval finishing on
                // real hardware) when the display is active.
                self.bus.ppu_mut().oam_reset_at_vblank();
                // Normalize the frame for host display: when any line
                // was hires the reported width is 512, so lo-res lines
                // (HDMA mid-frame mode sweeps) are doubled in place to
                // fill the full width. Must run before the display-size
                // sync below so the framebuffer and `DisplayInfo` agree.
                // Purely a framebuffer post-process (no serialized state),
                // so a headless frame skips it.
                if self.present {
                    self.bus.ppu().finalize_frame(&mut self.framebuffer);
                }
                // Sync the cached `DisplayInfo` to the live PPU
                // dimensions at the natural frame boundary so the
                // next host poll of `get_display_info` reflects any
                // hires / overscan toggles that happened during the
                // frame just ended.
                self.sync_display_info();
                // Latch the NMI flag ($4210 bit 7) regardless of the
                // enable bit and raise the HVBJOY VBlank flag. The CPU
                // NMI signal itself is gated by NMITIMEN bit 7.
                let regs = self.bus.system_regs_mut();
                regs.set_nmi_flag(true);
                regs.set_vblank(true);
                let nmi_enabled = regs.nmi_enabled();
                if nmi_enabled {
                    // Arm poll-aware delivery rather than firing the NMI
                    // straight away. The flag is already latched above with
                    // `nmi_flag_observed` reset, so the next main-thread
                    // `$4210` read this vblank is what releases the NMI (or
                    // the fallback window expires). See `nmi_deliver_armed`.
                    self.nmi_deliver_armed = true;
                    self.nmi_deliver_countdown = NMI_DELIVERY_MAX_INSNS;
                }
                // Event breakpoints: entering V-blank, and the NMI when
                // it's actually delivered. Only checked when armed.
                if self.debug_armed {
                    if self.debugger.check_event(SnesEvent::VBlank) {
                        self.pending_break = true;
                    }
                    if nmi_enabled && self.debugger.check_event(SnesEvent::Nmi) {
                        self.pending_break = true;
                    }
                }
                // Sample the controllers into the pads here, at the
                // start of VBlank, where the hardware's auto-read does.
                // This keeps manual-serial games fresh too, not just
                // auto-read.
                self.refresh_controllers_from_input();
                // Auto-joypad read (NMITIMEN bit 0): latch both pads
                // into JOY1/JOY2 and hold the busy flag for the read
                // window via a scheduled AutoJoypadDone.
                if self.bus.system_regs().nmitimen() & 0x01 != 0 {
                    self.bus.auto_joypad_read();
                    self.scheduler.schedule(
                        self.master_cycles + AUTO_JOYPAD_CYCLES,
                        EventKind::AutoJoypadDone,
                    );
                }
            }
            EventKind::VBlankEnd => {
                // Clears the RDNMI ($4210 bit 7) NMI flag latch and the
                // HVBJOY ($4212 bit 7) VBlank flag. Armed at the top of
                // the frame (line 0 / V:0), so it clears the flags that
                // the *previous* frame's VBlankStart raised: they stay
                // set through the last scanline and drop only when the
                // V-counter wraps to 0, matching hardware. (Clearing on
                // the last scanline instead would drop the VBlank flag one
                // line early and break the H/V-counter-latch sub-test of
                // the Electronics Test in Nintendo's SNES Test Program,
                // which waits for VBlank to end and then expects to latch
                // V=0.)
                let regs = self.bus.system_regs_mut();
                regs.set_nmi_flag(false);
                regs.set_vblank(false);
            }
            EventKind::IrqV { .. } | EventKind::IrqH { .. } | EventKind::IrqHV { .. } => {
                while self.irq_cycles.front().is_some_and(|&at| at <= at_cycle) {
                    self.irq_cycles.pop_front();
                }
                // Raise the level-triggered IRQ line. `step_cpu_internal`
                // mirrors it into the CPU each step; the handler acks by
                // reading $4211 (TIMEUP), which lowers the line again.
                //
                // A `$4211` read inside the instruction during which this
                // IRQ rose has seen and acknowledged it already. The 5A22
                // latched the interrupt before that read lowered the line,
                // so the line still goes up when the I flag lets the CPU
                // take it (the handler then reads bit 7 set, where the chip
                // would give it clear). With I set the read was all the IRQ
                // left, and a later read must not see it a second time.
                let seen_early = self.irq_read_early == Some(at_cycle);
                if seen_early {
                    self.irq_read_early = None;
                }
                if !seen_early || (self.cpu.p & FLAG_I) == 0 {
                    self.bus.system_regs_mut().set_irq_flag(true);
                }
                if self.debug_armed && self.debugger.check_event(SnesEvent::Irq) {
                    self.pending_break = true;
                }
            }
            EventKind::AutoJoypadDone => {
                // The auto-joypad read window elapsed: drop the HVBJOY
                // busy flag ($4212 bit 0) so a polling game proceeds.
                self.bus.auto_joypad_finish();
            }
            EventKind::FrameEnd => {
                // Bound APU staleness to one frame even when no mailbox
                // access happened this frame: advance the SPC700 and the
                // 32 kHz sample clock to the frame boundary, synthesising
                // any S-DSP samples that came due. Then pull those 32 kHz
                // samples through the resampler into the host audio ring.
                self.bus.apu_mut().catch_up_to(self.master_cycles);
                // The APU catch-up above advances all serialized audio
                // state; only the host-ring drain (resample) is
                // presentation, so a headless frame skips just that.
                if self.present {
                    self.drain_apu_to_host_audio();
                }
                // Keep a cartridge coprocessor (DSP-1, ...) ticking across
                // a frame with no register traffic, bounding its catch-up
                // staleness to one frame just like the APU above.
                self.bus.catch_up_coprocessor(self.master_cycles);
            }
            _ => {}
        }
    }

    /// Pull every queued 32 kHz `i16` sample out of the APU, resample it
    /// to the 44.1 kHz host rate, and push the `f32` result into the host
    /// audio ring. Called once per frame after the APU catch-up.
    ///
    /// Skipped entirely when audio is disabled; the APU's own 32 kHz ring
    /// then fills and bounds-overflows its oldest samples (benign), and
    /// resampling resumes from the live ring once audio is re-enabled.
    fn drain_apu_to_host_audio(&mut self) {
        if !self.audio_enabled {
            return;
        }
        let available = self.bus.apu_mut().audio_samples_available();
        if available == 0 {
            return;
        }
        self.apu_drain_buf.resize(available, 0);
        let drained = self.bus.apu_mut().drain_audio(&mut self.apu_drain_buf);
        self.audio_resample_buf.clear();
        self.resampler
            .resample_into(&self.apu_drain_buf[..drained], &mut self.audio_resample_buf);
        // The host ring is bounded; if the host is far behind, `push_slice`
        // drops the oldest samples and bumps `overflow_count`, surfaced via
        // `audio_overflows()`.
        self.audio_ring.push_slice(&self.audio_resample_buf);
    }

    /// Cancel the pending H/V IRQ events and re-arm them from the
    /// current frame base, keeping only triggers at or after the
    /// current master cycle (so a mid-frame HTIME / VTIME / NMITIMEN
    /// rewrite never re-fires a trigger for a scanline already drawn).
    fn reschedule_irq_events(&mut self) {
        self.scheduler.cancel_matching(EventKind::is_irq);
        self.arm_irq_events(self.frame_base_cycle, self.master_cycles);
    }

    /// Push the H/V IRQ events for a frame into the scheduler from the
    /// live NMITIMEN / HTIME / VTIME register state. `min_cycle`
    /// filters out triggers whose raster point has already passed.
    fn arm_irq_events(&mut self, base_cycle: u64, min_cycle: u64) {
        let (nmitimen, htime, vtime) = {
            let regs = self.bus.system_regs();
            (regs.nmitimen(), regs.htime(), regs.vtime())
        };
        schedule_irq_events(
            &mut self.scheduler,
            base_cycle,
            nmitimen,
            htime,
            vtime,
            self.timing,
            min_cycle,
        );
        self.collect_irq_cycles();
    }

    /// Rebuild [`Self::irq_cycles`] from the H/V IRQ events the
    /// scheduler holds.
    fn collect_irq_cycles(&mut self) {
        self.irq_cycles.clear();
        self.irq_cycles
            .extend(self.scheduler.cycles_of(EventKind::is_irq));
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
    /// (width, height) based on the bgmode + overscan bit; we
    /// mirror that into [`Self::system_info`] so the trait-bound
    /// `get_display_info(&self)` stays accurate without needing
    /// interior mutability. Called at VBlankStart, on `reset`, and
    /// inside `load_rom`.
    fn sync_display_info(&mut self) {
        let (w, h) = self.bus.ppu().active_display_size();
        self.system_info.display.width = w;
        self.system_info.display.height = h;
        // The refresh rate follows the timing profile, which `load_rom`
        // (header destination), `set_region_str` (netplay handshake) and a
        // savestate restore can all change, every one of them ends here.
        self.system_info.display.refresh_rate = self.timing.refresh_rate();
    }

    /// Run one frame with no debug instrumentation (the `Fast` tier).
    /// This is the hot path when the debugger panel is idle.
    fn run_frame_fast(&mut self) {
        let frame_start = self.master_cycles;
        self.frame_base_cycle = frame_start;
        let frame_end = frame_start + self.timing.cycles_per_frame();

        while self.master_cycles < frame_end {
            // Run CPU instructions until either (a) the next
            // scheduler event fires or (b) we've used the whole
            // frame budget. `target` is the lesser of the two.
            let next_event = self.scheduler.peek_cycle().unwrap_or(frame_end);
            let target = next_event.min(frame_end);

            while self.master_cycles < target {
                self.step_cpu_internal();
            }

            let events = self.scheduler.drain_up_to_with_cycles(self.master_cycles);
            for (at_cycle, ev) in events {
                self.handle_event_at(at_cycle, ev);
            }
        }

        self.end_of_frame_bookkeeping();
    }

    /// Headless frame for netplay rollback / catch-up: the full
    /// deterministic simulation with the host-facing presentation steps
    /// (framebuffer post-process + 32 kHz -> 44.1 kHz audio drain)
    /// suppressed. Runs the same `Fast`-tier loop as
    /// [`Self::run_frame_fast`], including the per-scanline PPU render
    /// (which updates the serialized sprite range/time-over flags), the
    /// interlace field toggle, the OAM re-anchor, and the APU + coprocessor
    /// catch-up, so it produces the exact same machine state (and
    /// `frame_count`) as a presented frame. Re-simulating a mispredicted
    /// span therefore leaves the emulator byte-identical to a live run,
    /// which [`Self::state_hash`] verifies.
    pub fn run_frame_headless(&mut self) {
        self.present = false;
        self.run_frame_fast();
        self.present = true;
    }

    /// Run one frame with per-instruction debug hooks (the `LightDebug`
    /// tier). Before each 65C816 instruction we check execute breakpoints,
    /// record coverage, and, when 65C816 tracing is enabled, capture a
    /// binary trace entry. `full` additionally refreshes the cached
    /// CPU-state snapshots; the tier dispatch passes `false`.
    ///
    /// A breakpoint hit flips the execution mode to `Paused` and returns
    /// mid-frame; the partial scheduler progress is drained on the next
    /// `run_frame` once the user resumes.
    fn run_frame_instrumented(&mut self, full: bool) {
        let frame_start = self.master_cycles;
        self.frame_base_cycle = frame_start;
        let frame_end = frame_start + self.timing.cycles_per_frame();
        self.pending_break = false;

        while self.master_cycles < frame_end {
            let next_event = self.scheduler.peek_cycle().unwrap_or(frame_end);
            let target = next_event.min(frame_end);

            while self.master_cycles < target {
                let pc24 = ((self.cpu.pbr as u32) << 16) | self.cpu.pc as u32;
                if self.debugger.check_execute_65816(pc24) {
                    self.finish_paused_frame(full);
                    return;
                }
                self.debugger.record_coverage_65816(pc24);
                if self.debugger.is_tracing_enabled(0) {
                    let entry = self.capture_trace_65816();
                    self.debugger.record_trace_65816(&entry);
                }
                self.maintain_call_stack_65816(pc24);
                // Discard the bus reads the debug capture above performed
                // so the watchpoint scan only sees this instruction's own
                // CPU accesses.
                if self.debug_armed {
                    self.bus.clear_debug_events();
                }
                self.step_cpu_internal();
                // Watchpoints + PPU/APU write events (recorded by the bus),
                // DSP-register events (recorded by the APU during a mailbox
                // catch-up), and the inline DMA-start event all funnel into
                // a single pause check. Bitwise-or so every match bumps its
                // hit count before we bail.
                if self.debug_armed
                    && (self.drain_bus_debug_events()
                        | self.drain_apu_dsp_events()
                        | self.pending_break)
                {
                    self.finish_paused_frame(full);
                    return;
                }
            }

            let events = self.scheduler.drain_up_to_with_cycles(self.master_cycles);
            for (at_cycle, ev) in events {
                self.handle_event_at(at_cycle, ev);
            }
            // Inline events (VBlank/NMI/IRQ/HDMA/scanline set `pending_break`)
            // and the FrameEnd APU catch-up's DSP-register writes are checked
            // after the event batch drains.
            if self.debug_armed && (self.pending_break | self.drain_apu_dsp_events()) {
                self.finish_paused_frame(full);
                return;
            }
        }

        self.end_of_frame_bookkeeping();
        self.drain_spc_trace();
        self.drain_gsu_trace();
        if full {
            self.update_debugger_state();
        }
    }

    /// The `FullDebug` tier. Reached only when the execution mode is not
    /// `Running` (paused or stepping). `Paused` just refreshes the cached
    /// state; the step modes advance one 65C816 instruction and pause.
    /// Step-over and step-out are not call-stack-aware: all three step
    /// modes single-step.
    fn run_frame_full(&mut self) {
        if self.debugger.get_execution_mode() == ExecutionMode::Paused {
            self.update_debugger_state();
            return;
        }
        self.debug_single_step_65816();
        self.debugger.set_execution_mode(ExecutionMode::Paused);
    }

    /// Advance exactly one 65C816 instruction with full instrumentation
    /// (coverage + trace + state refresh), draining any scheduler events
    /// that fall at the new master-cycle boundary so the PPU stays
    /// consistent. Used by the step modes and `EmulatorCore::step_cpu`.
    fn debug_single_step_65816(&mut self) {
        let pc24 = ((self.cpu.pbr as u32) << 16) | self.cpu.pc as u32;
        self.debugger.record_coverage_65816(pc24);
        if self.debugger.is_tracing_enabled(0) {
            let entry = self.capture_trace_65816();
            self.debugger.record_trace_65816(&entry);
        }
        self.maintain_call_stack_65816(pc24);
        self.record_history_65816(pc24);
        // Drop the bus reads the capture above performed before the real
        // step so the watchpoint scan sees only this instruction.
        if self.debug_armed {
            self.bus.clear_debug_events();
        }
        self.step_cpu_internal();
        self.drain_events_and_maybe_close_frame();
        // A single step always pauses, so the hit result isn't acted on,
        // but the recorders are still drained to keep watchpoint/event hit
        // counts accurate and to clear the buffers for the next step.
        if self.debug_armed {
            let _ = self.drain_bus_debug_events();
            let _ = self.drain_apu_dsp_events();
            self.pending_break = false;
        }
        self.drain_spc_trace();
        self.drain_gsu_trace();
        self.update_debugger_state();
    }

    /// Drain every scheduler event due at the current master cycle and,
    /// when the instruction just executed carried the clock past the frame
    /// boundary, run the end-of-frame tail and re-anchor the frame base.
    ///
    /// Single-stepping shares this with the frame loop so that one stepped
    /// instruction stays event-consistent: the PPU renders, the HVBJOY
    /// flags toggle, NMI/IRQ/HDMA fire. The frame-boundary close is the
    /// load-bearing part, `schedule_frame_events` only arms the *current*
    /// frame, so without re-running it at the boundary a stepped timeline
    /// would fall silent after frame 0 (no further VBlank/NMI) and an
    /// instruction-bounded trace would drift away from a
    /// continuously-running reference.
    fn drain_events_and_maybe_close_frame(&mut self) {
        let events = self.scheduler.drain_up_to_with_cycles(self.master_cycles);
        for (at_cycle, ev) in events {
            self.handle_event_at(at_cycle, ev);
        }
        // A general-DMA stall can fold many master cycles into a single
        // instruction. Like `run_frame`, close at most one frame per step
        // and re-anchor the base at the overshoot; the next step absorbs
        // any remaining slack.
        if self.master_cycles >= self.frame_base_cycle + self.timing.cycles_per_frame() {
            self.end_of_frame_bookkeeping();
            self.frame_base_cycle = self.master_cycles;
        }
    }

    /// Per-frame tail shared by every tier: advance the frame counter,
    /// pre-schedule the next frame's events, and re-arm the H/V IRQ
    /// triggers anchored at the current master cycle (which carries the
    /// small overshoot from this frame's last instruction).
    fn end_of_frame_bookkeeping(&mut self) {
        self.frame_count += 1;
        // Read the live overscan bit so the upcoming frame's VBlank/NMI
        // and HDMA range land on the right scanline (240 when overscan
        // is on, else the region default). Games set overscan during
        // VBlank and leave it stable, so the value is settled here.
        let overscan = self.bus.ppu().overscan();
        schedule_frame_events(&mut self.scheduler, self.master_cycles, overscan);
        self.arm_irq_events(self.master_cycles, self.master_cycles);
    }

    /// Snapshot the live 65C816 state into a binary trace entry, resolving
    /// the effective-address bracket against the bus (indirect modes need
    /// live memory). Called **before** the instruction executes, matching
    /// bsnes-plus, so the registers and raster position describe the state
    /// the instruction sees.
    fn capture_trace_65816(&mut self) -> TraceEntryBinary65816 {
        let pbr = self.cpu.pbr;
        let pc = self.cpu.pc;
        let pbr_pc = ((pbr as u32) << 16) | pc as u32;
        let p = self.cpu.p;
        let emulation = self.cpu.emulation;
        // Operand width follows the M (memory/accumulator) and X (index)
        // flags; in emulation mode both are forced 8-bit.
        let m8 = emulation || (p & 0x20) != 0;
        let x8 = emulation || (p & 0x10) != 0;

        let opcode = self.bus.read(pbr_pc);
        let info = &wdc65816::decode::OPCODE_TABLE[opcode as usize];
        let size = (1 + info.mode.operand_size(m8, x8)).min(4);

        let mut bytes = [0u8; 4];
        bytes[0] = opcode;
        for i in 1..size {
            // Operand bytes are fetched within the program bank, wrapping
            // the 16-bit PC, exactly as the CPU's own fetch does.
            let addr = ((pbr as u32) << 16) | (pc.wrapping_add(i as u16) as u32);
            bytes[i as usize] = self.bus.read(addr);
        }

        let regs = EffRegs {
            d: self.cpu.d,
            x: self.cpu.x,
            y: self.cpu.y,
            s: self.cpu.sp,
            db: self.cpu.dbr,
            pbr_pc,
        };
        let eff = effective_address(&mut self.bus, info.mode, &bytes[..size as usize], &regs);

        // V / H are derived from the live master cycle, decoded against
        // `frame_base_cycle` (see `raster_position`) so the trace column
        // matches the `$213D` value the running code observes and the
        // line its NMI/IRQ events fire on. H is in PPU dot units (4
        // master clocks per dot), which is what the trace format's `H`
        // column holds.
        let (vcounter, hdot) = self.raster_position(self.master_cycles);

        TraceEntryBinary65816 {
            master_cycle: self.master_cycles,
            pbr_pc,
            a: self.cpu.c,
            x: self.cpu.x,
            y: self.cpu.y,
            s: self.cpu.sp,
            d: self.cpu.d,
            vcounter,
            hdot,
            framecounter: self.frame_count as u32,
            eff_addr: eff.unwrap_or(0),
            dbr: self.cpu.dbr,
            p,
            e: emulation as u8,
            eff_present: eff.is_some() as u8,
            bytes_len: size,
            cycles: info.base_cycles,
            bytes,
        }
    }

    /// Set the debugger's execution mode. A pause also refreshes the register
    /// snapshots the debugger reports: a game running with no breakpoint and no
    /// trace runs the fast loop, which never refreshes them, so without it a
    /// pause would report the state of the last reset or step.
    pub fn set_execution_mode(&mut self, mode: ExecutionMode) {
        self.debugger.set_execution_mode(mode);
        if mode == ExecutionMode::Paused {
            self.update_debugger_state();
        }
    }

    /// Refresh the debugger's cached register snapshots for every CPU
    /// (65C816 from the main CPU, SPC700 from the APU, and, on an SA-1, Super FX
    /// or Cx4 cart, the chip's CPU as slot 2).
    fn update_debugger_state(&mut self) {
        let s0 = cpu_state_65816(&self.cpu);
        let s1 = cpu_state_spc700(self.bus.apu().spc700());
        let mut states = vec![s0, s1];
        // Each chip's snapshot stamps `cpu_id = 0` (the SA-1's comes from the
        // shared 65816 helper); rewrite it to the real slot index (2).
        if let Some(mut s2) = self
            .bus
            .cartridge_ref()
            .and_then(|c| c.secondary_cpu_state())
        {
            s2.cpu_id = 2;
            states.push(s2);
        }
        self.debugger.set_cpu_states(states);
    }

    /// Mirror the debugger's SPC700 (cpu 1) tracing flag into the APU so
    /// [`crate::apu::Apu::catch_up_to`] captures a trace entry before each
    /// SPC700 step. The APU runs via lazy catch-up deep inside mailbox
    /// accesses where the debugger isn't reachable, so this flag is the
    /// bridge, kept fresh at every debug entry point.
    fn sync_spc_trace_enable(&mut self) {
        let on = self.debugger.is_tracing_enabled(1);
        self.bus.apu_mut().set_spc_trace_enabled(on);
    }

    /// Move the SPC700 trace entries the APU captured this frame into the
    /// debugger's cpu-1 tracing buffer, preserving SPC execution order.
    /// Draining empties the APU buffer for the next catch-up window.
    fn drain_spc_trace(&mut self) {
        let entries = self.bus.apu_mut().take_spc_trace();
        for e in &entries {
            self.debugger.record_trace_spc700(e);
        }
    }

    /// Mirror the debugger's cpu-2 tracing flag into the cartridge so the
    /// slot-2 chip's `catch_up` burst captures a trace entry before each step
    /// of its CPU, the Super FX's GSU or the Cx4. That CPU runs deep inside
    /// the chip (mailbox catch-up + the per-instruction burst hook) where the
    /// debugger isn't reachable, so this flag is the bridge, kept fresh at
    /// every debug entry point (a no-op on any other cart).
    fn sync_gsu_trace_enable(&mut self) {
        let on = self.debugger.is_tracing_enabled(2);
        if let Some(cart) = self.bus.cartridge_mut() {
            cart.set_secondary_trace_enabled(on);
        }
    }

    /// Move the secondary-CPU trace entries the coprocessor captured this frame
    /// into the debugger's cpu-2 tracing buffer, preserving execution order.
    /// Draining empties the chip's buffer for the next catch-up window. The
    /// entry format is chip-specific (Super FX GSU vs Cx4 HG51B169), so the
    /// installed preset picks which ring parses the bytes.
    fn drain_gsu_trace(&mut self) {
        let raw = self
            .bus
            .cartridge_mut()
            .map(|c| c.take_secondary_trace())
            .unwrap_or_default();
        if raw.is_empty() {
            return;
        }
        if self.preset_kind == Some(PresetKind::Cx4) {
            self.debugger.record_trace_cx4_bytes(&raw);
        } else {
            self.debugger.record_trace_gsu_bytes(&raw);
        }
    }

    /// Mirror the debugger's "has watch/event breakpoints" state into the
    /// emulator flag, the bus (CPU access recording) and the APU (DSP
    /// register-write recording). Called at every debug entry so the
    /// recorders are armed exactly when an event/watchpoint could fire,
    /// and disarmed (clearing their buffers) otherwise.
    fn sync_debug_arm(&mut self) {
        let armed = self.debugger.has_watchpoints_or_events();
        self.debug_armed = armed;
        self.bus.set_debug_armed(armed);
        self.bus.apu_mut().set_dsp_event_recording(armed);
    }

    /// Drain the bus's recorded CPU accesses and match them against
    /// read/write watchpoints and PPU/APU write events. Returns `true`
    /// when any breakpoint matched. Every event is checked (no early
    /// exit) so per-breakpoint hit counts stay accurate.
    fn drain_bus_debug_events(&mut self) -> bool {
        self.debug_event_scratch.clear();
        self.debug_event_scratch
            .extend_from_slice(self.bus.debug_events());
        self.bus.clear_debug_events();
        let mut hit = false;
        for ev in &self.debug_event_scratch {
            let matched = match *ev {
                BusDebugEvent::Read { addr, value } => self
                    .debugger
                    .check_memory_access_valued_65816(addr, false, value),
                BusDebugEvent::Write { addr, value } => self
                    .debugger
                    .check_memory_access_valued_65816(addr, true, value),
                BusDebugEvent::PpuRegWrite { reg } => {
                    self.debugger.check_event(SnesEvent::PpuRegisterWrite(reg))
                }
                BusDebugEvent::ApuPortWrite { port } => {
                    self.debugger.check_event(SnesEvent::ApuPortWrite(port))
                }
            };
            hit |= matched;
        }
        hit
    }

    /// Drain the S-DSP register-write indices the APU captured (via the
    /// `$F3` DSPDATA path) and match them against `DspRegisterWrite`
    /// event breakpoints. Returns `true` when any matched.
    fn drain_apu_dsp_events(&mut self) -> bool {
        let regs = self.bus.apu_mut().take_dsp_events();
        let mut hit = false;
        for reg in regs {
            hit |= self.debugger.check_event(SnesEvent::DspRegisterWrite(reg));
        }
        hit
    }

    /// Pause the debugger and flush per-frame debug state on a mid-frame
    /// break (execute breakpoint, watchpoint, or event hit). Shared by
    /// every early-return site in the instrumented frame loop.
    fn finish_paused_frame(&mut self, full: bool) {
        self.debugger.set_execution_mode(ExecutionMode::Paused);
        // The SPC700 / GSU may have run (via mailbox catch-up) before the break
        // paused us mid-frame; flush their trace entries chronologically.
        self.drain_spc_trace();
        self.drain_gsu_trace();
        if full {
            self.update_debugger_state();
        }
    }

    /// Maintain the 65C816 call stack from the opcode about to execute at
    /// `pc24`: push a frame on a subroutine/interrupt call
    /// (JSR `$20` / JSR(abs,x) `$FC` / JSL `$22` / BRK `$00` / COP `$02`)
    /// and pop on a return (RTS `$60` / RTL `$6B` / RTI `$40`). Opcode-only
    /// classification, no allocation; the `function_name` stays `None`
    /// (the core resolves no symbols). The return address is the next
    /// instruction, wrapped inside the program bank like the CPU's own PC.
    fn maintain_call_stack_65816(&mut self, pc24: u32) {
        let opcode = self.bus.read(pc24);
        let bank = pc24 & 0x00FF_0000;
        let next = |size: u16| bank | (pc24 as u16).wrapping_add(size) as u32;
        match opcode {
            0x20 | 0xFC => self.debugger.push_call_65816(StackFrame {
                call_address: pc24,
                return_address: next(3),
                function_name: None,
            }),
            0x22 => self.debugger.push_call_65816(StackFrame {
                call_address: pc24,
                return_address: next(4),
                function_name: None,
            }),
            0x00 | 0x02 => self.debugger.push_call_65816(StackFrame {
                call_address: pc24,
                return_address: next(2),
                function_name: None,
            }),
            0x60 | 0x6B | 0x40 => {
                self.debugger.pop_call_65816();
            }
            _ => {}
        }
    }

    /// Record the instruction at `pc24` into the 65C816 instruction
    /// history with its pre-execution register snapshot. Called only from
    /// the single-step path, where building the disassembly string and a
    /// register snapshot per instruction is affordable.
    fn record_history_65816(&mut self, pc24: u32) {
        let state = wdc65816::disasm::DisasmState {
            emulation: self.cpu.emulation,
            flag_m: self.cpu.p & 0x20 != 0,
            flag_x: self.cpu.p & 0x10 != 0,
        };
        let (instruction, _len) = wdc65816::disasm::disassemble_at(&mut self.bus, pc24, &state);
        let opcode = self.bus.read(pc24);
        let cycle_count = wdc65816::decode::OPCODE_TABLE[opcode as usize].base_cycles as u32;
        let registers_before = cpu_state_65816(&self.cpu).registers;
        self.debugger.record_instruction_65816(InstructionTrace {
            address: pc24,
            instruction,
            registers_before,
            cycle_count,
        });
    }

    /// Bitmap-aware disassembly window around `pc` for the given CPU
    /// (`0` = 65C816, `1` = SPC700, `2` = the cartridge's CPU on an SA-1,
    /// Super FX or Cx4 cart).
    ///
    /// Thin wrapper that builds a [`SnesDisasmDriver`] and delegates to
    /// [`retro_core::debug::disassemble_window`]. The backward probe +
    /// forward decode algorithm lives in `retro-core`; only the
    /// CPU-specific decoding and the probe-offset table stay here. The
    /// SPC700 is decoded against a side-effect-free view of its memory
    /// so a window can never perturb the running audio program.
    pub fn disassemble_window(
        &mut self,
        cpu_id: usize,
        pc: u32,
        before_count: usize,
        after_count: usize,
    ) -> Vec<DisasmRow> {
        let mut driver = SnesDisasmDriver { emu: self, cpu_id };
        retro_core::disassemble_window(&mut driver, pc, before_count, after_count)
    }
}

/// Side-effect-free [`Addressable`] view of the APU's SPC700 memory for
/// disassembly. Reads resolve only the IPL-ROM window + ARAM (never a
/// DSP data port or a read-clearing timer), and writes are dropped, so
/// rendering a disassembly window can't perturb the audio program.
struct SpcPeekBus<'a> {
    apu: &'a crate::apu::Apu,
}

impl Addressable for SpcPeekBus<'_> {
    fn read(&mut self, addr: u32) -> u8 {
        self.apu.spc_peek(addr as u16)
    }
    fn write(&mut self, _addr: u32, _value: u8) {}
}

/// Decode a single instruction at `addr` for the given CPU into a
/// `Code`-kind [`DisasmRow`]. Shared by the [`SnesDisasmDriver`] window
/// walk and the forward [`EmulatorCore::disassemble_cpu`] path.
fn decode_one_snes(emu: &mut SnesEmulator, cpu_id: usize, addr: u32) -> DisasmRow {
    if cpu_id == 2 {
        // The slot-2 CPU (the SA-1's second 65816, the GSU or the Cx4): the chip
        // decodes one instruction against a side-effect-free view of its own
        // memory (for the SA-1, ROM via the Super MMC + I-RAM + BW-RAM, MMIO
        // read-inert). No such chip present (unreachable when slot 2 is active)
        // degrades to a one-byte `Unknown` placeholder.
        let addr24 = addr & 0x00FF_FFFF;
        return match emu
            .bus
            .cartridge_ref()
            .and_then(|c| c.disassemble_secondary(addr24, 1).into_iter().next())
        {
            Some(instr) => DisasmRow {
                address: addr24,
                kind: DisasmRowKind::Code,
                bytes: instr.bytes,
                mnemonic: instr.mnemonic,
                operands: instr.operands,
                comment: instr.comment,
            },
            None => DisasmRow {
                address: addr24,
                kind: DisasmRowKind::Unknown,
                bytes: vec![0],
                mnemonic: String::new(),
                operands: String::new(),
                comment: None,
            },
        };
    }
    if cpu_id == 1 {
        let addr16 = addr & 0xFFFF;
        let state = spc700::disasm::DisasmState {
            p: emu.bus.apu().spc700().psw & 0x20 != 0,
        };
        let mut bus = SpcPeekBus { apu: emu.bus.apu() };
        let res = spc700::disasm::disassemble_one(&mut bus, addr16, &state);
        DisasmRow {
            address: addr16,
            kind: DisasmRowKind::Code,
            bytes: res.bytes,
            mnemonic: res.mnemonic,
            operands: res.operands,
            comment: res.comment,
        }
    } else {
        let addr24 = addr & 0x00FF_FFFF;
        let state = wdc65816::disasm::DisasmState {
            emulation: emu.cpu.emulation,
            flag_m: emu.cpu.p & 0x20 != 0,
            flag_x: emu.cpu.p & 0x10 != 0,
        };
        let res = wdc65816::disasm::disassemble_one(&mut emu.bus, addr24, &state);
        DisasmRow {
            address: addr24,
            kind: DisasmRowKind::Code,
            bytes: res.bytes,
            mnemonic: res.mnemonic,
            operands: res.operands,
            comment: res.comment,
        }
    }
}

/// Adapter wiring [`SnesEmulator`] into the generic
/// [`retro_core::DisasmDriver`] contract for one CPU at a time. Owned
/// briefly inside [`SnesEmulator::disassemble_window`] so the emulator
/// can be borrowed mutably across the (backward + forward) walk.
struct SnesDisasmDriver<'a> {
    emu: &'a mut SnesEmulator,
    cpu_id: usize,
}

impl retro_core::DisasmDriver for SnesDisasmDriver<'_> {
    fn probe_offsets(&self) -> &'static [u32] {
        // 65C816 instructions (the 5A22 and the SA-1's second 65816) are
        // 1-4 bytes; SPC700 are 1-3. The slot-2 CPU gets the 1-4 set
        // whatever the chip, the GSU and the Cx4 included.
        if self.cpu_id == 1 {
            &[1, 2, 3]
        } else {
            &[1, 2, 3, 4]
        }
    }

    fn is_covered(&self, addr: u32) -> bool {
        match self.cpu_id {
            1 => self.emu.debugger.is_covered_spc700(addr & 0xFFFF),
            // The slot-2 CPU (the SA-1's 65816, the GSU or the Cx4) has no
            // coverage bitmap, so every row decodes as `Code` rather than an
            // `Unknown` placeholder.
            2 => true,
            _ => self.emu.debugger.is_covered_65816(addr & 0x00FF_FFFF),
        }
    }

    fn decode_at(&mut self, addr: u32) -> DisasmRow {
        decode_one_snes(self.emu, self.cpu_id, addr)
    }
}

impl EmulatorCore for SnesEmulator {
    fn system_info(&self) -> &SystemInfo {
        &self.system_info
    }

    fn reset(&mut self) {
        self.cpu.reset_with_bus(&mut self.bus);
        self.frame_count = 0;
        self.master_cycles = 0;
        self.frame_base_cycle = 0;
        self.nmi_deliver_armed = false;
        self.nmi_deliver_countdown = 0;
        self.irq_read_early = None;
        self.scheduler.reset(0);
        // Frame 0 schedules off the live overscan bit (off at the
        // reset/power-on state); subsequent frames re-read it in
        // `end_of_frame_bookkeeping`.
        schedule_frame_events(&mut self.scheduler, 0, self.bus.ppu().overscan());
        self.arm_irq_events(0, 0);
        self.audio_ring.clear();
        self.resampler.reset();
        self.apu_drain_buf.clear();
        self.audio_resample_buf.clear();
        self.audio_pending_drain.clear();
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
        // The bus reset zeroes the PPU register file -> bgmode 0
        // (non-hires) + overscan off -> resync the public display
        // dimensions to the default 256x224 view.
        self.sync_display_info();
        // Refresh the cached CPU-state snapshot so a debugger reading
        // `get_cpu_states()` right after load/reset sees the real
        // reset-vector PC + registers instead of the construction
        // defaults. Without this the disassembly view anchors at PC 0
        // until the first single-step.
        self.update_debugger_state();
    }

    fn step(&mut self) -> StepResult {
        let delta = self.step_cpu_internal();
        StepResult {
            cycles: delta.min(u32::MAX as u64) as u32,
            breakpoint_hit: false,
            instruction_executed: true,
        }
    }

    fn step_cpu(&mut self, cpu_id: usize) -> StepResult {
        match cpu_id {
            0 => {
                // Single-stepping the 5A22 records coverage + a trace
                // entry (when enabled) before executing, then refreshes
                // the cached register snapshots.
                self.sync_spc_trace_enable();
                self.sync_gsu_trace_enable();
                self.sync_debug_arm();
                let pc24 = ((self.cpu.pbr as u32) << 16) | self.cpu.pc as u32;
                self.debugger.record_coverage_65816(pc24);
                if self.debugger.is_tracing_enabled(0) {
                    let entry = self.capture_trace_65816();
                    self.debugger.record_trace_65816(&entry);
                }
                if self.debug_armed {
                    self.bus.clear_debug_events();
                }
                let result = self.step();
                // Fire the scheduler events this instruction reached
                // (RenderPos/VBlank/HBlank/NMI/IRQ/HDMA) and close the frame
                // at the boundary, mirroring the frame loop. Without this a
                // single-stepped or instruction-bounded run executes against
                // a dead, event-less timeline that never advances past
                // frame 0, the cause of instruction-trace drift vs. a
                // continuously-running reference.
                self.drain_events_and_maybe_close_frame();
                // The SPC700 can run via mailbox catch-up inside this one
                // 5A22 step; flush whatever it captured.
                if self.debug_armed {
                    let _ = self.drain_bus_debug_events();
                    let _ = self.drain_apu_dsp_events();
                    self.pending_break = false;
                }
                self.drain_spc_trace();
                self.drain_gsu_trace();
                self.update_debugger_state();
                result
            }
            2 => {
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
                self.update_debugger_state();
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
        self.focused_cpu
    }

    fn set_focused_cpu(&mut self, cpu_id: usize) {
        if cpu_id < self.system_info.cpus.len() {
            self.focused_cpu = cpu_id;
        }
    }

    fn run_frame(&mut self) {
        // Keep the APU's SPC700 trace capture in lock-step with the
        // debugger's cpu-1 tracing flag before any catch-up runs this
        // frame.
        self.sync_spc_trace_enable();
        // Same for the slot-2 trace capture, the Super FX's GSU or the Cx4
        // (no-op otherwise).
        self.sync_gsu_trace_enable();
        // Arm (or disarm) bus + APU debug-event recording to match the
        // current watch/event-breakpoint set before the loop runs.
        self.sync_debug_arm();
        // Dispatch on the current debug state: the fast path when nothing
        // is instrumented, a per-instruction light path when breakpoints
        // or tracing are active, and the full path while paused / stepping.
        match self.debugger.execution_tier() {
            ExecutionTier::Fast => self.run_frame_fast(),
            ExecutionTier::LightDebug => self.run_frame_instrumented(false),
            ExecutionTier::FullDebug => self.run_frame_full(),
        }
    }

    fn run_until_vblank(&mut self) {
        // VBlankStart raises NMI mid-frame, but the host-visible
        // contract here is "framebuffer ready", so running a whole
        // frame gives the same observable result without
        // partial-frame state to track.
        self.run_frame();
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
        let cart = match builder.build() {
            Ok(cart) => cart,
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
        // the detector would have rejected).
        let detected = detect_preset_with_threshold(data, i32::MIN);
        let (header_opt, preset_opt) = match detected {
            Some(DetectedPreset { kind, header, .. }) => (Some(header), Some(kind)),
            None => {
                let header = HEADER_CANDIDATES
                    .iter()
                    .find_map(|&offset| try_parse_header(data, offset));
                (header, None)
            }
        };
        self.header = header_opt;
        self.preset_kind = preset_opt;
        self.rom_size = data.len();
        // Hash the stripped ROM bytes (the manifest convention) so the
        // savestate envelope can refuse a state captured against a
        // different game. Survives `reset()` since the ROM stays loaded.
        self.rom_hash = rom_sha256(data);
        // Retain the raw bytes so `power_on_reset` can reconstruct a fresh
        // power-on machine (netplay cold boot) by re-running `load_rom`.
        self.last_rom = data.to_vec();

        let timing = self
            .header
            .as_ref()
            .map(|h| TimingProfile::from_destination(h.destination))
            .unwrap_or(TimingProfile::NTSC);
        self.timing = timing;
        self.scheduler = HEventScheduler::new(timing);
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
        self.bus = SnesBus::new();
        self.bus.install_cartridge(cart);
        // Propagate the PAL/NTSC choice to SystemRegs so STAT78
        // (`$213F` bit 4) reports the right region. PAL profiles
        // have 312 scanlines per frame; NTSC has 262.
        let is_pal = timing.scanlines_per_frame == 312;
        self.bus.system_regs_mut().set_timing_is_pal(is_pal);

        // Surface the ROM / SRAM sizes through the debugger's memory-region
        // map now that the cartridge is installed.
        let sram_size = self.bus.cartridge_sram().map(|s| s.len()).unwrap_or(0) as u32;
        self.debugger
            .set_memory_sizes(self.rom_size as u32, sram_size);

        // A cartridge that hosts a steppable secondary CPU (the SA-1's second
        // 65816, the Super FX's GSU or the Cx4) surfaces as a third debugger
        // CPU slot; a plain cart, or a DSP-1 / S-DD1 / SPC7110 cart, stays a
        // two-CPU console. Rebuild the roster (which clamps `set_focused_cpu`)
        // from the base on every load so a hot-swap back to a plain ROM drops
        // the extra slot, and mirror it into the debugger (whose copy backs
        // `get_cpu_info`).
        let has_secondary = self
            .bus
            .cartridge_ref()
            .map(|c| c.secondary_cpu_state().is_some())
            .unwrap_or(false);
        let mut cpus = snes_base_cpus();
        if has_secondary {
            cpus.push(secondary_cpu_info(self.preset_kind));
        }
        self.system_info.cpus = cpus;
        self.debugger.set_system_info(self.system_info.clone());

        self.reset();
        Ok(())
    }

    fn get_rom_info(&self) -> Option<RomInfo> {
        let header = self.header.as_ref()?;
        Some(RomInfo {
            title: header.title.clone(),
            region: destination_region_str(header.destination).to_string(),
            size: self.rom_size,
            checksum: header.checksum as u32,
            mapper: self.preset_kind.map(preset_kind_name),
            title_confidence: "high".to_string(),
        })
    }

    fn get_framebuffer(&self) -> &Framebuffer {
        &self.framebuffer
    }

    fn get_display_info(&self) -> &DisplayInfo {
        &self.system_info.display
    }

    fn get_audio_samples(&mut self) -> &[f32] {
        let want = self.audio_ring.available();
        self.audio_pending_drain.resize(want, 0.0);
        if want > 0 {
            self.audio_ring.drain_into(&mut self.audio_pending_drain);
        }
        &self.audio_pending_drain
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

    fn save_state(&self) -> Vec<u8> {
        use crate::savestate::SNES_SAVESTATE_VERSION;

        // Shared with `state_hash` so the hashed payload is exactly the
        // bytes persisted here (minus the envelope + compression).
        let payload = self.capture_save_state();
        let env = retro_core::savestate::SaveStateEnvelope {
            envelope_version: retro_core::savestate::RETRO_SAVESTATE_ENVELOPE_VERSION,
            core_id: "snes1".to_string(),
            core_version: SNES_SAVESTATE_VERSION,
            rom_hash: self.rom_hash.clone(),
            // The core leaves the timestamp at 0; the host shell sets a
            // real capture time when it persists the bytes. Keeping it
            // 0 here makes `save_state` deterministic for tests.
            created_at: 0,
            payload,
        };
        // zstd level 3 matches SG1: good ratio on the mostly-zero
        // RAM/VRAM/ARAM payload at ~1 ms. A serialization failure (out
        // of memory) degrades to empty bytes, which `load_state` then
        // rejects as "missing magic".
        retro_core::savestate::serialize_compressed(&env, 3).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        self.load_state_with_rom_check(data, retro_core::savestate::RomCheck::SameRom)
            .map_err(|e| e.to_string())
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

    /// Reads through the live bus, as the CPU would: a read of a register
    /// has its side effects, and every read sets the open-bus latch. Plain
    /// memory (WRAM, ROM, SRAM) reads back unchanged; a read with no side
    /// effects would need one per coprocessor, whose memories sit behind
    /// their registers.
    fn read_memory_debug(&mut self, addr: u32, size: u32) -> Vec<u8> {
        (0..size)
            .map(|i| self.bus.read(addr.wrapping_add(i)))
            .collect()
    }

    fn disassemble_at(&mut self, addr: u32, count: u32) -> Vec<Instruction> {
        self.disassemble_cpu(0, addr, count)
    }

    fn disassemble_cpu(&mut self, cpu_id: usize, addr: u32, count: u32) -> Vec<Instruction> {
        // Forward-decode `count` instructions from `addr`. The 65C816
        // uses the live bus + current M/X/E flags (immediate operand
        // width depends on them); the SPC700 uses a side-effect-free
        // view of its memory and the live `PSW.P` direct-page select.
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
                    let res = wdc65816::disasm::disassemble_one(&mut self.bus, cursor, &state);
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
                .cartridge_ref()
                .map(|c| c.disassemble_secondary(addr, count))
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

/// The two CPUs every SNES console has: the 5A22 main CPU (65C816 core) and
/// the SPC700 audio CPU. A cartridge coprocessor that hosts its own steppable
/// CPU (the SA-1, the Super FX or the Cx4) appends a third slot on top of this
/// base in [`EmulatorCore::load_rom`].
fn snes_base_cpus() -> Vec<CpuInfo> {
    vec![
        CpuInfo {
            id: 0,
            name: "Ricoh 5A22 (WDC 65C816)".to_string(),
            architecture: CpuArchitecture::Mos65816,
            clock_speed: 3_580_000,
            location: CpuLocation::Motherboard,
            address_width: 24,
            registers: Vec::new(),
        },
        CpuInfo {
            id: 1,
            name: "Sony SPC700".to_string(),
            architecture: CpuArchitecture::Spc700,
            clock_speed: 1_024_000,
            location: CpuLocation::Motherboard,
            address_width: 16,
            registers: Vec::new(),
        },
    ]
}

/// CPU-roster entry (slot 2) for a cartridge's steppable secondary CPU, keyed
/// off the installed preset. The SA-1 contributes its second 65816
/// (`Mos65816`), the Super FX the GSU (`SuperFx`) and the Cx4 the HG51B
/// (`Hg51b`). Each reports its own clock: about 10.74 MHz for the SA-1 and
/// for the GSU at its default CLSR divider, 20 MHz for the Cx4. Every entry
/// gets the 24-bit-address debugger treatment, but the architecture tag
/// drives which register / flag layout the panel renders.
fn secondary_cpu_info(preset: Option<PresetKind>) -> CpuInfo {
    let (name, architecture, clock_speed) = match preset {
        Some(PresetKind::SuperFx) => ("Super FX GSU", CpuArchitecture::SuperFx, 10_740_000),
        Some(PresetKind::Cx4) => ("Cx4 HG51B", CpuArchitecture::Hg51b, 20_000_000),
        _ => ("SA-1 (WDC 65C816)", CpuArchitecture::Mos65816, 10_740_000),
    };
    CpuInfo {
        id: 2,
        name: name.to_string(),
        architecture,
        clock_speed,
        location: CpuLocation::Cartridge,
        address_width: 24,
        registers: Vec::new(),
    }
}

/// Map a SNES destination byte (`$FFD9`) to a human-readable region
/// string used by [`RomInfo::region`]. The table is intentionally
/// coarse, "USA" covers both "USA" and "Canada". Unknown values
/// render as `"Unknown"`.
pub fn destination_region_str(byte: u8) -> &'static str {
    match byte {
        0x00 => "Japan",
        0x01 | 0x0F => "USA",
        0x02 => "Europe",
        0x03 => "Sweden",
        0x04 => "Finland",
        0x05 => "Denmark",
        0x06 => "France",
        0x07 => "Netherlands",
        0x08 => "Spain",
        0x09 => "Germany",
        0x0A => "Italy",
        0x0B => "China",
        0x0C => "Indonesia",
        0x0D => "Korea",
        0x10 => "Brazil",
        0x11 => "Australia",
        _ => "Unknown",
    }
}

/// Build the console's input manager: two ports with a default
/// keyboard provider bound to port 0.
///
/// The generic `GenericInputManager::new` registers no providers, so a
/// core that wants the "keyboard works out of the box" UX must register
/// one itself (mirrors the SG1 shim). Without this the host's
/// `keyDown`/`keyUp` events broadcast to zero keyboard providers and no
/// button ever reaches the pad. The keyboard uses the SNES default
/// layout from [`default_keyboard_mapping`].
fn default_input_manager() -> GenericInputManager {
    let mut manager = GenericInputManager::new(2);
    let keyboard = KeyboardInputProvider::with_mapping(default_keyboard_mapping());
    manager.add_keyboard_provider(keyboard);
    manager.assign_provider_to_port("keyboard".to_string(), 0);
    // Pads the frontend auto-registers on first input pick up the SNES
    // face-button order rather than the Genesis layout baked into the
    // shared provider.
    manager.set_default_gamepad_mapping(default_gamepad_mapping());
    manager
}

/// Standard-pad button layout advertised via [`InputInfo`]. The `id`
/// of each entry is the generic `InputState::buttons` bit index the
/// host must set; it matches the `BTN_*` constants the controller
/// subsystem maps to the SNES hardware order.
fn snes_button_layout() -> Vec<ButtonInfo> {
    use crate::controller::{
        BTN_A, BTN_B, BTN_DOWN, BTN_L, BTN_LEFT, BTN_R, BTN_RIGHT, BTN_SELECT, BTN_START, BTN_UP,
        BTN_X, BTN_Y,
    };
    let entry = |name: &str, id: u32| ButtonInfo {
        name: name.to_string(),
        id,
    };
    vec![
        entry("Up", BTN_UP),
        entry("Down", BTN_DOWN),
        entry("Left", BTN_LEFT),
        entry("Right", BTN_RIGHT),
        entry("A", BTN_A),
        entry("B", BTN_B),
        entry("X", BTN_X),
        entry("Y", BTN_Y),
        entry("L", BTN_L),
        entry("R", BTN_R),
        entry("Start", BTN_START),
        entry("Select", BTN_SELECT),
    ]
}

/// Stable name for a preset (used as the `mapper` field of
/// [`RomInfo`]).
fn preset_kind_name(kind: PresetKind) -> String {
    match kind {
        PresetKind::LoRom => "LoROM".to_string(),
        PresetKind::HiRom => "HiROM".to_string(),
        PresetKind::ExHiRom => "ExHiROM".to_string(),
        PresetKind::Sdd1 => "S-DD1".to_string(),
        PresetKind::Spc7110 => "SPC7110".to_string(),
        PresetKind::Sa1 => "SA-1".to_string(),
        PresetKind::SuperFx => "Super FX".to_string(),
        PresetKind::Cx4 => "Cx4".to_string(),
        PresetKind::Custom => "Custom".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_info_lists_both_cpus() {
        let emu = SnesEmulator::new();
        let info = emu.system_info();
        assert_eq!(info.cpus.len(), 2);
        assert_eq!(info.cpus[0].architecture, CpuArchitecture::Mos65816);
        assert_eq!(info.cpus[1].architecture, CpuArchitecture::Spc700);
        assert_eq!(info.short_name, "SNES");
    }

    #[test]
    fn display_info_matches_snes_geometry() {
        let emu = SnesEmulator::new();
        let display = emu.get_display_info();
        assert_eq!(display.width, 256);
        assert_eq!(display.height, 224);
        assert!((display.refresh_rate - 60.0985).abs() < 1e-4);
    }

    #[test]
    fn display_info_refresh_rate_follows_pal_region() {
        let mut emu = SnesEmulator::new();
        emu.set_region_str("pal").unwrap();
        assert!((emu.get_display_info().refresh_rate - 50.0070).abs() < 1e-4);
        emu.set_region_str("ntsc").unwrap();
        assert!((emu.get_display_info().refresh_rate - 60.0985).abs() < 1e-4);
    }

    #[test]
    fn load_rom_pal_destination_selects_pal_timing_and_refresh_rate() {
        let mut rom = synth_hirom_2kib_sram_rom();
        rom[0xFFB0 + 0x29] = 0x02; // Europe
        let mut emu = SnesEmulator::new();
        emu.load_rom(&rom).expect("loads");
        assert_eq!(emu.timing(), TimingProfile::PAL);
        assert!((emu.get_display_info().refresh_rate - 50.0070).abs() < 1e-4);
    }

    #[test]
    fn repaint_held_frame_runs_over_live_bus() {
        let mut emu = SnesEmulator::new();
        // No ROM/content loaded: this proves the paused-repaint wrapper
        // (render every visible scanline + finalize) runs over the real
        // bus without panicking and leaves a self-consistent framebuffer.
        emu.repaint_held_frame();
        let fb = emu.get_framebuffer();
        assert!(!fb.data.is_empty());
        assert_eq!(fb.data.len(), (fb.width * fb.height * 4) as usize);
    }

    #[test]
    fn bg_layout_wrapper_reads_reset_state() {
        let emu = SnesEmulator::new();
        let layout = emu.bg_layout(0);
        assert_eq!(layout.bg, 0);
        // Reset state: tilemap + character bases default to 0.
        assert_eq!(layout.tilemap_base_byte, 0);
        assert_eq!(layout.char_base_byte, 0);
    }

    #[test]
    fn new_defaults_to_ntsc_timing() {
        let emu = SnesEmulator::new();
        assert_eq!(emu.timing(), TimingProfile::NTSC);
        assert_eq!(emu.master_cycles(), 0);
        assert_eq!(emu.frame_count(), 0);
        assert!(emu.header().is_none());
        assert!(emu.preset_kind().is_none());
    }

    #[test]
    fn audio_disabled_by_default() {
        let emu = SnesEmulator::new();
        assert!(!emu.is_audio_enabled());
        assert_eq!(emu.get_audio_info().sample_rate, 44_100);
    }

    #[test]
    fn focused_cpu_clamps_to_known_range() {
        let mut emu = SnesEmulator::new();
        assert_eq!(emu.focused_cpu(), 0);
        emu.set_focused_cpu(1);
        assert_eq!(emu.focused_cpu(), 1);
        emu.set_focused_cpu(99);
        assert_eq!(emu.focused_cpu(), 1);
    }

    #[test]
    fn load_rom_invalid_returns_err() {
        let mut emu = SnesEmulator::new();
        let bad = vec![0u8; 16];
        let result = emu.load_rom(&bad);
        assert!(
            result.is_err(),
            "expected detection failure on tiny zero rom"
        );
        assert!(emu.get_rom_info().is_none());
    }

    /// 4 MiB HiROM image with a well-formed header at `$FFB0` declaring
    /// 2 KiB battery SRAM, enough for the detector to score it as
    /// HiROM and resolve a 2 KiB SRAM chip.
    fn synth_hirom_2kib_sram_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x40_0000];
        let offset = 0xFFB0;
        rom[offset + 0x10..offset + 0x10 + 21].copy_from_slice(b"OVERRIDE TEST ROM    ");
        rom[offset + 0x25] = 0x21; // HiROM, slow
        rom[offset + 0x26] = 0x02; // ROM + RAM + battery
        rom[offset + 0x27] = 0x0C; // 4 MiB
        rom[offset + 0x28] = 0x01; // 2 KiB SRAM
        rom[offset + 0x29] = 0x01; // USA
        rom[offset + 0x2A] = 0x01;
        let checksum: u16 = 0x1234;
        rom[offset + 0x2C..offset + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
        rom[offset + 0x2E..offset + 0x30].copy_from_slice(&checksum.to_le_bytes());
        rom[offset + 0x4C..offset + 0x4E].copy_from_slice(&0x8000u16.to_le_bytes());
        rom
    }

    #[test]
    fn a_pause_reports_where_the_fast_loop_stopped() {
        let mut emu = SnesEmulator::new();
        emu.load_rom(&synth_hirom_2kib_sram_rom()).expect("loads");
        let at_load = emu.debugger.get_cpu_states()[0].pc;
        emu.set_execution_mode(ExecutionMode::Running);
        for _ in 0..3 {
            emu.run_frame_fast();
        }
        // The fast loop refreshes no snapshot: the debugger still holds the
        // one the reset took.
        assert_eq!(emu.debugger.get_cpu_states()[0].pc, at_load);

        emu.set_execution_mode(ExecutionMode::Paused);

        let live = cpu_state_65816(emu.cpu()).pc;
        assert_ne!(live, at_load);
        assert_eq!(emu.debugger.get_cpu_states()[0].pc, live);
    }

    #[test]
    fn manifest_override_changes_installed_plan() {
        let rom = synth_hirom_2kib_sram_rom();
        let mut emu = SnesEmulator::new();

        // Pure detection: HiROM plan, 2 KiB SRAM.
        emu.load_rom(&rom).expect("loads via detection");
        assert_eq!(emu.get_save_ram().map(<[u8]>::len), Some(2048));
        assert_eq!(emu.bus.cartridge_ref().unwrap().map_plan().name, "hirom");

        // Override: force LoROM + 32 KiB SRAM, then reload.
        let mut over = emu.detected_manifest(&rom);
        over.mapping.preset = "lorom".to_string();
        over.mapping.sram_size = 32 * 1024;
        emu.set_cartridge_manifest_override(Some(over));
        emu.load_rom(&rom).expect("loads with override");
        assert_eq!(emu.get_save_ram().map(<[u8]>::len), Some(32 * 1024));
        assert_eq!(emu.bus.cartridge_ref().unwrap().map_plan().name, "lorom");

        // Clearing the override reverts to pure detection.
        emu.set_cartridge_manifest_override(None);
        emu.load_rom(&rom).expect("loads after clear");
        assert_eq!(emu.get_save_ram().map(<[u8]>::len), Some(2048));
        assert_eq!(emu.bus.cartridge_ref().unwrap().map_plan().name, "hirom");
    }

    #[test]
    fn reset_without_rom_does_not_panic() {
        let mut emu = SnesEmulator::new();
        emu.reset();
        // No cart -> bus returns 0 from open-bus -> reset vector
        // resolves to 0x0000.
        assert_eq!(emu.cpu().pc, 0x0000);
        assert_eq!(emu.cpu().pbr, 0x00);
    }

    #[test]
    fn power_on_reset_zeroes_wram() {
        let mut emu = SnesEmulator::new();
        emu.load_rom(&synth_hirom_2kib_sram_rom()).expect("loads");

        // Dirty WRAM through the real bus write path ($7E_0000.. is WRAM).
        for i in 0..256u32 {
            emu.bus.write_fast(0x7E_0000 + i, 0xAB);
        }
        assert!(
            emu.bus.wram().iter().any(|&b| b != 0),
            "WRAM should be dirty before the cold reset"
        );

        emu.power_on_reset();

        assert!(
            emu.bus.wram().iter().all(|&b| b == 0),
            "power-on reset must zero WRAM"
        );
        assert_eq!(emu.frame_count(), 0);
    }

    #[test]
    fn power_on_reset_converges_state_hash_across_divergent_history() {
        // The netplay premise: two peers that reached different states (a host
        // that ran the game vs a freshly-booted guest) must derive the SAME
        // frame-0 state hash after a cold reset, so both start the session
        // from the same state. Dirty both WRAM (via the real bus write path)
        // and the CPU (direct-page register), since a plain `reset()`
        // preserves both and would leave the peers divergent.
        let rom = synth_hirom_2kib_sram_rom();
        let mut host = SnesEmulator::new();
        let mut guest = SnesEmulator::new();
        host.load_rom(&rom).expect("host loads");
        guest.load_rom(&rom).expect("guest loads");

        for i in 0..256u32 {
            host.bus.write_fast(0x7E_0000 + i, 0x11);
            guest.bus.write_fast(0x7E_0000 + i, 0x22);
        }
        host.cpu_mut().d = 0x1234;
        host.cpu_mut().c = 0xBEEF;
        assert_ne!(
            host.state_hash(),
            guest.state_hash(),
            "divergent WRAM + CPU should hash differently before the cold reset"
        );

        host.power_on_reset();
        guest.power_on_reset();
        assert_eq!(
            host.state_hash(),
            guest.state_hash(),
            "cold reset must give both peers a byte-identical frame-0 baseline"
        );
    }

    #[test]
    fn step_cpu_one_returns_empty_step_result() {
        let mut emu = SnesEmulator::new();
        let result = emu.step_cpu(1);
        assert_eq!(result.cycles, 0);
        assert!(!result.instruction_executed);
        assert!(!result.breakpoint_hit);
    }

    #[test]
    fn step_cpu_out_of_range_returns_empty_step_result() {
        let mut emu = SnesEmulator::new();
        let result = emu.step_cpu(7);
        assert_eq!(result.cycles, 0);
        assert!(!result.instruction_executed);
    }

    #[test]
    fn get_audio_samples_empty_when_no_producer() {
        let mut emu = SnesEmulator::new();
        let samples = emu.get_audio_samples();
        assert!(samples.is_empty());
    }

    #[test]
    fn set_input_writes_through_to_port_state() {
        let mut emu = SnesEmulator::new();
        emu.set_input(
            0,
            InputState {
                buttons: 0xABCD_0000,
            },
        );
        assert_eq!(emu.input_manager.get_port_state(0), 0xABCD_0000);
    }

    #[test]
    fn set_input_out_of_range_player_is_silent_noop() {
        let mut emu = SnesEmulator::new();
        emu.set_input(
            9,
            InputState {
                buttons: 0xFFFFFFFF,
            },
        );
        assert_eq!(emu.input_manager.get_port_state(0), 0);
        assert_eq!(emu.input_manager.get_port_state(1), 0);
    }

    #[test]
    fn set_input_refreshes_controller_pads() {
        use crate::controller::BTN_B;
        let mut emu = SnesEmulator::new();
        emu.set_input(
            0,
            InputState {
                buttons: 1 << BTN_B,
            },
        );
        // The pad now holds B (0x8000); an auto-read surfaces it in
        // JOY1H ($4219 bit 7).
        emu.bus_mut().auto_joypad_read();
        assert_eq!(emu.bus_mut().read(0x00_4219) & 0x80, 0x80);
    }

    #[test]
    fn boots_with_keyboard_provider_bound_to_port_zero() {
        // The keyboard path (`keyDown`/`keyUp` -> `update_keyboard`) only
        // reaches a port if a keyboard provider is registered AND
        // assigned. A bare `GenericInputManager::new(2)` registers none,
        // which is exactly the bug this guards against.
        let emu = SnesEmulator::new();
        assert_eq!(
            emu.input_manager().get_port_assignment(0),
            Some("keyboard".to_string()),
            "snes1 must boot with the default keyboard bound to port 0"
        );
    }

    #[test]
    fn keyboard_key_drives_port_zero_state() {
        let mut emu = SnesEmulator::new();
        emu.input_manager_mut()
            .update_keyboard("ArrowUp".to_string(), true);
        assert_ne!(
            emu.input_manager().get_port_state(0),
            0,
            "a default-mapped key press must reach port 0"
        );
    }

    #[test]
    fn keyboard_keys_map_to_their_snes_hardware_bits() {
        let mut emu = SnesEmulator::new();
        let bit =
            |emu: &SnesEmulator| input_state_to_snes_pad(emu.input_manager().get_port_state(0));

        // Press one default key at a time and confirm it lands on the
        // SNES button the layout promises (not the Genesis position the
        // shared provider would otherwise pick).
        let cases = [
            ("KeyZ", 0x8000u16),    // B
            ("KeyX", 0x0080),       // A
            ("KeyA", 0x4000),       // Y
            ("KeyS", 0x0040),       // X
            ("KeyQ", 0x0020),       // L
            ("KeyW", 0x0010),       // R
            ("Enter", 0x1000),      // Start
            ("ShiftRight", 0x2000), // Select
            ("ArrowUp", 0x0800),    // Up
        ];
        for (code, expected) in cases {
            emu.input_manager_mut()
                .update_keyboard(code.to_string(), true);
            assert_eq!(bit(&emu), expected, "{code} must map to {expected:#06x}");
            emu.input_manager_mut()
                .update_keyboard(code.to_string(), false);
        }
    }

    #[test]
    fn keyboard_key_reaches_the_hardware_pad_end_to_end() {
        // Full path: key press -> port state -> controller pad -> auto-read
        // register. `KeyZ` is B, which surfaces in JOY1H ($4219 bit 7).
        let mut emu = SnesEmulator::new();
        emu.input_manager_mut()
            .update_keyboard("KeyZ".to_string(), true);
        emu.refresh_controllers_from_input();
        emu.bus_mut().auto_joypad_read();
        assert_eq!(emu.bus_mut().read(0x00_4219) & 0x80, 0x80);
    }

    #[test]
    fn auto_registered_gamepad_uses_snes_face_layout() {
        use crate::controller::{BTN_B, BTN_X};
        let mut emu = SnesEmulator::new();
        // First input from an unseen pad auto-registers it (onto port 0,
        // beside the keyboard) with snes1's default gamepad map: Web
        // button 0 -> B.
        emu.input_manager_mut()
            .update_gamepad_button("pad-0".to_string(), 0, true);
        assert_eq!(emu.input_manager().provider_port("pad-0"), Some(0));
        let mask = emu.input_manager().get_port_state(0);
        assert_eq!(
            mask & (1 << BTN_B),
            1 << BTN_B,
            "gamepad button 0 must be SNES B"
        );
        // Guard against the Genesis default leaking through: Genesis
        // maps button 0 -> bit 6, which on the SNES layout is X.
        assert_eq!(
            mask & (1 << BTN_X),
            0,
            "must not fall back to the Genesis button-0 -> bit-6 mapping"
        );
    }

    #[test]
    fn button_layout_lists_twelve_snes_buttons() {
        let emu = SnesEmulator::new();
        let layout = &emu.get_input_info().button_layout;
        assert_eq!(layout.len(), 12);
        // Ids are unique and cover 0..=11.
        let mut ids: Vec<u32> = layout.iter().map(|b| b.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, (0..12).collect::<Vec<_>>());
        // Spot-check a couple of names line up with the convention.
        let by_name = |n: &str| layout.iter().find(|b| b.name == n).map(|b| b.id);
        assert_eq!(by_name("Up"), Some(crate::controller::BTN_UP));
        assert_eq!(by_name("Select"), Some(crate::controller::BTN_SELECT));
    }

    #[test]
    fn destination_region_str_known_codes() {
        assert_eq!(destination_region_str(0x00), "Japan");
        assert_eq!(destination_region_str(0x01), "USA");
        assert_eq!(destination_region_str(0x02), "Europe");
        assert_eq!(destination_region_str(0x09), "Germany");
        assert_eq!(destination_region_str(0xFF), "Unknown");
    }

    #[test]
    fn preset_kind_name_lorom_hirom_exhirom_custom() {
        assert_eq!(preset_kind_name(PresetKind::LoRom), "LoROM");
        assert_eq!(preset_kind_name(PresetKind::HiRom), "HiROM");
        assert_eq!(preset_kind_name(PresetKind::ExHiRom), "ExHiROM");
        assert_eq!(preset_kind_name(PresetKind::Sdd1), "S-DD1");
        assert_eq!(preset_kind_name(PresetKind::Spc7110), "SPC7110");
        assert_eq!(preset_kind_name(PresetKind::Sa1), "SA-1");
        assert_eq!(preset_kind_name(PresetKind::Custom), "Custom");
    }

    #[test]
    fn save_state_emits_envelope_and_round_trips_without_rom() {
        // Even with no ROM loaded the emulator can snapshot/restore its
        // volatile state. The bytes carry the retro-core magic prefix.
        let mut emu = SnesEmulator::new();
        emu.reset();
        let bytes = emu.save_state();
        assert!(!bytes.is_empty());
        assert!(retro_core::savestate::has_magic(&bytes));
        assert!(emu.load_state(&bytes).is_ok());
    }

    #[test]
    fn load_state_rejects_empty_and_garbage() {
        let mut emu = SnesEmulator::new();
        // No magic prefix -> clear error, not a panic.
        assert!(emu.load_state(&[]).is_err());
        assert!(emu.load_state(&[0xDE, 0xAD, 0xBE, 0xEF]).is_err());
    }

    #[test]
    fn save_ram_is_none_without_a_cartridge() {
        let mut emu = SnesEmulator::new();
        assert!(emu.get_save_ram().is_none());
        emu.set_save_ram(&[1, 2, 3]); // no cartridge installed -> no-op, must not panic
        assert!(emu.get_save_ram().is_none());
    }

    #[test]
    fn disassemble_decodes_65816_and_skips_other_cpus() {
        let mut emu = SnesEmulator::new();
        // Forward-decode `count` 65C816 instructions; addresses advance by
        // each instruction's byte length.
        let rows = emu.disassemble_at(0, 10);
        assert_eq!(rows.len(), 10);
        assert_eq!(rows[0].address, 0);
        assert!(rows[1].address > rows[0].address);
        assert!(!rows[0].mnemonic.is_empty());
        assert_eq!(emu.disassemble_cpu(0, 0, 10).len(), 10);
        // The SPC700 decoder is wired: cpu 1 forward-decodes its memory
        // (ARAM here) against a side-effect-free view.
        let spc = emu.disassemble_cpu(1, 0, 10);
        assert_eq!(spc.len(), 10);
        assert_eq!(spc[0].address, 0);
        assert!(!spc[0].mnemonic.is_empty());
        // Slot 2 stays empty: no cartridge is loaded to host a CPU there.
        assert!(emu.disassemble_cpu(2, 0, 10).is_empty());
    }

    #[test]
    fn read_memory_debug_returns_open_bus_when_no_rom() {
        let mut emu = SnesEmulator::new();
        let bytes = emu.read_memory_debug(0x00_8000, 4);
        assert_eq!(bytes.len(), 4);
        // No cart installed -> cartridge pages stay as OpenBus ->
        // reads return 0 from the MDR latch.
        assert_eq!(bytes, vec![0, 0, 0, 0]);
    }

    #[test]
    fn ppu_accessor_returns_post_reset_state() {
        let emu = SnesEmulator::new();
        // PPU is reset on construction -> display forced blank,
        // brightness 0, VRAM auto-increment step 1, all memories
        // zeroed.
        assert!(emu.ppu().display_disable());
        assert_eq!(emu.ppu().display_brightness(), 0);
        assert_eq!(emu.ppu().vram_incsize(), 1);
        assert!(emu.ppu().vram().iter().all(|&w| w == 0));
    }

    #[test]
    fn reset_zeros_framebuffer_bytes() {
        let mut emu = SnesEmulator::new();
        // Scribble through the framebuffer to prove `reset` clears.
        for byte in emu.framebuffer.data.iter_mut() {
            *byte = 0xAB;
        }
        emu.reset();
        assert!(emu.framebuffer.data.iter().all(|&b| b == 0));
    }

    #[test]
    fn run_frame_renders_visible_scanlines_via_render_pos() {
        // Forced blank + brightness 0 means the renderer writes
        // black RGB with alpha 0xFF on every visible line that
        // RenderPos fires. The renderer maps SNES scanline N -> host
        // fb row N-1, so a full frame should flip alpha to 0xFF on
        // rows 0..=223 (NTSC visible lines 1..=224 -> rows 0..=223).
        // Skipping happens at the renderer for scanline 0 (pre-
        // render), so there's no untouched row at the top after
        // the first frame.
        let mut emu = SnesEmulator::new();
        emu.reset();
        emu.run_frame();
        let stride = (emu.framebuffer.width * 4) as usize;
        // First host row (= SNES scanline 1) must be alpha=0xFF.
        assert_eq!(
            emu.framebuffer.data[3], 0xFF,
            "row 0 alpha set by render_scanline(scanline=1)"
        );
        // Last host row (= SNES scanline 224) must also be touched.
        let last_visible_offset = stride * 223 + 3;
        assert_eq!(
            emu.framebuffer.data[last_visible_offset], 0xFF,
            "row 223 alpha set by render_scanline(scanline=224)"
        );
    }

    #[test]
    fn framebuffer_allocates_max_hires_size() {
        // The emulator owns a 512x480 framebuffer regardless of the
        // live SNES mode so a hot-swap into Mode 5/6 (and interlace,
        // which doubles the height to 448/480) doesn't need a
        // reallocation. The host crops via the dynamic `DisplayInfo`
        // (see `get_display_info`).
        let emu = SnesEmulator::new();
        assert_eq!(emu.framebuffer.width, 512);
        assert_eq!(emu.framebuffer.height, 480);
        assert_eq!(emu.framebuffer.data.len(), 512 * 480 * 4);
    }

    #[test]
    fn display_info_sync_tracks_hires_and_overscan() {
        // Default reset: bgmode 0 + overscan off -> 256x224.
        let mut emu = SnesEmulator::new();
        emu.reset();
        assert_eq!(emu.get_display_info().width, 256);
        assert_eq!(emu.get_display_info().height, 224);

        // Poke Mode 5 + overscan via PPU registers. The reported size is
        // FRAME-GLOBAL (built each frame at scanline 1), so render one
        // scanline to set the flags before syncing, mirrors what
        // VBlankStart does (finalize + sync after the frame's scanlines
        // render) without needing a full CPU frame / ROM.
        // `$2105 = 0x05` -> bgmode 5; `$2133 = 0x04` -> overscan.
        // Any hi-res frame reports doubled height (bsnes doubles
        // non-interlace hi-res for display). By default overscan is
        // cropped to the TV window, so overscan Mode 5 -> 512x448.
        emu.bus.write(0x00_2105, 0x05);
        emu.bus.write(0x00_2133, 0x04);
        emu.bus.ppu_mut().render_scanline(1, &mut emu.framebuffer);
        emu.sync_display_info();
        assert_eq!(emu.get_display_info().width, 512);
        assert_eq!(emu.get_display_info().height, 448);

        // The overscan toggle exposes the full 512x480 frame and the
        // setter re-syncs display info immediately.
        emu.set_show_overscan(true);
        assert_eq!(emu.get_display_info().height, 480);
        emu.set_show_overscan(false);
        assert_eq!(emu.get_display_info().height, 448);

        // Mode 5 + interlace ($2133 bit 0, overscan off) -> 512x448
        // through the same path.
        emu.bus.write(0x00_2133, 0x01);
        emu.bus.ppu_mut().render_scanline(1, &mut emu.framebuffer);
        emu.sync_display_info();
        assert_eq!(emu.get_display_info().width, 512);
        assert_eq!(emu.get_display_info().height, 448);
    }

    #[test]
    fn mixed_mode_frame_doubles_lores_band_to_full_width() {
        // Simulate an HDMA mid-frame BGMODE sweep (as in the PPU bus
        // activity test): a lo-res band (Mode 0, fills cols 0..255 only)
        // followed by a hires band (Mode 5, fills all 512). The hires band
        // forces the frame-global width to 512; `finalize_frame` (run at
        // VBlankStart, before the size sync) must expand the lo-res band to
        // fill all 512 columns so the host doesn't see a half-width right
        // edge.
        let mut emu = SnesEmulator::new();
        emu.reset();
        emu.bus.write(0x00_2100, 0x0F); // display on, full brightness

        // Scanline 1: Mode 0 (lo-res) -> row 0 fills cols 0..255.
        emu.bus.write(0x00_2105, 0x00);
        emu.bus.ppu_mut().render_scanline(1, &mut emu.framebuffer);
        // Scanline 2: Mode 5 (hires) -> row 1 fills all 512 cols.
        emu.bus.write(0x00_2105, 0x05);
        emu.bus.ppu_mut().render_scanline(2, &mut emu.framebuffer);

        // The hires band forces a frame-global 512 width and (because
        // this frame is non-interlace hi-res) a doubled 448 height.
        assert_eq!(emu.bus.ppu().active_display_size(), (512, 448));

        // Run the finalize post-pass the way VBlankStart does.
        emu.bus.ppu().finalize_frame(&mut emu.framebuffer);

        // With no BG enabled the composite is the (black) backdrop, so
        // the doubled lo-res band's right half is now opaque black,
        // proof finalize filled cols 256..511 (untouched by the Mode 0
        // render, which only wrote cols 0..255).
        let stride = 512 * 4usize;
        let right = 300 * 4; // a right-half display column
        assert_eq!(
            &emu.framebuffer.data[right..right + 4],
            &[0, 0, 0, 0xFF][..],
            "lo-res band right half filled by finalize doubling"
        );
        // Non-interlace vertical doubling copies source row 0 to output
        // rows 0 and 1, so output row 1 mirrors the lo-res band.
        let dup = stride + right;
        assert_eq!(
            &emu.framebuffer.data[dup..dup + 4],
            &[0, 0, 0, 0xFF][..],
            "output row 1 is the vertical-doubled copy of lo-res row 0"
        );
        // The hires band (source row 1) moved to output rows 2..3 and
        // already spanned 512 columns before doubling.
        let p2 = 2 * stride + right;
        assert_eq!(
            &emu.framebuffer.data[p2..p2 + 4],
            &[0, 0, 0, 0xFF][..],
            "hires band lands at output row 2 after vertical doubling"
        );
    }

    #[test]
    fn handle_event_vblank_start_toggles_ppu_field() {
        let mut emu = SnesEmulator::new();
        emu.reset();
        // Fresh reset -> field = false.
        assert!(!emu.bus.ppu().field());
        emu.handle_event(EventKind::VBlankStart);
        assert!(emu.bus.ppu().field(), "field flips on first VBlankStart");
        emu.handle_event(EventKind::VBlankStart);
        assert!(
            !emu.bus.ppu().field(),
            "second VBlankStart returns to false"
        );
    }

    #[test]
    fn hblank_start_sets_hvbjoy_bit6_and_render_pos_clears_it() {
        // Without an HBlankStart handler $4212 bit 6 would stay clear, and
        // a game polling `lda $4212 / bit #$40` (Chrono Trigger's NMI
        // handler) would spin forever. HBlankStart (dot 274) must raise it;
        // RenderPos (dot 22, active display resumes) must lower it.
        let mut emu = SnesEmulator::new();
        emu.reset();
        assert_eq!(
            emu.bus.system_regs_mut().read(0x4212) & 0x40,
            0x00,
            "HBlank flag clear after reset"
        );
        emu.handle_event(EventKind::HBlankStart);
        assert_eq!(
            emu.bus.system_regs_mut().read(0x4212) & 0x40,
            0x40,
            "HBlankStart raises $4212 bit 6"
        );
        emu.handle_event(EventKind::RenderPos);
        assert_eq!(
            emu.bus.system_regs_mut().read(0x4212) & 0x40,
            0x00,
            "RenderPos lowers the HBlank flag"
        );
    }

    #[test]
    fn deferred_hdma_event_re_anchors_raster_snapshot_to_scheduled_hblank_dot() {
        // A long atomic general DMA folds many scanlines into one step, so
        // the scheduler drains a *batch* of per-line HDMA triggers at once
        // while the live `master_cycles` has already walked deep into a later
        // active-display line. Handling each trigger against `master_cycles`
        // would snapshot the raster inside the active-scan window (dots
        // 22..274), so the CGRAM active-scan gate would redirect the per-line
        // HDMA water-palette writes to the internal address (colour 0) and
        // the sea of Yoshi's Island's intro would flash white.
        // `handle_event_at` must instead re-anchor the snapshot to each
        // event's own scheduled H-blank dot (276).
        let mut emu = SnesEmulator::new();
        emu.reset();
        // Display on so the active-scan gate is armed (blanked => never gates).
        emu.bus.write(0x00_2100, 0x0F);

        // Poison the snapshot to a mid-active-display dot on a *later* visible
        // line, exactly where the live cursor sits after a long general DMA.
        // Dot 150 in [22, 274) and line 120 is visible, i.e. inside the gate
        // (a CGRAM write here would be redirected to colour 0).
        emu.bus.set_raster_snapshot(150, 120);

        // A per-line HDMA trigger for line 100 is armed at dot 276 (H-blank),
        // the exact cycle the scheduler assigns it (`line_base + 276 * 4`).
        let cps = emu.timing.cycles_per_scanline as u64;
        let trigger_cycle = emu.frame_base_cycle + 100 * cps + 276 * 4;
        assert_eq!(
            emu.raster_position(trigger_cycle),
            (100, 276),
            "the trigger's scheduled cycle decodes to line 100, dot 276"
        );

        emu.handle_event_at(trigger_cycle, EventKind::HdmaTrigger);

        // The snapshot now reflects the trigger's own H-blank dot, not the
        // poisoned mid-active cursor: dot 276 is >= the active-scan gate's
        // H-end (274), so the per-line HDMA CGRAM/OAM writes land at their
        // real target address instead of being redirected to colour 0.
        assert_eq!(
            (emu.bus.cached_h_dot(), emu.bus.cached_v_scanline()),
            (276, 100),
            "HdmaTrigger re-anchors the raster snapshot to its scheduled dot"
        );

        // HdmaInit (top-of-frame, dot 6) re-anchors the same way.
        emu.bus.set_raster_snapshot(150, 120);
        let init_cycle = emu.frame_base_cycle + 6 * 4;
        emu.handle_event_at(init_cycle, EventKind::HdmaInit);
        assert_eq!(
            (emu.bus.cached_h_dot(), emu.bus.cached_v_scanline()),
            (6, 0),
            "HdmaInit re-anchors the raster snapshot to its scheduled dot"
        );
    }

    #[test]
    fn vblank_and_hblank_flags_are_independent() {
        // The VBlank handlers must drive only bit 7, leaving the HBlank
        // bit (6) the scheduler manages untouched, otherwise the
        // VBlank edges would stomp an in-progress HBlank flag.
        let mut emu = SnesEmulator::new();
        emu.reset();
        emu.handle_event(EventKind::VBlankStart);
        emu.handle_event(EventKind::HBlankStart);
        assert_eq!(
            emu.bus.system_regs_mut().read(0x4212) & 0xC0,
            0xC0,
            "VBlank (bit 7) and HBlank (bit 6) coexist"
        );
        emu.handle_event(EventKind::VBlankEnd);
        assert_eq!(
            emu.bus.system_regs_mut().read(0x4212) & 0xC0,
            0x40,
            "VBlankEnd clears bit 7 but preserves the HBlank bit 6"
        );
    }

    #[test]
    fn handle_event_vblank_start_resets_oam_address_when_display_enabled() {
        let mut emu = SnesEmulator::new();
        emu.reset();
        emu.bus.write(0x00_2100, 0x0F); // display on, full brightness
        emu.bus.write(0x00_2102, 0x10); // OAMADDL -> re-anchors oam_addr to base
        let base = emu.bus.ppu().oam_addr();
        // Two OAMDATA writes walk the pointer off the base.
        emu.bus.write(0x00_2104, 0xAA);
        emu.bus.write(0x00_2104, 0xBB);
        assert_ne!(
            emu.bus.ppu().oam_addr(),
            base,
            "writes advanced the pointer"
        );

        emu.handle_event(EventKind::VBlankStart);
        assert_eq!(
            emu.bus.ppu().oam_addr(),
            base,
            "VBlank entry re-anchors OAM address to base when display is on"
        );
    }

    #[test]
    fn handle_event_vblank_start_keeps_oam_address_in_forced_blank() {
        let mut emu = SnesEmulator::new();
        emu.reset();
        emu.bus.write(0x00_2100, 0x80); // forced blank -> display disabled
        emu.bus.write(0x00_2102, 0x10);
        emu.bus.write(0x00_2104, 0xAA);
        emu.bus.write(0x00_2104, 0xBB);
        let advanced = emu.bus.ppu().oam_addr();

        emu.handle_event(EventKind::VBlankStart);
        assert_eq!(
            emu.bus.ppu().oam_addr(),
            advanced,
            "forced blank suppresses the VBlank OAM reset"
        );
    }

    #[test]
    fn step_cpu_refreshes_bus_raster_snapshot() {
        let mut emu = SnesEmulator::new();
        // The frame base starts at 0. Stepping the CPU once should
        // advance the master clock and refresh the snapshot to the
        // frame-relative raster position at that boundary.
        emu.step();
        let (v, h) = emu.raster_position(emu.master_cycles);
        assert_eq!(emu.bus.cached_h_dot(), h);
        assert_eq!(emu.bus.cached_v_scanline(), v);
    }

    #[test]
    fn vblank_nmi_reads_vblank_vcounter_after_frame_overshoot() {
        // Regression: the CPU-visible vcounter must be decoded against
        // `frame_base_cycle`, not an absolute `master_cycles %
        // cycles_per_frame` grid. With the frame base pushed off the
        // absolute grid (the overshoot a real run accumulates), the
        // VBlankStart event and the `$213D` latch must still agree that
        // the raster is at the vblank start line.
        let mut emu = SnesEmulator::new();
        let cps = emu.timing.cycles_per_scanline as u64;
        let vbl = emu.timing.vblank_start_line as u64;
        // Anchor the frame base ~100 scanlines off the absolute grid.
        emu.frame_base_cycle = 100 * cps + 37;
        // The VBlankStart event is armed at frame_base + vbl * cps.
        emu.master_cycles = emu.frame_base_cycle + vbl * cps;
        let (v, _h) = emu.raster_position(emu.master_cycles);
        assert_eq!(
            v, emu.timing.vblank_start_line,
            "vcounter at the VBlankStart cycle must read the vblank start line \
             regardless of how far frame_base_cycle sits off the absolute grid"
        );
    }

    /// Write a `lda $4210 / bpl` vblank-wait at `base` in low RAM
    /// (the `$00:0000-$1FFF` WRAM mirror the CPU can fetch from).
    fn write_vblank_poll_loop(emu: &mut SnesEmulator, base: u32) {
        emu.bus.write(base, 0xAD); // LDA $4210 (absolute)
        emu.bus.write(base + 1, 0x10);
        emu.bus.write(base + 2, 0x42);
        emu.bus.write(base + 3, 0x10); // BPL -5 -> back to the LDA
        emu.bus.write(base + 4, 0xFB);
    }

    /// Phase A: the in-flight instruction at VBlank is the poll's
    /// `LDA $4210`. It reads the freshly-latched flag the very first
    /// step, so delivery releases the NMI immediately, but only *after*
    /// the poll observed bit 7.
    #[test]
    fn nmi_delivery_releases_after_poll_observes_flag_lda_in_flight() {
        let mut emu = SnesEmulator::new();
        emu.reset();
        emu.bus.write(0x00_4200, 0x80); // NMITIMEN: NMI on, no auto-joypad
        write_vblank_poll_loop(&mut emu, 0x0200);
        emu.cpu.pbr = 0;
        emu.cpu.dbr = 0;
        emu.cpu.pc = 0x0200; // in-flight = LDA $4210

        emu.handle_event(EventKind::VBlankStart);
        assert!(
            emu.nmi_deliver_armed,
            "VBlankStart arms delivery with NMI enabled"
        );
        assert!(
            !emu.cpu.nmi_pending,
            "NMI must not be vectored at the boundary"
        );

        emu.step_cpu_internal();
        assert_eq!(emu.cpu.c & 0xFF, 0x80, "the poll read the latched NMI flag");
        assert!(
            emu.cpu.nmi_pending,
            "NMI delivered once the poll observed the flag"
        );
        assert!(!emu.nmi_deliver_armed, "delivery disarmed after firing");
    }

    /// Phase B: the in-flight instruction is the poll's `BPL`. A fixed
    /// 1-instruction window loses here (the NMI vectors before the next
    /// `LDA` can read the flag, so the handler clears it and the loop
    /// spins forever: the Chrono Trigger black screen). Poll-aware
    /// delivery holds off until the following `LDA` observes bit 7.
    #[test]
    fn nmi_delivery_holds_off_until_poll_reads_flag_bpl_in_flight() {
        let mut emu = SnesEmulator::new();
        emu.reset();
        emu.bus.write(0x00_4200, 0x80);
        write_vblank_poll_loop(&mut emu, 0x0200);
        emu.cpu.pbr = 0;
        emu.cpu.dbr = 0;
        emu.cpu.pc = 0x0203; // in-flight = BPL
        emu.cpu.p &= !0x80; // N clear -> BPL taken (poll was reading $00)

        emu.handle_event(EventKind::VBlankStart);

        // The BPL reads no register: the NMI must stay armed, not fire.
        emu.step_cpu_internal();
        assert_eq!(emu.cpu.pc, 0x0200, "BPL branched back to the LDA");
        assert!(
            !emu.cpu.nmi_pending,
            "NMI held off until the poll reads the flag"
        );
        assert!(
            emu.nmi_deliver_armed,
            "still armed after the non-reading instruction"
        );

        // The LDA now observes the flag -> NMI released.
        emu.step_cpu_internal();
        assert_eq!(emu.cpu.c & 0xFF, 0x80, "the poll read the latched NMI flag");
        assert!(
            emu.cpu.nmi_pending,
            "NMI delivered after the poll observed the flag"
        );
        assert!(!emu.nmi_deliver_armed);
    }

    /// A main thread that drives NMI without polling `$4210` never sets
    /// the observed flag; delivery falls back to the bounded window so the
    /// NMI still fires promptly.
    #[test]
    fn nmi_delivery_falls_back_to_window_when_main_thread_does_not_poll() {
        let mut emu = SnesEmulator::new();
        emu.reset();
        emu.bus.write(0x00_4200, 0x80);
        for i in 0..8 {
            emu.bus.write(0x00_0200 + i, 0xEA); // NOP sled, never reads $4210
        }
        emu.cpu.pbr = 0;
        emu.cpu.pc = 0x0200;

        emu.handle_event(EventKind::VBlankStart);
        for n in 1..NMI_DELIVERY_MAX_INSNS {
            emu.step_cpu_internal();
            assert!(
                !emu.cpu.nmi_pending,
                "not delivered before the window elapses (step {n})"
            );
            assert!(emu.nmi_deliver_armed);
        }
        emu.step_cpu_internal();
        assert!(
            emu.cpu.nmi_pending,
            "NMI delivered when the fallback window expires"
        );
        assert!(!emu.nmi_deliver_armed);
    }

    #[test]
    fn the_third_cpu_reports_its_own_clock() {
        assert_eq!(secondary_cpu_info(None).clock_speed, 10_740_000);
        assert_eq!(
            secondary_cpu_info(Some(PresetKind::SuperFx)).clock_speed,
            10_740_000
        );
        assert_eq!(
            secondary_cpu_info(Some(PresetKind::Cx4)).clock_speed,
            20_000_000
        );
    }
}
