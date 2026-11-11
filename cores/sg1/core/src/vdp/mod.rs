//! Video Display Processor emulation.
//!
//! The VDP is responsible for graphics rendering on the Genesis.

pub mod debug;
pub mod dma;
pub mod planes;
pub mod renderer;
pub mod sprites;
pub mod tiles;
pub mod timing;
pub mod window;

use window::WindowArea;

/// VDP access codes (CD0-CD5 bits from control port)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VdpAccessMode {
    VramRead = 0x00,
    VramWrite = 0x01,
    CramWrite = 0x03,
    VsramWrite = 0x05,
    CramRead = 0x08,
    VsramRead = 0x04,
}

/// Master clocks per scanline (NTSC and PAL both ~3420; PAL differs in line
/// count, not line length). Shared time base for DMA metering and FIFO slots.
const MASTER_CLOCKS_PER_LINE: u64 = 3420;

/// Master-clock offsets within a scanline at which the VDP serves one
/// external (CPU or DMA) access during active display, the FIFO's slots,
/// for H32 and for H40.
///
/// The chip spends most of a line fetching its own data and leaves the
/// bus to the FIFO at fixed values of the H counter: sixteen a line in
/// H32, eighteen in H40 ([`ACCESS_SLOTS_H32`], [`ACCESS_SLOTS_H40`]). The
/// tables turn those positions into master clocks counted from the point
/// our scanline starts, in ascending order, and then list the line's
/// first twelve slots once more, one line later (`+ MASTER_CLOCKS_PER_LINE`), so
/// a write chained past the end of the current line resolves into the
/// next.
const FIFO_TIMING_H32: [u64; 28] = fifo_slot_table(false, &ACCESS_SLOTS_H32);
const FIFO_TIMING_H40: [u64; 30] = fifo_slot_table(true, &ACCESS_SLOTS_H40);

/// How many of a line's slots each table repeats for the next line.
const FIFO_SLOTS_OF_NEXT_LINE: usize = 12;

/// The H-counter values (the chip's internal nine-bit counter) at which an
/// external access is served during active display. Within the picture a
/// slot comes every sixteen pixels, three in a row and then one the chip
/// keeps for refreshing its memory; the last entries are the slots of the
/// blanking area. Hardware measurements, by Nemesis, as the Exodus
/// emulator records them (`S315-5313_Timing.cpp`, MIT).
const ACCESS_SLOTS_H32: [u16; 16] = [
    0x005, 0x015, 0x025, 0x045, 0x055, 0x065, 0x085, 0x095, 0x0A5, 0x0C5, 0x0D5, 0x0E5, 0x103,
    0x105, 0x121, 0x1E7,
];
const ACCESS_SLOTS_H40: [u16; 18] = [
    0x002, 0x012, 0x022, 0x042, 0x052, 0x062, 0x082, 0x092, 0x0A2, 0x0C2, 0x0D2, 0x0E2, 0x102,
    0x112, 0x122, 0x140, 0x142, 0x1CE,
];

/// The H counter does not count straight through a line: after the first
/// run of values it jumps over a gap during horizontal blanking and
/// finishes the line on the second run. `(first, last)` of each run.
const HCOUNTER_RUNS_H32: [(u16, u16); 2] = [(0x000, 0x127), (0x1D2, 0x1FF)];
const HCOUNTER_RUNS_H40: [(u16, u16); 2] = [(0x000, 0x16C), (0x1C9, 0x1FF)];

/// The H-counter value our scanline's master-clock count starts at: where
/// the V counter steps in H32, three pixels before that in H40.
const LINE_ORIGIN_H32: u16 = 0x10A;
const LINE_ORIGIN_H40: u16 = 0x147;

/// In H40 the console feeds the chip a slowed dot clock through horizontal
/// sync, these H-counter values.
const H40_SYNC: (u16, u16) = (0x1CD, 0x1EC);

/// H40's slot inside horizontal sync. By the documented dot clock it falls
/// 346 master clocks into our scanline; this core serves it 6 later (the
/// figure Genesis Plus GX uses), so that recorded runs do not move.
const H40_SYNC_SLOT: u16 = 0x1CE;
const H40_SYNC_SLOT_LATENESS: u64 = 6;

/// Master clocks one H-counter value lasts. H32 divides the master clock
/// by ten throughout. H40 divides it by eight, except through horizontal
/// sync, where each pixel is two ticks of the slowed clock, five master
/// clocks a tick with a pair of four after every fifteen, and on the one
/// pixel after it, which takes ten.
const fn dot_master_clocks(h40: bool, hcounter: u16) -> u64 {
    if !h40 {
        return 10;
    }
    if hcounter >= H40_SYNC.0 && hcounter <= H40_SYNC.1 {
        let tick = (hcounter - H40_SYNC.0) as u64 * 2;
        return h40_sync_tick(tick) + h40_sync_tick(tick + 1);
    }
    if hcounter == H40_SYNC.1 + 1 {
        return 10;
    }
    8
}

/// One tick of the slowed H40 clock, in master clocks: groups of seventeen
/// ticks, fifteen of five and then two of four.
const fn h40_sync_tick(tick: u64) -> u64 {
    if tick % 17 >= 15 {
        4
    } else {
        5
    }
}

/// The H-counter value that follows `hcounter`.
const fn next_hcounter(h40: bool, hcounter: u16) -> u16 {
    let runs = if h40 {
        HCOUNTER_RUNS_H40
    } else {
        HCOUNTER_RUNS_H32
    };
    if hcounter == runs[0].1 {
        runs[1].0
    } else if hcounter == runs[1].1 {
        runs[0].0
    } else {
        hcounter + 1
    }
}

/// Master clocks from the start of our scanline to the moment the H
/// counter reaches `hcounter`.
const fn master_clocks_to(h40: bool, hcounter: u16) -> u64 {
    let mut at = if h40 {
        LINE_ORIGIN_H40
    } else {
        LINE_ORIGIN_H32
    };
    let mut clocks = 0;
    while at != hcounter {
        clocks += dot_master_clocks(h40, at);
        at = next_hcounter(h40, at);
    }
    clocks
}

/// Build one slot table: each slot's time within the line in ascending
/// order, then the first [`FIFO_SLOTS_OF_NEXT_LINE`] again one line later.
const fn fifo_slot_table<const SLOTS: usize, const LEN: usize>(
    h40: bool,
    slots: &[u16; SLOTS],
) -> [u64; LEN] {
    assert!(LEN == SLOTS + FIFO_SLOTS_OF_NEXT_LINE);
    let mut times = [0u64; SLOTS];
    let mut i = 0;
    while i < SLOTS {
        let mut time = master_clocks_to(h40, slots[i]);
        if h40 && slots[i] == H40_SYNC_SLOT {
            time += H40_SYNC_SLOT_LATENESS;
        }
        // Insert in ascending order: the slots are listed as the H counter
        // counts, and our scanline starts part of the way through it.
        let mut at = i;
        while at > 0 && times[at - 1] > time {
            times[at] = times[at - 1];
            at -= 1;
        }
        times[at] = time;
        i += 1;
    }
    let mut table = [0u64; LEN];
    let mut k = 0;
    while k < LEN {
        table[k] = if k < SLOTS {
            times[k]
        } else {
            MASTER_CLOCKS_PER_LINE + times[k - SLOTS]
        };
        k += 1;
    }
    table
}

/// Video Display Processor.
pub struct Vdp {
    // Memory
    vram: Vec<u8>,  // 64KB VRAM
    cram: Vec<u16>, // 64 colors (9-bit RGB stored as 16-bit)
    // 64 addressable words of vertical scroll RAM. Real silicon holds 40
    // physical 11-bit entries (used by the renderer), but the VDP address
    // counter masks to `addr & 0x7E` (0..=63 words), so writes to slots 40..63
    // are kept; a data-port read there returns slot 0 (`prefetch_read_buffer`).
    // VDPFIFOTesting "DMA Fill to VSRAM" exercises the full range.
    vsram: Vec<u16>,

    // Registers (24 total)
    registers: [u8; 24],

    // Framebuffer (RGBA format)
    framebuffer: Vec<u8>, // 320x224x4 bytes

    // Access state
    control_word: u32,
    pending_write: bool,
    address: u16,
    code: u8,

    // Timing and interrupts
    timing: timing::VdpTiming,

    // DMA state
    dma: dma::DmaState,
    dma_fill_value: u16,
    dma_pending: bool, // Memory-to-VRAM DMA pending (needs CPU memory access)
    dma_busy: bool,    // VDP-internal DMA (VRAM Fill/Copy) running in the background
    dma_remaining_clocks: u64, // Master clocks left before a background Fill/Copy clears its busy bit
    // Master clocks the 68000 must stall for an armed 68k-bus DMA (to
    // VRAM/CRAM/VSRAM). The frame loop drains this into the per-line CPU budget
    // so the CPU is frozen for the transfer, as on the console (and as
    // Genesis Plus GX holds the 68000). 68k-bus DMA leaves the DMA-busy status bit
    // clear (the CPU is halted instead); only Fill/Copy assert that bit.
    pending_cpu_dma_mclks: u64,

    // Approximate 68K beam position within the current scanline, refreshed each
    // instruction by `set_hblank_approx`. Used to derive a meaningful H counter
    // for `0xC00008` reads (instead of a constant ~0). Transient within-line
    // state, so it is not part of the savestate.
    beam_cycles_into_line: u32,
    beam_cycles_per_line: u32,

    // Sprite cache (rebuilt when SAT changes or at frame start)
    sprite_cache: sprites::SpriteCache,
    sprite_cache_valid: bool,

    // Sprite status flags
    sprite_overflow: bool,  // Set when >20 sprites on a scanline (>16 in H32)
    sprite_collision: bool, // Set when two non-transparent sprite pixels overlap

    // Sprite masking: on the Mega Drive, a sprite with X position = 0 (raw SAT value)
    // masks all subsequent sprites on the same scanline, provided at least one sprite
    // with X > 0 was already processed. This flag carries over from the previous line
    // when the sprite pixel limit is reached.
    sprite_mask_overflow: bool,

    // Interlace state
    odd_frame: bool, // For interlace mode: true = odd field, false = even field

    // VDP Write FIFO (4 entries deep)
    // The VDP has a 4-word FIFO for data port writes. When the CPU writes to the
    // data port, the value is pushed into the FIFO. The VDP drains the FIFO during
    // available "access slots" - fewer during active display, more during blanking.
    // If the FIFO is full, the CPU is stalled until a slot opens.
    fifo_entries: u8,       // Current number of entries (0-4)
    fifo_mclk_counter: u64, // Master clocks accumulated for drain timing

    // FIFO data ring-buffer (the actual words written to the data port).
    // The VDP only drives data-bus bits 0..N for VSRAM (11)/CRAM (the BBB0GGG0RRR0
    // pattern)/byte VRAM reads; the remaining "unused" bits float to the most
    // recent FIFO contents (open bus). A read takes those bits from the oldest
    // of the four entries (checked against Genesis Plus GX). `fifo_idx` is the
    // next write slot, so `fifo[fifo_idx]` is that oldest entry (4 writes ago).
    fifo: [u16; 4],
    fifo_idx: usize,

    // Cycle-accurate FIFO empty/full reporting: one service time per entry
    // (the model Genesis Plus GX uses).
    // `fifo_clock` is a monotonic VDP master-clock counter advanced once per
    // CPU instruction in `tick_fifo`; `fifo_slot_mclks[i]` is the absolute
    // master clock at which the entry in ring slot `i` is serviced (drained).
    // The status FIFO-empty (bit 9) / full (bit 8) bits compare `fifo_clock`
    // against these timestamps instead of a running entry count, so a rapid
    // burst that out-paces the active-display access slots reads back as full
    // (VDPFIFOTesting "FIFO Full Before DMA Transfer"). Both are transient
    // sub-frame timing state (like `beam_cycles_into_line`), not savestated.
    fifo_clock: u64,
    fifo_slot_mclks: [u64; 4],

    /// CPU wait cycles from the last VDP operation (FIFO stall or DMA halt).
    /// Read and cleared by the bus after each VDP write.
    pub pending_cpu_wait_cycles: u32,

    // Pre-allocated per-scanline buffers (avoids 448 heap allocations per frame)
    sprite_pixel_buffer: Vec<bool>,

    // Sprite line buffer: pre-rendered sprite pixels for the current scanline.
    // Each entry: (color_index, palette, priority, has_pixel).
    // Sprites are rendered into this buffer ONCE per scanline, then compositing
    // just reads from it -- eliminating 71,680 Vec allocs + sprite iterations/frame.
    sprite_line_color: Vec<u8>,      // color_index (0 = transparent)
    sprite_line_palette: Vec<u8>,    // palette index (0-3)
    sprite_line_priority: Vec<bool>, // priority bit
    // Scratch buffer for sprite indices on current line (avoids Vec alloc)
    sprites_on_line_buf: Vec<u16>,

    // Per-line composited palette-index buffers (two-stage line renderer).
    // `composite_scanline` resolves BG + sprite priority into these as CRAM
    // indices; `remap_scanline` turns them into RGB via `cram_rgb`. Holding the
    // resolved indices (not RGB) lets a mid-line CRAM write re-tint the line by
    // re-running only the remap pass, the basis for raster palette effects.
    // Transient within-line state, never part of the savestate.
    line_index: Vec<u8>,      // final resolved CRAM index (0..63) per pixel
    line_sh_op: Vec<u8>,      // shadow/highlight operator (0 / 14 / 15)
    line_high_pri: Vec<bool>, // pixel resolved high-priority (S/H "normal" path)
    line_plane_a: Vec<i16>,   // plane A CRAM index, -1 transparent or window (layer modes)
    line_window: Vec<i16>,    // window CRAM index, -1 transparent or plane A (layer modes)
    line_plane_b: Vec<i16>,   // plane B CRAM index, -1 transparent (layer modes)

    // CRAM RGB lookup table: precomputed (r,g,b) for all 64 CRAM entries.
    // Updated whenever CRAM is written. Eliminates ~143K convert_genesis_color() calls/frame.
    cram_rgb: [(u8, u8, u8); 64],

    // VDP read buffer (prefetch). On real hardware, when a read command is set up
    // the VDP prefetches the first word into this buffer and auto-increments the
    // address. Data port reads return the buffer value and load the next word.
    read_buffer: u16,

    // Optional VRAM watch range for tracing writes to specific addresses.
    watch_vram: Option<(u16, u16)>,

    // Per-pixel composition diagnostic: when set, logs all composition
    // candidates for this (x, y) screen pixel during render_scanline.
    debug_pixel: Option<(u16, u16)>,

    // Layer-only rendering mode: when set, render_scanline only outputs the
    // specified layer (for debug visualization).
    render_layer_mode: RenderLayerMode,

    // Per-layer debugger force-disable mask (Plane A, Plane B, Window,
    // Sprites). A set bit suppresses that layer from composition so the
    // debugger can isolate each plane's contribution, without touching the
    // game's real VDP registers. Always all-`false` during normal play.
    debug_layer_disable: [bool; LAYER_COUNT],

    // A 68k-bus CRAM DMA that was triggered while the beam is in active display
    // (a "raster" palette/backdrop effect, e.g. Direct Color DMA). Such a
    // transfer is metered across the remaining visible scanlines instead of
    // landing instantly, so each line renders with the CRAM state the real DMA
    // would have reached by then. CRAM DMA during blanking, what ordinary
    // games do, stays instant; only the rare mid-active case takes this path.
    // Transient within-frame state, never part of the savestate.
    progressive_cram: Option<ProgressiveCramDma>,

    /// Latched HV counter (`Some` once reg0 bit1 / M3 is set in Mode 5). While
    /// latched, HV-counter reads return this frozen `(V<<8)|H` value instead of
    /// the live beam position; clearing reg0 bit1 frees it again. The freeze
    /// a lightgun and Sunset Riders rely on, and what VDPFIFOTesting "HV
    /// Counter Latch" checks (checked against Genesis Plus GX too).
    hvc_latch: Option<u16>,

    /// A VRAM Fill armed by a control-port write (CD5 + mode 0b10 while DMA is
    /// enabled) but not yet started by its data-port write. A one-shot, as
    /// in Genesis Plus GX: set at the arm, consumed (cleared) by the *first*
    /// data-port write that triggers the fill, so subsequent data writes, the
    /// ones the "DP Writes During DMA Fill" subtests issue mid-fill, never
    /// re-arm a fresh fill. Transient within-frame state, never serialized.
    dma_fill_pending: bool,

    /// In-flight VRAM/CRAM/VSRAM DMA fill metered cell-by-cell across master
    /// clocks instead of completing instantly, so a data/control-port access
    /// that arrives mid-fill observes the partially-advanced address, the
    /// VDPFIFOTesting "DP Writes During DMA Fill" / "DMA Fill Control Port
    /// Writes" / "DMA Busy Flag - Toggle Fill" subtests. The fill streams
    /// through the live `self.address`/`self.code` and re-reads its source word
    /// from the FIFO on every cell (a concurrent data write therefore redirects
    /// nothing, since the address is shared, but *re-sources* the rest of the
    /// fill to the just-written value, exactly as the hardware snoops the bus).
    /// Transient within-frame state, never part of the savestate.
    fill_state: Option<FillState>,
}

/// What a DMA moves, which sets how much of it one scanline carries
/// ([`Vdp::dma_line_rate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DmaTransfer {
    /// 68000 bus to CRAM or VSRAM: a word in each access slot.
    BusToCramOrVsram,
    /// 68000 bus to VRAM: a word is two byte accesses.
    BusToVram,
    /// VRAM fill: a byte in each access slot.
    Fill,
    /// VRAM copy: a read and a write for each byte.
    Copy,
}

/// A CRAM memory-DMA captured mid-active-display and applied
/// progressively, one chunk per scanline, to approximate the per-line palette
/// changes a real metered DMA produces (as Genesis Plus GX meters it). Full per-pixel
/// fidelity (PicoDrive) would require cycle-accurate mid-line CRAM painting,
/// which SG1's scanline-granular renderer cannot express.
#[derive(Debug, Clone)]
struct ProgressiveCramDma {
    /// Owned copy of the source words already read from the 68K bus.
    buffer: Vec<u8>,
    /// CRAM byte address at the start of the transfer.
    dest_start: u16,
    /// Auto-increment applied to the destination per word.
    auto_inc: u16,
    /// Words transferred per scanline to CRAM (the chip's DMA rates): 16/18 with the
    /// display on (H32/H40) or 161/198 when it is forcibly blanked, the case
    /// Direct Color DMA uses to stream a full picture in one frame.
    rate: usize,
    /// Word index, within a line's `rate`-word group, whose color the backdrop
    /// shows when the line is rendered. Genesis Plus GX's mid-line palette
    /// update makes the visible backdrop land ~83% into the group (measured:
    /// word 164 of each 198 gives its picture exactly for the H40 demo), not on the
    /// black right-edge word that ends each picture row.
    sample_offset: usize,
    /// Total words in the transfer.
    length: usize,
    /// Words applied to CRAM so far.
    cursor: usize,
}

/// A VRAM/CRAM/VSRAM DMA fill in progress, metered out of `tick_dma` one cell
/// at a time. The destination address and target code are read live from the
/// VDP (`self.address`/`self.code`) and the source word is re-read from the
/// FIFO on every cell, so a control-port write that lands mid-fill redirects
/// the remaining cells and a data-port write re-sources them (VDPFIFOTesting
/// "DMA Fill Control Port Writes" / "DP Writes During DMA Fill"). The
/// source-register writeback and length-register clear happen up front at the
/// trigger (in `write_data_port`); only the cell writes and the address advance
/// are deferred here.
#[derive(Clone, Copy)]
struct FillState {
    /// Fill cells still to write after the initial cell (the DMA length at the trigger).
    remaining: u32,
    /// Fractional `cells * mclk` carry so the per-tick rate integrates exactly.
    cell_accum: u64,
}

/// Number of toggleable Genesis VDP debug layers: Plane A, Plane B,
/// Window, and Sprites. The backdrop is always composited and is not
/// part of the toggle set.
pub(crate) const LAYER_COUNT: usize = 4;
/// Debug-layer index of the scrolling foreground plane.
pub(crate) const LAYER_PLANE_A: usize = 0;
/// Debug-layer index of the scrolling background plane.
pub(crate) const LAYER_PLANE_B: usize = 1;
/// Debug-layer index of the fixed window plane (replaces Plane A in its
/// region on real hardware).
pub(crate) const LAYER_WINDOW: usize = 2;
/// Debug-layer index of the sprite (OBJ) plane.
pub(crate) const LAYER_SPRITES: usize = 3;

/// The colour the single-layer views ([`RenderLayerMode`]) and the CLI's
/// tile exports give a transparent pixel: magenta, so black text on a
/// transparent plane stays visible. Its blue, 254, is a level no VDP colour
/// takes, so a pixel of exactly this colour is transparent, never the game's.
pub const TRANSPARENT_RGB: (u8, u8, u8) = (0xFF, 0x00, 0xFE);

/// Which layer(s) to render (for debug visualization).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderLayerMode {
    /// Normal composited output
    Normal,
    /// Plane B only (transparent pixels -> [`TRANSPARENT_RGB`])
    PlaneB,
    /// Plane A only, where the window does not cover (transparent pixels
    /// and the window's part -> [`TRANSPARENT_RGB`])
    PlaneA,
    /// Window only, where it covers (transparent pixels and plane A's
    /// part -> [`TRANSPARENT_RGB`])
    Window,
    /// Sprites only (transparent pixels -> [`TRANSPARENT_RGB`])
    Sprites,
    /// Normal output with priority overlay (high-pri pixels tinted green)
    PriorityOverlay,
    /// Sprites colored by their priority bit (red=high, blue=low)
    SpritePriority,
}

impl Vdp {
    /// Create a new VDP.
    pub fn new() -> Self {
        let mut vdp = Self {
            vram: vec![0; 0x10000], // 64KB VRAM
            cram: vec![0; 64],      // 64 colors
            vsram: vec![0; 64],     // Vertical scroll (64 addressable words)
            registers: [0; 24],
            framebuffer: vec![0; 320 * 224 * 4], // RGBA
            control_word: 0,
            pending_write: false,
            address: 0,
            code: 0,
            timing: timing::VdpTiming::new(timing::VideoRegion::Ntsc),
            dma: dma::DmaState::new(),
            dma_fill_value: 0,
            dma_pending: false,
            dma_busy: false,
            dma_remaining_clocks: 0,
            pending_cpu_dma_mclks: 0,
            progressive_cram: None,
            hvc_latch: None,
            dma_fill_pending: false,
            fill_state: None,
            beam_cycles_into_line: 0,
            beam_cycles_per_line: 0,
            sprite_cache: sprites::SpriteCache::new(),
            sprite_cache_valid: false,
            sprite_overflow: false,
            sprite_collision: false,
            sprite_mask_overflow: false,
            odd_frame: false,
            fifo_entries: 0,
            fifo_mclk_counter: 0,
            fifo: [0; 4],
            fifo_idx: 0,
            fifo_clock: 0,
            fifo_slot_mclks: [0; 4],
            pending_cpu_wait_cycles: 0,
            sprite_pixel_buffer: vec![false; 320],
            sprite_line_color: vec![0u8; 320],
            sprite_line_palette: vec![0u8; 320],
            sprite_line_priority: vec![false; 320],
            sprites_on_line_buf: Vec::with_capacity(20),
            line_index: vec![0u8; 320],
            line_sh_op: vec![0u8; 320],
            line_high_pri: vec![false; 320],
            line_plane_a: vec![-1i16; 320],
            line_window: vec![-1i16; 320],
            line_plane_b: vec![-1i16; 320],
            cram_rgb: [(0, 0, 0); 64],
            read_buffer: 0,
            watch_vram: None,
            debug_pixel: None,
            render_layer_mode: RenderLayerMode::Normal,
            debug_layer_disable: [false; LAYER_COUNT],
        };

        // Initialize with default register values
        vdp.reset();
        vdp
    }

    /// Reset the VDP to initial state.
    pub fn reset(&mut self) {
        // Default register values
        self.registers[0] = 0x04; // Mode register 1
        self.registers[1] = 0x04; // Mode register 2
        self.registers[12] = 0x81; // Mode register 4 (H40 mode)
        self.registers[15] = 0x02; // Auto-increment

        // Clear memory
        self.vram.fill(0);
        self.cram.fill(0);
        self.vsram.fill(0);

        // Reset state
        self.control_word = 0;
        self.pending_write = false;
        self.address = 0;
        self.code = 0;
        self.timing.reset();
        self.dma_pending = false;
        self.dma_busy = false;
        self.dma_remaining_clocks = 0;
        self.pending_cpu_dma_mclks = 0;
        self.progressive_cram = None;
        self.hvc_latch = None;
        self.dma_fill_pending = false;
        self.fill_state = None;
        self.beam_cycles_into_line = 0;
        self.beam_cycles_per_line = 0;
        self.sprite_cache_valid = false;
        self.sprite_overflow = false;
        self.sprite_collision = false;
        self.sprite_mask_overflow = false;
        self.fifo_entries = 0;
        self.fifo_mclk_counter = 0;
        self.fifo = [0; 4];
        self.fifo_idx = 0;
        self.fifo_clock = 0;
        self.fifo_slot_mclks = [0; 4];
        self.pending_cpu_wait_cycles = 0;
        self.odd_frame = false;

        // Render backdrop color
        self.render_backdrop();
    }

