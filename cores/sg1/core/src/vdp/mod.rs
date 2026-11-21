//! Video Display Processor emulation.
//!
//! The VDP is responsible for graphics rendering on the Genesis.

mod colour;
pub mod debug;
pub mod dma;
mod picture;
pub mod planes;
mod ports;
pub mod recording;
mod redraw;
mod render;
pub mod sources;
pub mod sprites;
mod state;
pub mod tiles;
pub mod timing;
pub mod window;
pub mod writes;

use picture::{FRAMEBUFFER_LINES, FRAMEBUFFER_WIDTH};
pub use recording::LineKind;
pub use sources::Shade;
pub use writes::{VideoEvent, VideoEventWatch, WriteTarget};

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
    line_high_pri: Vec<bool>, // pixel resolved high-priority
    line_shade: Vec<Shade>,   // what shadow/highlight do to the pixel (`sources::shade`)
    line_shaded: bool,        // the line's composite resolved `line_shade` (mode on)
    line_plane_a: Vec<i16>,   // plane A CRAM index, -1 transparent or window (layer modes)
    line_window: Vec<i16>,    // window CRAM index, -1 transparent or plane A (layer modes)
    line_plane_b: Vec<i16>,   // plane B CRAM index, -1 transparent (layer modes)

    // CRAM RGB lookup table: precomputed (r,g,b) for all 64 CRAM entries.
    // Updated whenever CRAM is written. Eliminates ~143K convert_genesis_color() calls/frame.
    cram_rgb: [(u8, u8, u8); 64],
    // The same 64 colours shadowed and highlighted, set with `cram_rgb`
    // (`set_palette_colour`) so the remap's every shade is a lookup.
    cram_rgb_shadowed: [(u8, u8, u8); 64],
    cram_rgb_highlighted: [(u8, u8, u8); 64],

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
    debug_layer_disable: retro_core::video::LayerMask<LAYER_COUNT>,

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

    /// The number of the frame being drawn, which the system sets as each
    /// frame starts and every record of the frame carries. Not part of the
    /// savestate.
    frame_being_drawn: u64,

    /// What is recorded of each frame for a capture ([`recording`]):
    /// `None` until a host arms something, so a frame recording nothing
    /// pays one check of it at each place that records. Never part of the
    /// savestate.
    recording: Option<Box<recording::Recording>>,

    /// The bus's word for where the value of the store being made through a
    /// port came from, the address of the last word the 68000 read, set
    /// before each port write ([`Vdp::note_port_origin`]) and read only
    /// while the writes are recorded. Not part of the savestate.
    port_origin: u32,
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

/// What drawing a line leaves in the VDP besides its pixels: the sprite
/// status flags (part of a savestate), the sprite list parsed from the
/// sprite table, and the line's own buffers, from which a palette write
/// later in the line re-tints it. A picture drawn again on demand (a
/// repaint, a single layer) keeps these and puts them back after, so the
/// next line the game draws, and the next savestate, find them as the
/// frame left them.
struct DrawState {
    sprite_overflow: bool,
    sprite_collision: bool,
    sprite_mask_overflow: bool,
    sprite_cache: sprites::SpriteCache,
    sprite_cache_valid: bool,
    sprite_pixel_buffer: Vec<bool>,
    sprite_line_color: Vec<u8>,
    sprite_line_palette: Vec<u8>,
    sprite_line_priority: Vec<bool>,
    sprites_on_line_buf: Vec<u16>,
    line_index: Vec<u8>,
    line_sh_op: Vec<u8>,
    line_high_pri: Vec<bool>,
    line_shade: Vec<Shade>,
    line_shaded: bool,
    line_plane_a: Vec<i16>,
    line_window: Vec<i16>,
    line_plane_b: Vec<i16>,
}

impl DrawState {
    /// A copy of what drawing changes in `vdp`.
    fn keep(vdp: &Vdp) -> Self {
        Self {
            sprite_overflow: vdp.sprite_overflow,
            sprite_collision: vdp.sprite_collision,
            sprite_mask_overflow: vdp.sprite_mask_overflow,
            sprite_cache: vdp.sprite_cache.clone(),
            sprite_cache_valid: vdp.sprite_cache_valid,
            sprite_pixel_buffer: vdp.sprite_pixel_buffer.clone(),
            sprite_line_color: vdp.sprite_line_color.clone(),
            sprite_line_palette: vdp.sprite_line_palette.clone(),
            sprite_line_priority: vdp.sprite_line_priority.clone(),
            sprites_on_line_buf: vdp.sprites_on_line_buf.clone(),
            line_index: vdp.line_index.clone(),
            line_sh_op: vdp.line_sh_op.clone(),
            line_high_pri: vdp.line_high_pri.clone(),
            line_shade: vdp.line_shade.clone(),
            line_shaded: vdp.line_shaded,
            line_plane_a: vdp.line_plane_a.clone(),
            line_window: vdp.line_window.clone(),
            line_plane_b: vdp.line_plane_b.clone(),
        }
    }