    /// Get the framebuffer for display.
    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer
    }

    /// Get display width based on register 12 (H40/H32 mode).
    pub fn display_width(&self) -> u32 {
        if (self.registers[12] & 0x01) != 0 {
            320 // H40 mode
        } else {
            256 // H32 mode
        }
    }

    /// Enable VRAM write tracing for a given address range (inclusive).
    pub fn set_watch_vram(&mut self, start: u16, end: u16) {
        self.watch_vram = Some((start, end));
    }

    /// Set a pixel coordinate to debug. During render_scanline, all composition
    /// candidates for this pixel will be logged to stderr.
    pub fn set_debug_pixel(&mut self, x: u16, y: u16) {
        self.debug_pixel = Some((x, y));
    }

    /// Clear the debug pixel.
    pub fn clear_debug_pixel(&mut self) {
        self.debug_pixel = None;
    }

    /// Set the layer-only rendering mode for debug visualization.
    pub fn set_render_layer_mode(&mut self, mode: RenderLayerMode) {
        self.render_layer_mode = mode;
    }

    /// Force-disable (`true`) or re-enable (`false`) debug layer `layer`
    /// (`0`=Plane A, `1`=Plane B, `2`=Window, `3`=Sprites). A disabled
    /// layer is suppressed from composition without touching the game's
    /// real VDP registers; this is debugger-only state and is never
    /// persisted in a savestate. Out-of-range indices are ignored.
    pub fn set_layer_debug_disabled(&mut self, layer: usize, disabled: bool) {
        if layer < LAYER_COUNT {
            self.debug_layer_disable[layer] = disabled;
        }
    }

    /// Whether debug layer `layer` is currently force-disabled by the
    /// debugger. Out-of-range indices read back `false`.
    pub fn layer_debug_disabled(&self, layer: usize) -> bool {
        self.debug_layer_disable
            .get(layer)
            .copied()
            .unwrap_or(false)
    }

    /// Re-render every visible scanline from the present VDP state into the
    /// framebuffer, without advancing timing or touching interrupts.
    ///
    /// Used by the debugger to refresh a paused frame after a layer toggle:
    /// the live frame loop composites each line as the beam scans it, so a
    /// paused core never re-composites and a `toggle_layer` would have no
    /// visible effect until the game resumes. This replays the same
    /// `render_scanline` path the running emulator uses (so the debug
    /// composite can never diverge from the real one), reading the current
    /// VRAM/CRAM/VSRAM/registers and the debugger layer mask. It is an
    /// end-of-frame approximation: mid-frame raster splits that depend on
    /// per-scanline register changes are flattened, which is exactly the
    /// right model for "show me this static frame with layer X hidden".
    pub fn rerender_current_frame(&mut self) {
        // The SAT cache is rebuilt lazily on the first sprite line, but a
        // VRAM edit while paused could have left it stale; force a rebuild
        // so sprites reflect current memory.
        self.sprite_cache_valid = false;
        let visible = self.timing.visible_scanlines();
        for line in 0..visible {
            self.render_scanline(line);
        }
    }

    /// Log a traced VRAM write if the address falls within the watched range.
    #[inline]
    fn trace_vram_write(&self, addr: u16, value: u16, source: &str) {
        if let Some((start, end)) = self.watch_vram {
            if addr >= start && addr <= end.saturating_add(1) {
                self.log_vram_watch(addr, value, source);
            }
        }
    }

    #[cold]
    fn log_vram_watch(&self, addr: u16, value: u16, source: &str) {
        let tile = value & 0x07FF;
        let palette = (value >> 13) & 0x03;
        let priority = (value >> 15) & 0x01;
        let hflip = (value >> 11) & 0x01;
        let vflip = (value >> 12) & 0x01;
        eprintln!(
            "[VRAM-WATCH] frame={} scan={} addr=0x{:04X} val=0x{:04X} tile={} pal={} pri={} hf={} vf={} src={}",
            self.timing.frame_count, self.timing.scanline, addr, value,
            tile, palette, priority, hflip, vflip, source,
        );
    }

    /// Get display height based on register 1 (V30/V28 mode).
    pub fn display_height(&self) -> u32 {
        if (self.registers[1] & 0x08) != 0 {
            240 // V30 mode
        } else {
            224 // V28 mode
        }
    }

    /// Read from VDP ports.
    /// Read a 16-bit word from VDP ports.
    /// This is the primary read interface - the Genesis VDP returns word-sized values.
    pub fn read_word_port(&mut self, addr: u32) -> u16 {
        let masked = addr & 0x1F;
        match masked {
            // Data port (0xC00000-0xC00003)
            0x00..=0x03 => {
                self.pending_write = false;
                self.read_data_port()
            }
            // Control port / Status register (0xC00004-0xC00007)
            0x04..=0x07 => {
                self.pending_write = false;
                let status = self.get_status_register();
                // Clear sprite flags after reading (as MAME and Genesis Plus GX do).
                // The F flag (vblank_pending, bit 7) is NOT cleared by status reads -
                // it's cleared by interrupt acknowledge (IACK) when the 68K takes the VINT.
                self.sprite_overflow = false;
                self.sprite_collision = false;
                status
            }
            // HV counter (0xC00008-0xC0000B)
            0x08..=0x0B => {
                if let Some(latched) = self.hvc_latch {
                    latched
                } else {
                    let v = self.hv_v_counter() as u16;
                    let h = self.hv_h_counter() as u16;
                    (v << 8) | h
                }
            }
            _ => 0xFFFF,
        }
    }

    pub fn read(&mut self, addr: u32) -> u8 {
        let masked = addr & 0x1F;
        match masked {
            // Data port (0xC00000-0xC00003)
            0x00 | 0x02 => {
                self.pending_write = false; // Cancel pending write

                (if (addr & 1) == 0 {
                    // Even address: high byte
                    self.read_data_port() >> 8
                } else {
                    // Odd address: low byte
                    self.read_data_port() & 0xFF
                }) as u8
            }
            // Control port / Status register (0xC00004-0xC00007)
            // Status is 16-bit, return high/low byte based on address
            // Reading status register clears sprite overflow/collision only.
            // The F flag (vblank_pending, bit 7) is NOT cleared by status reads -
            // it's cleared by interrupt acknowledge (IACK) when the 68K takes the VINT.
            0x04 | 0x06 => {
                self.pending_write = false;
                let status = self.get_status_register();
                self.sprite_overflow = false;
                self.sprite_collision = false;
                (status >> 8) as u8
            }
            0x05 | 0x07 => {
                self.pending_write = false;
                let status = self.get_status_register();
                self.sprite_overflow = false;
                self.sprite_collision = false;
                (status & 0xFF) as u8
            }
            // HV counter (0xC00008-0xC0000B)
            0x08..=0x0B => {
                let is_v = (addr & 1) == 0;
                if let Some(latched) = self.hvc_latch {
                    if is_v {
                        (latched >> 8) as u8
                    } else {
                        (latched & 0xFF) as u8
                    }
                } else if is_v {
                    self.hv_v_counter()
                } else {
                    self.hv_h_counter()
                }
            }
            _ => 0xFF,
        }
    }

    /// Read from data port. Returns the prefetch buffer and loads the next word.
    fn read_data_port(&mut self) -> u16 {
        let value = self.read_buffer;
        self.prefetch_read_buffer();
        value
    }

    /// Prefetch the word at the current address into the read buffer,
    /// then auto-increment the address.
    fn prefetch_read_buffer(&mut self) {
        self.read_buffer = match self.code & 0x0F {
            0x00 => {
                // VRAM read
                let addr = (self.address as usize) & 0xFFFE;
                if addr + 1 < self.vram.len() {
                    let high = self.vram[addr] as u16;
                    let low = self.vram[addr + 1] as u16;
                    (high << 8) | low
                } else {
                    0
                }
            }
            0x08 => {
                // CRAM read. CRAM is stored in the bus layout BBB0GGG0RRR0
                // (0x0EEE valid). The unused bits float to the most recent FIFO
                // entry (open bus; checked against Genesis Plus GX).
                let index = ((self.address & 0x7E) >> 1) as usize;
                let stored = if index < 64 { self.cram[index] } else { 0 };
                (stored & 0x0EEE) | (self.fifo[self.fifo_idx] & !0x0EEE)
            }
            0x04 => {
                // VSRAM read (11-bit valid). VSRAM is physically 40 words: a read
                // whose byte offset reaches 0x50 (word index >= 40) wraps back to
                // word 0 on hardware (checked against Genesis Plus GX). Writes
                // still land in the upper slots, but they are not readable there.
                // Unused upper 5 bits float to the most recent FIFO entry.
                let byte_index = (self.address & 0x7E) as usize;
                let index = if byte_index >= 0x50 {
                    0
                } else {
                    byte_index >> 1
                };
                let stored = self.vsram[index];
                (stored & 0x07FF) | (self.fifo[self.fifo_idx] & !0x07FF)
            }
            0x0C => {
                // Undocumented 8-bit VRAM read: one byte from the adjacent
                // address, upper 8 bits from the FIFO open bus.
                let addr = (self.address as usize) ^ 1;
                let byte = if addr < self.vram.len() {
                    self.vram[addr] as u16
                } else {
                    0
                };
                (byte & 0x00FF) | (self.fifo[self.fifo_idx] & !0x00FF)
            }
            _ => self.read_buffer,
        };
        self.address = self.address.wrapping_add(self.registers[15] as u16);
    }

    /// Side-effect-free peek of the 16-bit VDP status word.
    ///
    /// A real status-port read resets the control-port write latch;
    /// this does not. It is the read the platform's debugger and its
    /// lessons use to sample `vblank`/`hblank`/sprite flags every frame
    /// without perturbing a running game.
    pub fn status_word(&self) -> u16 {
        self.get_status_register()
    }

    /// Get VDP status register (16-bit).
    ///
    /// Status register bits:
    /// - Bit 15-10: Unused (return open bus / fixed value)
    /// - Bit 9: FIFO Empty
    /// - Bit 8: FIFO Full
    /// - Bit 7: V Interrupt Pending
    /// - Bit 6: Sprite Overflow
    /// - Bit 5: Sprite Collision
    /// - Bit 4: Odd Frame (interlace)
    /// - Bit 3: V Blank
    /// - Bit 2: H Blank
    /// - Bit 1: DMA Busy
    /// - Bit 0: PAL Mode (0=NTSC, 1=PAL)
    fn get_status_register(&self) -> u16 {
        // Base value: bits 13, 12 and 10 set (0x3400); bit 9 (FIFO empty) is set below
        // This matches observed MAME behavior for unused bits
        let mut status: u16 = 0x3400;

        // Bit 9: FIFO Empty
        if self.is_fifo_empty() {
            status |= 0x0200;
        }

        // Bit 8: FIFO Full
        if self.is_fifo_full() {
            status |= 0x0100;
        }

        // Bit 7: V Interrupt Pending
        if self.timing.vblank_pending {
            status |= 0x0080;
        }

        // Bit 6: Sprite Overflow (>20 sprites on a scanline, >16 in H32)
        if self.sprite_overflow {
            status |= 0x0040;
        }

        // Bit 5: Sprite Collision (two non-transparent sprite pixels overlap)
        if self.sprite_collision {
            status |= 0x0020;
        }

        // Bit 4: Odd Frame (for interlace mode)
        // Set in odd frames while register 12 bits 4-3 are non-zero, although
        // the interlace mode is bits 2-1 (bit 3 is shadow/highlight)
        let interlace_mode = (self.registers[12] >> 3) & 0x03;
        if interlace_mode != 0 && self.odd_frame {
            status |= 0x0010;
        }

        // Bit 3: V Blank.
        // Hardware reports VBLANK whenever the beam is in the vertical blanking
        // region OR whenever the display is disabled (reg1 bit 6 = 0): with the
        // display off the VDP is permanently "blanked" and accessible. Games
        // that load VRAM with the display off (e.g. Gunstar Heroes' intro) poll
        // this bit to know the VDP is writable and spin forever if it never
        // sets. (Genesis Plus GX sets the flag with the display off as well.)
        let display_enabled = (self.registers[1] & 0x40) != 0;
        if self.timing.in_vblank || !display_enabled {
            status |= 0x0008;
        }

        // Bit 2: H Blank
        if self.timing.in_hblank {
            status |= 0x0004;
        }

        // Bit 1: DMA Busy
        if self.dma_busy || self.dma.is_active() {
            status |= 0x0002;
        }

        // Bit 0: PAL Mode (0=NTSC)
        // status |= 0x0001; // Not set for NTSC

        status
    }

    /// Write a byte to VDP ports (used for byte-level access).
    /// Note: VDP control port should use write_word() for proper command handling.
    pub fn write(&mut self, addr: u32, value: u8) {
        match addr & 0x1F {
            // Data port. A 68000 byte write to an even address drives D15-D8
            // (the high lane); an odd address drives D7-D0. Contra triggers a
            // VRAM fill with `MOVE.B Dn, $C00000`, and the fill replicates
            // `value >> 8`, so a byte put in the low lane would fill every
            // trailing cell with 0.
            0x00..=0x03 => {
                self.pending_write = false;
                let word = if (addr & 1) == 0 {
                    (value as u16) << 8
                } else {
                    value as u16
                };
                self.write_data_port(word, (addr & 1) == 0);
            }
            // Control port - byte writes are unusual but should work
            0x04..=0x07 => {
                // Byte writes to control port are rare; treat as word with 0 for other byte
                let word = if (addr & 1) == 0 {
                    (value as u16) << 8
                } else {
                    value as u16
                };
                self.write_control_word(word);
            }
            _ => {}
        }
    }

    /// Write a 16-bit word to VDP ports.
    /// This is the primary write interface - the Genesis VDP expects word-sized accesses.
    pub fn write_word_port(&mut self, addr: u32, value: u16) {
        match addr & 0x1F {
            // Data port (0xC00000-0xC00003)
            0x00..=0x03 => {
                self.pending_write = false;
                self.write_data_port(value, true);
            }
            // Control port (0xC00004-0xC00007)
            0x04..=0x07 => {
                self.write_control_word(value);
            }
            _ => {}
        }
    }

    /// Write a 32-bit long word to VDP control port (two consecutive word writes).
    pub fn write_long_port(&mut self, addr: u32, value: u32) {
        let high_word = (value >> 16) as u16;
        let low_word = (value & 0xFFFF) as u16;
        self.write_word_port(addr, high_word);
        self.write_word_port(addr, low_word);
    }

    /// Process a word write to the VDP control port.
    fn write_control_word(&mut self, value: u16) {
        if !self.pending_write {
            // First word of a command.
            //
            // Per hardware testing (Charles MacDonald), checked against
            // Genesis Plus GX: the first write IMMEDIATELY updates the low 14
            // address bits and CD0-CD1, *unconditionally*, before the VDP even
            // decides whether this is a register write. There is no internal
            // staging. Games like Batman & Robin and EA titles depend on this to
            // change the VRAM address with a single control word, and the
            // VDPFIFOTesting "Register Write ... / Code Reg" subtests verify that
            // a register write (bits 15-14 = 10) ALSO latches these bits.
            self.address = (self.address & 0xC000) | (value & 0x3FFF);
            self.code = (self.code & 0x3C) | ((value >> 14) as u8 & 0x03);
            self.control_word = (self.control_word & 0xFFFF0000) | (value as u32);

            // Register write: bits 15-14 = 10 -> `100R RRRR DDDD DDDD`. The
            // address/code latch above already happened; the register number is
            // bits 12-8 only (bit 13 is masked out, going to the address latch),
            // and the operation completes in a single word (no pending second).
            if (value & 0xC000) == 0x8000 {
                let reg_num = ((value >> 8) & 0x1F) as usize;
                let reg_value = (value & 0xFF) as u8;
                self.write_register(reg_num, reg_value);
                self.pending_write = false;
            } else {
                self.pending_write = true;
            }
        } else {
            // Second word of a two-word command.
            // Updates address high 2 bits (A15-A14) and code bits CD2-CD5.
            self.control_word = (self.control_word & 0x0000FFFF) | ((value as u32) << 16);
            self.pending_write = false;

            self.address = (self.address & 0x3FFF) | ((value & 0x03) << 14);
            self.code = (self.code & 0x03) | (((value >> 2) & 0x3C) as u8);

            let cd5 = (self.code >> 5) & 1;

            // Check if DMA should be triggered (CD5=1 indicates DMA).
            // A control write that arrives while a fill is still streaming must
            // NOT re-arm DMA: that would clobber the running fill's metered
            // duration with the `u64::MAX` sentinel and hang the busy bit. The
            // address/code latch above still applies, so such a write redirects
            // the in-flight fill instead (VDPFIFOTesting "DMA Fill Control Port
            // Writes").
            if cd5 == 1 && (self.registers[1] & 0x10) != 0 && self.fill_state.is_none() {
                let dma_mode = (self.registers[23] >> 6) & 0x03;
                match dma_mode {
                    0b00 | 0b01 => {
                        // Memory to VRAM - needs CPU memory access
                        self.dma_pending = true;
                    }
                    0b11 => {
                        // VRAM Copy - VDP-internal operation; the CPU keeps
                        // running while the VDP reports DMA-busy until it finishes.
                        let raw_length =
                            ((self.registers[20] as u16) << 8) | (self.registers[19] as u16);
                        // DMA length 0 means 65536 (pre-decrementing counter)
                        let length: u32 = if raw_length == 0 {
                            0x10000
                        } else {
                            raw_length as u32
                        };
                        let source =
                            ((self.registers[22] as u16) << 8) | (self.registers[21] as u16);
                        self.execute_dma_vram_copy(source, self.address, length);
                        // Assert DMA-busy for the metered duration.
                        self.dma_busy = true;
                        self.dma_remaining_clocks =
                            self.dma_busy_master_clocks(DmaTransfer::Copy, length);
                    }
                    _ => {
                        // 0b10 = VRAM Fill. The fill *data* is supplied by a
                        // subsequent data-port write, but the DMA-busy status
                        // bit is asserted by THIS control-port write (the CD5
                        // arm), not by that later data write (verified on
                        // hardware; Genesis Plus GX does the same). The
                        // fill duration is unknown until the data write provides
                        // the length, so hold the busy bit with a sentinel that
                        // `tick_dma` refuses to drain; the data write replaces it
                        // with the real metered duration. VDPFIFOTesting "DMA
                        // Busy Flag - DMA Fill" reads the bit in the gap between
                        // the two writes and expects it already set. The one-shot
                        // `dma_fill_pending` is what the next
                        // data write consumes to actually start the fill.
                        self.dma_fill_pending = true;
                        self.dma_busy = true;
                        self.dma_remaining_clocks = u64::MAX;
                    }
                }
            } else if cd5 == 0 {
                // Non-DMA read command: prefetch the first word into the read
                // buffer and auto-increment address. Only for read codes
                // (VRAM 0x00, VSRAM 0x04, CRAM 0x08, undocumented 8-bit VRAM 0x0C).
                let code_low = self.code & 0x0F;
                if code_low == 0x00 || code_low == 0x04 || code_low == 0x08 || code_low == 0x0C {
                    self.prefetch_read_buffer();
                }
            }
        }
    }

    /// Write to data port based on current access mode.
    fn write_data_port(&mut self, value: u16, _is_high_byte: bool) {
        // Store value for potential DMA fill
        self.dma_fill_value = value;

        // Reserve this write's VDP FIFO access slot (drain timestamp) before it
        // joins the ring, so the empty/full status bits track it. Only the CPU
        // data-port path records a slot; DMA pushes (`execute_pending_dma`)
        // account for their own access slots separately (Genesis Plus GX too
        // times the FIFO in its data-port write, not in its DMA loop).
        self.fifo_record_slot();

        // Push the written word into the FIFO data ring (open-bus source for
        // subsequent VSRAM/CRAM/byte-VRAM reads). Every data-port write does
        // this *before* the DMA-fill check (as in Genesis Plus GX), so a fill's
        // source word and the post-write open-bus both see it.
        self.push_fifo_word(value);

        // DMA Fill triggers off the one-shot `dma_fill_pending` flag armed by
        // the control-port write (CD5 + mode 0b10 while DMA was enabled).
        // Consuming it here means a *second* data-port write (the
        // ones the "DP Writes During DMA Fill" subtests issue while the fill is
        // still streaming), and any write after a short fill has completed, never
        // re-arms a fresh fill; it falls through to the normal write path,
        // landing at the fill's partially-advanced (shared) address and
        // re-sourcing the remaining cells via the FIFO. Clearing DMA-enable
        // between the arm and this write does NOT cancel the pending fill (the
        // "Toggle Fill" subtest): the fill was latched at the arm.
        if self.dma_fill_pending {
            self.dma_fill_pending = false;
            // DMA Fill triggered by data port write.
            let raw_length = ((self.registers[20] as u16) << 8) | (self.registers[19] as u16);
            // DMA length 0 means 65536 (pre-decrementing counter)
            let length: u32 = if raw_length == 0 {
                0x10000
            } else {
                raw_length as u32
            };

            // Step 1: the triggering data-port write deposits the FULL word at
            // the current address, exactly like a normal write, *before* the
            // fill runs (with the bytes swapped on an odd VRAM address; checked
            // against Genesis Plus GX). The fill then only writes the high byte to the
            // *subsequent* cells, so the initial cell keeps the whole word. This
            // is what makes a post-fill read see e.g. `1234` (not `1212`) in the
            // first cell, VDPFIFOTesting "DMA Fill FIFO Usage".
            let auto_increment = self.registers[15] as u16;
            let init_addr = self.address;
            match self.code & 0x0F {
                0x01 => {
                    // VRAM: write the full word (byteswapped on odd address).
                    let raw_addr = init_addr as usize;
                    let wa = raw_addr & 0xFFFE;
                    if wa + 1 < self.vram.len() {
                        if (raw_addr & 1) == 0 {
                            self.vram[wa] = (value >> 8) as u8;
                            self.vram[wa + 1] = (value & 0xFF) as u8;
                        } else {
                            self.vram[wa] = (value & 0xFF) as u8;
                            self.vram[wa + 1] = (value >> 8) as u8;
                        }
                        self.trace_vram_write(wa as u16, value, "dma-fill");
                    }
                }
                0x03 => {
                    // CRAM: write full word
                    let index = ((init_addr & 0x7E) >> 1) as usize;
                    if index < 64 {
                        self.cram[index] = value & 0x0EEE;
                        self.cram_rgb[index] = Self::convert_genesis_color(value & 0x0EEE);
                    }
                }
                0x05 => {
                    // VSRAM: write full word
                    let index = ((init_addr & 0x7E) >> 1) as usize;
                    if index < 64 {
                        self.vsram[index] = value & 0x07FF;
                    }
                }
                _ => {}
            }

            // The source-register writeback + length-clear happen now, at the
            // trigger, so "DMA Fill Source/Length Reg Update" observe the final
            // register values. Only the cell writes are deferred to
            // `advance_fill`/`tick_dma` below.
            self.dma_fill_source_writeback();

            // Step 2: the remaining `length` cells stream out over the metered
            // duration (advanced from `tick_dma`), starting one increment past
            // the preserved initial cell: `length + 1` cells in all, as the
            // instant `execute_dma_vram_fill` writes.
            self.address = init_addr.wrapping_add(auto_increment);
            self.fill_state = Some(FillState {
                remaining: length,
                cell_accum: 0,
            });

            // Front-load the cells the chip would have written from this trigger
            // cycle to the end of the current scanline (Genesis Plus GX runs its
            // DMA to the write's time the same way), so a back-to-back data/control write
            // observes the same partially-advanced address the hardware does,
            // VDPFIFOTesting "DP Writes During DMA Fill" / "DMA Fill Control Port
            // Writes". In VBLANK the whole fill lands here; otherwise the
            // remainder streams from `tick_dma`.
            let chunk = self.fill_initial_chunk(length);
            self.advance_fill(chunk);

            // VRAM Fill is a VDP-internal transfer: the 68000 keeps running
            // while the VDP reports DMA-busy (status bit 1) for the metered
            // duration. The busy bit clears once the fill's
            // duration elapses; the F flag (bit 7) is unaffected.
            self.dma_busy = true;
            self.dma_remaining_clocks = self.dma_busy_master_clocks(DmaTransfer::Fill, length);
            return;
        }

        // FIFO entry counter, kept because the savestate carries it; the status
        // FIFO-empty/full bits derive from the `fifo_slot_mclks` drain timestamps
        // recorded above. The CPU is NOT stalled on a full FIFO: MAME's
        // totalcycles counter does not include FIFO wait states, and the
        // timestamp model needs no stall to report full.
        if self.fifo_entries < 4 {
            self.fifo_entries += 1;
        }

        match self.code & 0x0F {
            0x01 => {
                // VRAM write - always writes a 16-bit word.
                // The VDP clears bit 0 for the base address; if the original
                // address had bit 0 set, the written bytes are swapped.
                let raw_addr = self.address as usize;
                let addr = raw_addr & 0xFFFE; // Word-align
                if addr + 1 < self.vram.len() {
                    if (raw_addr & 1) == 0 {
                        self.vram[addr] = (value >> 8) as u8;
                        self.vram[addr + 1] = (value & 0xFF) as u8;
                    } else {
                        self.vram[addr] = (value & 0xFF) as u8;
                        self.vram[addr + 1] = (value >> 8) as u8;
                    }
                    self.trace_vram_write(addr as u16, value, "data-port");
                }
                self.address = self.address.wrapping_add(self.registers[15] as u16);
            }
            0x03 => {
                // CRAM write
                let index = ((self.address & 0x7E) >> 1) as usize;
                if index < 64 {
                    let masked = value & 0x0EEE;
                    self.cram[index] = masked;
                    self.cram_rgb[index] = Self::convert_genesis_color(masked);
                    // A palette change during active display re-tints the
                    // current line (as in Genesis Plus GX). Display on -> re-tint only from
                    // the beam rightward (beam-X segment: pixels already scanned
                    // keep their colour, so multiple writes build colour segments,
                    // beyond Genesis Plus GX's whole-line update). Display off -> only a
                    // backdrop-pen change is visible, so re-blank the line.
                    if self.midline_render_window() {
                        let line = self.timing.scanline;
                        if (self.registers[1] & 0x40) != 0 {
                            let x0 = self.beam_active_x();
                            let w = self.display_width() as usize;
                            self.remap_scanline_segment(line, x0, w);
                        } else if index == self.backdrop_index() {
                            self.render_scanline(line);
                        }
                    }
                }
                self.address = self.address.wrapping_add(self.registers[15] as u16);
            }
            0x05 => {
                // VSRAM write
                let index = ((self.address & 0x7E) >> 1) as usize;
                if index < 64 {
                    self.vsram[index] = value & 0x07FF; // 11-bit value
                }
                self.address = self.address.wrapping_add(self.registers[15] as u16);
                // NB: no mid-line VSRAM re-composite hook. Genesis Plus GX
                // re-renders the line on a 2-cell-mode VSRAM write within its
                // 860-mclk window,
                // but SG1's beam lands HINT-driven scroll writes at a different
                // sub-frame position, so a whole-line re-composite here repaints
                // boundary lines that should stay as first rendered (it breaks the
                // Gunstar Heroes intro). SG1's per-line scroll sampling at
                // line start already applies the new value from the next line on.
            }
            _ => {}
        }
    }

    /// Write to a VDP register.
    fn write_register(&mut self, reg_num: usize, value: u8) {
        // VDP registers 11-23 cannot be updated while in Mode 4 (the M5 bit,
        // register 1 bit 2, is clear). Real games depend on this: Captain
        // Planet and Bass Master Classic write high register indices in Mode 4
        // expecting them to be ignored (checked against Genesis Plus GX). The DMA engine
        // updates registers 19-23 directly (not through this path), so its
        // post-transfer writeback is unaffected.
        if (self.registers[1] & 0x04) == 0 && reg_num > 10 {
            return;
        }
        self.apply_register_write(reg_num, value);
    }

    /// Commit a register value and run its side effects, bypassing the Mode-4
    /// access mask. This is the shared body of `write_register`; the test/debug
    /// register override (`set_test_register`) calls it directly so it can force
    /// any register regardless of the current display mode.
    fn apply_register_write(&mut self, reg_num: usize, value: u8) {
        if reg_num < 24 {
            let prev = self.registers[reg_num];
            self.registers[reg_num] = value;

            // Handle special cases
            if reg_num == 0 {
                // HV-counter latch (reg0 bit1 / M3). On a *change* of the bit
                // while Mode 5 (reg1 bit2) is active, the current HV counter
                // is latched or freed again (checked against Genesis Plus
                // GX). Reads then return the frozen
                // value (lightgun / Sunset Riders / VDPFIFOTesting "HV Counter
                // Latch").
                if ((prev ^ value) & 0x02) != 0 && (self.registers[1] & 0x04) != 0 {
                    if (value & 0x02) != 0 {
                        let v = self.hv_v_counter() as u16;
                        let h = self.hv_h_counter() as u16;
                        self.hvc_latch = Some((v << 8) | h);
                    } else {
                        self.hvc_latch = None;
                    }
                }
            } else if reg_num == 1 {
                // Register 1: Mode register 2
                // If VINT enable (bit 5) is being set and the F flag (vblank_pending)
                // is already active from a previous VBlank, re-assert vint_irq_pending.
                // On real hardware, the VDP's /INT output = F_FLAG AND VINT_ENABLE,
                // so enabling VINT while the F flag is set immediately asserts the
                // interrupt line. Without this, interrupts are lost when VBlank occurs
                // while VINT is disabled and VINT is enabled later.
                let vint_enabled = (value & 0x20) != 0;
                if vint_enabled && self.timing.vblank_pending {
                    self.timing.vint_irq_pending = true;
                }
                // NB: deliberately NO mid-line display-enable (bit 6) re-render
                // here. Genesis Plus GX redraws the line when display
                // is re-enabled within its 860-mclk window, but SG1 already
                // reproduces display-enable *windows* at line granularity (the
                // toggle takes effect from the next line's start), which gives
                // that emulator's picture for the Gunstar Heroes intro. An
                // exact-cycle hook here would over-trigger: SG1's display-enable
                // write lands at a different sub-frame beam position, so it would
                // re-render a boundary line that Genesis Plus GX leaves blank.
                //
                // For the same reason there is no mid-line re-render hook on the
                // plane-base registers (2/3/4) or backdrop (7): Genesis Plus GX
                // re-renders the line on those mid-active writes, but SG1's coarser sub-frame
                // timing makes a whole-line re-composite repaint boundary lines a
                // game intended for the *next* line. SG1's line-granular register
                // handling (the change takes effect from the next line) already
                // gives the same picture on the validated scenes; only palette re-tints
                // (CRAM, in `write_data_port`) are safe to reflect on the current line, because
                // they never re-composite, they only recolour resolved indices.
            } else if reg_num == 7 {
                // Register 7: Backdrop color
                // Bits 5-4: palette, bits 3-0: color
                // Note: Do NOT call render_backdrop() here - it would overwrite
                // already-rendered scanlines. render_scanline() reads the backdrop
                // color from CRAM on every line it draws, through `backdrop_index()`.
            } else if reg_num == 10 {
                // Register 10: H interrupt counter reload value.
                // Writing R10 updates ONLY the reload value. The live counter
                // keeps decrementing and is reloaded from R10 the next time it
                // underflows (or during vblank); it is NOT reset on write.
                // Resetting the live counter here desynchronises raster splits
                // that rewrite R10 mid-frame (e.g. the Gunstar Heroes intro's
                // HINT-driven display-enable window), pushing the display-enable
                // HINT far down the frame so only a bottom strip renders.
                self.timing.h_counter_reload = value;
            }
        }
    }

    /// Render backdrop color to entire framebuffer.
    fn render_backdrop(&mut self) {
        // Register 7 contains backdrop color
        let palette_index = ((self.registers[7] >> 4) & 0x3) as usize;
        let color_index = (self.registers[7] & 0xF) as usize;
        let cram_index = palette_index * 16 + color_index;

        // Get color from CRAM (9-bit RGB: 0000 BBB0 GGG0 RRR0)
        let color_9bit = self.cram[cram_index];
        let (r, g, b) = Self::convert_genesis_color(color_9bit);

        // Fill framebuffer with backdrop color
        for pixel in self.framebuffer.as_chunks_mut::<4>().0 {
            pixel[0] = r;
            pixel[1] = g;
            pixel[2] = b;
            pixel[3] = 255; // Alpha
        }
    }

    /// Convert 9-bit Genesis color to 24-bit RGB.
    /// Genesis color format: 0000 BBB0 GGG0 RRR0 (3 bits per channel)
    pub fn convert_genesis_color(color: u16) -> (u8, u8, u8) {
        let r = ((color >> 1) & 0x7) as u8;
        let g = ((color >> 5) & 0x7) as u8;
        let b = ((color >> 9) & 0x7) as u8;

        // Expand 3-bit to 8-bit: multiply by 36.42857... ~ (255/7)
        // Using bit replication for accurate conversion: xyzxyzxy
        let r8 = (r << 5) | (r << 2) | (r >> 1);
        let g8 = (g << 5) | (g << 2) | (g >> 1);
        let b8 = (b << 5) | (b << 2) | (b >> 1);

        (r8, g8, b8)
    }

    /// Rebuild the CRAM RGB lookup table from current CRAM contents.
    /// Called whenever CRAM is written.
    fn rebuild_cram_rgb(&mut self) {
        for i in 0..64 {
            if i < self.cram.len() {
                self.cram_rgb[i] = Self::convert_genesis_color(self.cram[i]);
            }
        }
    }

    /// Read a single pixel (4-bit color index) directly from a VRAM tile pattern.
    /// This avoids decoding all 64 pixels of a tile when only 1 is needed.
    /// pattern_index: tile number in VRAM, px/py: pixel coords within tile (0-7).
    #[inline(always)]
    fn read_tile_pixel_from_vram(vram: &[u8], pattern_index: u16, px: u8, py: u8) -> u8 {
        let addr = (pattern_index as usize) * 32 + (py as usize) * 4 + (px as usize) / 2;
        if addr >= vram.len() {
            return 0;
        }
        let byte = vram[addr];
        if px & 1 == 0 {
            byte >> 4
        } else {
            byte & 0x0F
        }
    }

    /// Read a single pixel from a VRAM tile pattern with flip support.
    #[inline(always)]
    fn read_tile_pixel_flipped(
        vram: &[u8],
        pattern_index: u16,
        px: u8,
        py: u8,
        flip_h: bool,
        flip_v: bool,
    ) -> u8 {
        let px = if flip_h { 7 - px } else { px };
        let py = if flip_v { 7 - py } else { py };
        Self::read_tile_pixel_from_vram(vram, pattern_index, px, py)
    }

    /// Render all sprite pixels for a given scanline into the sprite line buffer.
    /// Called ONCE per scanline. The buffer is then read during compositing.
    ///
    /// Implements sprite masking per the Mega Drive VDP:
    /// - When a sprite with raw X position = 0 is encountered AND at least one
    ///   sprite with X > 0 was already seen, all remaining sprites on this
    ///   scanline are masked (not drawn).
    /// - Per-scanline pixel count limit (320 in H40, 256 in H32).
    /// - The masking flag carries over to the next scanline when the pixel
    ///   limit is reached.
    fn render_sprite_line(&mut self, line: u16) {
        let width = self.display_width() as usize;
        let is_h40 = (self.registers[12] & 0x01) != 0;
        let max_sprite_pixels: i32 = if is_h40 { 320 } else { 256 };
        // Per-line sprite count limit: 20 in H40, 16 in H32 (viewport.w >> 4).
        let max_sprites_per_line: u16 = if is_h40 { 20 } else { 16 };

        // Ensure sprite line buffers are large enough
        if self.sprite_line_color.len() < width {
            self.sprite_line_color.resize(width, 0);
            self.sprite_line_palette.resize(width, 0);
            self.sprite_line_priority.resize(width, false);
        }

        // Clear sprite line buffers
        for i in 0..width {
            self.sprite_line_color[i] = 0;
            self.sprite_line_palette[i] = 0;
            self.sprite_line_priority[i] = false;
        }

        // Clear collision tracking
        for i in 0..width {
            self.sprite_pixel_buffer[i] = false;
        }

        if !self.sprite_cache_valid {
            self.rebuild_sprite_cache();
        }

        // Build list of sprites on this line with masking (reuse scratch buffer)
        //
        // Sprites are taken in link order (front to back). A sprite at raw
        // X = 0 (screen X = -128) hides every sprite after it on the line,
        // but only once the mask is armed: by an earlier sprite of this line
        // at any other X, or by the line above having run out of sprite dots.
        // (Both cases are documented from hardware tests; checked against
        // Genesis Plus GX.)
        self.sprites_on_line_buf.clear();
        let scanline = line as i16;
        let mut count = 0u16;
        let mut masked = false;
        let mut mask_armed = self.sprite_mask_overflow;
        let mut pixel_count: i32 = 0;
        // When the per-line dot budget is crossed mid-sprite, that one sprite
        // is drawn clipped to the remaining dots. -1 = no sprite clipped.
        let mut clip_sprite_idx: i32 = -1;
        let mut clip_x_end: i16 = 0;

        for (idx, sprite) in self.sprite_cache.sprites().iter().enumerate() {
            let (_, y1, _, y2) = sprite.bounds();
            if scanline >= y1 && scanline < y2 {
                count += 1;
                if count > max_sprites_per_line {
                    self.sprite_overflow = true;
                    break;
                }

                // Raw SAT X position: sprite.x is screen-adjusted (raw - 128)
                let raw_x = sprite.x + 128;
                if raw_x != 0 {
                    mask_armed = true;
                } else if mask_armed {
                    masked = true;
                }

                let sprite_width_px = sprite.width_tiles as i32 * 8;
                let pixel_count_before = pixel_count;
                pixel_count += sprite_width_px;

                if !masked {
                    self.sprites_on_line_buf.push(idx as u16);
                }

                if pixel_count >= max_sprite_pixels {
                    // Dot limit crossed mid-sprite: that sprite is drawn clipped
                    // to the remaining budget. The chip draws whole 8px tiles
                    // measured from the sprite's left edge (off-screen dots still
                    // count toward the budget, so clip in raw sprite space;
                    // checked against Genesis Plus GX).
                    if !masked {
                        let allowed_tiles = (max_sprite_pixels - pixel_count_before).max(0) >> 3;
                        clip_sprite_idx = idx as i32;
                        clip_x_end = sprite.x + allowed_tiles as i16 * 8;
                    }
                    self.sprite_mask_overflow = pixel_count >= width as i32;
                    // The limit was hit, so the flag is not cleared below.
                    break;
                }
            }
        }

        // Clear masking for next line if pixel limit was NOT reached
        if pixel_count < max_sprite_pixels {
            self.sprite_mask_overflow = false;
        }

        // Render sprites in REVERSE order (lowest priority first) so that
        // higher-priority sprites (earlier in list) overwrite lower ones.
        let num_sprites = self.sprites_on_line_buf.len();
        for si in (0..num_sprites).rev() {
            let sprite_idx = self.sprites_on_line_buf[si] as usize;
            let sprite = self.sprite_cache.sprites()[sprite_idx];
            let (sx1, sy1, sx2, _sy2) = sprite.bounds();

            let local_y = (scanline - sy1) as u16;
            let flipped_y = if sprite.flip_v {
                sprite.height_tiles as u16 * 8 - 1 - local_y
            } else {
                local_y
            };

            let sprite_width_px = sprite.width_tiles as i16 * 8;

            // Clip to screen (ensure bounds are valid)
            let x_start = sx1.max(0) as usize;
            let mut x_end_raw = sx2;
            if sprite_idx as i32 == clip_sprite_idx {
                x_end_raw = x_end_raw.min(clip_x_end);
            }
            let x_end = x_end_raw.clamp(0, width as i16) as usize;

            for screen_x in x_start..x_end {
                let local_x = (screen_x as i16 - sx1) as u16;
                let flipped_x = if sprite.flip_h {
                    sprite_width_px as u16 - 1 - local_x
                } else {
                    local_x
                };

                let tile_x = flipped_x / 8;
                let tile_y = flipped_y / 8;
                let pixel_x = (flipped_x % 8) as u8;
                let pixel_y = (flipped_y % 8) as u8;

                // Column-major tile layout
                let tile_offset = tile_x * sprite.height_tiles as u16 + tile_y;
                let pattern_index = sprite.pattern_base + tile_offset;

                let color_index =
                    Self::read_tile_pixel_from_vram(&self.vram, pattern_index, pixel_x, pixel_y);

                if color_index == 0 {
                    continue; // Transparent
                }

                // Track collision: if a sprite pixel already exists at this x
                if self.sprite_line_color[screen_x] != 0 {
                    self.sprite_collision = true;
                }

                // Write to line buffer (earlier sprites overwrite later ones)
                self.sprite_line_color[screen_x] = color_index;
                self.sprite_line_palette[screen_x] = sprite.palette;
                self.sprite_line_priority[screen_x] = sprite.priority;
            }
        }
    }

    /// Read horizontal scroll value from VRAM for a given plane and scanline.
    ///
    /// Modes (register 11, bits 0-1):
    ///   0 = Full screen scroll (single value)
    ///   1 = First 8 lines (line & 7, prohibited but used by Populous)
    ///   2 = Per cell (every 8 scanlines)
    ///   3 = Per line
    #[inline(always)]
    fn read_hscroll(&self, hscroll_base: usize, plane_offset: usize, mode: u8, line: u16) -> u16 {
        let addr = match mode {
            0 => hscroll_base + plane_offset,
            1 => {
                // "Prohibited" mode: only the line's low three bits pick the
                // entry, so it wraps within the first 8.
                let l = (line & 7) as usize;
                hscroll_base + l * 4 + plane_offset
            }
            2 => {
                // Per-cell: the line counts in eight bits and its low three
                // are dropped, so the table can't run past the 8-bit line
                // range.
                let cell = ((line & 0xFF) / 8) as usize;
                hscroll_base + cell * 32 + plane_offset
            }
            3 => {
                // Per-line: the line counts in eight bits.
                let l = (line & 0xFF) as usize;
                hscroll_base + l * 4 + plane_offset
            }
            _ => return 0,
        };
        if addr + 1 < self.vram.len() {
            let high = self.vram[addr] as u16;
            let low = self.vram[addr + 1] as u16;
            ((high << 8) | low) & 0x03FF
        } else {
            0
        }
    }

    /// Execute one scanline of VDP rendering.
    pub fn scanline(&mut self, line: u16) {
        self.timing.scanline = line;
        self.timing.h_counter = 0; // Reset H counter at scanline start
        self.timing.in_hblank = false; // HBLANK is cleared at scanline start

        // Update VBLANK/HBLANK status based on current scanline
        let visible = self.timing.visible_scanlines();

        let was_in_vblank = self.timing.in_vblank;
        self.timing.in_vblank = line >= visible;

        // Toggle odd/even frame at start of VBLANK (for interlace mode)
        if self.timing.in_vblank && !was_in_vblank {
            self.odd_frame = !self.odd_frame;
            // Invalidate sprite cache at frame boundary
            self.sprite_cache_valid = false;
            if self.timing.first_frame_after_reset {
                // The VDP comes out of reset already past VBLANK, so the first
                // frame after a reset raises no VINT. Skip the F flag +
                // level-6 request for exactly the first post-reset VBLANK so a
                // game's first VINT lands one frame later; without this, SG1
                // would run one logic frame ahead of Genesis Plus GX from boot
                // in every game.
                self.timing.first_frame_after_reset = false;
            } else {
                // Set the F flag (status bit 7) and Z80 IRQ edge now, but defer
                // the level-6 68K interrupt request to vint_delay_mclks() into
                // this line (real hardware asserts VINT part-way into line
                // 224, not at its start). The F flag is ONLY cleared by
                // IACK, never by status reads, and persists across frames.
                self.timing.set_vint_pending();
                let delay = self.vint_delay_mclks();
                self.timing.arm_vint_request_delay(delay);
            }
        }

        // Note: We do NOT clear vblank_pending when exiting VBLANK.
        // The F flag (VINT occurred, bit 7 of status register) persists until
        // the CPU acknowledges the interrupt (IACK). MAME and Genesis Plus GX
        // agree.

        // Update H counter for HINT
        // On real hardware, HINT fires when the H counter reaches 0.
        // R10=0 means HINT fires every scanline. The counter decrements
        // each scanline during active display and reloads on overflow.
        if line < visible {
            // Active display area: process HINT counter
            if self.timing.h_counter_current == 0 {
                self.timing.h_counter_current = self.timing.h_counter_reload;
                if (self.registers[0] & 0x10) != 0 {
                    self.timing.set_hint_pending();
                }
            } else {
                self.timing.h_counter_current -= 1;
            }
        } else {
            // VBLANK area: reload counter from register
            self.timing.h_counter_current = self.timing.h_counter_reload;
        }

        // Render this scanline if in visible area
        if line < visible {
            if self.progressive_cram.is_some() {
                // Direct-Color-DMA streams CRAM with the display forcibly off, so
                // the screen is all backdrop and each ~2-pixel run shows the DMA
                // word for its beam position. Paint that per beam-X (the way
                // PicoDrive shows these pictures), a per-write approximation
                // of per-pixel CRAM that resolves the actual picture instead
                // of one colour per line. With the display on, a CRAM DMA is an ordinary
                // raster palette/backdrop effect: keep the per-line band model.
                if (self.registers[1] & 0x40) == 0 {
                    self.render_dcdma_line_beamx(line);
                } else {
                    // Land this line's share of the metered mid-active CRAM DMA
                    // before sampling the palette, so the per-line colour is right.
                    self.apply_progressive_cram_for_line(line);
                    self.render_scanline(line);
                }
            } else {
                self.render_scanline(line);
            }
        }
    }

    /// Render a single scanline. Two-stage pipeline: `composite_scanline`
    /// resolves BG + sprite priority into a per-line palette-index buffer, then
    /// `remap_scanline` turns indices into RGB via `cram_rgb`. Keeping them
    /// separate lets a mid-line CRAM write re-tint the line (remap only) without
    /// recompositing; an ordinary frame just runs both back to back.
    fn render_scanline(&mut self, line: u16) {
        let display_enabled = (self.registers[1] & 0x40) != 0;
        if !display_enabled {
            self.render_blanked_line(line);
            return;
        }
        self.composite_scanline(line);
        self.remap_scanline(line);
    }

    /// Backdrop CRAM index (palette*16 + colour) from reg7.
    fn backdrop_index(&self) -> usize {
        let bd_pal = ((self.registers[7] >> 4) & 0x3) as usize;
        let bd_col = (self.registers[7] & 0xF) as usize;
        bd_pal * 16 + bd_col
    }

    /// Backdrop RGB from reg7 via the CRAM LUT.
    fn backdrop_rgb(&self) -> (u8, u8, u8) {
        let bd_idx = self.backdrop_index();
        if bd_idx < 64 {
            self.cram_rgb[bd_idx]
        } else {
            (0, 0, 0)
        }
    }

    /// True when a VDP write on the current (visible) line lands early enough to
    /// be a raster effect for THIS line rather than setup for the next one.
    /// The window is the first 860 of the line's 3420 master clocks (the
    /// window Genesis Plus GX uses), expressed in the 68K-cycle beam
    /// units SG1 tracks. Inert until a frame has run (`beam_cycles_per_line`
    /// still 0), so pure-unit-test register pokes never trigger a re-render.
    ///
    /// SG1 advances the beam *after* each instruction, so a write in the very
    /// first instruction of a line still sees the previous line's high beam
    /// value and is skipped. Under-triggering (a missed effect) is safe; the
    /// alternative (a stale-low beam) would over-trigger and corrupt lines.
    fn midline_render_window(&self) -> bool {
        if self.beam_cycles_per_line == 0 {
            return false;
        }
        if self.timing.scanline >= self.timing.visible_scanlines() {
            return false;
        }
        let window = (self.beam_cycles_per_line as u64 * 860 / 3420) as u32;
        self.beam_cycles_into_line <= window
    }

    /// Active-display pixel column under the beam right now, from the
    /// per-instruction beam cycle position. SG1 places HBLANK in the last ~72
    /// 68K cycles of the line; the active scanout is the rest, mapped linearly
    /// onto the display width. A mid-line CRAM write re-tints `[beam_active_x,
    /// width)` so the already-scanned left of the line keeps its colour.
    fn beam_active_x(&self) -> usize {
        let per_line = self.beam_cycles_per_line.max(1);
        let active = per_line.saturating_sub(72).max(1);
        let pos = self.beam_cycles_into_line.min(active);
        let width = self.display_width() as usize;
        ((pos as u64 * width as u64) / active as u64) as usize
    }

    /// Fill a scanline with the backdrop colour when the display is disabled
    /// (reg1 bit 6 clear): every layer mode shows the backdrop, as on hardware.
    fn render_blanked_line(&mut self, line: u16) {
        let width = self.display_width() as usize;
        let internal_width = 320usize;
        let fb_offset = (line as usize) * internal_width * 4;
        let backdrop_color = self.backdrop_rgb();
        let bd_idx = self.backdrop_index().min(63) as u8;
        for x in 0..width {
            // Keep the index buffers consistent with the backdrop fill: a
            // disabled line holds the backdrop pen, so a later mid-line CRAM
            // re-tint stays backdrop. Without this, `remap_scanline`
            // (a CRAM mid-line hook on a now-enabled line) would re-tint a
            // previous line's stale composite into this blanked line.
            self.line_index[x] = bd_idx;
            self.line_sh_op[x] = 0;
            self.line_high_pri[x] = false;
            self.line_plane_a[x] = -1;
            self.line_window[x] = -1;
            self.line_plane_b[x] = -1;
            let idx = fb_offset + x * 4;
            if idx + 3 < self.framebuffer.len() {
                self.framebuffer[idx] = backdrop_color.0;
                self.framebuffer[idx + 1] = backdrop_color.1;
                self.framebuffer[idx + 2] = backdrop_color.2;
                self.framebuffer[idx + 3] = 255;
            }
        }
    }

    /// Resolve plane B / plane A / window and sprites into this line's
    /// palette-index buffers. Produces no RGB (that is `remap_scanline`'s job),
    /// so the line can be re-tinted after a mid-line CRAM change.
    fn composite_scanline(&mut self, line: u16) {
        let width = self.display_width() as usize;
        let shadow_highlight = (self.registers[12] & 0x08) != 0;
        let bd_idx = self.backdrop_index().min(63) as u8;

        // Debugger per-layer force-disable mask (read once per line).
        let dis_plane_a = self.debug_layer_disable[LAYER_PLANE_A];
        let dis_plane_b = self.debug_layer_disable[LAYER_PLANE_B];
        let dis_window = self.debug_layer_disable[LAYER_WINDOW];
        let dis_sprites = self.debug_layer_disable[LAYER_SPRITES];

        // Render sprites for this scanline into the line buffer (ONCE)
        self.render_sprite_line(line);

        // Pre-compute plane parameters to avoid repeated register reads
        let plane_a_nametable = ((self.registers[2] & 0x38) as u16) << 10;
        let plane_b_nametable = ((self.registers[4] & 0x07) as u16) << 13;
        // Plane geometry. `pf_shift` collapses to 0 for the invalid
        // horizontal size 0b10 (rows repeat); the masks wrap the tile
        // column / pixel row exactly like real hardware.
        let (pf_shift, pf_col_mask, pf_row_mask) = self.playfield_geometry();
        let reg11 = self.registers[11];
        let h_scroll_mode = reg11 & 0x03;
        let v_scroll_mode = (reg11 >> 2) & 0x01;
        let hscroll_base = ((self.registers[13] & 0x3F) as usize) << 10;

        // H40 select is RS0 (reg12 bit 0), as in display_width().
        let is_h40 = (self.registers[12] & 0x01) != 0;
        let window_nametable = self.get_window_nametable();
        let window_width_tiles: u16 = if is_h40 { 64 } else { 32 };
        let window_area = self.window_area();
        let line_is_window = window_area.takes_line(line);

        // Horizontal wrap span in pixels (tile-column count derived from the
        // column mask). The vertical wrap is an AND with `pf_row_mask` so the
        // invalid size 0b10 (mask 0x2FF) wraps non-power-of-two like hardware.
        let plane_w_px = ((pf_col_mask as i32) + 1) * 8;

        // Read horizontal scroll values (depends on mode but NOT on x)
        // Mode 0: full screen, Mode 2: per-cell (8 lines), Mode 3: per-line
        let h_scroll_a = self.read_hscroll(hscroll_base, 0, h_scroll_mode, line);
        let h_scroll_b = self.read_hscroll(hscroll_base, 2, h_scroll_mode, line);

        // Read vertical scroll for full-screen mode (mode 0)
        let v_scroll_a_full = if v_scroll_mode == 0 {
            if !self.vsram.is_empty() {
                self.vsram[0] & 0x03FF
            } else {
                0
            }
        } else {
            0
        };
        let v_scroll_b_full = if v_scroll_mode == 0 {
            if 1 < self.vsram.len() {
                self.vsram[1] & 0x03FF
            } else {
                0
            }
        } else {
            0
        };

        // Left-most 2-cell column vertical-scroll quirk:
        // with per-column vscroll active, a line whose horizontal scroll is not
        // 16px-aligned shows a partial left-edge column that does NOT read VSRAM
        // column 0. H40 shares column 19's Plane A AND Plane B value across both
        // planes; H32 disables it (value 0). The exact extent/value is
        // underdocumented (Genesis Plus GX and PicoDrive disagree); SG1 gives
        // the picture Genesis Plus GX gives. The fine shift is the hscroll's low 4 bits (the
        // offset within a 16px column); only the first `shift` pixels are special.
        let left_col_vs: u16 = if is_h40 && 39 < self.vsram.len() {
            (self.vsram[38] & self.vsram[39]) & 0x03FF
        } else {
            0
        };
        let shift_a = h_scroll_a & 0x0F;
        let shift_b = h_scroll_b & 0x0F;

        // Composite each pixel
        for x in 0..width {
            let xu16 = x as u16;

            // ---- Plane B ----
            let v_scroll_b = if v_scroll_mode == 0 {
                v_scroll_b_full
            } else if shift_b != 0 && xu16 < shift_b {
                // Partial left-edge column: special left-most-column vscroll.
                left_col_vs
            } else {
                let col = (xu16 / 16) as usize;
                let vi = col * 2 + 1;
                if vi < self.vsram.len() {
                    self.vsram[vi] & 0x03FF
                } else {
                    0
                }
            };

            let scrolled_x_b = (xu16 as i32 - h_scroll_b as i32).rem_euclid(plane_w_px) as u16;
            let v_line_b = (line as u32 + v_scroll_b as u32) & pf_row_mask as u32;
            let tile_col_b = (scrolled_x_b / 8) & pf_col_mask;
            let tile_row_b = (v_line_b >> 3) as u16;
            let px_b = (scrolled_x_b % 8) as u8;
            let py_b = (v_line_b & 7) as u8;

            let row_off_b = (((tile_row_b as u32) << pf_shift) & 0x1FC0) as u16;
            let nt_off_b = row_off_b + tile_col_b * 2;
            let addr_b = (plane_b_nametable + nt_off_b) as usize;

            let (plane_b_idx, plane_b_pri) = if !dis_plane_b && addr_b + 1 < self.vram.len() {
                let word_b = ((self.vram[addr_b] as u16) << 8) | (self.vram[addr_b + 1] as u16);
                let pattern_b = word_b & 0x07FF;
                let flip_h_b = (word_b & 0x0800) != 0;
                let flip_v_b = (word_b & 0x1000) != 0;
                let palette_b = ((word_b >> 13) & 0x03) as usize;
                let priority_b = (word_b & 0x8000) != 0;

                let ci_b = Self::read_tile_pixel_flipped(
                    &self.vram, pattern_b, px_b, py_b, flip_h_b, flip_v_b,
                );
                if ci_b != 0 {
                    let cram_i = palette_b * 16 + ci_b as usize;
                    (Some(cram_i.min(63) as u8), priority_b)
                } else {
                    (None, false)
                }
            } else {
                (None, false)
            };

            // ---- Plane A or Window ----
            let in_window = line_is_window || window_area.takes_column(xu16);
            let (plane_a_idx, plane_a_pri) = if in_window {
                // Window plane (no scrolling)
                let wtx = xu16 / 8;
                let wty = line / 8;
                let wpx = (xu16 % 8) as u8;
                let wpy = (line % 8) as u8;
                let wnt_off = (wty * window_width_tiles + wtx) * 2;
                let waddr = (window_nametable + wnt_off) as usize;

                if !dis_window && waddr + 1 < self.vram.len() {
                    let wword = ((self.vram[waddr] as u16) << 8) | (self.vram[waddr + 1] as u16);
                    let wpat = wword & 0x07FF;
                    let wfh = (wword & 0x0800) != 0;
                    let wfv = (wword & 0x1000) != 0;
                    let wpal = ((wword >> 13) & 0x03) as usize;
                    let wpri = (wword & 0x8000) != 0;

                    let wci = Self::read_tile_pixel_flipped(&self.vram, wpat, wpx, wpy, wfh, wfv);
                    if wci != 0 {
                        let cram_i = wpal * 16 + wci as usize;
                        (Some(cram_i.min(63) as u8), wpri)
                    } else {
                        (None, false)
                    }
                } else {
                    (None, false)
                }
            } else {
                // Plane A (with scrolling)
                let v_scroll_a = if v_scroll_mode == 0 {
                    v_scroll_a_full
                } else if shift_a != 0 && xu16 < shift_a {
                    // Partial left-edge column: special left-most-column vscroll.
                    left_col_vs
                } else {
                    let col = (xu16 / 16) as usize;
                    let vi = col * 2;
                    if vi < self.vsram.len() {
                        self.vsram[vi] & 0x03FF
                    } else {
                        0
                    }
                };

                let scrolled_x_a = (xu16 as i32 - h_scroll_a as i32).rem_euclid(plane_w_px) as u16;
                let v_line_a = (line as u32 + v_scroll_a as u32) & pf_row_mask as u32;
                let tile_col_a = (scrolled_x_a / 8) & pf_col_mask;
                let tile_row_a = (v_line_a >> 3) as u16;
                let px_a = (scrolled_x_a % 8) as u8;
                let py_a = (v_line_a & 7) as u8;

                let row_off_a = (((tile_row_a as u32) << pf_shift) & 0x1FC0) as u16;
                let nt_off_a = row_off_a + tile_col_a * 2;
                let addr_a = (plane_a_nametable + nt_off_a) as usize;

                if !dis_plane_a && addr_a + 1 < self.vram.len() {
                    let word_a = ((self.vram[addr_a] as u16) << 8) | (self.vram[addr_a + 1] as u16);
                    let pattern_a = word_a & 0x07FF;
                    let flip_h_a = (word_a & 0x0800) != 0;
                    let flip_v_a = (word_a & 0x1000) != 0;
                    let palette_a = ((word_a >> 13) & 0x03) as usize;
                    let priority_a = (word_a & 0x8000) != 0;

                    let ci_a = Self::read_tile_pixel_flipped(
                        &self.vram, pattern_a, px_a, py_a, flip_h_a, flip_v_a,
                    );
                    if ci_a != 0 {
                        let cram_i = palette_a * 16 + ci_a as usize;
                        (Some(cram_i.min(63) as u8), priority_a)
                    } else {
                        (None, false)
                    }
                } else {
                    (None, false)
                }
            };

            // ---- Merge planes (priority only; colour resolved in remap) ----
            let (merged_idx, merged_pri) = match (plane_b_idx, plane_a_idx) {
                (None, None) => (None, false),
                (Some(b), None) => (Some(b), plane_b_pri),
                (None, Some(a)) => (Some(a), plane_a_pri),
                (Some(b), Some(a)) => {
                    if plane_a_pri {
                        (Some(a), true)
                    } else if plane_b_pri {
                        (Some(b), true)
                    } else {
                        (Some(a), false)
                    }
                }
            };

            // ---- Sprite from line buffer ----
            let s_ci = self.sprite_line_color[x];
            let has_sprite = s_ci != 0 && !dis_sprites;

            // ---- Composite to a final palette index ----
            let (final_index, is_high_priority, sh_operator) = if has_sprite {
                let s_pal = self.sprite_line_palette[x] as usize;
                let s_pri = self.sprite_line_priority[x];
                let s_idx = (s_pal * 16 + s_ci as usize).min(63) as u8;

                // Shadow/Highlight operator sprites (palette 3, colour 14/15) are
                // transparent brightness operators, not drawn pixels.
                let is_sh_operator = shadow_highlight && s_pal == 3 && (s_ci == 14 || s_ci == 15);
                let sh_op = if is_sh_operator { s_ci } else { 0 };

                if is_sh_operator {
                    // Operator sprite: transparent, keep the underlying pixel index.
                    match merged_idx {
                        None => (bd_idx, false, sh_op),
                        Some(bi) => (bi, merged_pri, sh_op),
                    }
                } else {
                    let (idx, pri) = match merged_idx {
                        None => (s_idx, false),
                        Some(bi) => {
                            if s_pri {
                                (s_idx, true)
                            } else if merged_pri {
                                (bi, true)
                            } else {
                                (s_idx, false)
                            }
                        }
                    };
                    (idx, pri, 0)
                }
            } else {
                match merged_idx {
                    None => (bd_idx, false, 0u8),
                    Some(bi) => (bi, merged_pri, 0u8),
                }
            };

            if let Some((dbg_x, dbg_y)) = self.debug_pixel {
                if xu16 == dbg_x && line == dbg_y {
                    let plane_b_color = plane_b_idx.map(|i| self.cram_rgb[i as usize]);
                    let plane_a_color = plane_a_idx.map(|i| self.cram_rgb[i as usize]);
                    let merged_color = merged_idx.map(|i| self.cram_rgb[i as usize]);
                    let final_color = self.cram_rgb[final_index as usize];
                    let backdrop_color = self.cram_rgb[bd_idx as usize];
                    self.log_debug_pixel(
                        x,
                        line,
                        in_window,
                        addr_b,
                        plane_a_nametable,
                        plane_b_nametable,
                        window_nametable,
                        pf_shift,
                        pf_col_mask,
                        pf_row_mask,
                        h_scroll_mode,
                        v_scroll_mode,
                        is_h40,
                        hscroll_base,
                        h_scroll_a,
                        h_scroll_b,
                        v_scroll_a_full,
                        v_scroll_b_full,
                        plane_w_px,
                        plane_b_color,
                        plane_b_pri,
                        plane_a_color,
                        plane_a_pri,
                        merged_color,
                        merged_pri,
                        has_sprite,
                        final_color,
                        is_high_priority,
                        sh_operator,
                        backdrop_color,
                    );
                }
            }

            // ---- Store this pixel's resolved indices for the remap pass ----
            self.line_index[x] = final_index;
            self.line_sh_op[x] = sh_operator;
            self.line_high_pri[x] = is_high_priority;
            // `plane_a_idx` holds the window's pixel where the window covers.
            let plane_a_or_window = plane_a_idx.map_or(-1, |i| i as i16);
            if in_window {
                self.line_plane_a[x] = -1;
                self.line_window[x] = plane_a_or_window;
            } else {
                self.line_plane_a[x] = plane_a_or_window;
                self.line_window[x] = -1;
            }
            self.line_plane_b[x] = plane_b_idx.map_or(-1, |i| i as i16);
        }
    }

    /// Turn this line's palette-index buffers into RGB in the framebuffer,
    /// applying shadow/highlight and the active render-layer mode. Cheap and
    /// re-runnable after a mid-line CRAM change (no recompositing).
    fn remap_scanline(&mut self, line: u16) {
        let width = self.display_width() as usize;
        self.remap_scanline_segment(line, 0, width);
    }

    /// Remap a horizontal span `[x_start, x_end)` of this line's index buffers
    /// to RGB. A mid-line CRAM write re-tints only `[beam_x, width)`, the
    /// pixels at and to the right of the beam, not yet scanned out, so several
    /// writes across a line build up multiple colour segments (a per-write
    /// approximation of per-pixel CRAM, finer than a whole-line update).
    /// `remap_scanline` is the whole-line case `[0, width)`.
    fn remap_scanline_segment(&mut self, line: u16, x_start: usize, x_end: usize) {
        let internal_width = 320usize;
        let fb_offset = (line as usize) * internal_width * 4;
        let shadow_highlight = (self.registers[12] & 0x08) != 0;

        for x in x_start..x_end {
            let final_index = self.line_index[x];
            let sh_operator = self.line_sh_op[x];
            let is_high_priority = self.line_high_pri[x];
            let base_color = self.cram_rgb[final_index as usize];

            // ---- Shadow/Highlight final color adjustment ----
            let final_color = if shadow_highlight {
                if sh_operator == 14 {
                    Self::highlight_color(base_color)
                } else if sh_operator == 15 || is_high_priority {
                    base_color
                } else {
                    Self::shadow_color(base_color)
                }
            } else {
                base_color
            };

            // ---- Layer-mode rendering (debug visualization) ----
            let output_color = match self.render_layer_mode {
                RenderLayerMode::Normal => final_color,
                RenderLayerMode::PlaneB => {
                    if self.line_plane_b[x] >= 0 {
                        self.cram_rgb[self.line_plane_b[x] as usize]
                    } else {
                        TRANSPARENT_RGB
                    }
                }
                RenderLayerMode::PlaneA => {
                    if self.line_plane_a[x] >= 0 {
                        self.cram_rgb[self.line_plane_a[x] as usize]
                    } else {
                        TRANSPARENT_RGB
                    }
                }
                RenderLayerMode::Window => {
                    if self.line_window[x] >= 0 {
                        self.cram_rgb[self.line_window[x] as usize]
                    } else {
                        TRANSPARENT_RGB
                    }
                }
                RenderLayerMode::Sprites => {
                    let s_ci = self.sprite_line_color[x];
                    if s_ci != 0 && sh_operator == 0 {
                        let sp = self.sprite_line_palette[x] as usize;
                        let ci = (sp * 16 + s_ci as usize).min(63);
                        self.cram_rgb[ci]
                    } else {
                        TRANSPARENT_RGB
                    }
                }
                RenderLayerMode::PriorityOverlay => {
                    if is_high_priority {
                        // Tint green for high-priority pixels
                        let r = (final_color.0 as u16 * 6 / 10) as u8;
                        let g = ((final_color.1 as u16).saturating_add(80)).min(255) as u8;
                        let b = (final_color.2 as u16 * 6 / 10) as u8;
                        (r, g, b)
                    } else {
                        final_color
                    }
                }
                RenderLayerMode::SpritePriority => {
                    let s_ci = self.sprite_line_color[x];
                    if s_ci != 0 && sh_operator == 0 {
                        if self.sprite_line_priority[x] {
                            (220, 60, 60) // Red = high-priority sprite
                        } else {
                            (60, 60, 220) // Blue = low-priority sprite
                        }
                    } else {
                        (final_color.0 / 3, final_color.1 / 3, final_color.2 / 3)
                    }
                }
            };

            let idx = fb_offset + x * 4;
            if idx + 3 < self.framebuffer.len() {
                self.framebuffer[idx] = output_color.0;
                self.framebuffer[idx + 1] = output_color.1;
                self.framebuffer[idx + 2] = output_color.2;
                self.framebuffer[idx + 3] = 255;
            }
        }
    }

    /// Detailed per-pixel composition diagnostic (stderr).
    /// Called from render_scanline only when debug_pixel is set and the
    /// current pixel matches. Kept separate to avoid bloating the hot loop.
    #[cold]
    #[allow(clippy::too_many_arguments)]
    fn log_debug_pixel(
        &self,
        x: usize,
        line: u16,
        in_window: bool,
        addr_b: usize,
        plane_a_nametable: u16,
        plane_b_nametable: u16,
        window_nametable: u16,
        pf_shift: u32,
        pf_col_mask: u16,
        pf_row_mask: u16,
        h_scroll_mode: u8,
        v_scroll_mode: u8,
        is_h40: bool,
        hscroll_base: usize,
        h_scroll_a: u16,
        h_scroll_b: u16,
        v_scroll_a_full: u16,
        v_scroll_b_full: u16,
        plane_w_px: i32,
        plane_b_color: Option<(u8, u8, u8)>,
        plane_b_pri: bool,
        plane_a_color: Option<(u8, u8, u8)>,
        plane_a_pri: bool,
        merged_color: Option<(u8, u8, u8)>,
        merged_pri: bool,
        has_sprite: bool,
        final_color: (u8, u8, u8),
        is_high_priority: bool,
        sh_operator: u8,
        backdrop_color: (u8, u8, u8),
    ) {
        let xu16 = x as u16;
        let s_ci = self.sprite_line_color[x];
        let s_pal = self.sprite_line_palette[x];
        let s_pri = self.sprite_line_priority[x];

        let winner = if !has_sprite {
            if merged_color.is_some() {
                "plane"
            } else {
                "backdrop"
            }
        } else if sh_operator != 0 {
            "sh_operator"
        } else if s_pri {
            "sprite(hi)"
        } else if merged_pri {
            "plane(hi)"
        } else {
            "sprite(lo)"
        };

        eprintln!(
            "=== DEBUG PIXEL ({}, {}) frame={} ===",
            xu16, line, self.timing.frame_count
        );

        eprintln!("  --- VDP Registers ---");
        eprintln!(
            "  r2(PlaneA)=0x{:02X} -> base=0x{:04X}  r4(PlaneB)=0x{:02X} -> base=0x{:04X}",
            self.registers[2], plane_a_nametable, self.registers[4], plane_b_nametable
        );
        let (scroll_width, scroll_height) = self.get_scroll_size();
        eprintln!(
            "  r3(Window)=0x{:02X} -> base=0x{:04X}  r16(scroll_size)=0x{:02X} -> {}x{} tiles (shift={} col_mask=0x{:02X} row_mask=0x{:03X})",
            self.registers[3], window_nametable, self.registers[16], scroll_width, scroll_height,
            pf_shift, pf_col_mask, pf_row_mask
        );
        eprintln!(
            "  r11(mode3)=0x{:02X} -> hscroll_mode={} vscroll_mode={}",
            self.registers[11], h_scroll_mode, v_scroll_mode
        );
        eprintln!(
            "  r12(mode4)=0x{:02X} -> H40={}  r13(hscroll_base)=0x{:02X} -> 0x{:04X}",
            self.registers[12], is_h40, self.registers[13], hscroll_base
        );
        eprintln!(
            "  r15(auto_inc)=0x{:02X}  r17(WinH)=0x{:02X}  r18(WinV)=0x{:02X}",
            self.registers[15], self.registers[17], self.registers[18]
        );

        eprintln!("  --- Window Selection ---");
        let reg17 = self.registers[17];
        let reg18 = self.registers[18];
        let v_pos_w = (reg18 & 0x1F) as u16 * 8;
        let v_down_w = (reg18 & 0x80) != 0;
        let h_pos_w = (reg17 & 0x1F) as u16;
        let h_right_w = (reg17 & 0x80) != 0;
        eprintln!(
            "  window: v_pos={} v_down={} h_pos={} h_right={} -> in_window={}",
            v_pos_w, v_down_w, h_pos_w, h_right_w, in_window
        );

        // Plane B geometry
        eprintln!("  --- Plane B Geometry ---");
        let dbg_vscroll_b = if v_scroll_mode == 0 {
            v_scroll_b_full
        } else {
            let col = (xu16 / 16) as usize;
            let vi = col * 2 + 1;
            if vi < self.vsram.len() {
                self.vsram[vi] & 0x03FF
            } else {
                0
            }
        };
        let dbg_sx_b = (xu16 as i32 - h_scroll_b as i32).rem_euclid(plane_w_px) as u16;
        let dbg_vline_b = (line as u32 + dbg_vscroll_b as u32) & pf_row_mask as u32;
        let dbg_tr_b = (dbg_vline_b >> 3) as u16;
        let dbg_tc_b = (dbg_sx_b / 8) & pf_col_mask;
        eprintln!(
            "  hscroll_b={} vscroll_b={} scrolled=({},{}) tile=({},{}) px=({},{})",
            h_scroll_b,
            dbg_vscroll_b,
            dbg_sx_b,
            dbg_vline_b,
            dbg_tc_b,
            dbg_tr_b,
            dbg_sx_b % 8,
            dbg_vline_b & 7
        );
        eprintln!(
            "  nt_base=0x{:04X} nt_off=0x{:04X} -> addr=0x{:04X}",
            plane_b_nametable,
            (((dbg_tr_b as u32) << pf_shift) & 0x1FC0) as u16 + dbg_tc_b * 2,
            addr_b
        );
        let word_b = if addr_b + 1 < self.vram.len() {
            let w = ((self.vram[addr_b] as u16) << 8) | (self.vram[addr_b + 1] as u16);
            eprintln!(
                "  raw_bytes=[0x{:02X}, 0x{:02X}] -> word=0x{:04X}",
                self.vram[addr_b],
                self.vram[addr_b + 1],
                w
            );
            w
        } else {
            0
        };
        let ci_b = Self::read_tile_pixel_flipped(
            &self.vram,
            word_b & 0x07FF,
            (dbg_sx_b % 8) as u8,
            (dbg_vline_b & 7) as u8,
            (word_b & 0x0800) != 0,
            (word_b & 0x1000) != 0,
        );
        eprintln!(
            "  decoded: pat={} pal={} flipH={} flipV={} pri={} ci={} color={:?}",
            word_b & 0x07FF,
            (word_b >> 13) & 0x03,
            (word_b & 0x0800) != 0,
            (word_b & 0x1000) != 0,
            plane_b_pri,
            ci_b,
            plane_b_color
        );

        // Plane A / Window geometry
        eprintln!("  --- Plane A Geometry ---");
        if in_window {
            let wtx = xu16 / 8;
            let wty = line / 8;
            let win_width_tiles: u16 = if is_h40 { 64 } else { 32 };
            let wnt_off = (wty * win_width_tiles + wtx) * 2;
            let waddr = (window_nametable + wnt_off) as usize;
            eprintln!(
                "  SOURCE=WINDOW  tile=({},{}) window_base=0x{:04X} addr=0x{:04X}",
                wtx, wty, window_nametable, waddr
            );
            if waddr + 1 < self.vram.len() {
                let w = ((self.vram[waddr] as u16) << 8) | (self.vram[waddr + 1] as u16);
                eprintln!(
                    "  raw_bytes=[0x{:02X}, 0x{:02X}] -> word=0x{:04X}",
                    self.vram[waddr],
                    self.vram[waddr + 1],
                    w
                );
                eprintln!(
                    "  decoded: pat={} pal={} flipH={} flipV={} pri={} color={:?}",
                    w & 0x07FF,
                    (w >> 13) & 0x03,
                    (w & 0x0800) != 0,
                    (w & 0x1000) != 0,
                    plane_a_pri,
                    plane_a_color
                );
            }
        } else {
            let dbg_vscroll_a = if v_scroll_mode == 0 {
                v_scroll_a_full
            } else {
                let col = (xu16 / 16) as usize;
                let vi = col * 2;
                if vi < self.vsram.len() {
                    self.vsram[vi] & 0x03FF
                } else {
                    0
                }
            };
            let dbg_sx_a = (xu16 as i32 - h_scroll_a as i32).rem_euclid(plane_w_px) as u16;
            let dbg_vline_a = (line as u32 + dbg_vscroll_a as u32) & pf_row_mask as u32;
            let dbg_tr_a = (dbg_vline_a >> 3) as u16;
            let dbg_tc_a = (dbg_sx_a / 8) & pf_col_mask;
            eprintln!("  SOURCE=PLANE_A  hscroll_a={} vscroll_a={} scrolled=({},{}) tile=({},{}) px=({},{})",
                h_scroll_a, dbg_vscroll_a, dbg_sx_a, dbg_vline_a,
                dbg_tc_a, dbg_tr_a, dbg_sx_a % 8, dbg_vline_a & 7);
            let nt_off_a = (((dbg_tr_a as u32) << pf_shift) & 0x1FC0) as u16 + dbg_tc_a * 2;
            let addr_a = (plane_a_nametable + nt_off_a) as usize;
            eprintln!(
                "  nt_base=0x{:04X} nt_off=0x{:04X} -> addr=0x{:04X}",
                plane_a_nametable, nt_off_a, addr_a
            );
            if addr_a + 1 < self.vram.len() {
                let w = ((self.vram[addr_a] as u16) << 8) | (self.vram[addr_a + 1] as u16);
                eprintln!(
                    "  raw_bytes=[0x{:02X}, 0x{:02X}] -> word=0x{:04X}",
                    self.vram[addr_a],
                    self.vram[addr_a + 1],
                    w
                );
                eprintln!(
                    "  decoded: pat={} pal={} flipH={} flipV={} pri={} color={:?}",
                    w & 0x07FF,
                    (w >> 13) & 0x03,
                    (w & 0x0800) != 0,
                    (w & 0x1000) != 0,
                    plane_a_pri,
                    plane_a_color
                );
            }
        }

        eprintln!("  --- Merge & Composite ---");
        eprintln!("  merged: color={:?} pri={}", merged_color, merged_pri);
        eprintln!(
            "  sprite: ci={} pal={} pri={} has_sprite={}",
            s_ci, s_pal, s_pri, has_sprite
        );
        eprintln!(
            "  winner={} final_color={:?} is_high_pri={} sh_op={}",
            winner, final_color, is_high_priority, sh_operator
        );
        eprintln!("  backdrop: {:?}", backdrop_color);
        eprintln!("=== END DEBUG PIXEL ===");
    }

    /// Apply shadow effect (half brightness).
    fn shadow_color(color: (u8, u8, u8)) -> (u8, u8, u8) {
        (color.0 / 2, color.1 / 2, color.2 / 2)
    }

    /// Apply highlight effect (increased brightness).
    fn highlight_color(color: (u8, u8, u8)) -> (u8, u8, u8) {
        // Increase brightness by 50% (capped at 255)
        let r = ((color.0 as u16 * 3) / 2).min(255) as u8;
        let g = ((color.1 as u16 * 3) / 2).min(255) as u8;
        let b = ((color.2 as u16 * 3) / 2).min(255) as u8;
        (r, g, b)
    }

    /// Get scroll plane size from register 16.
    fn get_scroll_size(&self) -> (u16, u16) {
        let reg16 = self.registers[16];
        let h_size = reg16 & 0x03;
        let v_size = (reg16 >> 4) & 0x03;

        let width = match h_size {
            0 => 32,
            1 => 64,
            3 => 128,
            _ => 32,
        };

        let height = match v_size {
            0 => 32,
            1 => 64,
            3 => 128,
            _ => 32,
        };

        (width, height)
    }

    /// Scroll-plane geometry derived from register 16.
    ///
    /// Returns `(row_shift, col_mask_tiles, row_mask_px)`:
    /// - `row_shift`: how far the tile-row index is shifted to form the
    ///   nametable row offset (also the log2 of the byte stride per row).
    /// - `col_mask_tiles`: AND-mask that wraps the horizontal tile column.
    /// - `row_mask_px`: AND-mask that wraps the vertical pixel position.
    ///
    /// A plane is 32, 64 or 128 tiles wide (sizes 0, 1 and 3), and its two
    /// vertical size bits are bits 8 and 9 of the row mask, above eight
    /// bits that are always set. The horizontal size `0b10` is *invalid* on
    /// real hardware: the row offset stays 0, so every scanline re-reads
    /// nametable row 0 of a plane 32 tiles wide. Likewise the vertical size
    /// `0b10` yields the non-power-of-two mask `0x2FF`. Scroll-size / window
    /// test ROMs depend on these quirks, so the render path uses this model
    /// instead of a plain width multiply (checked against Genesis Plus GX).
    fn playfield_geometry(&self) -> (u32, u16, u16) {
        let reg16 = self.registers[16];
        let (row_shift, col_mask_tiles) = match reg16 & 0x03 {
            0b00 => (6, 0x1F),
            0b01 => (7, 0x3F),
            0b11 => (8, 0x7F),
            _ => (0, 0x1F),
        };
        let v_size = u16::from((reg16 >> 4) & 0x03);
        (row_shift, col_mask_tiles, (v_size << 8) | 0x0FF)
    }

    /// The part of the screen the window plane takes from plane A, from
    /// registers 17 and 18 (see [`WindowArea`] for the rule).
    pub fn window_area(&self) -> WindowArea {
        WindowArea::from_registers(self.registers[17], self.registers[18])
    }

    /// Rebuild sprite cache from SAT.
    fn rebuild_sprite_cache(&mut self) {
        // Get SAT address from register 5
        // Format: bits 6-0 x 0x200 (H40 mode: bits 6-1 x 0x400)
        let reg5 = self.registers[5];
        let is_h40 = (self.registers[12] & 0x01) != 0;

        let sat_addr = if is_h40 {
            // H40 mode: bits 6-1 x 0x400
            ((reg5 & 0x7E) as u16) << 9
        } else {
            // H32 mode: bits 6-0 x 0x200
            ((reg5 & 0x7F) as u16) << 9
        };

        // Total sprite limit: 80 in H40, 64 in H32 (max_sprite_pixels >> 2).
        let max_sprites = if is_h40 { 80 } else { 64 };
        self.sprite_cache.build(&self.vram, sat_addr, max_sprites);
        self.sprite_cache_valid = true;
    }

    /// Invalidate sprite cache (call when SAT or VRAM changes).
    pub fn invalidate_sprite_cache(&mut self) {
        self.sprite_cache_valid = false;
    }

    /// Check if VBLANK interrupt should be triggered.
    /// Uses the vint_irq_pending flag which is separate from the status register F flag.
    /// This flag is cleared when the system acknowledges the interrupt request via ack_vint(),
    /// preventing the same VINT from being re-requested on every instruction cycle.
    pub fn should_trigger_vint(&self) -> bool {
        self.timing.vint_irq_pending
    }

    /// Check if HBLANK interrupt should be triggered.
    pub fn should_trigger_hint(&self) -> bool {
        self.timing.hblank_pending
    }

    /// Acknowledge VBLANK interrupt.
    pub fn ack_vint(&mut self) {
        self.timing.ack_vint();
    }

    /// Acknowledge HBLANK interrupt.
    pub fn ack_hint(&mut self) {
        self.timing.ack_hint();
    }

    /// Get current timing state (for debugging).
    pub fn timing(&self) -> &timing::VdpTiming {
        &self.timing
    }

    /// Get mutable timing state.
    pub fn timing_mut(&mut self) -> &mut timing::VdpTiming {
        &mut self.timing
    }

    /// Interrupt acknowledge (IACK) for VINT.
    /// Called when the 68K CPU actually takes the level 6 interrupt.
    /// Clears the F flag and vint_irq_pending (MAME and Genesis Plus GX agree).
    pub fn ack_vint_iack(&mut self) {
        self.timing.ack_vint_iack();
    }

    /// Get current scanline.
    pub fn current_scanline(&self) -> u16 {
        self.timing.scanline
    }

    /// Check if in VBLANK.
    pub fn in_vblank(&self) -> bool {
        self.timing.in_vblank
    }

    /// Check if in HBLANK.
    pub fn in_hblank(&self) -> bool {
        self.timing.in_hblank
    }

    /// Update HBLANK status based on the 68K's cycle position within the
    /// current scanline. HBLANK occupies the last portion of each scanline
    /// (~37 H-clocks out of 420, roughly the last 72 68K cycles of ~488).
    pub fn set_hblank_approx(&mut self, cycles_into_scanline: u32, cycles_per_scanline: u32) {
        // HBLANK is approximately the last 72 68K cycles of the scanline
        // (37 H-clocks * 2 68K cycles per H-clock ~ 74, we use 72 for safety)
        let hblank_start = cycles_per_scanline.saturating_sub(72);
        self.timing.in_hblank = cycles_into_scanline >= hblank_start;
        // Remember the beam position so HV-counter reads reflect it.
        self.beam_cycles_into_line = cycles_into_scanline;
        self.beam_cycles_per_line = cycles_per_scanline;
    }

    /// V-counter overflow value for the current mode. The counter counts up to
    /// this value, then wraps by `lines_per_frame` so the low byte jumps (the
    /// documented jump points: NTSC 224-line = 0xEA, 240-line = 0x106; PAL
    /// 224 = 0x102, 240 = 0x10A).
    fn vc_max(&self) -> i32 {
        // PAL runs 312 lines/frame, NTSC 262.
        let pal = self.timing.total_scanlines() > 300;
        let mode_240 = (self.registers[1] & 0x08) != 0;
        match (mode_240, pal) {
            (false, false) => 0xEA,
            (false, true) => 0x102,
            (true, false) => 0x106,
            (true, true) => 0x10A,
        }
    }

    /// Genesis V counter for `0xC00008` reads. Reflects the current scanline
    /// with the hardware's mid-frame counter jump (the byte is not a plain
    /// `line & 0xFF`): once the line passes `vc_max`, the counter wraps by the
    /// total line count so e.g. NTSC 224-line reads 0x00..0xEA then 0xE5..0xFF.
    fn hv_v_counter(&self) -> u8 {
        let lines_per_frame = self.timing.total_scanlines() as i32;
        let vc_max = self.vc_max();
        let mut vc = self.timing.scanline as i32;
        if vc > vc_max {
            vc -= lines_per_frame;
        }
        (vc & 0xFF) as u8
    }

    /// Genesis H counter for `0xC00008` reads, approximated from the 68K beam
    /// position within the line. Real hardware exposes a two-segment ramp with a
    /// gap (H40: 0x00..0xB6 then 0xE5..0xFF; H32: 0x00..0x93 then 0xE9..0xFF);
    /// we map the beam fraction onto that ramp. This is a per-line approximation
    /// (SG1 advances the beam per instruction, not per pixel), but it makes the
    /// low byte meaningful for code that polls it instead of returning ~0.
    fn hv_h_counter(&self) -> u8 {
        let per_line = self.beam_cycles_per_line.max(1) as u64;
        let pos = (self.beam_cycles_into_line as u64).min(per_line);
        let h40 = (self.registers[12] & 0x01) != 0;
        if h40 {
            // 183 values 0x00..=0xB6, then 27 values 0xE5..=0xFF (210 steps).
            // Clamp to the last index so a beam exactly at line end reads 0xFF,
            // not a wrapped 0x00.
            let p = ((pos * 210 / per_line) as u32).min(209);
            if p <= 0xB6 {
                p as u8
            } else {
                (0xE5 + (p - 0xB7)) as u8
            }
        } else {
            // 148 values 0x00..=0x93, then 23 values 0xE9..=0xFF (171 steps).
            let p = ((pos * 171 / per_line) as u32).min(170);
            if p <= 0x93 {
                p as u8
            } else {
                (0xE9 + (p - 0x94)) as u8
            }
        }
    }

    /// Check if DMA is currently active (includes background VRAM Fill/Copy).
    pub fn is_dma_active(&self) -> bool {
        self.dma_busy || self.dma.is_active()
    }

    /// Check if a Memory-to-VRAM DMA is pending (needs CPU memory access).
    pub fn is_dma_pending(&self) -> bool {
        self.dma_pending
    }

    /// Execute pending Memory-to-VRAM DMA with a pre-read buffer of CPU memory.
    /// The buffer contains the bytes to be written, already read from the 68K address space.
    pub fn execute_pending_dma(&mut self, dma_buffer: &[u8]) {
        if !self.dma_pending {
            return;
        }
        self.dma_pending = false;

        let raw_length = ((self.registers[20] as u16) << 8) | (self.registers[19] as u16);
        // DMA length 0 means 65536 words (pre-decrementing counter on hardware)
        let length: u32 = if raw_length == 0 {
            0x10000
        } else {
            raw_length as u32
        };

        let auto_increment = self.registers[15] as u16;
        let dest_start = self.address;

        // Compute source address for tracking and post-DMA register update
        let addr_low = self.registers[21] as u32;
        let addr_mid = self.registers[22] as u32;
        // Use 7 bits of R23 (& 0x7F): bit 6 is the A23 address bit
        // DMA mode 00: A23=0 (ROM/cartridge space 0x000000-0x7FFFFF)
        // DMA mode 01: A23=1 (work RAM/IO space 0x800000-0xFFFFFF)
        let addr_high = (self.registers[23] & 0x7F) as u32;
        let source_start = ((addr_high << 16) | (addr_mid << 8) | addr_low) << 1;

        let code = self.code & 0x0F;
        // bus.rs pre-reads `length` words, so this normally equals `length`;
        // clamp to the buffer to stay in bounds.
        let words = (length as usize).min(dma_buffer.len() / 2);

        // A CRAM transfer issued while the beam is in active display is a raster
        // palette/backdrop effect (e.g. Direct Color DMA): meter it across the
        // remaining visible scanlines (see apply_progressive_cram_for_line)
        // instead of collapsing every per-line color into one final value.
        // Everything else (all VRAM/VSRAM, and any CRAM DMA during blanking,
        // which is what ordinary games do) lands instantly here.
        let progressive = code == 0x03
            && !self.timing.in_vblank
            && self.timing.scanline < self.timing.visible_scanlines();

        if progressive {
            // Words per line to CRAM: the blank rate applies when
            // the display is forcibly off (reg1 bit6=0), which is exactly how
            // Direct Color DMA streams a full picture in one frame, and the
            // active rate applies with the display on. H40 (RS0=reg12 bit0) is
            // wider than H32. Blank rates are the refresh-adjusted CRAM values
            // (204->198 H40, 166->161 H32).
            let h40 = (self.registers[12] & 0x01) != 0;
            let display_off = (self.registers[1] & 0x40) == 0;
            let rate = match (display_off, h40) {
                (true, true) => 198,
                (true, false) => 161,
                (false, true) => 18,
                (false, false) => 16,
            };
            // ~83% into the line's slot group (164/198), where the backdrop
            // stands when Genesis Plus GX draws the line (see `sample_offset`).
            let sample_offset = (rate * 164) / 198;
            self.progressive_cram = Some(ProgressiveCramDma {
                buffer: dma_buffer[..words * 2].to_vec(),
                dest_start,
                auto_inc: auto_increment,
                rate,
                sample_offset,
                length: words,
                cursor: 0,
            });
            // The whole transfer still streams through the FIFO, so the last
            // words remain the open-bus source for any post-DMA read.
            for i in 0..words {
                let bo = i * 2;
                self.push_fifo_word(((dma_buffer[bo] as u16) << 8) | (dma_buffer[bo + 1] as u16));
            }
            // The 68000 is frozen for the whole 68k-bus DMA, so from its point of
            // view the transfer completes atomically: a CRAM data-port read-back
            // taken right after it (VDPFIFOTesting "DMA Transfer to CRAM ...")
            // must observe the FINAL palette. Commit the logical CRAM now. The
            // per-line appliers below (`apply_progressive_cram_for_line` /
            // `render_dcdma_line_beamx`) only re-tint the *render* table
            // (`cram_rgb`) for the raster effect; they deliberately leave this
            // read-back state (`cram`) at its final value.
            let mut commit_dest = dest_start;
            for i in 0..words {
                let bo = i * 2;
                let word = ((dma_buffer[bo] as u16) << 8) | (dma_buffer[bo + 1] as u16);
                let index = ((commit_dest & 0x7E) >> 1) as usize;
                if index < 64 {
                    self.cram[index] = word & 0x0EEE;
                }
                commit_dest = commit_dest.wrapping_add(auto_increment);
            }
        } else {
            let mut dest = dest_start;
            for i in 0..words {
                let byte_offset = i * 2;
                let word =
                    ((dma_buffer[byte_offset] as u16) << 8) | (dma_buffer[byte_offset + 1] as u16);

                // Each DMA word passes through the FIFO, like a data-port
                // write, so it becomes the open-bus source for any read
                // issued after the transfer.
                self.push_fifo_word(word);

                match code {
                    0x01 => {
                        // VRAM write: an odd address swaps the word's bytes (as in MAME)
                        let raw_addr = dest as usize;
                        let addr = raw_addr & 0xFFFE; // Word-align
                        if addr + 1 < self.vram.len() {
                            if (raw_addr & 1) != 0 {
                                self.vram[addr] = (word & 0xFF) as u8;
                                self.vram[addr + 1] = (word >> 8) as u8;
                            } else {
                                self.vram[addr] = (word >> 8) as u8;
                                self.vram[addr + 1] = (word & 0xFF) as u8;
                            }
                            self.trace_vram_write(addr as u16, word, "dma-mem");
                        }
                    }
                    0x03 => {
                        // CRAM write (blanking path, instant)
                        let index = ((dest & 0x7E) >> 1) as usize;
                        if index < 64 {
                            let masked = word & 0x0EEE;
                            self.cram[index] = masked;
                            self.cram_rgb[index] = Self::convert_genesis_color(masked);
                        }
                    }
                    0x05 => {
                        // VSRAM write
                        let index = ((dest & 0x7E) >> 1) as usize;
                        if index < 64 {
                            self.vsram[index] = word & 0x07FF;
                        }
                    }
                    _ => {}
                }
                dest = dest.wrapping_add(auto_increment);
            }
        }

        // Registers reflect the completed transfer regardless of path: the 68000
        // is frozen for the DMA's full duration, so it never observes the
        // intermediate state of a metered CRAM transfer.
        self.address = dest_start.wrapping_add((words as u16).wrapping_mul(auto_increment));

        // Clear DMA length registers after completion (per MAME)
        self.registers[19] = 0;
        self.registers[20] = 0;

        // Update the DMA source registers (21/22) with the post-transfer value.
        // The source advances inside a fixed 128 KB window: only the low 16
        // bits of the word address count, so the counter wraps at 0x10000
        // while register 23, the window base, is NEVER modified by the
        // transfer (checked against Genesis Plus GX). So when a transfer's source
        // crosses a 128 KB boundary, registers 21/22 wrap to the start of the
        // window (VDPFIFOTesting "DMA source register update / wrapping").
        let orig_word_src = ((self.registers[22] as u32) << 8) | (self.registers[21] as u32);
        let new_word_src = orig_word_src.wrapping_add(words as u32) & 0xFFFF;
        self.registers[21] = (new_word_src & 0xFF) as u8;
        self.registers[22] = ((new_word_src >> 8) & 0xFF) as u8;
        // Reconstruct the full byte source only for the debug bookkeeping below.
        let source = (source_start & !0x1FFFF) | ((new_word_src << 1) & 0x1FFFF);

        // Bookkeeping for the instant data copy (kept for debug inspection).
        let raw_length_u16 = raw_length;
        self.dma.start(
            dma::DmaOperation::MemoryToVram {
                source,
                dest: self.address,
                length: raw_length_u16,
            },
            self.in_vblank(),
        );
        self.dma.complete();

        // The bytes land instantly above, but a 68k-bus DMA freezes the 68000
        // for the transfer's duration on real hardware. Record the master-clock
        // cost (a word to CRAM/VSRAM, or to VRAM) so the frame loop drains it
        // into the per-line CPU budget. No DMA-busy bit is asserted for 68k-bus
        // DMA: the CPU is halted instead, so it can never observe one.
        let transfer = self.dma_bus_transfer();
        self.pending_cpu_dma_mclks += self.dma_busy_master_clocks(transfer, length);
    }

    /// Advance a deferred mid-active CRAM DMA up to the slot that line `line`
    /// renders with (the picture was checked against Genesis Plus GX).
    /// The transfer runs at `rate` words/line (the 68000 is frozen for its whole
    /// duration), and the line shows the backdrop as of `line*rate +
    /// sample_offset`, ~83% across the line rather
    /// than on the black right-edge word that ends each picture row. Applying
    /// every word up to that slot keeps non-backdrop CRAM indices correct for
    /// auto-incrementing transfers too. True per-pixel fidelity (PicoDrive)
    /// would need cycle-accurate mid-line CRAM painting, beyond a scanline
    /// renderer.
    fn apply_progressive_cram_for_line(&mut self, line: u16) {
        let mut prog = match self.progressive_cram.take() {
            Some(p) => p,
            None => return,
        };

        let target = ((line as usize) * prog.rate + prog.sample_offset + 1).min(prog.length);
        while prog.cursor < target {
            let byte_offset = prog.cursor * 2;
            if byte_offset + 1 >= prog.buffer.len() {
                prog.cursor = prog.length; // buffer exhausted; nothing left to apply
                break;
            }
            let word =
                ((prog.buffer[byte_offset] as u16) << 8) | (prog.buffer[byte_offset + 1] as u16);
            // dest wraps mod 65536; (cursor as u16) preserves the value mod 65536.
            let dest = prog
                .dest_start
                .wrapping_add((prog.cursor as u16).wrapping_mul(prog.auto_inc));
            let index = ((dest & 0x7E) >> 1) as usize;
            if index < 64 {
                // Only re-tint the render table; `cram` (read-back state) was
                // already committed to its final value when the DMA executed.
                self.cram_rgb[index] = Self::convert_genesis_color(word & 0x0EEE);
            }
            prog.cursor += 1;
        }

        // Keep the transfer alive while later visible lines still draw from it;
        // it is re-armed every frame by the demo, so this never leaks.
        if prog.cursor < prog.length {
            self.progressive_cram = Some(prog);
        }
    }

    /// Paint one Direct-Color-DMA line straight from the DMA source buffer,
    /// approximating per-pixel CRAM by beam-X: with the display off the whole
    /// line is backdrop, each DMA word shows for two pixels, the source
    /// advances one word per pair, and the words that land during horizontal
    /// sync (38 in H40, 33 in H32) are skipped at the line end, which sums to
    /// the same per-line word `rate` SG1 already meters. (PicoDrive shows these
    /// pictures the same way.)
    /// This resolves the streamed picture (the spaceship) that a whole-line
    /// remap can only render as horizontal bands, with no cycle-accurate CPU.
    fn render_dcdma_line_beamx(&mut self, line: u16) {
        let mut prog = match self.progressive_cram.take() {
            Some(p) => p,
            None => {
                self.render_scanline(line);
                return;
            }
        };

        let width = self.display_width() as usize;
        let internal_width = 320usize;
        let fb_offset = (line as usize) * internal_width * 4;
        // The first word of this line sits at the running cursor. Two pixels per
        // word.
        let base = prog.cursor;
        let backdrop = self.backdrop_rgb();
        for x in 0..width {
            let slot = base + (x / 2);
            let bo = slot * 2;
            let (r, g, b) = if bo + 1 < prog.buffer.len() {
                let word = ((prog.buffer[bo] as u16) << 8) | (prog.buffer[bo + 1] as u16);
                Self::convert_genesis_color(word & 0x0EEE)
            } else {
                backdrop
            };
            let idx = fb_offset + x * 4;
            if idx + 3 < self.framebuffer.len() {
                self.framebuffer[idx] = r;
                self.framebuffer[idx + 1] = g;
                self.framebuffer[idx + 2] = b;
                self.framebuffer[idx + 3] = 255;
            }
        }
        // Keep the index buffers consistent (backdrop) so any later mid-line
        // remap on this line stays well-defined rather than reusing a stale
        // composite, as `render_blanked_line` does.
        let bd_idx = self.backdrop_index().min(63) as u8;
        for x in 0..width {
            self.line_index[x] = bd_idx;
            self.line_sh_op[x] = 0;
            self.line_high_pri[x] = false;
            self.line_plane_a[x] = -1;
            self.line_window[x] = -1;
            self.line_plane_b[x] = -1;
        }

        // Advance the running CRAM state by this line's whole word group (painted
        // words + HSYNC words = `rate`), applying auto-increment so reads and the
        // next line start from the correct CRAM, exactly like the band path.
        let end = (base + prog.rate).min(prog.length);
        while prog.cursor < end {
            let bo = prog.cursor * 2;
            if bo + 1 >= prog.buffer.len() {
                prog.cursor = prog.length;
                break;
            }
            let word = ((prog.buffer[bo] as u16) << 8) | (prog.buffer[bo + 1] as u16);
            let dest = prog
                .dest_start
                .wrapping_add((prog.cursor as u16).wrapping_mul(prog.auto_inc));
            let index = ((dest & 0x7E) >> 1) as usize;
            if index < 64 {
                // Render table only; `cram` read-back state stays at its
                // DMA-final value (committed in `execute_pending_dma`).
                self.cram_rgb[index] = Self::convert_genesis_color(word & 0x0EEE);
            }
            prog.cursor += 1;
        }

        if prog.cursor < prog.length {
            self.progressive_cram = Some(prog);
        }
    }

    /// Get DMA state (for debugging).
    pub fn dma_state(&self) -> &dma::DmaState {
        &self.dma
    }

    /// Get VDP registers (for testing/debugging).
    pub fn registers(&self) -> &[u8; 24] {
        &self.registers
    }

    /// Get current address and code (for testing/debugging).
    pub fn current_access(&self) -> (u16, u8) {
        (self.address, self.code)
    }

    /// Set VDP registers directly (for testing).
    pub fn set_test_register(&mut self, reg_num: usize, value: u8) {
        // Force the register (and its side effects) regardless of the Mode-4
        // access mask: this is a test/debug override, not a guest control-port
        // write, so it must set exactly what the caller asks for.
        self.apply_register_write(reg_num, value);
    }

    /// Set address and code directly (for testing).
    pub fn set_test_access(&mut self, address: u16, code: u8) {
        self.address = address;
        self.code = code;
    }

    /// Start a DMA operation based on current registers and control word.
    pub fn start_dma(&mut self, cpu_memory: &[u8]) {
        // Parse DMA operation from registers
        if let Some(mut operation) =
            dma::parse_dma_registers(&self.registers, self.address, self.code)
        {
            // For VRAM fill, use the last value written to data port
            if let dma::DmaOperation::VramFill { ref mut value, .. } = operation {
                *value = self.dma_fill_value;
            }

            // Execute the DMA operation immediately
            match operation {
                dma::DmaOperation::MemoryToVram {
                    source,
                    dest,
                    length,
                } => {
                    self.execute_dma_memory_to_vram(source, dest, length, cpu_memory);
                }
                dma::DmaOperation::VramFill {
                    dest,
                    value,
                    length,
                } => {
                    self.execute_dma_vram_fill(dest, value, length as u32);
                }
                dma::DmaOperation::VramCopy {
                    source,
                    dest,
                    length,
                } => {
                    self.execute_dma_vram_copy(source, dest, length as u32);
                }
            }

            // Start DMA state tracking
            self.dma.start(operation, self.in_vblank());
        }
    }

    /// Execute Memory to VRAM DMA.
    fn execute_dma_memory_to_vram(
        &mut self,
        source: u32,
        dest: u16,
        length: u16,
        cpu_memory: &[u8],
    ) {
        let auto_increment = self.registers[15] as u16;
        let mut addr = dest;
        let mut src = source as usize;

        for _ in 0..length {
            if src + 1 < cpu_memory.len() {
                let high = cpu_memory[src];
                let low = cpu_memory[src + 1];

                let vram_addr = (addr as usize) & 0xFFFE;
                if vram_addr + 1 < self.vram.len() {
                    if (addr as usize & 1) != 0 {
                        // Odd destination: byte-swap
                        self.vram[vram_addr] = low;
                        self.vram[vram_addr + 1] = high;
                    } else {
                        self.vram[vram_addr] = high;
                        self.vram[vram_addr + 1] = low;
                    }
                    self.trace_vram_write(
                        vram_addr as u16,
                        ((high as u16) << 8) | (low as u16),
                        "dma-mem-old",
                    );
                }

                src += 2;
                addr = addr.wrapping_add(auto_increment);
            } else {
                break;
            }
        }

        self.address = addr;
    }

    /// Source triple `(fill_byte, cram_word, vsram_word)` for a fill triggered
    /// by data-port word `value`. VRAM replicates the *newest* FIFO entry's high
    /// byte (which equals `value >> 8`, since
    /// the triggering write pushed `value` last). CRAM/VSRAM do NOT use the
    /// just-written word: they replicate the *oldest* FIFO entry
    /// (checked against Genesis Plus GX). The initial cell still holds
    /// the written value (deposited by the data-port write before the fill);
    /// only the trailing cells take the FIFO ring's oldest entry.
    fn fill_source_words(&self, value: u16) -> (u8, u16, u16) {
        let fifo_oldest = self.fifo[self.fifo_idx];
        (
            (value >> 8) as u8,
            fifo_oldest & 0x0EEE,
            fifo_oldest & 0x07FF,
        )
    }

    /// Source triple for the *metered* fill, re-read live from the FIFO ring on
    /// every cell: the newest entry for VRAM, the oldest for CRAM/VSRAM (the
    /// documented hardware quirk). A data-port write that lands
    /// mid-fill pushes a new entry and the *rest* of the fill picks up the just
    /// written value. At the trigger this equals `fill_source_words(value)`
    /// (that write was the newest entry); after a concurrent write it tracks the
    /// new value, VDPFIFOTesting "DP Writes During DMA Fill".
    fn fill_source_words_live(&self) -> (u8, u16, u16) {
        let newest = self.fifo[(self.fifo_idx + 3) & 3];
        let oldest = self.fifo[self.fifo_idx];
        ((newest >> 8) as u8, oldest & 0x0EEE, oldest & 0x07FF)
    }

    /// Number of fill cells the real chip would have written between the
    /// triggering data-port write and the point a back-to-back follow-up access
    /// observes: the trigger line's remaining slots, served at once. In VBLANK
    /// the whole transfer lands now. On a forced-blank active line only
    /// the rest-of-line chunk lands now and the remainder streams from
    /// `tick_dma`. A fill starts two slots after its trigger, hence the
    /// 2-cell trim (the figure Genesis Plus GX uses).
    fn fill_initial_chunk(&self, length: u32) -> u64 {
        let len = length as u64;
        let display_on = (self.registers[1] & 0x40) != 0;
        let blanking = self.timing.in_vblank || !display_on;
        if self.timing.in_vblank || self.beam_cycles_per_line == 0 {
            return len;
        }
        let rate = self.dma_line_rate(DmaTransfer::Fill, blanking);
        let per_line = self.beam_cycles_per_line as u64;
        let into = (self.beam_cycles_into_line as u64).min(per_line);
        let cells_to_eol = (per_line - into) * rate / per_line;
        cells_to_eol.saturating_sub(2).min(len)
    }

    /// Write one fill cell at the live `self.address` (target chosen by the live
    /// `self.code`), then advance `self.address` by the auto-increment. Shared by
    /// the instant `execute_dma_vram_fill` (the `start_dma` path and unit tests)
    /// and the metered `advance_fill` (the `write_data_port` path) so both emit
    /// byte-for-byte identical cells. Reading address/code live is what lets a
    /// control-port write redirect a fill in flight (VDPFIFOTesting "DMA Fill
    /// Control Port Writes").
    fn fill_one_cell(&mut self, fill_byte: u8, cram_word: u16, vsram_word: u16) {
        let auto_increment = self.registers[15] as u16;
        let addr = self.address;
        match self.code & 0x0F {
            0x01 => {
                // VRAM fill: write high byte
                let vram_addr = addr as usize;
                if (vram_addr & 1) != 0 {
                    let word_addr = vram_addr ^ 1;
                    if word_addr < self.vram.len() {
                        self.vram[word_addr] = fill_byte;
                    }
                } else if vram_addr + 1 < self.vram.len() {
                    self.vram[vram_addr + 1] = fill_byte;
                }
                let wa = vram_addr & 0xFFFE;
                if wa + 1 < self.vram.len() {
                    let w = ((self.vram[wa] as u16) << 8) | (self.vram[wa + 1] as u16);
                    self.trace_vram_write(wa as u16, w, "dma-fill");
                }
            }
            0x03 => {
                // CRAM fill: source is the oldest FIFO entry, not `value`.
                let index = ((addr & 0x7E) >> 1) as usize;
                if index < 64 {
                    self.cram[index] = cram_word;
                    self.cram_rgb[index] = Self::convert_genesis_color(cram_word);
                }
            }
            0x05 => {
                // VSRAM fill: source is the oldest FIFO entry, not `value`.
                let index = ((addr & 0x7E) >> 1) as usize;
                if index < 64 {
                    self.vsram[index] = vsram_word;
                }
            }
            _ => {
                // Invalid target: the address still advances.
            }
        }
        self.address = addr.wrapping_add(auto_increment);
    }

    /// DMA source-address registers (21/22) are incremented during a DMA *even
    /// for a Fill*, which never reads the source. On completion they have
    /// advanced by the length-register value, and the length registers read
    /// zero. VDPFIFOTesting "DMA Fill Source Reg Update" checks this (and
    /// Genesis Plus GX does the same).
    fn dma_fill_source_writeback(&mut self) {
        let length_reg = ((self.registers[20] as u16) << 8) | (self.registers[19] as u16);
        let src = ((self.registers[22] as u16) << 8) | (self.registers[21] as u16);
        let end_src = src.wrapping_add(length_reg);
        self.registers[21] = (end_src & 0xFF) as u8;
        self.registers[22] = ((end_src >> 8) & 0xFF) as u8;
        // Clear DMA length registers after completion (per MAME)
        self.registers[19] = 0;
        self.registers[20] = 0;
    }

    /// Execute VRAM Fill DMA **instantly** (the `start_dma` path and the direct
    /// unit tests). The metered `write_data_port` path uses `advance_fill`
    /// instead. Fills with the high byte of `value`; supports VRAM/CRAM/VSRAM.
    /// Uses inclusive loop count (length + 1 iterations) per MAME behavior.
    fn execute_dma_vram_fill(&mut self, dest: u16, value: u16, length: u32) {
        self.address = dest;
        let (fill_byte, cram_word, vsram_word) = self.fill_source_words(value);

        // Fill loop: length + 1 iterations, as in MAME (James Pond 3 depends on it).
        for _ in 0..=length {
            self.fill_one_cell(fill_byte, cram_word, vsram_word);
        }

        self.dma_fill_source_writeback();
    }

    /// Stream up to `max_cells` pending fill cells through the live address/code
    /// (the metered counterpart of `execute_dma_vram_fill`'s loop). On
    /// completion clears `fill_state`; the source-register writeback and
    /// length-clear already ran at trigger in `write_data_port`.
    fn advance_fill(&mut self, max_cells: u64) {
        let Some(state) = self.fill_state else {
            return;
        };
        let n = max_cells.min(state.remaining as u64) as u32;
        for _ in 0..n {
            let (fill_byte, cram_word, vsram_word) = self.fill_source_words_live();
            self.fill_one_cell(fill_byte, cram_word, vsram_word);
        }
        let remaining = state.remaining - n;
        if remaining == 0 {
            self.fill_state = None;
        } else if let Some(s) = self.fill_state.as_mut() {
            s.remaining = remaining;
        }
    }

    /// Execute VRAM Copy DMA.
    fn execute_dma_vram_copy(&mut self, source: u16, dest: u16, length: u32) {
        let auto_increment = self.registers[15] as u16;
        let mut src = source;
        let mut dst = dest;

        // Copy byte-by-byte IN PLACE (read one byte, immediately write it, then
        // advance), NOT through a temp buffer. Overlapping src/dst regions must
        // propagate: a byte just written can become the source for a later
        // iteration, which is what the VDPFIFOTesting "DMA Copy" overlap
        // subtests verify. Source advances by one byte; destination advances by
        // the auto-increment register (checked against Genesis Plus GX).
        for _ in 0..length {
            let src_addr = (src ^ 1) as usize; // VRAM is kept byte-swapped
            let byte = if src_addr < self.vram.len() {
                self.vram[src_addr]
            } else {
                0
            };

            let dst_addr = (dst ^ 1) as usize; // VRAM is kept byte-swapped
            if dst_addr < self.vram.len() {
                self.vram[dst_addr] = byte;
                let wa = dst_addr & 0xFFFE;
                if wa + 1 < self.vram.len() {
                    let w = ((self.vram[wa] as u16) << 8) | (self.vram[wa + 1] as u16);
                    self.trace_vram_write(wa as u16, w, "dma-copy");
                }
            }

            src = src.wrapping_add(1);
            dst = dst.wrapping_add(auto_increment);
        }

        self.address = dst;

        // Clear DMA length registers after completion (per MAME)
        self.registers[19] = 0;
        self.registers[20] = 0;

        // Update source address registers
        self.registers[21] = (src & 0xFF) as u8;
        self.registers[22] = ((src >> 8) & 0xFF) as u8;
    }

    /// Execute DMA for a given number of cycles.
    /// Returns true if DMA completed.
    pub fn execute_dma_cycles(&mut self, cycles: u32) -> bool {
        self.dma.execute(cycles)
    }

    /// Single-rate reference for the DMA duration of `length` length-register
    /// units: `length * 3420 / rate`, the rate evaluated once at the
    /// current beam state. The emulator uses the beam-aware
    /// [`Self::dma_busy_master_clocks`] (which integrates the rate as the beam
    /// crosses active display into VBLANK); this single-rate formula is what
    /// the rate-model unit tests assert against.
    #[cfg(test)]
    fn dma_master_clocks(&self, transfer: DmaTransfer, length: u32) -> u64 {
        let display_on = (self.registers[1] & 0x40) != 0;
        let blanking = self.timing.in_vblank || !display_on;
        let rate = self.dma_line_rate(transfer, blanking);
        let len = if length == 0 { 0x10000 } else { length as u64 };
        len * MASTER_CLOCKS_PER_LINE / rate
    }

    /// Units of the DMA length counter that one scanline carries, for a
    /// transfer of the given kind, while the line is `blanking` (vertical
    /// blank, or the display forced off) or drawing the picture.
    ///
    /// A line offers the transfer 16 access slots in H32 and 18 in H40
    /// while the picture is drawn, and 166 or 204 while it is blanked. A
    /// fill writes a byte in each. A word to VRAM takes two, and so does a
    /// copied byte (a read and a write), so those move half as many units.
    /// A word to CRAM or VSRAM takes one, and a blanked line carries 161 or
    /// 198 of them, the memory refresh taking five or six slots. (The
    /// counts are the model Genesis Plus GX uses; completion times were
    /// checked against it.)
    fn dma_line_rate(&self, transfer: DmaTransfer, blanking: bool) -> u64 {
        let h40 = (self.registers[12] & 0x01) != 0;
        let slots: u64 = match (blanking, h40) {
            (false, false) => 16,
            (false, true) => 18,
            (true, false) => 166,
            (true, true) => 204,
        };
        match transfer {
            DmaTransfer::Fill => slots,
            DmaTransfer::BusToVram | DmaTransfer::Copy => slots / 2,
            DmaTransfer::BusToCramOrVsram if !blanking => slots,
            DmaTransfer::BusToCramOrVsram if h40 => 198,
            DmaTransfer::BusToCramOrVsram => 161,
        }
    }

    /// Master clocks a DMA transfer of `length` length-register units occupies
    /// **from the current beam position**, adding up the rate of each line
    /// it crosses instead of locking one rate for the whole transfer.
    ///
    /// The access rate belongs to the line the beam is on: a
    /// transfer kicked off during active display drains at the slow active
    /// rate only until the beam reaches VBLANK, where the ~10x faster blanking
    /// rate empties the remainder. The naive `length * 3420 / rate` (what
    /// [`Self::dma_master_clocks`] returns) assumes the trigger-time rate holds
    /// for the entire transfer, which overstates the DMA-busy window by up to
    /// ~10x for a large fill started mid-active-display with the display on.
    /// Contra Hard Corps' boot VRAM-clear fills (`0x1FFF` units, display on,
    /// lines ~133/202) would each stall for ~1.7 frames that way, and its
    /// attract demo would part from Genesis Plus GX's. Walking the scanlines
    /// gives the completion time that emulator arrives at.
    fn dma_busy_master_clocks(&self, transfer: DmaTransfer, length: u32) -> u64 {
        let display_on = (self.registers[1] & 0x40) != 0;
        let total_lines = (self.timing.total_scanlines() as u32).max(1);
        let active_lines = self.timing.visible_scanlines() as u32;
        let mut remaining = if length == 0 {
            0x10000u64
        } else {
            length as u64
        };
        let mut mclks: u64 = 0;
        let mut line = self.timing.scanline as u32 % total_lines;
        // A 0x10000 unit transfer at the slowest active rate spans a few
        // hundred lines; cap the walk generously so a pathological state can
        // never spin forever, then approximate any residual tail.
        for _ in 0..(total_lines * 64) {
            let blanking = !display_on || line >= active_lines;
            let rate = self.dma_line_rate(transfer, blanking);
            if remaining <= rate {
                mclks += remaining * MASTER_CLOCKS_PER_LINE / rate;
                remaining = 0;
                break;
            }
            remaining -= rate;
            mclks += MASTER_CLOCKS_PER_LINE;
            line += 1;
            if line >= total_lines {
                line = 0;
            }
        }
        if remaining > 0 {
            mclks += remaining * MASTER_CLOCKS_PER_LINE / self.dma_line_rate(transfer, true);
        }
        mclks
    }

    /// What a 68k-bus transfer to the current destination moves: words to
    /// CRAM or VSRAM (either of the code's bits 1 and 2 set), or to VRAM.
    fn dma_bus_transfer(&self) -> DmaTransfer {
        if (self.code & 0x06) != 0 {
            DmaTransfer::BusToCramOrVsram
        } else {
            DmaTransfer::BusToVram
        }
    }

    /// Master-clock offset into the VBLANK line at which the 68K vertical
    /// interrupt is asserted; this core sets the F flag at the line's start.
    /// Hardware raises VINT this far into line 224 rather than at its start:
    /// ~788 mclks in H40, ~770 in H32 (the offsets Genesis Plus GX uses).
    fn vint_delay_mclks(&self) -> u32 {
        if (self.registers[12] & 0x01) != 0 {
            788 // H40
        } else {
            770 // H32
        }
    }

    /// Advance the deferred 68K vertical-interrupt request delay by
    /// `master_clocks`. Once it elapses, the level-6 68K IRQ is allowed to
    /// assert. Called by the system alongside `tick_dma` after each CPU step so
    /// the delay is honoured on every execution path.
    pub fn tick_vint_delay(&mut self, master_clocks: u64) {
        self.timing.tick_vint_request_delay(master_clocks);
    }

    /// True while the 68K vertical-interrupt request is held off (beam has not
    /// reached the interrupt's point on the VBLANK line). The F flag is already set.
    pub fn vint_request_delayed(&self) -> bool {
        self.timing.vint_request_delayed()
    }

    /// Advance a background VRAM Fill/Copy by `master_clocks` and drop the
    /// DMA-busy status bit once the metered duration elapses. A Copy writes its
    /// data instantly, so the busy bit is a pure duration timer; a Fill streams
    /// its cells here proportionally to elapsed clocks (`fill_state`) so a
    /// data/control-port access that lands mid-fill observes the partially
    /// advanced address. Either way
    /// games that poll the busy bit see it asserted for the whole window, and a
    /// fill that nobody observes still completes (and writes every cell) by the
    /// time the duration runs out. 68k-bus DMA does not use this path; it
    /// stalls the CPU via `pending_cpu_dma_mclks`.
    pub fn tick_dma(&mut self, master_clocks: u64) {
        if !self.dma_busy {
            return;
        }
        // A fill armed by the control-port write but not yet started by its
        // data-port write holds the busy bit with the `u64::MAX` sentinel.
        // The transfer is not running
        // yet, so don't drain time against it; the data write installs the
        // real duration.
        if self.dma_remaining_clocks == u64::MAX {
            return;
        }
        self.dma_remaining_clocks = self.dma_remaining_clocks.saturating_sub(master_clocks);

        // Stream the in-flight fill. The per-tick cell count uses the same
        // fill rate (`dma_line_rate(DmaTransfer::Fill, ..)`) the busy duration was derived from,
        // so the two drain in step; when the duration runs out, flush whatever
        // is left so the cells and the busy timer always finish together (keeps
        // the post-fill address/VRAM final for a non-observing caller).
        if self.fill_state.is_some() {
            if self.dma_remaining_clocks == 0 {
                let left = self.fill_state.map_or(0, |s| s.remaining as u64);
                self.advance_fill(left);
            } else {
                let display_on = (self.registers[1] & 0x40) != 0;
                let blanking = self.timing.in_vblank || !display_on;
                let rate = self.dma_line_rate(DmaTransfer::Fill, blanking);
                let mut accum = self.fill_state.map_or(0, |s| s.cell_accum);
                accum += master_clocks * rate;
                let cells = accum / MASTER_CLOCKS_PER_LINE;
                if let Some(s) = self.fill_state.as_mut() {
                    s.cell_accum = accum - cells * MASTER_CLOCKS_PER_LINE;
                }
                self.advance_fill(cells);
            }
        }

        if self.dma_remaining_clocks == 0 {
            self.dma_busy = false;
        }
    }

    /// Drain up to `budget_mclks` master clocks of the pending 68k-bus DMA
    /// stall, returning the amount drained and leaving the remainder for the
    /// next call. The frame loop calls this once per scanline (budget = one
    /// line) so a multi-line transfer freezes the CPU line-by-line instead of
    /// stalling a whole frame at once (Genesis Plus GX meters its DMA per
    /// line too). The leftover persists across lines and frames in this field.
    pub fn drain_dma_stall_mclks(&mut self, budget_mclks: u64) -> u64 {
        let drained = self.pending_cpu_dma_mclks.min(budget_mclks);
        self.pending_cpu_dma_mclks -= drained;
        drained
    }

    // ======================================================================
    // VDP FIFO (Write FIFO)
    // ======================================================================

    /// Get the master clocks per access slot based on current display state.
    /// During active display, fewer slots are available (VDP uses most for rendering).
    /// During VBLANK many more slots are available (HBLANK keeps the active rate here).
    fn fifo_mclks_per_slot(&self) -> u64 {
        let is_h40 = (self.registers[12] & 0x01) != 0;
        let is_blanking = self.timing.in_vblank;

        // Access slot rates from hardware documentation (SpriteMind.Net):
        // H40 active display: 18 external access slots per scanline
        // H40 blanking: ~204 external access slots per scanline
        // H32 active display: 16 external access slots per scanline
        // H32 blanking: ~167 external access slots per scanline
        // Master clocks per scanline = 3420
        if is_blanking {
            if is_h40 {
                3420 / 204
            } else {
                3420 / 167
            } // ~16-20 mclks per slot
        } else if is_h40 {
            3420 / 18
        } else {
            3420 / 16
        } // ~190-214 mclks per slot
    }

    /// Tick the FIFO: drain entries based on elapsed master clocks.
    /// Called by the system before each CPU instruction.
    pub fn tick_fifo(&mut self, master_clocks: u64) {
        // Advance the monotonic FIFO reference clock first: the empty/full
        // drain timestamps are measured against it, so it must move every
        // instruction even once the entry counter has drained to 0.
        self.fifo_clock = self.fifo_clock.wrapping_add(master_clocks);

        if self.fifo_entries == 0 {
            self.fifo_mclk_counter = 0;
            return;
        }

        self.fifo_mclk_counter += master_clocks;
        let mclks_per_slot = self.fifo_mclks_per_slot();

        while self.fifo_entries > 0 && self.fifo_mclk_counter >= mclks_per_slot {
            self.fifo_mclk_counter -= mclks_per_slot;
            self.fifo_entries -= 1;
        }

        // If FIFO drained completely, reset counter
        if self.fifo_entries == 0 {
            self.fifo_mclk_counter = 0;
        }
    }

    /// Record a written word in the FIFO data ring (the open-bus source for
    /// later partial-width reads). Advances `fifo_idx` so `fifo[fifo_idx]` is the
    /// oldest of the last four words.
    fn push_fifo_word(&mut self, value: u16) {
        self.fifo[self.fifo_idx] = value;
        self.fifo_idx = (self.fifo_idx + 1) & 3;
    }

    /// Push a data port write into the FIFO.
    /// Returns the number of CPU wait cycles if the FIFO was full (CPU stalls).
    /// Returns 0 if there was room in the FIFO.
    pub fn push_fifo(&mut self) -> u32 {
        if self.fifo_entries < 4 {
            // Room in FIFO, no stall
            self.fifo_entries += 1;
            return 0;
        }

        // FIFO is full - CPU must wait until one slot drains
        let mclks_per_slot = self.fifo_mclks_per_slot();
        let remaining = mclks_per_slot.saturating_sub(self.fifo_mclk_counter);

        // Drain one entry
        self.fifo_mclk_counter = 0;
        // FIFO stays at 4 since we're pushing one and draining one

        // Convert master clocks to CPU cycles (round up)
        remaining.div_ceil(7) as u32
    }

    /// Reserve the VDP FIFO access slot for the data-port write about to be
    /// pushed at `fifo_idx`, recording its absolute drain time in
    /// `fifo_slot_mclks[fifo_idx]`. During active display the VDP only
    /// services the FIFO at fixed dots (`FIFO_TIMING_*`), so consecutive
    /// writes chain onto successive future slots and the status full/empty
    /// bits then read off those timestamps (the model Genesis Plus GX uses;
    /// the status bits were checked against it and against VDPFIFOTesting).
    /// Outside active display the FIFO drains effectively
    /// instantly, so the slot is left untouched (its stale, past timestamp
    /// reads as empty).
    ///
    /// On hardware a write that lands on an already-full FIFO holds the 68k
    /// until the oldest entry has been served. We apply
    /// that wait to the **VDP-internal `fifo_clock` only**, never the CPU
    /// cycle count, so two invariants both hold: a long back-to-back burst's
    /// drain progression is tracked (so the FIFO reads empty again a few slots
    /// after it ends; VDPFIFOTesting "DMA Busy Flag" probes read the empty bit
    /// after 16 writes), yet 68k instruction timing and MAME-trace parity are
    /// left completely untouched (no `pending_cpu_wait_cycles` is raised). The
    /// wait also bounds the chain to ~4 slots ahead of the clock, so the model
    /// never pretends more than the real four entries are in flight.
    fn fifo_record_slot(&mut self) {
        let display_on = (self.registers[1] & 0x40) != 0;
        if !display_on || self.timing.in_vblank {
            return;
        }

        let slots: &[u64] = if (self.registers[12] & 0x01) != 0 {
            &FIFO_TIMING_H40
        } else {
            &FIFO_TIMING_H32
        };

        let newest_served_at = self.fifo_slot_mclks[(self.fifo_idx + 3) & 3];
        let oldest_served_at = self.fifo_slot_mclks[self.fifo_idx];

        // While the write before this one still waits, this one queues
        // behind it. If all four entries are waiting, it cannot enter before
        // the oldest has left, so the VDP-internal clock moves to that
        // moment (the 68k FIFO wait, applied to status timing only, see
        // the doc comment).
        let queued = self.fifo_clock < newest_served_at;
        if queued {
            self.fifo_clock = self.fifo_clock.max(oldest_served_at);
        }
        let ready_at = if queued {
            newest_served_at
        } else {
            self.fifo_clock
        };

        // The first access slot after `ready_at`, counted from the start of
        // the scanline the clock is on. VRAM is byte-wide internally (a word
        // write is two accesses), so it is served one slot later; CRAM and
        // VSRAM take the word in one. The table runs on into the next line,
        // and the clamp guards an index past its end.
        let line_start = self.fifo_clock - (self.fifo_clock % MASTER_CLOCKS_PER_LINE);
        let into_line = ready_at.saturating_sub(line_start);
        let next_slot = slots.partition_point(|&at| at <= into_line);
        let second_access = usize::from((self.code & 0x06) == 0);
        let served = (next_slot + second_access).min(slots.len() - 1);
        self.fifo_slot_mclks[self.fifo_idx] = line_start + slots[served];
    }

    /// Check if FIFO is empty (for status register bit 9).
    pub fn is_fifo_empty(&self) -> bool {
        // Empty once the most-recently-queued entry has been serviced.
        self.fifo_clock >= self.fifo_slot_mclks[(self.fifo_idx + 3) & 3]
    }

    /// Check if FIFO is full (for status register bit 8).
    pub fn is_fifo_full(&self) -> bool {
        // Full while the oldest queued entry has not yet been serviced.
        self.fifo_clock < self.fifo_slot_mclks[self.fifo_idx]
    }

    // ======================================================================
    // DMA Halt Timing
    // ======================================================================

    /// Compute CPU halt cycles for Memory-to-VRAM DMA.
    /// The 68K is completely halted during this DMA type while data is transferred
    /// through the VDP's access slots.
    pub fn compute_dma_halt_cycles(&self, word_count: u16) -> u32 {
        let mclks_per_word = self.fifo_mclks_per_slot();
        let total_mclks = (word_count as u64) * mclks_per_word;
        // Convert to CPU cycles (round up)
        total_mclks.div_ceil(7) as u32
    }
}

impl Default for Vdp {
    fn default() -> Self {
        Self::new()
    }
}

// ===== Debugger Methods (public, outside of tests) =====
impl Vdp {
    /// Get tile data as RGBA (for debugger visualization).
    pub fn get_tile_rgba(&self, tile_index: u32, palette_index: u8) -> Vec<u8> {
        use crate::vdp::tiles::Tile;

        let tile_addr = (tile_index * 32) as usize;
        if tile_addr + 32 > self.vram.len() {
            return vec![0; 8 * 8 * 4]; // Return empty tile
        }

        let tile = Tile::decode(&self.vram[tile_addr..tile_addr + 32]);

        // Build RGBA palette
        let palette_start = (palette_index as usize) * 16;
        let mut rgba_palette = [0u8; 16 * 4];
        for i in 0..16 {
            if palette_start + i < self.cram.len() {
                let (r, g, b) = Self::convert_genesis_color(self.cram[palette_start + i]);
                rgba_palette[i * 4] = r;
                rgba_palette[i * 4 + 1] = g;
                rgba_palette[i * 4 + 2] = b;
                rgba_palette[i * 4 + 3] = 255;
            }
        }

        // Render tile to RGBA
        let mut rgba = vec![0u8; 8 * 8 * 4];
        for y in 0..8 {
            for x in 0..8 {
                let color_index = tile.pixels[y * 8 + x] as usize;
                let pixel_offset = (y * 8 + x) * 4;
                rgba[pixel_offset] = rgba_palette[color_index * 4];
                rgba[pixel_offset + 1] = rgba_palette[color_index * 4 + 1];
                rgba[pixel_offset + 2] = rgba_palette[color_index * 4 + 2];
                rgba[pixel_offset + 3] = if color_index == 0 { 0 } else { 255 };
            }
        }

        rgba
    }