    /// Put the copy back into `vdp`.
    fn restore(self, vdp: &mut Vdp) {
        vdp.sprite_overflow = self.sprite_overflow;
        vdp.sprite_collision = self.sprite_collision;
        vdp.sprite_mask_overflow = self.sprite_mask_overflow;
        vdp.sprite_cache = self.sprite_cache;
        vdp.sprite_cache_valid = self.sprite_cache_valid;
        vdp.sprite_pixel_buffer = self.sprite_pixel_buffer;
        vdp.sprite_line_color = self.sprite_line_color;
        vdp.sprite_line_palette = self.sprite_line_palette;
        vdp.sprite_line_priority = self.sprite_line_priority;
        vdp.sprites_on_line_buf = self.sprites_on_line_buf;
        vdp.line_index = self.line_index;
        vdp.line_sh_op = self.line_sh_op;
        vdp.line_high_pri = self.line_high_pri;
        vdp.line_shade = self.line_shade;
        vdp.line_shaded = self.line_shaded;
        vdp.line_plane_a = self.line_plane_a;
        vdp.line_window = self.line_window;
        vdp.line_plane_b = self.line_plane_b;
    }
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
pub const LAYER_COUNT: usize = 4;
/// Debug-layer index of the scrolling foreground plane.
pub const LAYER_PLANE_A: usize = 0;
/// Debug-layer index of the scrolling background plane.
pub const LAYER_PLANE_B: usize = 1;
/// Debug-layer index of the fixed window plane (replaces Plane A in its
/// region on real hardware).
pub const LAYER_WINDOW: usize = 2;
/// Debug-layer index of the sprite (OBJ) plane.
pub const LAYER_SPRITES: usize = 3;

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
            framebuffer: vec![0; (FRAMEBUFFER_WIDTH * FRAMEBUFFER_LINES * 4) as usize], // RGBA
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
            line_shade: vec![Shade::AsIs; 320],
            line_shaded: false,
            line_plane_a: vec![-1i16; 320],
            line_window: vec![-1i16; 320],
            line_plane_b: vec![-1i16; 320],
            // CRAM starts all 0, black: every entry its colours.
            cram_rgb: [Self::shaded_colour(0, Shade::AsIs); 64],
            cram_rgb_shadowed: [Self::shaded_colour(0, Shade::Shadowed); 64],
            cram_rgb_highlighted: [Self::shaded_colour(0, Shade::Highlighted); 64],
            read_buffer: 0,
            watch_vram: None,
            debug_pixel: None,
            render_layer_mode: RenderLayerMode::Normal,
            debug_layer_disable: Default::default(),
            frame_being_drawn: 0,
            recording: None,
            port_origin: 0,
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
        // What was recorded described the machine before the reset; what is
        // armed stays armed.
        self.clear_recorded_frames();

        // Render backdrop color
        self.render_backdrop();
    }

    /// Get the framebuffer for display.
    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer
    }

    /// Enable VRAM write tracing for a given address range (inclusive).
    pub fn set_watch_vram(&mut self, start: u16, end: u16) {
        self.watch_vram = Some((start, end));
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
        self.debug_layer_disable.set_hidden(layer, disabled);
    }

    /// Whether debug layer `layer` is currently force-disabled by the
    /// debugger. Out-of-range indices read back `false`.
    pub fn layer_debug_disabled(&self, layer: usize) -> bool {
        self.debug_layer_disable.is_hidden(layer)
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
            let display_on = (self.registers[1] & 0x40) != 0;
            if self.progressive_cram.is_some() {
                // Direct-Color-DMA streams CRAM with the display forcibly off, so
                // the screen is all backdrop and each ~2-pixel run shows the DMA
                // word for its beam position. Paint that per beam-X (the way
                // PicoDrive shows these pictures), a per-write approximation
                // of per-pixel CRAM that resolves the actual picture instead
                // of one colour per line. With the display on, a CRAM DMA is an ordinary
                // raster palette/backdrop effect: keep the per-line band model.
                if !display_on {
                    if self.recording.is_some() {
                        self.record_line(line, LineKind::DirectColour);
                    }
                    self.render_dcdma_line_beamx(line);
                } else {
                    // Land this line's share of the metered mid-active CRAM DMA
                    // before sampling the palette, so the per-line colour is right.
                    self.apply_progressive_cram_for_line(line);
                    if self.recording.is_some() {
                        self.record_line(line, LineKind::Drawn);
                    }
                    self.render_scanline(line);
                }
            } else {
                if self.recording.is_some() {
                    let kind = if display_on {
                        LineKind::Drawn
                    } else {
                        LineKind::Blank
                    };
                    self.record_line(line, kind);
                }
                self.render_scanline(line);
            }
        }
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
        sprites::link_chain(&self.vram, self.sat_base(), self.sprite_table_slots()).count()
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

impl Vdp {
    /// Switch the VDP's video region (NTSC/PAL). Only the region flag
    /// is updated; the transient scanline/counter state is left intact.
    /// Intended for session setup (before frames run): the fast-frame
    /// loop drives the scanline from 0 each frame, so the region flag is
    /// what actually selects the 262 vs 313 line count and VBLANK window.
    pub fn set_region(&mut self, region: timing::VideoRegion) {
        self.timing.region = region;
    }
}

#[cfg(test)]
mod tests;