    /// Get all palettes as RGBA (4 palettes x 16 colors).
    pub fn get_all_palettes_rgba(&self) -> Vec<u8> {
        let mut rgba = vec![0u8; 64 * 4]; // 4 palettes x 16 colors x RGBA

        for i in 0..64 {
            let (r, g, b) = Self::convert_genesis_color(self.cram[i]);
            rgba[i * 4] = r;
            rgba[i * 4 + 1] = g;
            rgba[i * 4 + 2] = b;
            rgba[i * 4 + 3] = 255;
        }

        rgba
    }

    /// The number of sprites in the sprite table's link chain: the ones the
    /// renderer walks from sprite 0, following each link until one of 0 or
    /// one past the table (80 sprites in H40, 64 in H32). Sprite 0 is always
    /// in the chain.
    pub fn get_sprite_count_debug(&self) -> usize {
        retro_core::video::VideoDebugger::resolved_sprite_render_order(self).len()
    }

    /// Get VRAM for debugging (read-only access).
    pub fn get_vram(&self) -> &[u8] {
        &self.vram
    }

    /// Get mutable VRAM for debugging/testing.
    pub fn get_vram_mut(&mut self) -> &mut [u8] {
        &mut self.vram
    }

    /// Get CRAM for debugging (read-only access).
    pub fn get_cram(&self) -> &[u16] {
        &self.cram
    }

    /// Get mutable CRAM for debugging/testing.
    pub fn get_cram_mut(&mut self) -> &mut [u16] {
        &mut self.cram
    }

    /// Get VSRAM for debugging (read-only access).
    pub fn get_vsram(&self) -> &[u16] {
        &self.vsram
    }

    /// Get mutable VSRAM for debugging/testing.
    pub fn get_vsram_mut(&mut self) -> &mut [u16] {
        &mut self.vsram
    }

    /// Get plane nametable address.
    pub fn get_plane_a_nametable(&self) -> u16 {
        ((self.registers[2] as u16) << 10) & 0xE000
    }

    /// Get plane B nametable address.
    pub fn get_plane_b_nametable(&self) -> u16 {
        ((self.registers[4] as u16) << 13) & 0xE000
    }

    /// Get window nametable address. In H40 the table is 4 KB aligned:
    /// register 3's bit 1 (address bit 11) is ignored there.
    pub fn get_window_nametable(&self) -> u16 {
        let mask = if (self.registers[12] & 0x01) != 0 {
            0xF000
        } else {
            0xF800
        };
        ((self.registers[3] as u16) << 10) & mask
    }
}

// Savestate support
use crate::system::savestate::{VdpState, VdpTimingState};

impl Vdp {
    /// Switch the VDP's video region (NTSC/PAL). Only the region flag
    /// is updated; the transient scanline/counter state is left intact.
    /// Intended for session setup (before frames run): the fast-frame
    /// loop drives the scanline from 0 each frame, so the region flag is
    /// what actually selects the 262 vs 313 line count and VBLANK window.
    pub fn set_region(&mut self, region: timing::VideoRegion) {
        self.timing.region = region;
    }

    pub fn to_savestate(&self) -> VdpState {
        let t = &self.timing;
        VdpState {
            vram: self.vram.clone(),
            cram: self.cram.clone(),
            vsram: self.vsram.clone(),
            registers: self.registers,
            control_word: self.control_word,
            pending_write: self.pending_write,
            address: self.address,
            code: self.code,
            timing: VdpTimingState {
                region: match t.region {
                    timing::VideoRegion::Ntsc => 0,
                    timing::VideoRegion::Pal => 1,
                },
                scanline: t.scanline,
                h_counter: t.h_counter,
                frame_cycles: t.frame_cycles,
                frame_count: t.frame_count,
                in_vblank: t.in_vblank,
                in_hblank: t.in_hblank,
                vblank_pending: t.vblank_pending,
                vint_irq_pending: t.vint_irq_pending,
                hblank_pending: t.hblank_pending,
                h_counter_reload: t.h_counter_reload,
                h_counter_current: t.h_counter_current,
            },
            dma_fill_value: self.dma_fill_value,
            dma_pending: self.dma_pending,
            dma_busy: self.dma_busy,
            dma_remaining_clocks: self.dma_remaining_clocks,
            // Reserved field retained for savestate layout compatibility.
            dma_crosses_vblank: false,
            sprite_overflow: self.sprite_overflow,
            sprite_collision: self.sprite_collision,
            sprite_mask_overflow: self.sprite_mask_overflow,
            odd_frame: self.odd_frame,
            fifo_entries: self.fifo_entries,
            fifo_mclk_counter: self.fifo_mclk_counter,
            pending_cpu_dma_mclks: self.pending_cpu_dma_mclks,
            read_buffer: self.read_buffer,
            hvc_latch: self.hvc_latch,
            fifo: self.fifo,
            fifo_idx: self.fifo_idx as u8,
            fifo_clock: self.fifo_clock,
            fifo_slot_mclks: self.fifo_slot_mclks,
            dma_fill_pending: self.dma_fill_pending,
        }
    }

    pub fn from_savestate(&mut self, s: &VdpState) {
        self.vram = s.vram.clone();
        self.cram = s.cram.clone();
        self.vsram = s.vsram.clone();
        // A savestate may carry only the 40 physical words; the addressable range is 64.
        self.vsram.resize(64, 0);
        self.registers = s.registers;
        self.control_word = s.control_word;
        self.pending_write = s.pending_write;
        self.address = s.address;
        self.code = s.code;

        let t = &s.timing;
        self.timing.region = if t.region == 0 {
            timing::VideoRegion::Ntsc
        } else {
            timing::VideoRegion::Pal
        };
        self.timing.scanline = t.scanline;
        self.timing.h_counter = t.h_counter;
        self.timing.frame_cycles = t.frame_cycles;
        self.timing.frame_count = t.frame_count;
        self.timing.in_vblank = t.in_vblank;
        self.timing.in_hblank = t.in_hblank;
        self.timing.vblank_pending = t.vblank_pending;
        self.timing.vint_irq_pending = t.vint_irq_pending;
        self.timing.hblank_pending = t.hblank_pending;
        self.timing.h_counter_reload = t.h_counter_reload;
        self.timing.h_counter_current = t.h_counter_current;

        // `vint_request_delay_mclks` and `first_frame_after_reset` are
        // within-frame/boot-transient and not carried in the savestate.
        // A savestate is only ever captured at a frame boundary, where both
        // have reached their resolved values: the VINT-request delay has
        // elapsed (`-1`), and the first post-reset VBLANK has long passed
        // (`false`). Canonicalize them here so a state loaded into a FRESH
        // emulator (whose defaults are `-1` / `true`) matches the instance
        // that produced it. Without the `false` reset, a fresh instance keeps
        // the default `true` and silently swallows its first VBLANK interrupt
        // after load, a one-frame desync that breaks netplay rollback and
        // shows as a subtle glitch on every ordinary savestate load.
        self.timing.vint_request_delay_mclks = -1;
        self.timing.first_frame_after_reset = false;

        self.dma_fill_value = s.dma_fill_value;
        self.dma_pending = s.dma_pending;
        self.dma_busy = s.dma_busy;
        self.dma_remaining_clocks = s.dma_remaining_clocks;
        // Transient within-frame raster state; a freshly loaded state never
        // carries a half-applied mid-active CRAM DMA.
        self.progressive_cram = None;
        self.sprite_overflow = s.sprite_overflow;
        self.sprite_collision = s.sprite_collision;
        self.sprite_mask_overflow = s.sprite_mask_overflow;
        self.odd_frame = s.odd_frame;
        self.fifo_entries = s.fifo_entries;
        self.fifo_mclk_counter = s.fifo_mclk_counter;
        self.pending_cpu_dma_mclks = s.pending_cpu_dma_mclks;
        self.read_buffer = s.read_buffer;
        self.hvc_latch = s.hvc_latch;
        self.fifo = s.fifo;
        self.fifo_idx = (s.fifo_idx as usize) & 3;
        self.fifo_clock = s.fifo_clock;
        self.fifo_slot_mclks = s.fifo_slot_mclks;
        self.dma_fill_pending = s.dma_fill_pending;

        // Rebuild derived state
        self.sprite_cache_valid = false;
        self.rebuild_cram_rgb();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vdp_creation() {
        let vdp = Vdp::new();
        assert_eq!(vdp.vram.len(), 0x10000);
        assert_eq!(vdp.cram.len(), 64);
        assert_eq!(vdp.vsram.len(), 64);
        assert_eq!(vdp.framebuffer.len(), 320 * 224 * 4);
    }

    #[test]
    fn rerender_current_frame_reflects_layer_mask() {
        let mut vdp = Vdp::new();

        // Display on, V28 (224 lines), H32.
        vdp.registers[1] = 0x44; // display enable (bit6) + bit2 (mode5)
        vdp.registers[12] = 0x00; // H32

        // Plane A nametable at 0xE000 (reg2 bits 5-3), Plane B at 0x0000
        // (reg4=0). A non-overlapping base keeps the nametable clear of the
        // tile we author at index 1.
        vdp.registers[2] = 0x38;

        // Author tile #1 (VRAM 32..64) fully as palette colour 1.
        for b in 0..32 {
            vdp.vram[32 + b] = 0x11;
        }
        // Plane A cell (0,0) -> tile #1, palette 0.
        vdp.vram[0xE000] = 0x00;
        vdp.vram[0xE001] = 0x01;

        // Distinct palette entries: colour 1 = bright blue (plane), colour
        // 2 = bright red (backdrop via reg7 -> palette 0 colour 2).
        vdp.cram[1] = 0x0E00;
        vdp.cram[2] = 0x000E;
        vdp.registers[7] = 0x02;
        vdp.rebuild_cram_rgb();

        let plane_rgb = vdp.cram_rgb[1];
        let backdrop_rgb = vdp.cram_rgb[2];
        assert_ne!(plane_rgb, backdrop_rgb, "test colours must differ");

        // Paused re-render with the layer visible paints the plane colour.
        vdp.rerender_current_frame();
        let px = |v: &Vdp| (v.framebuffer[0], v.framebuffer[1], v.framebuffer[2]);
        assert_eq!(px(&vdp), plane_rgb, "plane A should be visible at (0,0)");

        // Hide Plane A and re-render the *same* frame: (0,0) falls back to
        // the backdrop without the emulator advancing a single cycle.
        vdp.set_layer_debug_disabled(LAYER_PLANE_A, true);
        vdp.rerender_current_frame();
        assert_eq!(px(&vdp), backdrop_rgb, "hidden plane A exposes backdrop");

        // Re-enabling restores the plane on the next paused re-render.
        vdp.set_layer_debug_disabled(LAYER_PLANE_A, false);
        vdp.rerender_current_frame();
        assert_eq!(px(&vdp), plane_rgb, "re-enabled plane A returns");
    }

    #[test]
    fn test_vdp_reset() {
        let mut vdp = Vdp::new();

        // Modify some state
        vdp.registers[0] = 0xFF;
        vdp.address = 0x1234;
        vdp.vram[0] = 0xAB;

        // Reset
        vdp.reset();

        // Check defaults
        assert_eq!(vdp.registers[0], 0x04);
        assert_eq!(vdp.registers[1], 0x04);
        assert_eq!(vdp.registers[12], 0x81);
        assert_eq!(vdp.registers[15], 0x02);
        assert_eq!(vdp.address, 0);
        assert_eq!(vdp.vram[0], 0);
    }

    #[test]
    fn test_vdp_status_read() {
        let mut vdp = Vdp::new();
        // Read high byte of status (0xC00004)
        let status_high = vdp.read(0xC00004);
        // Read low byte of status (0xC00005)
        let status_low = vdp.read(0xC00005);
        let status = ((status_high as u16) << 8) | (status_low as u16);
        assert_eq!(status & 0x0200, 0x0200); // FIFO empty bit set

        // Set VBLANK
        vdp.timing.in_vblank = true;
        let status_high = vdp.read(0xC00004);
        let status_low = vdp.read(0xC00005);
        let status = ((status_high as u16) << 8) | (status_low as u16);
        assert_eq!(status & 0x0008, 0x0008); // VBLANK bit set
    }

    #[test]
    fn test_control_port_write() {
        let mut vdp = Vdp::new();

        // Write control word (two writes)
        vdp.write(0xC00004, 0x00);
        assert!(vdp.pending_write);

        vdp.write(0xC00004, 0x40);
        assert!(!vdp.pending_write);
    }

    #[test]
    fn test_vram_write() {
        let mut vdp = Vdp::new();

        // Set VRAM write mode directly
        vdp.code = 0x01; // VRAM write
        vdp.address = 0x1000;
        // Default auto-increment is 2 (register 15 = 0x02)

        // Write 16-bit word: high byte goes to even addr, low byte to odd addr
        vdp.write_data_port(0x1234, true);
        assert_eq!(vdp.vram[0x1000], 0x12); // high byte
        assert_eq!(vdp.vram[0x1001], 0x34); // low byte
                                            // Auto-increment by 2
        assert_eq!(vdp.address, 0x1002);

        // Write data at new address
        vdp.write_data_port(0x5678, true);
        assert_eq!(vdp.vram[0x1002], 0x56); // high byte
        assert_eq!(vdp.vram[0x1003], 0x78); // low byte
        assert_eq!(vdp.address, 0x1004);
    }

    #[test]
    fn test_cram_write() {
        let mut vdp = Vdp::new();

        // Set CRAM write mode
        vdp.code = 0x03; // CRAM write
        vdp.address = 0x00;
        vdp.registers[15] = 2; // Auto-increment by 2

        // Write a color (9-bit RGB: 0000 BBB0 GGG0 RRR0)
        vdp.write_data_port(0x0EEE, true); // Bright white

        // Check CRAM
        assert_eq!(vdp.cram[0], 0x0EEE);
        assert_eq!(vdp.address, 2); // Auto-incremented
    }

    #[test]
    fn test_color_conversion() {
        // Test black (000)
        let (r, g, b) = Vdp::convert_genesis_color(0x0000);
        assert_eq!((r, g, b), (0, 0, 0));

        // Test white (777) -> 0x0EEE
        let (r, g, b) = Vdp::convert_genesis_color(0x0EEE);
        assert_eq!((r, g, b), (255, 255, 255));

        // Test red (700) -> 0x000E
        let (r, g, b) = Vdp::convert_genesis_color(0x000E);
        assert_eq!(r, 255);
        assert_eq!(g, 0);
        assert_eq!(b, 0);

        // Test green (070) -> 0x00E0
        let (r, g, b) = Vdp::convert_genesis_color(0x00E0);
        assert_eq!(r, 0);
        assert_eq!(g, 255);
        assert_eq!(b, 0);

        // Test blue (007) -> 0x0E00
        let (r, g, b) = Vdp::convert_genesis_color(0x0E00);
        assert_eq!(r, 0);
        assert_eq!(g, 0);
        assert_eq!(b, 255);
    }

    #[test]
    fn test_backdrop_rendering() {
        let mut vdp = Vdp::new();

        // Set backdrop color to bright red (palette 0, color 1)
        vdp.cram[1] = 0x000E; // Red
        vdp.registers[7] = 0x01; // Backdrop: palette 0, color 1

        // Render backdrop
        vdp.render_backdrop();

        // Check first pixel
        assert_eq!(vdp.framebuffer[0], 255); // R
        assert_eq!(vdp.framebuffer[1], 0); // G
        assert_eq!(vdp.framebuffer[2], 0); // B
        assert_eq!(vdp.framebuffer[3], 255); // A
    }

    #[test]
    fn test_display_dimensions() {
        let mut vdp = Vdp::new();

        // Default: H40, V28
        assert_eq!(vdp.display_width(), 320);
        assert_eq!(vdp.display_height(), 224);

        // H32 mode
        vdp.registers[12] &= !0x01;
        assert_eq!(vdp.display_width(), 256);

        // V30 mode
        vdp.registers[1] |= 0x08;
        assert_eq!(vdp.display_height(), 240);
    }

    #[test]
    fn test_scanline_vblank() {
        let mut vdp = Vdp::new();

        // Before VBLANK
        vdp.scanline(100);
        assert!(!vdp.in_vblank());

        // During VBLANK (line 224+)
        vdp.scanline(224);
        assert!(vdp.in_vblank());
    }

    #[test]
    fn test_vint_enable() {
        let mut vdp = Vdp::new();

        // VINT disabled
        vdp.timing.in_vblank = true;
        vdp.timing.set_vint_pending();
        vdp.registers[1] &= !0x20;

        // Should still be pending (enable flag only controls triggering)
        assert!(vdp.should_trigger_vint());

        // Clear pending
        vdp.ack_vint();
        assert!(!vdp.should_trigger_vint());
    }

    #[test]
    fn test_register_write() {
        let mut vdp = Vdp::new();

        // Write to register 7 (backdrop color)
        vdp.write_register(7, 0x3F);
        assert_eq!(vdp.registers[7], 0x3F);

        // Write to invalid register (should be ignored or handled safely)
        vdp.write_register(25, 0xFF);
        // No panic = good
    }

    #[test]
    fn test_scroll_size_register() {
        let vdp = Vdp::new();

        // Default scroll size should be 32x32
        let (width, height) = vdp.get_scroll_size();
        assert_eq!(width, 32);
        assert_eq!(height, 32);
    }

    #[test]
    fn test_scroll_size_64x64() {
        let mut vdp = Vdp::new();

        // Set register 16: H=64 (bits 0-1 = 01), V=64 (bits 4-5 = 01)
        vdp.set_test_register(16, 0x11);
        let (width, height) = vdp.get_scroll_size();
        assert_eq!(width, 64);
        assert_eq!(height, 64);
    }

    #[test]
    fn test_scroll_size_128x32() {
        let mut vdp = Vdp::new();

        // Set register 16: H=128 (bits 0-1 = 11), V=32 (bits 4-5 = 00)
        vdp.set_test_register(16, 0x03);
        let (width, height) = vdp.get_scroll_size();
        assert_eq!(width, 128);
        assert_eq!(height, 32);
    }

    #[test]
    fn test_playfield_geometry_valid_sizes() {
        let mut vdp = Vdp::new();

        // 32x32 (H=00, V=00): shift 6, col mask 0x1F (32 tiles), row mask 0xFF.
        vdp.set_test_register(16, 0x00);
        assert_eq!(vdp.playfield_geometry(), (6, 0x1F, 0x0FF));

        // 64x64 (H=01, V=01): shift 7, col mask 0x3F (64), row mask 0x1FF.
        vdp.set_test_register(16, 0x11);
        assert_eq!(vdp.playfield_geometry(), (7, 0x3F, 0x1FF));

        // 128x128 (H=11, V=11): shift 8, col mask 0x7F (128), row mask 0x3FF.
        vdp.set_test_register(16, 0x33);
        assert_eq!(vdp.playfield_geometry(), (8, 0x7F, 0x3FF));
    }

    #[test]
    fn test_playfield_geometry_invalid_sizes() {
        let mut vdp = Vdp::new();

        // Invalid horizontal size 0b10 forces shift=0 so the nametable row
        // offset `(tile_row << 0) & 0x1FC0` is always 0: every scanline
        // re-reads row 0 (the VDP scroll-size/window test-ROM quirk). The
        // column width stays 32 tiles (mask 0x1F).
        vdp.set_test_register(16, 0x02);
        let (shift, col_mask, _row_mask) = vdp.playfield_geometry();
        assert_eq!(shift, 0);
        assert_eq!(col_mask, 0x1F);
        // Row offset collapses to 0 for any tile row.
        for tile_row in 0u32..32 {
            assert_eq!((tile_row << shift) & 0x1FC0, 0);
        }

        // Invalid vertical size 0b10 yields the non-power-of-two mask 0x2FF.
        vdp.set_test_register(16, 0x20);
        assert_eq!(vdp.playfield_geometry().2, 0x2FF);
    }

    #[test]
    fn test_display_enable_flag() {
        let mut vdp = Vdp::new();

        // Enable the display (bit 6 of reg 1; reset leaves it clear)
        vdp.set_test_register(1, 0x44); // Display enabled
        assert_eq!(vdp.registers[1] & 0x40, 0x40);

        // Disable display
        vdp.set_test_register(1, 0x04);
        assert_eq!(vdp.registers[1] & 0x40, 0x00);
    }

    #[test]
    fn test_shadow_highlight_mode_register() {
        let mut vdp = Vdp::new();

        // Shadow/Highlight disabled by default
        assert_eq!(vdp.registers[12] & 0x08, 0x00);

        // Enable Shadow/Highlight mode (bit 3 of reg 12)
        vdp.set_test_register(12, 0x89); // H40 + S/H
        assert_eq!(vdp.registers[12] & 0x08, 0x08);
    }

    #[test]
    fn test_interlace_mode_register() {
        let mut vdp = Vdp::new();

        // Interlace disabled by default
        let interlace_mode = (vdp.registers[12] >> 1) & 0x03;
        assert_eq!(interlace_mode, 0);

        // Enable interlace mode 1 (bits 1-2 of reg 12)
        vdp.set_test_register(12, 0x83); // H40 + interlace mode 1
        let interlace_mode = (vdp.registers[12] >> 1) & 0x03;
        assert_eq!(interlace_mode, 1);
    }

    #[test]
    fn test_window_position_disabled() {
        let mut vdp = Vdp::new();

        // Both edges at 0, growing from the top and the left: no window.
        vdp.set_test_register(17, 0x00);
        vdp.set_test_register(18, 0x00);
        assert!(!vdp.window_area().covers(100, 100));
    }

    #[test]
    fn test_window_position_right_side() {
        let mut vdp = Vdp::new();

        // Bit 7 = 1 (right side), bits 4-0 = 10 two-cell columns = pixel 160.
        vdp.set_test_register(17, 0x8A);
        // Line edge at 0 growing from the top: no whole lines.
        vdp.set_test_register(18, 0x00);

        assert!(!vdp.window_area().covers(150, 100));
        assert!(vdp.window_area().covers(170, 100));
    }

    #[test]
    fn test_window_position_left_side() {
        let mut vdp = Vdp::new();

        // Bit 7 = 0 (left side), bits 4-0 = 10 two-cell columns = pixel 160.
        vdp.set_test_register(17, 0x0A);
        // Line edge at 0 growing from the top: no whole lines.
        vdp.set_test_register(18, 0x00);

        assert!(vdp.window_area().covers(100, 100));
        assert!(!vdp.window_area().covers(170, 100));
    }

    #[test]
    fn test_window_position_right_side_from_column_zero() {
        let mut vdp = Vdp::new();

        // Bit 7 = 1 with the column edge at 0: the window starts at column 0
        // and takes every line (Rings of Power: R17=$80, R18=$00).
        vdp.set_test_register(17, 0x80);
        vdp.set_test_register(18, 0x00);
        assert!(vdp.window_area().covers(0, 0));
        assert!(vdp.window_area().covers(100, 100));
        assert!(vdp.window_area().covers(319, 223));
    }

    /// A display-on H40 VDP whose plane A is red everywhere (colour 1 of
    /// palette 0) and whose window is blue everywhere (colour 2), with plane
    /// B transparent and a black backdrop; registers 17 and 18 are left at 0.
    fn red_plane_a_blue_window() -> Vdp {
        let mut vdp = Vdp::new();
        vdp.registers[1] = 0x44; // display on, mode 5, V28
        vdp.registers[12] = 0x81; // H40
        vdp.registers[2] = 0x38; // plane A nametable at 0xE000 (32x32 cells)
        vdp.registers[3] = 0x2C; // window nametable at 0xB000 (64x32 cells)
        vdp.registers[4] = 0x06; // plane B at 0xC000, left empty (transparent)

        // Tile 1 is all colour 1, tile 2 all colour 2.
        for b in 0..32 {
            vdp.vram[32 + b] = 0x11;
            vdp.vram[64 + b] = 0x22;
        }
        for cell in 0..32 * 32 {
            vdp.vram[0xE000 + cell * 2 + 1] = 0x01; // plane A: tile 1
        }
        for cell in 0..64 * 32 {
            vdp.vram[0xB000 + cell * 2 + 1] = 0x02; // window: tile 2
        }
        vdp.cram[1] = 0x000E; // red
        vdp.cram[2] = 0x0E00; // blue
        vdp.rebuild_cram_rgb();
        vdp
    }

    fn framebuffer_rgb(vdp: &Vdp, x: usize, y: usize) -> (u8, u8, u8) {
        let i = (y * 320 + x) * 4;
        (
            vdp.framebuffer[i],
            vdp.framebuffer[i + 1],
            vdp.framebuffer[i + 2],
        )
    }

    const SCREEN_CORNERS: [(usize, usize); 5] =
        [(0, 0), (160, 112), (319, 0), (0, 223), (319, 223)];

    /// The renderer draws the window wherever its area covers: a right-side
    /// window with its edge at column 0 hides plane A on the whole screen,
    /// and with both edges at 0 growing from the top and left plane A shows
    /// everywhere.
    #[test]
    fn a_right_side_window_from_column_zero_is_drawn_over_the_whole_screen() {
        let mut vdp = red_plane_a_blue_window();
        let plane_a_rgb = vdp.cram_rgb[1];
        let window_rgb = vdp.cram_rgb[2];

        vdp.registers[17] = 0x80;
        vdp.registers[18] = 0x00;
        vdp.rerender_current_frame();
        for (x, y) in SCREEN_CORNERS {
            assert_eq!(
                framebuffer_rgb(&vdp, x, y),
                window_rgb,
                "window at ({x},{y})"
            );
        }

        vdp.registers[17] = 0x00;
        vdp.rerender_current_frame();
        for (x, y) in SCREEN_CORNERS {
            assert_eq!(
                framebuffer_rgb(&vdp, x, y),
                plane_a_rgb,
                "plane A at ({x},{y})"
            );
        }
    }

    /// The plane A and window debug layers each show only their own part of
    /// the screen: the window's part is transparent in the plane A layer,
    /// and plane A's part in the window layer.
    #[test]
    fn the_plane_a_and_window_layers_show_only_their_own_part() {
        let mut vdp = red_plane_a_blue_window();
        let plane_a_rgb = vdp.cram_rgb[1];
        let window_rgb = vdp.cram_rgb[2];
        // The window takes the columns from pixel 160 rightwards.
        vdp.registers[17] = 0x8A;
        vdp.registers[18] = 0x00;

        vdp.set_render_layer_mode(RenderLayerMode::PlaneA);
        vdp.rerender_current_frame();
        assert_eq!(framebuffer_rgb(&vdp, 100, 100), plane_a_rgb);
        assert_eq!(framebuffer_rgb(&vdp, 200, 100), TRANSPARENT_RGB);

        vdp.set_render_layer_mode(RenderLayerMode::Window);
        vdp.rerender_current_frame();
        assert_eq!(framebuffer_rgb(&vdp, 100, 100), TRANSPARENT_RGB);
        assert_eq!(framebuffer_rgb(&vdp, 200, 100), window_rgb);
    }

    /// A black pixel a plane draws stays black in its layer view, and a
    /// transparent one is magenta, so the two can be told apart.
    #[test]
    fn a_layer_view_tells_black_from_transparent() {
        let mut vdp = red_plane_a_blue_window();
        vdp.cram[1] = 0x0000; // plane A's colour 1 is black, like the backdrop
        vdp.rebuild_cram_rgb();
        vdp.registers[17] = 0x8A; // window from pixel 160 rightwards
        vdp.set_render_layer_mode(RenderLayerMode::PlaneA);
        vdp.rerender_current_frame();
        assert_eq!(framebuffer_rgb(&vdp, 100, 100), (0, 0, 0));
        assert_eq!(framebuffer_rgb(&vdp, 200, 100), TRANSPARENT_RGB);
    }

    /// No colour the VDP shows can be mistaken for the transparent one.
    #[test]
    fn no_vdp_colour_is_the_transparent_colour() {
        for raw in 0..=0x0FFFu16 {
            assert_ne!(Vdp::convert_genesis_color(raw), TRANSPARENT_RGB);
            assert_ne!(tiles::convert_genesis_color_to_rgb(raw), TRANSPARENT_RGB);
        }
    }

    #[test]
    fn the_window_nametable_is_4kb_aligned_in_h40() {
        let mut vdp = Vdp::new();
        vdp.set_test_register(3, 0x3E); // address bits 15-11 = 0xF800
        vdp.set_test_register(12, 0x00); // H32
        assert_eq!(vdp.get_window_nametable(), 0xF800);
        vdp.set_test_register(12, 0x81); // H40 ignores address bit 11
        assert_eq!(vdp.get_window_nametable(), 0xF000);
    }

    #[test]
    fn test_sprite_overflow_flag() {
        let mut vdp = Vdp::new();

        // Initially no overflow
        assert!(!vdp.sprite_overflow);

        // Set overflow flag manually for testing
        vdp.sprite_overflow = true;

        // Check status register includes overflow bit (bit 6)
        let status = vdp.get_status_register();
        assert_eq!(status & 0x0040, 0x0040);
    }

    #[test]
    fn test_sprite_collision_flag() {
        let mut vdp = Vdp::new();

        // Initially no collision
        assert!(!vdp.sprite_collision);

        // Set collision flag manually for testing
        vdp.sprite_collision = true;

        // Check status register includes collision bit (bit 5)
        let status = vdp.get_status_register();
        assert_eq!(status & 0x0020, 0x0020);
    }

    #[test]
    fn test_status_flags_cleared_on_read() {
        let mut vdp = Vdp::new();

        // Set both sprite flags
        vdp.sprite_overflow = true;
        vdp.sprite_collision = true;

        // Read status register (should clear flags)
        let _ = vdp.read(0xC00004);

        // Flags should be cleared
        assert!(!vdp.sprite_overflow);
        assert!(!vdp.sprite_collision);
    }

    #[test]
    fn test_status_word_is_side_effect_free() {
        let mut vdp = Vdp::new();
        vdp.sprite_overflow = true;
        vdp.sprite_collision = true;

        // `status_word()` returns the same value the port read would
        // compute, but must NOT clear the sprite flags (it is the peek
        // the platform's debugger and lessons rely on).
        let peeked = vdp.status_word();
        assert_eq!(peeked, vdp.get_status_register());
        assert!(vdp.sprite_overflow);
        assert!(vdp.sprite_collision);
    }

    #[test]
    fn test_odd_frame_toggle() {
        let mut vdp = Vdp::new();

        // Initially even frame
        assert!(!vdp.odd_frame);

        // Simulate entering VBLANK (triggers frame toggle)
        vdp.scanline(223); // Last visible line
        assert!(!vdp.odd_frame);

        vdp.scanline(224); // First VBLANK line
        assert!(vdp.odd_frame); // Should toggle to odd

        // Simulate full frame
        for line in 225..262 {
            vdp.scanline(line);
        }
        for line in 0..224 {
            vdp.scanline(line);
        }
        vdp.scanline(224); // Next VBLANK
        assert!(!vdp.odd_frame); // Should toggle back to even
    }

    #[test]
    fn test_vram_auto_increment() {
        let mut vdp = Vdp::new();

        // Set auto-increment to 2
        vdp.set_test_register(15, 2);

        // Set VRAM write mode
        vdp.set_test_access(0x1000, 0x01);

        // Write a byte
        vdp.write_data_port(0x42, true);

        // Address should have incremented by 2
        let (addr, _) = vdp.current_access();
        assert_eq!(addr, 0x1002);
    }

    #[test]
    fn test_cram_auto_increment() {
        let mut vdp = Vdp::new();

        // Set auto-increment to 2
        vdp.set_test_register(15, 2);

        // Set CRAM write mode
        vdp.set_test_access(0x00, 0x03);

        // Write a color
        vdp.write_data_port(0x0EEE, true);

        // Address should have incremented by 2
        let (addr, _) = vdp.current_access();
        assert_eq!(addr, 0x02);

        // Check color was written
        assert_eq!(vdp.cram[0], 0x0EEE);
    }

    #[test]
    fn test_hscroll_modes() {
        let mut vdp = Vdp::new();

        // Mode 0: Full screen scroll (bits 1-0 = 00)
        vdp.set_test_register(11, 0x00);
        assert_eq!(vdp.registers[11] & 0x03, 0x00);

        // Mode 2: Per-cell scroll (bits 1-0 = 10)
        vdp.set_test_register(11, 0x02);
        assert_eq!((vdp.registers[11] >> 2) & 0x03, 0x00); // Bits 3-2 stay 0
                                                           // The h_scroll_mode is bits 1-0
    }

    /// Per-line (mode 3) and per-cell (mode 2) horizontal-scroll table reads
    /// mask the line index to 8 bits. Visible lines never exceed 240 so this is a robustness
    /// guard, but it keeps the table index from running past the 8-bit range.
    #[test]
    fn test_read_hscroll_masks_line_to_8_bits() {
        let mut vdp = Vdp::new();
        let base = 0x1000usize;

        // Entry for the masked line (256 & 0xFF == 0): base + 0.
        vdp.vram[base] = 0x01;
        vdp.vram[base + 1] = 0x23; // 0x0123 & 0x03FF = 0x123

        // Entry an UNMASKED per-line / per-cell index would hit at line 256
        // (both land at base + 1024): proves we don't read it.
        let unmasked = base + 256 * 4;
        vdp.vram[unmasked] = 0x02;
        vdp.vram[unmasked + 1] = 0x22; // 0x0222

        // Per-line (mode 3): line 256 -> entry 0.
        assert_eq!(vdp.read_hscroll(base, 0, 3, 256), 0x123);
        // Per-cell (mode 2): (256 & 0xFF) / 8 == 0 -> entry 0.
        assert_eq!(vdp.read_hscroll(base, 0, 2, 256), 0x123);

        // Sanity: an in-range line is unchanged (per-line line 1 -> base + 4).
        vdp.vram[base + 4] = 0x01;
        vdp.vram[base + 5] = 0x11; // 0x0111
        assert_eq!(vdp.read_hscroll(base, 0, 3, 1), 0x111);
    }

    /// Build a VDP whose Plane B nametable row 0 is solid CRAM index 1 (tile 1)
    /// and row 1 is solid CRAM index 2 (tile 2), with per-2-cell vertical scroll
    /// enabled and Plane B fine horizontal scroll = `shift`. Because every tile
    /// column of a row decodes to the same index, the vertical-scroll value the
    /// renderer picks for a pixel maps directly to `line_plane_b` (idx 1 = row 0,
    /// idx 2 = row 1), so a test can read back *which* vscroll was used.
    fn setup_left_col_vscroll_vdp(shift: u16) -> Vdp {
        let mut vdp = Vdp::new();
        vdp.set_test_register(1, 0x44); // display on + Mode 5
        vdp.set_test_register(11, 0x04); // per-column vscroll; hscroll mode 0
        vdp.set_test_register(16, 0x00); // planes 32x32
        vdp.set_test_register(2, 0x30); // Plane A nametable 0xC000 (transparent tile 0)
        vdp.set_test_register(4, 0x07); // Plane B nametable 0xE000
        vdp.set_test_register(5, 0x58); // SAT at 0xB000 (empty -> no sprites)
        vdp.set_test_register(13, 0x34); // hscroll table base 0xD000

        // Tile 1 = solid CRAM index 1; tile 2 = solid CRAM index 2.
        for b in 0x20..0x40 {
            vdp.vram[b] = 0x11;
        }
        for b in 0x40..0x60 {
            vdp.vram[b] = 0x22;
        }

        // Plane B nametable row 0 -> tile 1, row 1 -> tile 2 (all 32 columns).
        let pb = 0xE000usize;
        for col in 0..32usize {
            let r0 = pb + col * 2;
            vdp.vram[r0] = 0x00;
            vdp.vram[r0 + 1] = 0x01;
            let r1 = pb + 0x40 + col * 2; // row 1 stride = 1 << pf_shift(6)
            vdp.vram[r1] = 0x00;
            vdp.vram[r1 + 1] = 0x02;
        }

        // Plane B horizontal scroll (full-screen mode, plane offset 2) = `shift`.
        let hb = 0xD000usize;
        vdp.vram[hb + 2] = (shift >> 8) as u8;
        vdp.vram[hb + 3] = (shift & 0xFF) as u8;

        vdp
    }

    /// H40 left-most-column vertical-scroll quirk: when
    /// per-column vscroll is on and hscroll is not 16px-aligned, the partial
    /// left-edge column uses column 19's `vsram[A] & vsram[B]`, not column 0.
    #[test]
    fn test_left_column_vscroll_quirk_h40_uses_col19() {
        let shift = 4u16;
        let mut vdp = setup_left_col_vscroll_vdp(shift);
        vdp.set_test_register(12, 0x01); // H40
        vdp.vsram[1] = 0; // Plane B column 0 -> row 0 (idx 1)
        vdp.vsram[38] = 8; // column 19 Plane A
        vdp.vsram[39] = 8; // column 19 Plane B -> AND = 8 -> row 1 (idx 2)

        vdp.composite_scanline(0);

        for x in 0..shift as usize {
            assert_eq!(
                vdp.line_plane_b[x], 2,
                "x={x}: left sliver must use column-19 vscroll (idx 2)"
            );
        }
        for x in shift as usize..16 {
            assert_eq!(
                vdp.line_plane_b[x], 1,
                "x={x}: must use column-0 vscroll (idx 1)"
            );
        }
    }

    /// H32 disables the left-most-column vscroll (value forced to 0), so the
    /// partial left-edge column reads row 0 even when column 0/19 scroll down.
    #[test]
    fn test_left_column_vscroll_quirk_h32_disabled() {
        let shift = 4u16;
        let mut vdp = setup_left_col_vscroll_vdp(shift);
        vdp.set_test_register(12, 0x00); // H32
        vdp.vsram[1] = 8; // column 0 -> row 1 (idx 2), so it differs from override
        vdp.vsram[38] = 8; // column 19 set non-zero to prove it is NOT used in H32
        vdp.vsram[39] = 8;

        vdp.composite_scanline(0);

        for x in 0..shift as usize {
            assert_eq!(
                vdp.line_plane_b[x], 1,
                "x={x}: H32 left-column vscroll is disabled (idx 1)"
            );
        }
        for x in shift as usize..16 {
            assert_eq!(
                vdp.line_plane_b[x], 2,
                "x={x}: must use column-0 vscroll (idx 2)"
            );
        }
    }

    /// A 16px-aligned horizontal scroll (`hscroll & 0x0F == 0`) must NOT trigger
    /// the left-column override, even in H40 with per-column vscroll: the whole
    /// left edge reads column 0.
    #[test]
    fn test_left_column_vscroll_no_override_when_16px_aligned() {
        let mut vdp = setup_left_col_vscroll_vdp(0);
        vdp.set_test_register(12, 0x01); // H40
        vdp.vsram[1] = 0; // column 0 -> idx 1
        vdp.vsram[38] = 8; // column 19 would give idx 2 if (wrongly) used
        vdp.vsram[39] = 8;

        vdp.composite_scanline(0);

        for x in 0..16usize {
            assert_eq!(
                vdp.line_plane_b[x], 1,
                "x={x}: aligned hscroll must not trigger the left-column override"
            );
        }
    }

    /// The debugger's per-layer disable mask suppresses a plane from
    /// composition: with Plane B solid (idx 1) it composites normally,
    /// but once `set_layer_debug_disabled(LAYER_PLANE_B, true)` is set the
    /// pixels read back transparent (-1) and fall through to the backdrop.
    #[test]
    fn test_debug_layer_disable_suppresses_plane_b() {
        let mut vdp = setup_left_col_vscroll_vdp(0);

        vdp.composite_scanline(0);
        assert_eq!(vdp.line_plane_b[0], 1, "Plane B composites when enabled");

        vdp.set_layer_debug_disabled(LAYER_PLANE_B, true);
        vdp.composite_scanline(0);
        assert_eq!(
            vdp.line_plane_b[0], -1,
            "disabled Plane B is suppressed (transparent) in the composite"
        );
        // Plane A is tile 0 (transparent) here, so the final pixel is the
        // backdrop index, not Plane B.
        assert_eq!(vdp.line_index[0], vdp.backdrop_index().min(63) as u8);

        // Re-enabling restores the layer.
        vdp.set_layer_debug_disabled(LAYER_PLANE_B, false);
        vdp.composite_scanline(0);
        assert_eq!(
            vdp.line_plane_b[0], 1,
            "re-enabled Plane B composites again"
        );
    }

    /// Sprites are gated by the same mask. Place one opaque 8x8 sprite at
    /// screen (0,0): with the layer enabled it wins over the low-priority
    /// Plane B pixel; once disabled the BG shows through.
    #[test]
    fn test_debug_layer_disable_suppresses_sprites() {
        let mut vdp = setup_left_col_vscroll_vdp(0);
        // Sprite pattern tile 3 = solid colour index 5.
        for b in 0x60..0x80 {
            vdp.vram[b] = 0x55;
        }
        // SAT base 0xB000 (reg5=0x58, H40 from reset). Sprite 0: 1x1 at screen (0,0),
        // pattern 3, palette 1, link 0 (end of list).
        let sat = 0xB000usize;
        vdp.vram[sat] = 0x00; // word0: Y = 128 -> screen 0
        vdp.vram[sat + 1] = 0x80;
        vdp.vram[sat + 2] = 0x00; // word1: vsize=hsize=0, link=0
        vdp.vram[sat + 3] = 0x00;
        let attr = 0x2000u16 | 3; // palette 1, pattern 3, low priority
        vdp.vram[sat + 4] = (attr >> 8) as u8;
        vdp.vram[sat + 5] = (attr & 0xFF) as u8;
        vdp.vram[sat + 6] = 0x00; // word3: X = 128 -> screen 0
        vdp.vram[sat + 7] = 0x80;
        vdp.invalidate_sprite_cache();

        vdp.composite_scanline(0);
        // Low-pri sprite (pal 1, colour 5 -> idx 21) beats low-pri Plane B.
        assert_eq!(vdp.line_index[0], 21, "sprite shows over BG when enabled");

        vdp.set_layer_debug_disabled(LAYER_SPRITES, true);
        vdp.composite_scanline(0);
        assert_eq!(
            vdp.line_index[0], 1,
            "sprites suppressed; Plane B (idx 1) shows through"
        );
    }

    #[test]
    fn test_shadow_color() {
        let color = (200, 100, 50);
        let shadowed = Vdp::shadow_color(color);
        assert_eq!(shadowed, (100, 50, 25));
    }

    #[test]
    fn test_highlight_color() {
        let color = (100, 100, 100);
        let highlighted = Vdp::highlight_color(color);
        // 100 * 3 / 2 = 150
        assert_eq!(highlighted, (150, 150, 150));
    }

    #[test]
    fn test_highlight_color_clamped() {
        let color = (200, 200, 200);
        let highlighted = Vdp::highlight_color(color);
        // 200 * 3 / 2 = 300, clamped to 255
        assert_eq!(highlighted, (255, 255, 255));
    }

    // ===== DMA Integration Tests =====

    #[test]
    fn test_dma_vram_fill() {
        let mut vdp = Vdp::new();

        // Set auto-increment to 1 and code to VRAM write
        vdp.set_test_register(15, 1);
        vdp.code = 0x01; // VRAM write

        // Execute VRAM fill (length 16 means 17 iterations per MAME behavior)
        vdp.execute_dma_vram_fill(0x1000, 0xAB00, 16);

        // Check that VRAM was filled (high byte of fill value)
        // VRAM fill writes the high byte to alternating byte positions:
        // Even addr -> writes to odd byte (low byte of word)
        // Odd addr -> writes to even byte (high byte of word)
        for i in 0..16 {
            assert_eq!(
                vdp.vram[0x1000 + i],
                0xAB,
                "VRAM at {:04X} should be 0xAB",
                0x1000 + i
            );
        }
    }

    #[test]
    fn test_dma_vram_copy() {
        let mut vdp = Vdp::new();

        // Set auto-increment to 1
        vdp.set_test_register(15, 1);

        // Set source data
        for i in 0..8 {
            vdp.vram[0x2000 + i] = (0x10 + i) as u8;
        }

        // Execute VRAM copy
        vdp.execute_dma_vram_copy(0x2000, 0x3000, 8);

        // Check that data was copied
        for i in 0..8 {
            assert_eq!(
                vdp.vram[0x3000 + i],
                (0x10 + i) as u8,
                "VRAM at {:04X} should be {:02X}",
                0x3000 + i,
                0x10 + i
            );
        }
    }

    #[test]
    fn test_dma_memory_to_vram() {
        let mut vdp = Vdp::new();

        // Set auto-increment to 2 (word access)
        vdp.set_test_register(15, 2);

        // Create fake CPU memory
        let mut cpu_memory = vec![0u8; 0x10000];
        cpu_memory[0x1000] = 0x12;
        cpu_memory[0x1001] = 0x34;
        cpu_memory[0x1002] = 0x56;
        cpu_memory[0x1003] = 0x78;

        // Execute DMA
        vdp.execute_dma_memory_to_vram(0x1000, 0x0000, 2, &cpu_memory);

        // Check VRAM (big-endian)
        assert_eq!(vdp.vram[0x0000], 0x12);
        assert_eq!(vdp.vram[0x0001], 0x34);
        assert_eq!(vdp.vram[0x0002], 0x56);
        assert_eq!(vdp.vram[0x0003], 0x78);
    }

    #[test]
    fn test_dma_vram_fill_with_auto_increment() {
        let mut vdp = Vdp::new();

        // Set auto-increment to 2 (skip bytes) and code to VRAM write
        vdp.set_test_register(15, 2);
        vdp.code = 0x01; // VRAM write

        // Execute VRAM fill (length 4 -> 5 iterations per MAME)
        vdp.execute_dma_vram_fill(0x0000, 0xCC00, 4);

        // With auto-increment 2, addresses are: 0, 2, 4, 6, 8
        // Even addresses: fill_byte written to odd byte position (addr+1)
        // So: byte[1]=CC, byte[3]=CC, byte[5]=CC, byte[7]=CC, byte[9]=CC
        assert_eq!(vdp.vram[0x0001], 0xCC);
        assert_eq!(vdp.vram[0x0000], 0x00); // High byte of word not filled
        assert_eq!(vdp.vram[0x0003], 0xCC);
        assert_eq!(vdp.vram[0x0002], 0x00);
        assert_eq!(vdp.vram[0x0005], 0xCC);
        assert_eq!(vdp.vram[0x0004], 0x00);
        assert_eq!(vdp.vram[0x0007], 0xCC);
    }

    #[test]
    fn test_vram_fill_byte_trigger_uses_even_data_lane() {
        // Contra's HSCROLL upload does `MOVE.B Dn, $C00000` to start a VRAM
        // fill. The even-address byte must land in the high lane so trailing
        // cells replicate that byte (`value >> 8`), not zero.
        let mut vdp = Vdp::new();
        vdp.set_test_register(1, 0x10); // DMA enable
        vdp.set_test_register(15, 4); // auto-inc 4 (plane-B HSCROLL stride)
        vdp.set_test_register(19, 3); // length = 3 trailing cells after the trigger word
        vdp.set_test_register(20, 0);
        vdp.scanline(224); // VBLANK -> fill_initial_chunk commits the whole transfer
        vdp.address = 0xBA02;
        vdp.code = 0x01; // VRAM write
        vdp.dma_fill_pending = true;
        vdp.dma_busy = true;
        vdp.dma_remaining_clocks = u64::MAX;

        // MOVE.B of 0x0F onto the even data port must become word 0x0F00.
        vdp.write(0xC00000, 0x0F);

        assert_eq!(
            u16::from_be_bytes([vdp.vram[0xBA02], vdp.vram[0xBA03]]),
            0x0F00,
            "trigger word"
        );
        // Even-address fill cells write `value>>8` into the low byte of each
        // subsequent word (as in Genesis Plus GX and MAME). A zero-extended
        // byte trigger would make that byte 0, and every trailing cell 0000.
        for line in 1..=3u16 {
            let off = 0xBA02 + (line as usize) * 4;
            let word = u16::from_be_bytes([vdp.vram[off], vdp.vram[off + 1]]);
            assert_eq!(word, 0x000F, "fill cell at line+{line}");
        }
    }

    #[test]
    fn test_dma_state_tracking() {
        let mut vdp = Vdp::new();

        // DMA should not be active initially
        assert!(!vdp.is_dma_active());

        // Start DMA manually
        vdp.dma.start(
            dma::DmaOperation::VramFill {
                dest: 0,
                value: 0,
                length: 100,
            },
            false,
        );

        // DMA should be active
        assert!(vdp.is_dma_active());

        // Status register should have DMA busy bit (bit 1)
        let status = vdp.get_status_register();
        assert_eq!(status & 0x0002, 0x0002);

        // Complete DMA
        vdp.dma.complete();
        assert!(!vdp.is_dma_active());
    }

    #[test]
    fn test_dma_master_clocks_rate_model() {
        let mut vdp = Vdp::new();
        // Active display, H40 (R1 bit6 = display on, R12 bit0 = H40).
        vdp.set_test_register(1, 0x40);
        vdp.set_test_register(12, 0x01);
        vdp.scanline(100); // active region -> not in vblank
        assert!(!vdp.timing.in_vblank);

        // 18 access-slots/line in H40 active. A transfer of `rate` words always
        // takes exactly one scanline (3420 mclks). 68k->VRAM and Copy cost
        // two slots/word -> rate halves to 9.
        use DmaTransfer::{BusToCramOrVsram, BusToVram, Copy, Fill};
        assert_eq!(vdp.dma_master_clocks(BusToCramOrVsram, 18), 3420); // 1 slot/word
        assert_eq!(vdp.dma_master_clocks(Fill, 18), 3420); // 1 slot/byte
        assert_eq!(vdp.dma_master_clocks(BusToVram, 9), 3420); // 2 slots/word
        assert_eq!(vdp.dma_master_clocks(Copy, 9), 3420); // 2 slots/byte
        assert_eq!(vdp.dma_master_clocks(BusToVram, 18), 6840); // two lines

        // Length 0 means a full 0x10000-word transfer (pre-decrementing counter).
        assert_eq!(vdp.dma_master_clocks(BusToVram, 0), 0x10000u64 * 3420 / 9);

        // H32 active: 16 access-slots/line.
        vdp.set_test_register(12, 0x00);
        assert_eq!(vdp.dma_master_clocks(BusToCramOrVsram, 16), 3420);
        assert_eq!(vdp.dma_master_clocks(BusToVram, 8), 3420);
    }

    #[test]
    fn test_dma_master_clocks_blanking() {
        let mut vdp = Vdp::new();
        // Display off forces the blanking rate even outside vblank.
        vdp.set_test_register(1, 0x00);
        vdp.scanline(100);
        assert!(!vdp.timing.in_vblank);

        // H40 blanking: 204 slots/line; a word to VRAM takes two -> 102.
        use DmaTransfer::{BusToCramOrVsram, BusToVram};
        vdp.set_test_register(12, 0x01);
        assert_eq!(vdp.dma_master_clocks(BusToVram, 102), 3420);
        // 68k->CRAM/VSRAM loses refresh slots: 204 -> 198.
        assert_eq!(vdp.dma_master_clocks(BusToCramOrVsram, 198), 3420);

        // H32 blanking: 166 slots; CRAM/VSRAM loses refresh slots: 166 -> 161.
        vdp.set_test_register(12, 0x00);
        assert_eq!(vdp.dma_master_clocks(BusToCramOrVsram, 161), 3420);
        assert_eq!(vdp.dma_master_clocks(BusToVram, 83), 83 * 3420 / 83); // 166 / 2 = 83
    }

    #[test]
    fn test_dma_busy_master_clocks_beam_aware() {
        let mut vdp = Vdp::new();
        vdp.set_test_register(12, 0x01); // H40
        let active = vdp.timing.visible_scanlines() as u64; // 224 (NTSC)

        // Display OFF blanks every line, so the beam-aware integral has no
        // active-rate lines to cross and collapses to the single blanking-rate
        // duration for every transfer type.
        vdp.set_test_register(1, 0x00);
        vdp.scanline(100);
        for &(transfer, len) in &[
            (DmaTransfer::Fill, 204 * 3),
            (DmaTransfer::Copy, 102 * 5),
            (DmaTransfer::BusToVram, 102 * 4),
            (DmaTransfer::BusToCramOrVsram, 198 * 2),
        ] {
            assert_eq!(
                vdp.dma_busy_master_clocks(transfer, len),
                vdp.dma_master_clocks(transfer, len),
                "display-off DMA should equal the single blanking rate ({transfer:?})"
            );
        }

        // Display ON, large fill kicked off mid-active-display (line 100): the
        // chip drains the remaining active lines at the slow active rate (18) then
        // empties the rest during the ~10x faster VBLANK. The busy window must
        // be far shorter than locking the active rate for the whole transfer,
        // and never shorter than draining it entirely at the blanking rate.
        vdp.set_test_register(1, 0x40);
        vdp.scanline(100);
        assert!(!vdp.timing.in_vblank);
        let len: u32 = 0x1fff;
        let busy = vdp.dma_busy_master_clocks(DmaTransfer::Fill, len);
        let locked_active = vdp.dma_master_clocks(DmaTransfer::Fill, len);
        let pure_blanking = (len as u64) * 3420 / 204;
        assert!(
            busy < locked_active,
            "beam-aware ({busy}) must beat locked active rate ({locked_active})"
        );
        assert!(
            busy >= pure_blanking,
            "beam-aware ({busy}) cannot beat a pure blanking drain ({pure_blanking})"
        );

        // Exact integral: active lines [100, 224) at 18 units/line, then the
        // remainder during VBLANK at 204 units/line (fits before line wrap).
        let active_units = (active - 100) * 18;
        let rem = len as u64 - active_units;
        let expected = (active - 100) * 3420 + (rem / 204) * 3420 + (rem % 204) * 3420 / 204;
        assert_eq!(busy, expected, "beam-aware integral mismatch");
    }

    #[test]
    fn test_hv_v_counter_ntsc_jump() {
        let mut vdp = Vdp::new();
        // NTSC 224-line: V counter counts 0x00..=0xEA then jumps to 0xE5..=0xFF.
        vdp.timing.scanline = 0x00;
        assert_eq!(vdp.hv_v_counter(), 0x00);
        vdp.timing.scanline = 0xEA;
        assert_eq!(vdp.hv_v_counter(), 0xEA);
        vdp.timing.scanline = 0xEB; // 235 - 262 = -27 -> 0xE5
        assert_eq!(vdp.hv_v_counter(), 0xE5);
        vdp.timing.scanline = 261; // last NTSC line -> 0xFF
        assert_eq!(vdp.hv_v_counter(), 0xFF);
    }

    #[test]
    fn test_hv_h_counter_from_beam() {
        let mut vdp = Vdp::new();
        vdp.set_test_register(12, 0x01); // H40
                                         // Beam at line start -> H counter 0.
        vdp.set_hblank_approx(0, 3420);
        assert_eq!(vdp.hv_h_counter(), 0x00);
        // Mid-line stays in the low (pre-gap) segment: 50% -> p=105.
        vdp.set_hblank_approx(1710, 3420);
        assert_eq!(vdp.hv_h_counter(), 105);
        // Beam at line end -> top of the H40 ramp (0xFF, not a wrapped 0x00).
        vdp.set_hblank_approx(3420, 3420);
        assert_eq!(vdp.hv_h_counter(), 0xFF);
    }

    // ===== Interrupt Tests =====

    /// Drive one full NTSC frame of `scanline()` calls (0..262), like the
    /// per-line loop every frame tier runs. The VDP boots inside VBLANK at line
    /// 224, so the first active->VBLANK transition only occurs partway through
    /// this first frame, exactly the transition the hardware suppresses.
    fn advance_one_frame(vdp: &mut Vdp) {
        for line in 0..262 {
            vdp.scanline(line);
        }
    }

    #[test]
    fn test_vint_trigger_at_vblank_start() {
        let mut vdp = Vdp::new();

        // Enable VINT (register 1 bit 5)
        vdp.set_test_register(1, 0x24);

        // The first frame after a VDP reset raises NO VINT (the VDP comes out
        // of reset already past VBLANK). Consume it, then assert it stayed
        // quiet.
        advance_one_frame(&mut vdp);
        assert!(
            !vdp.should_trigger_vint(),
            "first post-reset frame must not raise VINT"
        );

        // Before VBLANK - VINT should not trigger
        vdp.scanline(223);
        assert!(!vdp.should_trigger_vint());

        // Enter VBLANK on the next frame - VINT now triggers normally
        vdp.scanline(224);
        assert!(vdp.should_trigger_vint());

        // After acknowledging, should not trigger again until next frame
        vdp.ack_vint();
        assert!(!vdp.should_trigger_vint());
    }

    /// The VDP boots already inside VBLANK, so the very first
    /// frame after a reset raises neither the F flag nor the level-6 request.
    /// Without this, SG1 would deliver every game's first VINT one frame early
    /// and run a logic frame ahead of Genesis Plus GX from boot. The
    /// suppression must re-arm on every `reset()` (a soft reset restarts the
    /// same cadence).
    #[test]
    fn test_first_frame_vint_suppressed_after_reset() {
        let mut vdp = Vdp::new();
        vdp.set_test_register(1, 0x24); // enable VINT + display

        // First frame after construction: its VBLANK is suppressed, no F flag.
        advance_one_frame(&mut vdp);
        assert!(!vdp.should_trigger_vint());
        assert!(!vdp.timing.vblank_pending);

        // Second frame: VBLANK now raises the VINT + F flag normally.
        advance_one_frame(&mut vdp);
        assert!(vdp.should_trigger_vint());
        assert!(vdp.timing.vblank_pending);

        // A soft reset re-arms the one-frame suppression.
        vdp.reset();
        vdp.set_test_register(1, 0x24);
        advance_one_frame(&mut vdp);
        assert!(
            !vdp.should_trigger_vint(),
            "reset must re-arm first-frame VINT suppression"
        );
    }

    #[test]
    fn test_vint_pending_cleared_on_frame_start() {
        let mut vdp = Vdp::new();

        // Enable VINT
        vdp.set_test_register(1, 0x24);

        // Consume the first post-reset frame, whose VBLANK raises no VINT
        // (see above); every frame after it behaves normally.
        advance_one_frame(&mut vdp);
        assert!(!vdp.should_trigger_vint());

        // The frame above left the VDP at scanline 261 (VBLANK); exit VBLANK by
        // running the VBLANK scanlines again and wrapping to scanline 0
        for line in 225..262 {
            vdp.scanline(line);
        }
        vdp.scanline(0); // Exit VBLANK (new frame)
        assert!(!vdp.timing.in_vblank);

        // Run through visible area
        for line in 1..224 {
            vdp.scanline(line);
        }

        // Enter VBLANK and trigger VINT
        vdp.scanline(224);
        assert!(vdp.should_trigger_vint());

        // Run through VBLANK
        for line in 225..262 {
            vdp.scanline(line);
        }

        // Start new frame (scanline 0) - F flag persists until IACK
        vdp.scanline(0);
        assert!(vdp.timing.vblank_pending); // F flag NOT cleared by scanline transition

        // Reading status register does NOT clear F flag (only IACK does)
        let _status = vdp.read_word_port(0xC00004);
        assert!(vdp.timing.vblank_pending); // Still set - only IACK clears it

        // Simulate IACK (interrupt acknowledge) clearing the F flag
        vdp.ack_vint_iack();
        assert!(!vdp.timing.vblank_pending); // NOW cleared by IACK

        // Next VBLANK should trigger VINT again
        for line in 1..224 {
            vdp.scanline(line);
        }
        vdp.scanline(224);
        assert!(vdp.should_trigger_vint());
    }

    #[test]
    fn test_vint_disabled_when_register_clear() {
        let mut vdp = Vdp::new();

        // VINT disabled (register 1 bit 5 = 0)
        vdp.set_test_register(1, 0x04);

        // Enter VBLANK
        vdp.scanline(224);

        // VINT should not trigger (disabled)
        assert!(!vdp.should_trigger_vint());
    }

    #[test]
    fn test_hint_counter_reload() {
        let mut vdp = Vdp::new();

        // Set HINT counter reload value to 3 (interrupt every 4 scanlines)
        vdp.set_test_register(10, 3);

        // Writing R10 updates ONLY the reload value; the live counter is
        // reloaded from it during vblank / on underflow,
        // NOT reset on the register write itself.
        assert_eq!(vdp.timing.h_counter_reload, 3);

        // A vblank scanline reloads the live counter from the reload value.
        vdp.scanline(240);
        assert_eq!(vdp.timing.h_counter_current, 3);
    }

    #[test]
    fn test_hint_trigger_at_counter_zero() {
        let mut vdp = Vdp::new();

        // Enable HINT (register 0 bit 4)
        vdp.set_test_register(0, 0x14);

        // Set HINT counter to 2 (triggers after 3 scanlines)
        vdp.set_test_register(10, 2);

        // Prime the live counter from the reload value via a vblank line:
        // writing R10 does not reset the live counter directly.
        vdp.scanline(240);
        assert_eq!(vdp.timing.h_counter_current, 2);

        // Scanline 1: counter 2 -> 1
        vdp.scanline(1);
        assert!(!vdp.should_trigger_hint());

        // Scanline 2: counter 1 -> 0
        vdp.scanline(2);
        assert!(!vdp.should_trigger_hint()); // Not yet

        // Scanline 3: counter 0 -> trigger and reload
        vdp.scanline(3);
        assert!(vdp.should_trigger_hint());

        // Acknowledge HINT
        vdp.ack_hint();
        assert!(!vdp.should_trigger_hint());
    }

    #[test]
    fn test_hint_disabled_when_register_clear() {
        let mut vdp = Vdp::new();

        // HINT disabled (register 0 bit 4 = 0)
        vdp.set_test_register(0, 0x04);

        // Set HINT counter to trigger immediately
        vdp.set_test_register(10, 0);
        vdp.timing.h_counter_current = 0;

        // Process scanlines
        vdp.scanline(1);

        // HINT should not trigger (disabled)
        assert!(!vdp.should_trigger_hint());
    }

    /// Verify that the renderer correctly decodes all tile attribute bits
    /// from a nametable word stored in VRAM, using the exact same code path
    /// as render_scanline.
    #[test]
    fn test_nametable_word_decode_in_render_path() {
        let test_cases: &[(u16, bool, u16, u8, bool, bool)] = &[
            // (word, expected_pri, expected_pat, expected_pal, expected_fh, expected_fv)
            (0x8001, true, 0x001, 0, false, false), // priority only
            (0xE001, true, 0x001, 3, false, false), // priority + palette 3
            (0x6001, false, 0x001, 3, false, false), // no priority, palette 3
            (0x0001, false, 0x001, 0, false, false), // no attributes
            (0x825C, true, 0x25C, 0, false, false), // the Sonic 1 ribbon case
            (0x025C, false, 0x25C, 0, false, false), // the same tile, priority clear
            (0x9800, true, 0x000, 0, true, true),   // priority + both flips
            (0x0800, false, 0x000, 0, true, false), // h-flip only
            (0x1000, false, 0x000, 0, false, true), // v-flip only
            (0xFFFF, true, 0x7FF, 3, true, true),   // all bits set
        ];

        for &(word, exp_pri, exp_pat, exp_pal, exp_fh, exp_fv) in test_cases {
            // Write word to VRAM at a known address, big-endian
            let addr: usize = 0x1000;
            let mut vram = vec![0u8; 0x10000];
            vram[addr] = (word >> 8) as u8;
            vram[addr + 1] = (word & 0xFF) as u8;

            // Read back using the exact same decode as render_scanline
            let read_word = ((vram[addr] as u16) << 8) | (vram[addr + 1] as u16);
            let priority = (read_word & 0x8000) != 0;
            let pattern = read_word & 0x07FF;
            let palette = ((read_word >> 13) & 0x03) as u8;
            let flip_h = (read_word & 0x0800) != 0;
            let flip_v = (read_word & 0x1000) != 0;

            assert_eq!(
                read_word, word,
                "Word readback mismatch for 0x{:04X}: got 0x{:04X}",
                word, read_word
            );
            assert_eq!(
                priority, exp_pri,
                "Priority mismatch for word 0x{:04X}: got {} expected {}",
                word, priority, exp_pri
            );
            assert_eq!(
                pattern, exp_pat,
                "Pattern mismatch for word 0x{:04X}: got 0x{:03X} expected 0x{:03X}",
                word, pattern, exp_pat
            );
            assert_eq!(
                palette, exp_pal,
                "Palette mismatch for word 0x{:04X}: got {} expected {}",
                word, palette, exp_pal
            );
            assert_eq!(
                flip_h, exp_fh,
                "FlipH mismatch for word 0x{:04X}: got {} expected {}",
                word, flip_h, exp_fh
            );
            assert_eq!(
                flip_v, exp_fv,
                "FlipV mismatch for word 0x{:04X}: got {} expected {}",
                word, flip_v, exp_fv
            );
        }
    }

    /// Verify that writing a word through the VDP data port and reading it
    /// back through the renderer produces consistent tile attributes.
    #[test]
    fn test_vram_write_read_roundtrip_priority() {
        let mut vdp = Vdp::new();

        // Set up VRAM write mode: CD0=1 (VRAM write), address=0x1000
        vdp.code = 0x01;
        vdp.address = 0x1000;
        vdp.registers[15] = 2; // auto-increment

        let test_words: &[u16] = &[0x8001, 0x825C, 0xE001, 0x0001, 0x025C, 0xFFFF];

        for &word in test_words {
            vdp.address = 0x1000;
            vdp.write_data_port(word, true);

            // Read back from VRAM using renderer's decode path
            let hi = vdp.vram[0x1000] as u16;
            let lo = vdp.vram[0x1001] as u16;
            let readback = (hi << 8) | lo;

            assert_eq!(
                readback, word,
                "Write/read roundtrip failed for 0x{:04X}: got 0x{:04X}",
                word, readback
            );

            let pri = (readback & 0x8000) != 0;
            let expected_pri = (word & 0x8000) != 0;
            assert_eq!(
                pri, expected_pri,
                "Priority roundtrip failed for 0x{:04X}: pri={} expected={}",
                word, pri, expected_pri
            );
        }
    }

    /// Render-path shadow/highlight test. With S/H enabled a low-priority
    /// plane-A pixel must be shadowed and a high-priority one must stay normal.
    /// Drives the full composite->index-buffer then remap->RGB pipeline (the
    /// remap pass owns S/H), so S/H is checked without a game that uses it.
    #[test]
    fn test_shadow_highlight_render_path() {
        for (priority_word, high_pri) in [(0x0000u16, false), (0x8000u16, true)] {
            let mut vdp = Vdp::new();
            // Display on; H40 (reg12 bit0) + Shadow/Highlight (reg12 bit3).
            vdp.set_test_register(1, 0x44);
            vdp.set_test_register(12, 0x09);
            // Plane A nametable at 0xC000 (reg2 bits 5-3 = 0b110).
            vdp.set_test_register(2, 0x30);
            // Tile 1 pattern: pixel (0,0) = colour index 1 (high nibble of byte 0).
            vdp.vram[0x20] = 0x10;
            // Plane A cell (0,0) -> tile 1, palette 0, priority per case.
            let word = priority_word | 0x0001;
            vdp.vram[0xC000] = (word >> 8) as u8;
            vdp.vram[0xC001] = (word & 0xFF) as u8;
            // Palette 0 colour 1 = an asymmetric colour so shadow is visible.
            vdp.cram[1] = 0x008E;
            vdp.rebuild_cram_rgb();
            let base = vdp.cram_rgb[1];

            vdp.render_scanline(0);

            let got = (vdp.framebuffer[0], vdp.framebuffer[1], vdp.framebuffer[2]);
            let expected = if high_pri {
                base
            } else {
                Vdp::shadow_color(base)
            };
            assert_eq!(
                got, expected,
                "S/H render mismatch (high_pri={high_pri}): got {got:?} expected {expected:?} base {base:?}"
            );
            if !high_pri {
                assert_ne!(got, base, "shadow must darken the low-priority pixel");
            }
        }
    }

    /// Mid-line CRAM beam-X segment re-tint: an
    /// in-window palette write recolours only `[beam_x, width)` so the already
    /// scanned-out left keeps its colour; a write past the 860-mclk window does
    /// not touch the line at all. Locks in the segment boundary and the window
    /// so a future change can't silently widen, drop, or whole-line it.
    #[test]
    fn test_cram_midline_retint_window() {
        let mut vdp = Vdp::new();
        // Display on, H40, S/H off; plane A base 0xC000.
        vdp.set_test_register(1, 0x44);
        vdp.set_test_register(12, 0x01);
        vdp.set_test_register(2, 0x30);
        // Tile 1 = colour index 1 for every pixel; fill plane A row 0 with it so
        // the whole visible line resolves to CRAM index 1.
        for i in 0..32 {
            vdp.vram[0x20 + i] = 0x11;
        }
        for col in 0..64usize {
            let a = 0xC000 + col * 2;
            vdp.vram[a] = 0x00;
            vdp.vram[a + 1] = 0x01;
        }
        // Palette 0 colour 1 starts red.
        vdp.cram[1] = 0x000E;
        vdp.rebuild_cram_rgb();
        vdp.timing.scanline = 0;
        vdp.render_scanline(0);
        let red = Vdp::convert_genesis_color(0x000E);
        let width = vdp.display_width() as usize;
        assert_eq!(
            (vdp.framebuffer[0], vdp.framebuffer[1], vdp.framebuffer[2]),
            red
        );

        // Beam mid-line but inside the 860-mclk window: re-tint only [beam_x,w).
        vdp.beam_cycles_per_line = 488;
        vdp.beam_cycles_into_line = 100;
        let beam_x = vdp.beam_active_x();
        assert!(
            beam_x > 0 && beam_x < width,
            "beam_x={beam_x} width={width}"
        );
        vdp.code = 0x03; // CRAM write
        vdp.address = 0x02; // index 1
        vdp.write_data_port(0x00E0, true); // green
        let green = Vdp::convert_genesis_color(0x00E0);
        let lx = (beam_x - 1) * 4;
        let rx = (beam_x + 4) * 4;
        assert_eq!(
            (
                vdp.framebuffer[lx],
                vdp.framebuffer[lx + 1],
                vdp.framebuffer[lx + 2]
            ),
            red,
            "left of the beam must keep the pre-write colour"
        );
        assert_eq!(
            (
                vdp.framebuffer[rx],
                vdp.framebuffer[rx + 1],
                vdp.framebuffer[rx + 2]
            ),
            green,
            "at/right of the beam must show the new colour"
        );

        // Beam past the window: CRAM updates but the line is left untouched.
        vdp.beam_cycles_into_line = 480;
        vdp.code = 0x03;
        vdp.address = 0x02;
        vdp.write_data_port(0x0E00, true); // blue
        assert_eq!(
            (
                vdp.framebuffer[rx],
                vdp.framebuffer[rx + 1],
                vdp.framebuffer[rx + 2]
            ),
            green,
            "late-beam CRAM write must NOT re-tint the current line"
        );
        assert_eq!(vdp.cram_rgb[1], Vdp::convert_genesis_color(0x0E00));
    }

    // -- The FIFO's access slots -----------------------------------------

    /// FNV-1a over a table's entries, so the whole table is pinned by one
    /// number.
    fn table_hash(table: &[u64]) -> u64 {
        let bytes: Vec<u8> = table.iter().flat_map(|v| v.to_le_bytes()).collect();
        retro_core::fnv1a64(&bytes)
    }

    #[test]
    fn the_fifo_slot_tables_are_the_recorded_ones() {
        // Pinned to the slot times the recorded runs were made with: any
        // change to a slot's time changes these.
        assert_eq!(table_hash(&FIFO_TIMING_H32), 0xd7f0_55cb_1722_762c);
        assert_eq!(table_hash(&FIFO_TIMING_H40), 0x653e_3cb7_7c81_d5a1);
    }

    #[test]
    fn a_line_has_sixteen_slots_in_h32_and_eighteen_in_h40() {
        for (table, slots) in [(&FIFO_TIMING_H32[..], 16), (&FIFO_TIMING_H40[..], 18)] {
            assert_eq!(table.len(), slots + FIFO_SLOTS_OF_NEXT_LINE);
            assert!(table.windows(2).all(|w| w[0] < w[1]), "ascending");
            assert!(table[..slots].iter().all(|&t| t < MASTER_CLOCKS_PER_LINE));
            for k in 0..FIFO_SLOTS_OF_NEXT_LINE {
                assert_eq!(
                    table[slots + k],
                    MASTER_CLOCKS_PER_LINE + table[k],
                    "slot {k} of the next line"
                );
            }
        }
    }

    #[test]
    fn within_the_picture_slots_come_three_in_a_row_every_sixteen_pixels() {
        // Sixteen pixels are 160 master clocks in H32 and 128 in H40; the
        // fourth position of each group is the chip's own refresh.
        let gaps = |table: &[u64], from: usize, count: usize| -> Vec<u64> {
            table[from..from + count]
                .windows(2)
                .map(|w| w[1] - w[0])
                .collect()
        };
        assert_eq!(
            gaps(&FIFO_TIMING_H32, 2, 12),
            [160, 160, 320, 160, 160, 320, 160, 160, 320, 160, 160]
        );
        assert_eq!(
            gaps(&FIFO_TIMING_H40, 1, 15),
            [128, 128, 256, 128, 128, 256, 128, 128, 256, 128, 128, 256, 128, 128]
        );
    }

    #[test]
    fn the_h_counter_takes_one_line_to_come_round() {
        for h40 in [false, true] {
            let origin = if h40 {
                LINE_ORIGIN_H40
            } else {
                LINE_ORIGIN_H32
            };
            let (mut at, mut clocks, mut values) = (origin, 0u64, 0u32);
            loop {
                clocks += dot_master_clocks(h40, at);
                at = next_hcounter(h40, at);
                values += 1;
                if at == origin {
                    break;
                }
            }
            assert_eq!(clocks, MASTER_CLOCKS_PER_LINE, "h40={h40}");
            assert_eq!(
                values,
                if h40 { 420 } else { 342 },
                "pixels a line, h40={h40}"
            );
        }
        // Horizontal sync in H40: 32 pixels in 314 master clocks.
        let sync: u64 = (H40_SYNC.0..=H40_SYNC.1)
            .map(|h| dot_master_clocks(true, h))
            .sum();
        assert_eq!(sync, 314);
    }

    #[test]
    fn every_slot_is_at_its_documented_position_but_the_one_in_h40_sync() {
        for &h in &ACCESS_SLOTS_H32 {
            assert!(
                FIFO_TIMING_H32[..16].contains(&master_clocks_to(false, h)),
                "{h:#05x}"
            );
        }
        for &h in &ACCESS_SLOTS_H40 {
            let documented = master_clocks_to(true, h);
            let kept = if h == H40_SYNC_SLOT {
                documented + H40_SYNC_SLOT_LATENESS
            } else {
                documented
            };
            assert!(FIFO_TIMING_H40[..18].contains(&kept), "{h:#05x}");
        }
        assert_eq!(master_clocks_to(true, H40_SYNC_SLOT), 346);
        assert_eq!(FIFO_TIMING_H40[0], 352);
    }

    /// The sixteen rates of the DMA model: what one scanline carries of each
    /// kind of transfer, by width and by whether the line is blanked.
    #[test]
    fn dma_line_rates_by_transfer_width_and_blanking() {
        use DmaTransfer::{BusToCramOrVsram, BusToVram, Copy, Fill};
        let mut vdp = Vdp::new();
        // Each row: H32 active, H40 active, H32 blanked, H40 blanked.
        let expected = [
            (BusToCramOrVsram, [16, 18, 161, 198]),
            (BusToVram, [8, 9, 83, 102]),
            (Fill, [16, 18, 166, 204]),
            (Copy, [8, 9, 83, 102]),
        ];
        for (transfer, rates) in expected {
            for (case, rate) in rates.into_iter().enumerate() {
                vdp.registers[12] = (case & 1) as u8;
                assert_eq!(
                    vdp.dma_line_rate(transfer, case >= 2),
                    rate,
                    "{transfer:?}, case {case}"
                );
            }
        }
    }

    /// Register 16's two size fields: 32, 64 or 128 tiles across (the
    /// invalid code repeats row 0 of a 32-tile plane), and 256 to 1024
    /// pixels down, the invalid code giving a mask that is not a power of
    /// two less one. The other bits of the register change nothing.
    #[test]
    fn playfield_geometry_of_every_size_code() {
        let mut vdp = Vdp::new();
        let across = [(6, 0x1F), (7, 0x3F), (0, 0x1F), (8, 0x7F)];
        let down = [0x0FF, 0x1FF, 0x2FF, 0x3FF];
        for value in 0..=0xFFu8 {
            vdp.registers[16] = value;
            let (row_shift, col_mask) = across[usize::from(value & 0x03)];
            let row_mask = down[usize::from((value >> 4) & 0x03)];
            assert_eq!(
                vdp.playfield_geometry(),
                (row_shift, col_mask, row_mask),
                "register 16 = {value:#04x}"
            );
        }
    }
}
