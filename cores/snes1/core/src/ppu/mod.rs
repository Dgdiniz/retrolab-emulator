//! Picture Processing Unit (S-PPU1 + S-PPU2).
//!
//! Owns VRAM (64 KiB), CGRAM (512 B), and OAM (544 B); the full
//! `$2100-$213F` register file; the BG scroll / Mode 7 / VRAM /
//! OAM / CGRAM internal latches; and the two PPU MDR latches
//! (PPU1 = `$2134-$2138`, PPU2 = `$213B-$213F`).
//!
//! Hosts the per-scanline renderer for Modes 0-7 (see [`render`]
//! and [`tile`]). The renderer is invoked by
//! `SnesEmulator::handle_event_at` (`system.rs`) on every
//! [`crate::scheduler::EventKind::RenderPos`] event with a mutable
//! framebuffer borrow; the PPU only reads its own VRAM/CGRAM and
//! never reaches back into the bus or the scheduler. Backgrounds
//! (Modes 0-7), mosaic, sprites (OBJ), window masking, the
//! sub-screen, and colour math (add/sub with the fixed colour or the
//! sub-screen) all render here.
//!
//! ## Submodule split
//!
//! All handler methods live in [`impl Ppu`] blocks across
//! sibling files, Rust lets us split an inherent impl across
//! modules without ceremony. The split is by register group and by
//! rendering stage:
//!
//! | File         | Owns registers                                      |
//! |--------------|-----------------------------------------------------|
//! | [`vram`]     | `$2115-$2119` (VMAIN, VMADDL/H, VMDATAL/H), `$2139/$213A` (VMDATA*READ) |
//! | [`oam`]      | `$2102-$2104` (OAMADDL/H, OAMDATA), `$2138` (OAMDATAREAD)                |
//! | [`cgram`]    | `$2121-$2122` (CGADD, CGDATA), `$213B` (CGDATAREAD)                       |
//! | [`mode7`]    | `$211A-$2120` (M7SEL, M7A-D, M7X/Y), `$2134-$2136` (MPYL/M/H)             |
//! | [`regs`]     | everything else, `$2100/$2101/$2105-$2114/$2123-$2133/$213E`             |
//! | [`tile`]     | pure tile/tilemap decoders for the renderer (no `&self`)                  |
//! | [`sprite`]   | pure sprite (OBJ) eval/raster helpers (no `&self`)                        |
//! | [`window`]   | pure window-mask + colour-region helpers (no `&self`)                     |
//! | [`colormath`]| pure colour add/sub arithmetic (no `&self`)                              |
//! | [`render`]   | per-scanline raster, Modes 0-7, mosaic, OBJ, sub-screen, windows, colour math |
//! | `mod.rs`     | struct, [`PpuAccessCtx`], [`Ppu::reset`], [`Ppu::read_reg`] / [`Ppu::write_reg`] dispatch |
//!
//! ## Access context
//!
//! Every dispatch call receives a [`PpuAccessCtx`] carrying the
//! current MDR latch and the raster position `(h_dot, v_scanline)`.
//! Real hardware gates several access paths on whether the chip is
//! in active-draw or VBlank (e.g. `vram_write` drops during active
//! scanlines unless `display_disable` is set), and the OAM/CGRAM
//! handlers consult the internal sprite-eval / colour-math addresses
//! during the active period instead of the externally-visible
//! `oam_addr` / `cgram_addr`. The context isolates the bus from
//! having to push raster state into the PPU on every register write.

#![allow(clippy::too_many_lines)]

pub mod cgram;
pub mod colormath;
mod debug;
pub use debug::{BgLayout, SpriteDescriptor, SpriteTileSettings, TileSetSource, ViewerImage};
pub mod mode7;
pub mod oam;
pub mod regs;
pub mod render;
pub mod sprite;
pub mod tile;
pub mod vram;
pub mod window;

use crate::ppu::render::{BgRowBuffer, BgRowPixel, MAX_OUTPUT_ROWS};

/// Side-channel inputs every PPU register access needs from the bus.
///
/// Built by [`crate::bus::SnesBus`] from `mdr` plus a cached raster
/// snapshot (see `SnesBus::set_raster_snapshot`). The PPU never
/// reaches back into the bus or the scheduler, every dependency
/// arrives through this struct. `Copy` because it is at most
/// 8 bytes wide; the dispatcher passes it by value into each
/// handler.
#[derive(Debug, Clone, Copy)]
pub struct PpuAccessCtx {
    /// Current MDR latch from the A-bus. Several PPU registers
    /// (notably `$2134-$2136`, `$213C-$213F`) leave some bits
    /// undriven and they read back as whatever the MDR last saw.
    pub mdr: u8,
    /// Current H dot position (0..339) snapshotted by the bus at
    /// the end of the last CPU instruction. The OAM/CGRAM access
    /// gates use this to decide between the public address pointer
    /// and the internal sprite-eval pointer.
    pub h_dot: u16,
    /// Current scanline (0..261 NTSC, 0..311 PAL) snapshotted by
    /// the bus. Drives the active-draw vs VBlank gate for VRAM /
    /// OAM / CGRAM access.
    pub v_scanline: u16,
}

impl PpuAccessCtx {
    /// Pseudo-context for tests that don't care about timing,
    /// reports MDR `0`, raster at `(0, 0)` (scanline 0, dot 0).
    pub const ZERO: Self = Self {
        mdr: 0,
        h_dot: 0,
        v_scanline: 0,
    };
}

/// Per-layer window configuration shared by `$2123-$2125` writes.
///
/// Indexed via [`Ppu::window_enable`]; one struct per layer lets
/// the dispatcher write `windows[layer] =
/// WindowEnable { ... }` in one shot.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WindowEnable {
    pub w1_enable: bool,
    pub w1_invert: bool,
    pub w2_enable: bool,
    pub w2_invert: bool,
}

/// Index into [`Ppu::window_enable`] and [`Ppu::window_combine`].
///
/// The PPU keeps six layer-keyed window selections (4 backgrounds +
/// sprites + colour-math); these constants name the slots so callers
/// don't sprinkle magic indices.
pub mod window_layer {
    pub const BG1: usize = 0;
    pub const BG2: usize = 1;
    pub const BG3: usize = 2;
    pub const BG4: usize = 3;
    pub const OBJ: usize = 4;
    pub const COL: usize = 5;
}

/// Cached tilemap-entry row for one background, reused across the
/// scanlines that fall inside the same tile band.
///
/// [`render::Ppu::render_bg_row`] fetches one tilemap entry per 8-px
/// tile; the 8 scanlines of a single tile band would otherwise each
/// re-run that fetch for the same ~32 entries. This holds the whole
/// row, indexed by the absolute tile column `(screen_x & mask_x) >>
/// 3`, up to 64 columns for a 64-wide page, and serves it until the
/// BG addressing, the tile row, or VRAM changes.
///
/// Coherence rests on [`Ppu::vram_generation`]: every landed VRAM
/// write bumps it, and writes only land while the access gate is open
/// (VBlank or forced blank), so during active draw the generation is
/// frozen and the cache always matches what the renderer reads from
/// VRAM. Only 8x8-tile BGs in Modes 0/1/3 use this, offset-per-tile
/// (2/4/6) has a per-column `voffset`, hires (5/6) a 16-px stride and
/// 16x16 tiles an entry that spans two 8-px tile rows, all of which
/// break the "one tile row per scanline" assumption, so they keep the
/// direct fetch path.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BgTilemapRowCache {
    valid: bool,
    generation: u64,
    screen_addr: u32,
    screen_size: u8,
    ty: u16,
    entries: [tile::TilemapEntry; 64],
}

impl BgTilemapRowCache {
    /// Empty (always-misses) slot. `const` so the boxed `[_; 4]` can
    /// be built with `vec![EMPTY; 4]`; the std `Default` blanket for
    /// arrays stops at 32, so `#[derive(Default)]` is unavailable here.
    const EMPTY: Self = Self {
        valid: false,
        generation: 0,
        screen_addr: 0,
        screen_size: 0,
        ty: 0,
        entries: [tile::TilemapEntry {
            tile: 0,
            palette: 0,
            priority: false,
            flip_h: false,
            flip_v: false,
        }; 64],
    };

    /// `true` when the slot holds the tilemap row for exactly this key.
    fn matches(&self, generation: u64, screen_addr: u32, screen_size: u8, ty: u16) -> bool {
        self.valid
            && self.generation == generation
            && self.screen_addr == screen_addr
            && self.screen_size == screen_size
            && self.ty == ty
    }

    /// Replace the cached row and its key, marking the slot valid.
    fn store(
        &mut self,
        generation: u64,
        screen_addr: u32,
        screen_size: u8,
        ty: u16,
        entries: [tile::TilemapEntry; 64],
    ) {
        self.valid = true;
        self.generation = generation;
        self.screen_addr = screen_addr;
        self.screen_size = screen_size;
        self.ty = ty;
        self.entries = entries;
    }

    /// Borrow the cached entries (meaningful only after a hit or
    /// [`Self::store`]).
    fn entries(&self) -> &[tile::TilemapEntry; 64] {
        &self.entries
    }
}

/// One decoded 4bpp sprite tile: eight rows of eight 4-bit pixels,
/// row-major (`pixels[row * 8 + col]`, each `0..=15`).
#[derive(Debug, Clone, Copy)]
struct SpriteTileSlot {
    valid: bool,
    generation: u64,
    pixels: [u8; 64],
}

impl SpriteTileSlot {
    const EMPTY: Self = Self {
        valid: false,
        generation: 0,
        pixels: [0; 64],
    };
}

/// Decoded-sprite-tile cache.
///
/// Sprites are always 4bpp, so the 64 KiB of VRAM holds at most 2048
/// distinct 8x8 sprite tiles (32 bytes each). The 11-bit tile number
/// that [`sprite::sprite_tile_number`] returns indexes those slots
/// 1:1, so the renderer can decode any sprite tile once and reuse it
/// across the eight scanlines of a tall sprite, and across every
/// other sprite that references the same tile.
///
/// Coherence rides the same [`Ppu::vram_generation`] counter as
/// [`BgTilemapRowCache`]: a slot records the generation it was decoded
/// under and re-decodes lazily when the counter moves. VRAM writes
/// only land while the access gate is open (VBlank / forced blank),
/// so the counter is frozen across an active frame and every cached
/// tile matches what the renderer would read straight from VRAM.
pub(crate) struct SpriteTileCache {
    slots: Box<[SpriteTileSlot; 2048]>,
}

impl SpriteTileCache {
    /// Allocate an all-invalid cache on the heap (160 KiB) so the
    /// big tile array never lands on the stack.
    fn new() -> Self {
        let slots: Box<[SpriteTileSlot; 2048]> = vec![SpriteTileSlot::EMPTY; 2048]
            .into_boxed_slice()
            .try_into()
            .expect("sprite tile cache alloc 2048 slots");
        Self { slots }
    }

    /// Invalidate every slot (reset / debugger poke).
    fn reset(&mut self) {
        for slot in self.slots.iter_mut() {
            *slot = SpriteTileSlot::EMPTY;
        }
    }

    /// `true` when the slot for `tile` already holds a decode taken at
    /// `generation`.
    fn matches(&self, tile: u16, generation: u64) -> bool {
        let slot = &self.slots[(tile & 0x07FF) as usize];
        slot.valid && slot.generation == generation
    }

    /// Replace the decoded pixels for `tile` and mark the slot valid.
    fn store(&mut self, tile: u16, generation: u64, pixels: [u8; 64]) {
        let slot = &mut self.slots[(tile & 0x07FF) as usize];
        slot.valid = true;
        slot.generation = generation;
        slot.pixels = pixels;
    }

    /// Borrow the decoded pixels for `tile` (meaningful only after a
    /// hit or [`Self::store`]).
    fn pixels(&self, tile: u16) -> &[u8; 64] {
        &self.slots[(tile & 0x07FF) as usize].pixels
    }
}

/// Picture processing unit register file + memories.
///
/// The struct holds **every** observable PPU register, even the ones
/// whose visible side effects are not wired (`$2133` bit 3, pseudo-hires,
/// is stored but not drawn). Keeping the surface complete means those
/// effects drop in without having to extend the dispatch matrix or the
/// test fixtures.
pub struct Ppu {
    // -- Memories -------------------------------------------------
    /// 64 KiB of VRAM, addressed as 32 K 16-bit words. The 5A22
    /// drives a word address (15 bits) into `vram_addr`; the
    /// `vram_mapping` bits of `$2115` apply a configurable swap
    /// before the lookup.
    vram: Box<[u16; 0x8000]>,
    /// 512 + 32 = 544 bytes of OAM. The first 512 bytes hold the
    /// low-table sprite list (4 bytes x 128 sprites); the trailing
    /// 32 bytes hold the high-table (2 bits x 128 sprites packed
    /// 4-per-byte).
    oam: Box<[u8; 0x220]>,
    /// 256 x 15-bit BGR colour palette. Stored as `u16` with bit
    /// 15 always 0; `$2122` writes mask the high byte with `0x7F`
    /// to enforce the invariant.
    cgram: Box<[u16; 0x100]>,

    // -- INIDISP ($2100) ------------------------------------------
    /// Bit 7: forced blank. When `true` the renderer outputs black
    /// and the VRAM/OAM/CGRAM access gates open up regardless of
    /// raster position.
    display_disable: bool,
    /// Bits 0-3: master brightness `0..=15`. `0` is full dim
    /// (output is forced black even with `display_disable = false`)
    /// and `15` is 100 % unattenuated.
    display_brightness: u8,

    // -- OBSEL ($2101) --------------------------------------------
    /// Bits 5-7: sprite size selector index (0..7). Decodes via
    /// the canonical small/large pair table.
    obj_base_size: u8,
    /// Bits 3-4: name-select offset for the second sprite tilemap
    /// page (only meaningful when sprite size is 16x16 or larger).
    obj_name_select: u8,
    /// Bits 0-2 shifted left 14: sprite character data base
    /// byte-address in VRAM (so `0..7 -> 0x0000, 0x4000, 0x8000,
    /// ..., 0x1C000`). Stored as `u32` because the max value
    /// (`0x1C000`) overflows `u16`; the renderer masks with the
    /// VRAM size when it reads.
    obj_tiledata_addr: u32,

    // -- OAMADDL/H ($2102/$2103) + internal state ----------------
    /// 10-bit base address loaded by `oam_address_reset()` (called on
    /// `$2102/$2103` writes and at VBlank start when display is
    /// enabled). The hardware doubles the byte unit before
    /// applying it, see `write_2102` for the `<< 1`.
    oam_baseaddr: u16,
    /// Current 10-bit OAM byte pointer used by `$2104`/`$2138`.
    /// Increments by 1 on each access and wraps inside the 0..0x3FF
    /// window; the mirror to `$200-$21F` is applied at lookup time.
    oam_addr: u16,
    /// `$2103` bit 7: when set, sprite priority rotation starts
    /// from `oam_baseaddr >> 2` instead of sprite 0.
    oam_priority: bool,
    /// Internal sprite-evaluation pointer. The active-scanline
    /// branches of `$2104`/`$2138` consult this instead of
    /// `oam_addr`. The sprite evaluator re-points it at the last
    /// in-range sprite's high-table word each scanline, so reads
    /// during active draw track the eval position.
    oam_iaddr: u16,
    /// 1-byte buffer holding the data of an even-byte write to
    /// `$2104`. The pair commits to OAM when the matching odd
    /// byte arrives, mirroring the real bus's 16-bit access.
    oam_latchdata: u8,
    /// Sprite index the per-scanline evaluator starts scanning from.
    /// `0` normally; when `$2103` bit 7 (priority rotation) is set it
    /// becomes `oam_baseaddr >> 2`, which lifts that sprite to the top
    /// of the priority order. Recomputed by [`Self::oam_address_reset`].
    oam_first_sprite: u8,

    // -- BGMODE ($2105) -------------------------------------------
    /// Bits 0-2: background mode `0..=7`.
    bgmode: u8,
    /// Bit 3: in mode 1 only, lifts BG3 to high priority.
    bg3_priority: bool,
    /// Bits 4-7: per-BG tile size. `false` = 8x8, `true` = 16x16.
    bg_tile_size: [bool; 4],

    // -- MOSAIC ($2106) -------------------------------------------
    /// Bits 4-7 + 1. Stored as `1..=16` so the renderer can divide
    /// directly without an extra `+ 1`.
    mosaic_size: u8,
    /// Bits 0-3: per-BG mosaic enable.
    bg_mosaic_enable: [bool; 4],
    /// Internal mosaic vertical counter. Reloaded when mosaic
    /// goes from disabled -> enabled.
    mosaic_vcounter: u8,

    // -- BGxSC ($2107-$210A) --------------------------------------
    /// Bits 2-7 << 9: per-BG tilemap byte address in VRAM. Stored
    /// as `u32` because `0xFC << 9 = 0x1F800` overflows `u16`.
    bg_screen_addr: [u32; 4],
    /// Bits 0-1: tilemap dimensions. `0` = 32x32, `1` = 64x32,
    /// `2` = 32x64, `3` = 64x64.
    bg_screen_size: [u8; 4],

    // -- BGxNBA ($210B-$210C) -------------------------------------
    /// Per-background character data byte-address in VRAM. Two
    /// backgrounds per register (low nibble for BG1/BG3, high
    /// nibble for BG2/BG4); each nibble is left-shifted 13. Stored
    /// as `u32` because the max value (`0xF << 13 = 0x1E000`)
    /// overflows `u16`.
    bg_tiledata_addr: [u32; 4],

    // -- BGxxOFS ($210D-$2114) ------------------------------------
    /// Per-background horizontal scroll. Stored as `u16`; only the
    /// bottom 10 bits matter to the renderer, but the full 16 bits
    /// are kept as written so the latch chain reads back raw. 13 bits
    /// are used in Mode 7.
    bg_hoffset: [u16; 4],
    /// Per-background vertical scroll. Same conventions as
    /// `bg_hoffset`.
    bg_voffset: [u16; 4],
    /// Shared PPU1 latch for BG scroll writes. Updated on every
    /// `$210D-$2114` write with the byte just written. Used by the
    /// chained-write formula `new = (data << 8) | (ppu1 & ~7) |
    /// ppu2` (H offset) or `new = (data << 8) | ppu1` (V offset).
    bgofs_ppu1_latch: u8,
    /// Shared PPU2 latch (3 bits, stored in a `u8`). Updated only
    /// on H-offset writes.
    bgofs_ppu2_latch: u8,

    // -- Mode 7 H/V offsets (BG1HOFS/BG1VOFS also write these) --
    /// 16-bit Mode 7 horizontal offset. Built via the shared
    /// `mode7_latchdata` latch, separately from the BG1 H offset
    /// latch, the same `$210D` write feeds both.
    mode7_hoffset: u16,
    /// Mode 7 vertical offset. Same chain via `mode7_latchdata`.
    mode7_voffset: u16,

    // -- VMAIN/VMADDL/VMADDH ($2115-$2117) -----------------------
    /// Raw `$2115` byte. Kept around for diagnostic readback even
    /// though `vram_incmode` / `vram_mapping` / `vram_incsize`
    /// already decode the useful bits.
    vmain_byte: u8,
    /// Bit 7 of `$2115`. `false` = auto-increment fires on
    /// `$2118` writes and `$2139` reads (the low-byte side);
    /// `true` = increment fires on `$2119` writes and `$213A`
    /// reads (the high-byte side).
    vram_incmode: bool,
    /// Bits 2-3 of `$2115`, VRAM address remap mode `0..=3`.
    vram_mapping: u8,
    /// Decoded increment step (`1`, `32`, `128`, `128`).
    vram_incsize: u16,
    /// 16-bit VRAM word address pointer. The remap kicks in only
    /// at access time; this field holds the *un-remapped* value
    /// the CPU writes via `$2116/$2117`.
    vram_addr: u16,
    /// 16-bit VRAM read prefetch buffer. Filled on every
    /// `$2116/$2117` write and after each `$2139/$213A` read on
    /// the matching half. `$2139` returns the low byte and
    /// `$213A` returns the high byte; both pre-load the buffer
    /// before applying auto-increment.
    vram_readbuffer: u16,

    // -- M7SEL ($211A) --------------------------------------------
    /// Bits 6-7: screen-over wrap mode for out-of-tilemap
    /// fetches.
    mode7_repeat: u8,
    /// Bit 1: vertical flip.
    mode7_vflip: bool,
    /// Bit 0: horizontal flip.
    mode7_hflip: bool,

    // -- M7A-M7Y ($211B-$2120) -----------------------------------
    /// 16-bit Mode 7 matrix coefficient A. Built via the shared
    /// `mode7_latchdata` latch on each write.
    m7a: u16,
    /// 16-bit Mode 7 matrix coefficient B. The MPYL/MPYM/MPYH
    /// read path computes `(int16)m7a * (int8)(m7b >> 8)` ->
    /// 24-bit signed result.
    m7b: u16,
    /// 16-bit Mode 7 matrix coefficient C.
    m7c: u16,
    /// 16-bit Mode 7 matrix coefficient D.
    m7d: u16,
    /// 13-bit Mode 7 centre X (sign-extended to 16 bits on read).
    m7x: u16,
    /// 13-bit Mode 7 centre Y.
    m7y: u16,
    /// Shared 8-bit latch for the Mode 7 matrix writes
    /// (M7A/B/C/D/X/Y), `new = (data << 8) | latch; latch = data`.
    /// BG1HOFS/BG1VOFS go through this latch too.
    mode7_latchdata: u8,

    // -- CGRAM ($2121-$2122) -------------------------------------
    /// 9-bit byte pointer (`$2121` writes shift left by 1 to
    /// double the colour-index unit into a byte unit).
    cgram_addr: u16,
    /// Internal colour-math byte pointer; consulted by the
    /// active-scanline branches of `$2122/$213B`. The renderer reads its
    /// palette entries straight from the array, so nothing moves it and it
    /// stays at 0.
    cgram_iaddr: u16,
    /// 1-byte buffer holding the low half of a 2-byte CGRAM
    /// write. Commits on the matching high-byte write.
    cgram_latchdata: u8,

    // -- Windows: WnSEL ($2123-$2125), WHn ($2126-$2129) --------
    /// Six per-layer window-enable records (BG1, BG2, BG3, BG4,
    /// OBJ, COL). Indexed via [`window_layer`].
    window_enable: [WindowEnable; 6],
    /// Window 1 left edge (raw byte from `$2126`).
    window_one_left: u8,
    /// Window 1 right edge (`$2127`).
    window_one_right: u8,
    /// Window 2 left edge (`$2128`).
    window_two_left: u8,
    /// Window 2 right edge (`$2129`).
    window_two_right: u8,
    /// Two-bit combine logic per layer (`0` = OR, `1` = AND,
    /// `2` = XOR, `3` = XNOR). Indexed via [`window_layer`];
    /// values for `COL` come from `$212B`, others from `$212A`.
    window_combine: [u8; 6],

    // -- TM/TS ($212C/$212D), TMW/TSW ($212E/$212F) -------------
    /// Main-screen per-BG enable bits (`$212C` bits 0-3).
    bg_main_enable: [bool; 4],
    /// Main-screen OBJ enable (`$212C` bit 4).
    obj_main_enable: bool,
    /// Sub-screen per-BG enable bits (`$212D` bits 0-3).
    bg_sub_enable: [bool; 4],
    /// Sub-screen OBJ enable (`$212D` bit 4).
    obj_sub_enable: bool,
    /// Main-screen per-BG window-disable bits (`$212E`).
    bg_main_window_disable: [bool; 4],
    /// Main-screen OBJ window-disable (`$212E` bit 4).
    obj_main_window_disable: bool,
    /// Sub-screen per-BG window-disable bits (`$212F`).
    bg_sub_window_disable: [bool; 4],
    /// Sub-screen OBJ window-disable (`$212F` bit 4).
    obj_sub_window_disable: bool,

    // -- CGWSEL ($2130), CGADDSUB ($2131), COLDATA ($2132) -----
    /// Colour-window mask for the main screen (`$2130` bits 6-7).
    col_main_mask: u8,
    /// Colour-window mask for the sub-screen (`$2130` bits 4-5).
    col_sub_mask: u8,
    /// Bit 1 of `$2130`: when set, the colour-math result is
    /// blended with the sub-screen instead of the fixed colour.
    addsub_mode: bool,
    /// Bit 0 of `$2130`: direct-colour mode in BG1/BG2 for
    /// 256-colour modes.
    direct_color: bool,
    /// Bit 7 of `$2131`: subtract instead of add.
    color_math_subtract: bool,
    /// Bit 6 of `$2131`: halve the result.
    color_math_halve: bool,
    /// Bit 5 of `$2131`: backdrop colour-math enable.
    color_math_back_enable: bool,
    /// Bit 4 of `$2131`: sprite colour-math enable (only sprites
    /// in palette 4-7 actually participate; the renderer applies
    /// that finer rule).
    color_math_oam_enable: bool,
    /// Bits 0-3 of `$2131`: per-BG colour-math enable.
    color_math_bg_enable: [bool; 4],
    /// 15-bit BGR fixed colour built up across multiple `$2132`
    /// writes, each write selects which of R/G/B to update via
    /// bits 5-7.
    fixed_color: u16,

    // -- SETINI ($2133) ------------------------------------------
    /// Bit 6: Mode 7 BG2 (EXTBG) enable.
    mode7_extbg: bool,
    /// Bit 3: pseudo-hires (mode-5 style without enabling mode 5
    /// proper).
    pseudo_hires: bool,
    /// Bit 2: overscan, extends visible scanlines from 224 to
    /// 240, shifts the VBlank gate accordingly.
    overscan: bool,
    /// Bit 1: sprite interlace.
    obj_interlace: bool,
    /// Bit 0: frame interlace.
    interlace: bool,
    /// Current interlace field (`false = first / even field`,
    /// `true = second / odd field`). Toggled at VBlankStart by
    /// `SnesEmulator::handle_event_at` (`system.rs`). The field
    /// state is observable via [`Self::field`] and surfaces on
    /// `$213F` (STAT78) bit 7 even when `interlace` itself is
    /// disabled, as on the hardware.
    field: bool,

    // -- Host display preference (not a hardware register) -------
    /// When `false` (default) the visible frame is cropped to the
    /// 224-line NTSC "TV window": an overscan frame drops the top 7
    /// and bottom 8 scanlines, which is what bsnes-plus and snes9x
    /// show by default. When `true`, the full 240-line (480
    /// hires) overscan frame is exposed. This is a UI toggle, so it
    /// persists across reset and savestate restore, it is set by the
    /// host, never by emulated code.
    show_overscan: bool,

    // -- MDR latches ---------------------------------------------
    /// PPU1 MDR. Updated by every PPU1 read (`$2134-$2138`).
    /// Reads of undriven bits in those registers see this value.
    ppu1_mdr: u8,
    /// PPU2 MDR. Updated by every PPU2 read (`$2139-$213F`,
    /// which excludes the BBus-side intercepts the bus handles
    /// itself).
    ppu2_mdr: u8,

    // -- STAT77 sprite flags ------------------------------------
    /// `$213E` bit 7: more than 34 sprite tiles fetched on some
    /// scanline this frame. Set (`|=`) by the sprite evaluator and
    /// cleared once per frame at the frame top.
    sprite_time_over: bool,
    /// `$213E` bit 6: more than 32 sprites fell on some scanline this
    /// frame. Same set/clear lifecycle as [`Self::sprite_time_over`].
    sprite_range_over: bool,

    // -- Renderer state -----------------------------------------
    /// Previous scanline's per-BG output, used by the vertical
    /// mosaic replicator in [`render::Ppu::render_scanline`]. The
    /// buffer is 512 pixels wide so the same allocation backs both
    /// the non-hires renderer (which only touches the leading 256
    /// slots) and the hires Mode 5/6 renderer; ~2 KiB per BG x 4
    /// BGs = 8 KiB, boxed so the inline struct size stays bounded
    /// for `mem::swap` / `Default` callers.
    pub(crate) previous_bg_rows: Box<[BgRowBuffer; 4]>,

    /// Per-BG tilemap-row cache (see [`BgTilemapRowCache`]). Boxed
    /// next to `previous_bg_rows` so the ~2 KiB of entries stays off
    /// the inline struct. Populated only by Modes 0/1/3.
    pub(crate) bg_tilemap_row_cache: Box<[BgTilemapRowCache; 4]>,

    /// Monotonic VRAM-write counter driving tilemap-cache coherence.
    /// [`vram::Ppu::vram_write_byte`] bumps it on every landed write;
    /// each [`BgTilemapRowCache`] stores the value it was built under
    /// and rebuilds when it changes. Writes only land while the access
    /// gate is open (VBlank / forced blank), so the counter is frozen
    /// across an active frame and the cache stays coherent for the
    /// whole visible region.
    vram_generation: u64,

    /// Decoded-sprite-tile cache (see [`SpriteTileCache`]). Shares the
    /// `vram_generation` coherence contract with the tilemap-row cache.
    sprite_tile_cache: SpriteTileCache,

    /// Debugger layer-disable mask (not part of any savestate). Index
    /// `0..=3` blanks BG1-4, index `4` blanks OBJ. When a slot is set
    /// the renderer drops that layer from the composite so the debugger
    /// can isolate what each plane contributes. Always all-`false`
    /// during normal play; toggled via [`Ppu::set_layer_debug_disabled`].
    debug_layer_disable: [bool; LAYER_COUNT],

    // -- Frame-global display-size state (transient, not savestated) --
    /// ORs `is_hires_mode(bgmode)` across every visible scanline of the
    /// current frame so [`render::Ppu::active_display_size`] reports 512
    /// width if *any* line was hires (Mode 5/6), as bsnes-plus also
    /// reports it. Reset at the frame top (`render_scanline`
    /// scanline 1) and consumed by [`render::Ppu::finalize_frame`].
    pub(crate) frame_hires: bool,
    /// Interlace decision (`$2133` bit 0) latched for the whole frame at
    /// scanline 1. Drives both the [`render`] weave (two woven rows per
    /// scanline) and the reported height, so the rendered row count and
    /// the display size can't disagree when the game toggles interlace
    /// mid-frame or during VBlank.
    pub(crate) render_interlace: bool,
    /// Per-output-row hires record. `true` for rows a hires scanline
    /// produced (already 512 wide); `false` for lo-res rows (256 wide)
    /// that [`render::Ppu::finalize_frame`] expands to 512 when
    /// `frame_hires`. Sized for the worst case (240 overscan x 2
    /// interlace = 480 rows). Rebuilt every frame, never savestated.
    pub(crate) line_hires: [bool; MAX_OUTPUT_ROWS],
}

/// Number of toggleable debug layers: BG1-4 plus OBJ.
pub(crate) const LAYER_COUNT: usize = 5;

/// Debug-layer index of the OBJ (sprite) plane (BG1-4 occupy `0..=3`).
pub(crate) const LAYER_OBJ: usize = 4;

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
}

impl Ppu {
    /// Build a freshly-reset PPU. All memories zeroed,
    /// `display_disable = true`, `vram_incsize = 1`: same surface
    /// as [`Ppu::reset`] but constructed from scratch.
    pub fn new() -> Self {
        // Allocate the three big memories on the heap so the
        // struct stays small on the stack, 64 KiB of VRAM alone
        // would otherwise dominate every `mem::swap` and the like.
        let vram = vec![0u16; 0x8000]
            .into_boxed_slice()
            .try_into()
            .expect("vram alloc 32K words");
        let oam = vec![0u8; 0x220]
            .into_boxed_slice()
            .try_into()
            .expect("oam alloc 544 bytes");
        let cgram = vec![0u16; 0x100]
            .into_boxed_slice()
            .try_into()
            .expect("cgram alloc 256 words");
        let previous_bg_rows: Box<[BgRowBuffer; 4]> = vec![[BgRowPixel::TRANSPARENT; 512]; 4]
            .into_boxed_slice()
            .try_into()
            .expect("previous_bg_rows alloc 4 buffers");
        let bg_tilemap_row_cache: Box<[BgTilemapRowCache; 4]> = vec![BgTilemapRowCache::EMPTY; 4]
            .into_boxed_slice()
            .try_into()
            .expect("bg_tilemap_row_cache alloc 4 slots");
        let sprite_tile_cache = SpriteTileCache::new();

        let mut ppu = Self {
            vram,
            oam,
            cgram,
            display_disable: true,
            display_brightness: 0,
            obj_base_size: 0,
            obj_name_select: 0,
            obj_tiledata_addr: 0,
            oam_baseaddr: 0,
            oam_addr: 0,
            oam_priority: false,
            oam_iaddr: 0,
            oam_latchdata: 0,
            oam_first_sprite: 0,
            bgmode: 0,
            bg3_priority: false,
            bg_tile_size: [false; 4],
            mosaic_size: 1,
            bg_mosaic_enable: [false; 4],
            mosaic_vcounter: 0,
            bg_screen_addr: [0; 4],
            bg_screen_size: [0; 4],
            bg_tiledata_addr: [0; 4],
            bg_hoffset: [0; 4],
            bg_voffset: [0; 4],
            bgofs_ppu1_latch: 0,
            bgofs_ppu2_latch: 0,
            mode7_hoffset: 0,
            mode7_voffset: 0,
            vmain_byte: 0,
            vram_incmode: false,
            vram_mapping: 0,
            vram_incsize: 1,
            vram_addr: 0,
            vram_readbuffer: 0,
            mode7_repeat: 0,
            mode7_vflip: false,
            mode7_hflip: false,
            m7a: 0,
            m7b: 0,
            m7c: 0,
            m7d: 0,
            m7x: 0,
            m7y: 0,
            mode7_latchdata: 0,
            cgram_addr: 0,
            cgram_iaddr: 0,
            cgram_latchdata: 0,
            window_enable: [WindowEnable::default(); 6],
            window_one_left: 0,
            window_one_right: 0,
            window_two_left: 0,
            window_two_right: 0,
            window_combine: [0; 6],
            bg_main_enable: [false; 4],
            obj_main_enable: false,
            bg_sub_enable: [false; 4],
            obj_sub_enable: false,
            bg_main_window_disable: [false; 4],
            obj_main_window_disable: false,
            bg_sub_window_disable: [false; 4],
            obj_sub_window_disable: false,
            col_main_mask: 0,
            col_sub_mask: 0,
            addsub_mode: false,
            direct_color: false,
            color_math_subtract: false,
            color_math_halve: false,
            color_math_back_enable: false,
            color_math_oam_enable: false,
            color_math_bg_enable: [false; 4],
            fixed_color: 0,
            mode7_extbg: false,
            pseudo_hires: false,
            overscan: false,
            obj_interlace: false,
            interlace: false,
            field: false,
            show_overscan: false,
            ppu1_mdr: 0,
            ppu2_mdr: 0,
            sprite_time_over: false,
            sprite_range_over: false,
            previous_bg_rows,
            bg_tilemap_row_cache,
            vram_generation: 0,
            sprite_tile_cache,
            debug_layer_disable: [false; LAYER_COUNT],
            frame_hires: false,
            render_interlace: false,
            line_hires: [false; MAX_OUTPUT_ROWS],
        };
        ppu.reset();
        ppu
    }

    /// Capture the full register file + VRAM/OAM/CGRAM into a snapshot.
    /// Render caches are not captured, they rebuild lazily on the next
    /// render after a restore.
    pub fn to_savestate(&self) -> crate::savestate::PpuState {
        crate::savestate::PpuState {
            vram: self.vram.to_vec(),
            oam: self.oam.to_vec(),
            cgram: self.cgram.to_vec(),
            display_disable: self.display_disable,
            display_brightness: self.display_brightness,
            obj_base_size: self.obj_base_size,
            obj_name_select: self.obj_name_select,
            obj_tiledata_addr: self.obj_tiledata_addr,
            oam_baseaddr: self.oam_baseaddr,
            oam_addr: self.oam_addr,
            oam_priority: self.oam_priority,
            oam_iaddr: self.oam_iaddr,
            oam_latchdata: self.oam_latchdata,
            oam_first_sprite: self.oam_first_sprite,
            bgmode: self.bgmode,
            bg3_priority: self.bg3_priority,
            bg_tile_size: self.bg_tile_size,
            mosaic_size: self.mosaic_size,
            bg_mosaic_enable: self.bg_mosaic_enable,
            mosaic_vcounter: self.mosaic_vcounter,
            bg_screen_addr: self.bg_screen_addr,
            bg_screen_size: self.bg_screen_size,
            bg_tiledata_addr: self.bg_tiledata_addr,
            bg_hoffset: self.bg_hoffset,
            bg_voffset: self.bg_voffset,
            bgofs_ppu1_latch: self.bgofs_ppu1_latch,
            bgofs_ppu2_latch: self.bgofs_ppu2_latch,
            mode7_hoffset: self.mode7_hoffset,
            mode7_voffset: self.mode7_voffset,
            vmain_byte: self.vmain_byte,
            vram_incmode: self.vram_incmode,
            vram_mapping: self.vram_mapping,
            vram_incsize: self.vram_incsize,
            vram_addr: self.vram_addr,
            vram_readbuffer: self.vram_readbuffer,
            mode7_repeat: self.mode7_repeat,
            mode7_vflip: self.mode7_vflip,
            mode7_hflip: self.mode7_hflip,
            m7a: self.m7a,
            m7b: self.m7b,
            m7c: self.m7c,
            m7d: self.m7d,
            m7x: self.m7x,
            m7y: self.m7y,
            mode7_latchdata: self.mode7_latchdata,
            cgram_addr: self.cgram_addr,
            cgram_iaddr: self.cgram_iaddr,
            cgram_latchdata: self.cgram_latchdata,
            window_enable: core::array::from_fn(|i| {
                crate::savestate::pack_window_enable(&self.window_enable[i])
            }),
            window_one_left: self.window_one_left,
            window_one_right: self.window_one_right,
            window_two_left: self.window_two_left,
            window_two_right: self.window_two_right,
            window_combine: self.window_combine,
            bg_main_enable: self.bg_main_enable,
            obj_main_enable: self.obj_main_enable,
            bg_sub_enable: self.bg_sub_enable,
            obj_sub_enable: self.obj_sub_enable,
            bg_main_window_disable: self.bg_main_window_disable,
            obj_main_window_disable: self.obj_main_window_disable,
            bg_sub_window_disable: self.bg_sub_window_disable,
            obj_sub_window_disable: self.obj_sub_window_disable,
            col_main_mask: self.col_main_mask,
            col_sub_mask: self.col_sub_mask,
            addsub_mode: self.addsub_mode,
            direct_color: self.direct_color,
            color_math_subtract: self.color_math_subtract,
            color_math_halve: self.color_math_halve,
            color_math_back_enable: self.color_math_back_enable,
            color_math_oam_enable: self.color_math_oam_enable,
            color_math_bg_enable: self.color_math_bg_enable,
            fixed_color: self.fixed_color,
            mode7_extbg: self.mode7_extbg,
            pseudo_hires: self.pseudo_hires,
            overscan: self.overscan,
            obj_interlace: self.obj_interlace,
            interlace: self.interlace,
            field: self.field,
            ppu1_mdr: self.ppu1_mdr,
            ppu2_mdr: self.ppu2_mdr,
            sprite_time_over: self.sprite_time_over,
            sprite_range_over: self.sprite_range_over,
        }
    }

    /// Overwrite the register file + memories from a snapshot, then
    /// drop every render cache so the next render rebuilds from the
    /// restored VRAM. Lengths are assumed validated by
    /// [`crate::savestate::SnesSaveState::validate`].
    pub fn from_savestate(&mut self, s: &crate::savestate::PpuState) {
        self.vram.copy_from_slice(&s.vram);
        self.oam.copy_from_slice(&s.oam);
        self.cgram.copy_from_slice(&s.cgram);
        self.display_disable = s.display_disable;
        self.display_brightness = s.display_brightness;
        self.obj_base_size = s.obj_base_size;
        self.obj_name_select = s.obj_name_select;
        self.obj_tiledata_addr = s.obj_tiledata_addr;
        self.oam_baseaddr = s.oam_baseaddr;
        self.oam_addr = s.oam_addr;
        self.oam_priority = s.oam_priority;
        self.oam_iaddr = s.oam_iaddr;
        self.oam_latchdata = s.oam_latchdata;
        self.oam_first_sprite = s.oam_first_sprite;
        self.bgmode = s.bgmode;
        self.bg3_priority = s.bg3_priority;
        self.bg_tile_size = s.bg_tile_size;
        self.mosaic_size = s.mosaic_size;
        self.bg_mosaic_enable = s.bg_mosaic_enable;
        self.mosaic_vcounter = s.mosaic_vcounter;
        self.bg_screen_addr = s.bg_screen_addr;
        self.bg_screen_size = s.bg_screen_size;
        self.bg_tiledata_addr = s.bg_tiledata_addr;
        self.bg_hoffset = s.bg_hoffset;
        self.bg_voffset = s.bg_voffset;
        self.bgofs_ppu1_latch = s.bgofs_ppu1_latch;
        self.bgofs_ppu2_latch = s.bgofs_ppu2_latch;
        self.mode7_hoffset = s.mode7_hoffset;
        self.mode7_voffset = s.mode7_voffset;
        self.vmain_byte = s.vmain_byte;
        self.vram_incmode = s.vram_incmode;
        self.vram_mapping = s.vram_mapping;
        self.vram_incsize = s.vram_incsize;
        self.vram_addr = s.vram_addr;
        self.vram_readbuffer = s.vram_readbuffer;
        self.mode7_repeat = s.mode7_repeat;
        self.mode7_vflip = s.mode7_vflip;
        self.mode7_hflip = s.mode7_hflip;
        self.m7a = s.m7a;
        self.m7b = s.m7b;
        self.m7c = s.m7c;
        self.m7d = s.m7d;
        self.m7x = s.m7x;
        self.m7y = s.m7y;
        self.mode7_latchdata = s.mode7_latchdata;
        self.cgram_addr = s.cgram_addr;
        self.cgram_iaddr = s.cgram_iaddr;
        self.cgram_latchdata = s.cgram_latchdata;
        for (dst, &bits) in self.window_enable.iter_mut().zip(s.window_enable.iter()) {
            *dst = crate::savestate::unpack_window_enable(bits);
        }
        self.window_one_left = s.window_one_left;
        self.window_one_right = s.window_one_right;
        self.window_two_left = s.window_two_left;
        self.window_two_right = s.window_two_right;
        self.window_combine = s.window_combine;
        self.bg_main_enable = s.bg_main_enable;
        self.obj_main_enable = s.obj_main_enable;
        self.bg_sub_enable = s.bg_sub_enable;
        self.obj_sub_enable = s.obj_sub_enable;
        self.bg_main_window_disable = s.bg_main_window_disable;
        self.obj_main_window_disable = s.obj_main_window_disable;
        self.bg_sub_window_disable = s.bg_sub_window_disable;
        self.obj_sub_window_disable = s.obj_sub_window_disable;
        self.col_main_mask = s.col_main_mask;
        self.col_sub_mask = s.col_sub_mask;
        self.addsub_mode = s.addsub_mode;
        self.direct_color = s.direct_color;
        self.color_math_subtract = s.color_math_subtract;
        self.color_math_halve = s.color_math_halve;
        self.color_math_back_enable = s.color_math_back_enable;
        self.color_math_oam_enable = s.color_math_oam_enable;
        self.color_math_bg_enable = s.color_math_bg_enable;
        self.fixed_color = s.fixed_color;
        self.mode7_extbg = s.mode7_extbg;
        self.pseudo_hires = s.pseudo_hires;
        self.overscan = s.overscan;
        self.obj_interlace = s.obj_interlace;
        self.interlace = s.interlace;
        self.field = s.field;
        self.ppu1_mdr = s.ppu1_mdr;
        self.ppu2_mdr = s.ppu2_mdr;
        self.sprite_time_over = s.sprite_time_over;
        self.sprite_range_over = s.sprite_range_over;

        // Render caches are not part of the wire format: drop them so
        // the next render rebuilds against the restored VRAM. Their
        // `valid` flags gate every lookup, so a cleared cache always
        // misses regardless of the generation counter.
        for row in self.previous_bg_rows.iter_mut() {
            *row = [BgRowPixel::TRANSPARENT; 512];
        }
        for slot in self.bg_tilemap_row_cache.iter_mut() {
            *slot = BgTilemapRowCache::EMPTY;
        }
        self.vram_generation = 0;
        self.sprite_tile_cache.reset();

        // Frame-global render-output state is transient and rebuilt on
        // the next render; drop it so a display-size poll right after a
        // restore reports the lo-res default until the first frame runs.
        self.frame_hires = false;
        self.render_interlace = false;
        self.line_hires = [false; MAX_OUTPUT_ROWS];
    }

    /// Reset to the canonical PPU power-on state. Memories are
    /// **not** zeroed (so a quick reset preserves whatever the
    /// debugger was inspecting); registers are reset to the
    /// settings the chip settles on (checked against bsnes-plus):
    /// `display_disable = true`, `vram_incsize = 1`, everything
    /// else cleared. Real silicon initialises most registers to
    /// random patterns, software that runs on a cold boot
    /// always writes them itself, so the deterministic zero is
    /// the safe choice for replays and tests.
    pub fn reset(&mut self) {
        // Register-side reset only, VRAM/OAM/CGRAM persist so a
        // debugger reset is non-destructive. The boot ROM writes
        // its own initial palette / sprite table before enabling
        // the display anyway.
        self.display_disable = true;
        self.display_brightness = 0;

        self.obj_base_size = 0;
        self.obj_name_select = 0;
        self.obj_tiledata_addr = 0;

        self.oam_baseaddr = 0;
        self.oam_addr = 0;
        self.oam_priority = false;
        self.oam_iaddr = 0;
        self.oam_latchdata = 0;
        self.oam_first_sprite = 0;

        self.bgmode = 0;
        self.bg3_priority = false;
        self.bg_tile_size = [false; 4];

        self.mosaic_size = 1;
        self.bg_mosaic_enable = [false; 4];
        self.mosaic_vcounter = 0;

        self.bg_screen_addr = [0; 4];
        self.bg_screen_size = [0; 4];
        self.bg_tiledata_addr = [0; 4];
        self.bg_hoffset = [0; 4];
        self.bg_voffset = [0; 4];
        self.bgofs_ppu1_latch = 0;
        self.bgofs_ppu2_latch = 0;
        self.mode7_hoffset = 0;
        self.mode7_voffset = 0;

        self.vmain_byte = 0;
        self.vram_incmode = false;
        self.vram_mapping = 0;
        self.vram_incsize = 1;
        self.vram_addr = 0;
        self.vram_readbuffer = 0;

        self.mode7_repeat = 0;
        self.mode7_vflip = false;
        self.mode7_hflip = false;
        self.m7a = 0;
        self.m7b = 0;
        self.m7c = 0;
        self.m7d = 0;
        self.m7x = 0;
        self.m7y = 0;
        self.mode7_latchdata = 0;

        self.cgram_addr = 0;
        self.cgram_iaddr = 0;
        self.cgram_latchdata = 0;

        self.window_enable = [WindowEnable::default(); 6];
        self.window_one_left = 0;
        self.window_one_right = 0;
        self.window_two_left = 0;
        self.window_two_right = 0;
        self.window_combine = [0; 6];

        self.bg_main_enable = [false; 4];
        self.obj_main_enable = false;
        self.bg_sub_enable = [false; 4];
        self.obj_sub_enable = false;
        self.bg_main_window_disable = [false; 4];
        self.obj_main_window_disable = false;
        self.bg_sub_window_disable = [false; 4];
        self.obj_sub_window_disable = false;

        self.col_main_mask = 0;
        self.col_sub_mask = 0;
        self.addsub_mode = false;
        self.direct_color = false;
        self.color_math_subtract = false;
        self.color_math_halve = false;
        self.color_math_back_enable = false;
        self.color_math_oam_enable = false;
        self.color_math_bg_enable = [false; 4];
        self.fixed_color = 0;

        self.mode7_extbg = false;
        self.pseudo_hires = false;
        self.overscan = false;
        self.obj_interlace = false;
        self.interlace = false;
        // The interlace field is part of the visible-output state,
        // not a register, but reset still returns to the canonical
        // "first / even field" so determinism holds across resets.
        self.field = false;

        // Frame-global render-output state is rebuilt every frame at
        // scanline 1; clear it so a poll of `active_display_size` before
        // the first post-reset frame reports the lo-res default.
        self.frame_hires = false;
        self.render_interlace = false;
        self.line_hires = [false; MAX_OUTPUT_ROWS];

        self.ppu1_mdr = 0;
        self.ppu2_mdr = 0;
        self.sprite_time_over = false;
        self.sprite_range_over = false;

        // Mosaic history: drop any pixels left over from a prior
        // run so a reset -> forced-blank-lifted -> mosaic-enable
        // sequence doesn't pull in stale colours. Buffers are
        // 512-wide so hires (Modes 5/6) shares the same vmosaic
        // path, non-hires only ever uses the leading 256 slots.
        for row in self.previous_bg_rows.iter_mut() {
            *row = [BgRowPixel::TRANSPARENT; 512];
        }

        // Tilemap-row cache: drop rows cached under the pre-reset VRAM
        // contents and rewind the generation so the first post-reset
        // render rebuilds cleanly.
        for slot in self.bg_tilemap_row_cache.iter_mut() {
            *slot = BgTilemapRowCache::EMPTY;
        }
        self.vram_generation = 0;

        // Sprite-tile cache rides the same generation reset.
        self.sprite_tile_cache.reset();
    }

    /// Force every per-BG tilemap-row cache to miss on the next
    /// render. Test-only: production VRAM mutations all flow through
    /// [`vram::Ppu::vram_write_byte`], which bumps the generation, so
    /// this exists purely for tests that poke `vram` directly.
    #[cfg(test)]
    pub(crate) fn invalidate_tilemap_caches(&mut self) {
        for slot in self.bg_tilemap_row_cache.iter_mut() {
            slot.valid = false;
        }
    }

    // -- Read-only accessors (debugger + tests) -----------------

    /// Borrow the VRAM word array (32 K x 16-bit).
    pub fn vram(&self) -> &[u16; 0x8000] {
        &self.vram
    }

    /// Borrow OAM (544 bytes, 512 low table + 32 high table).
    pub fn oam(&self) -> &[u8; 0x220] {
        &self.oam
    }

    /// Borrow CGRAM (256 x 15-bit BGR colours).
    pub fn cgram(&self) -> &[u16; 0x100] {
        &self.cgram
    }

    /// Force-disable or re-enable a debug layer (`0..=3` = BG1-4,
    /// `4` = OBJ). Out-of-range ids are ignored. A disabled layer is
    /// dropped from the composite so the debugger can isolate each
    /// plane's contribution; this is debugger-only state and is never
    /// persisted in a savestate.
    pub fn set_layer_debug_disabled(&mut self, layer: usize, disabled: bool) {
        if layer < LAYER_COUNT {
            self.debug_layer_disable[layer] = disabled;
        }
    }

    /// Whether debug layer `layer` (`0..=3` = BG1-4, `4` = OBJ) is
    /// currently force-disabled by the debugger.
    pub fn layer_debug_disabled(&self, layer: usize) -> bool {
        self.debug_layer_disable
            .get(layer)
            .copied()
            .unwrap_or(false)
    }

    /// Whether the display is in forced blank (`$2100` bit 7).
    pub fn display_disable(&self) -> bool {
        self.display_disable
    }

    /// Master brightness `0..=15` (`$2100` bits 0-3).
    pub fn display_brightness(&self) -> u8 {
        self.display_brightness
    }

    /// Current BG mode `0..=7` (`$2105` bits 0-2).
    pub fn bgmode(&self) -> u8 {
        self.bgmode
    }

    /// Mode 7 screen-over repeat mode (`$211A` bits 6-7): 0/1 wrap, 2
    /// transparent outside the plane, 3 forces character 0 outside.
    pub fn mode7_repeat(&self) -> u8 {
        self.mode7_repeat
    }

    /// Current VRAM word-address pointer (un-remapped).
    pub fn vram_addr(&self) -> u16 {
        self.vram_addr
    }

    /// Decoded VRAM auto-increment step (`1`, `32`, or `128`).
    pub fn vram_incsize(&self) -> u16 {
        self.vram_incsize
    }

    /// Current OAM byte pointer (10 bits).
    pub fn oam_addr(&self) -> u16 {
        self.oam_addr
    }

    /// Current CGRAM byte pointer (9 bits).
    pub fn cgram_addr(&self) -> u16 {
        self.cgram_addr
    }

    /// Last value seen on PPU1's MDR.
    pub fn ppu1_mdr(&self) -> u8 {
        self.ppu1_mdr
    }

    /// Last value seen on PPU2's MDR.
    pub fn ppu2_mdr(&self) -> u8 {
        self.ppu2_mdr
    }

    /// Update the PPU2 MDR latch. Used by [`crate::bus::SnesBus`]
    /// after it builds STAT78, that byte is the most recent value
    /// PPU2 drove on the data lines, so any later read of an
    /// MDR-only PPU2 register sees it.
    pub(crate) fn set_ppu2_mdr(&mut self, value: u8) {
        self.ppu2_mdr = value;
    }

    /// Mode 7 matrix coefficient A.
    pub fn m7a(&self) -> u16 {
        self.m7a
    }

    /// Mode 7 matrix coefficient B.
    pub fn m7b(&self) -> u16 {
        self.m7b
    }

    /// Mode 7 matrix coefficient C.
    pub fn m7c(&self) -> u16 {
        self.m7c
    }

    /// Mode 7 matrix coefficient D.
    pub fn m7d(&self) -> u16 {
        self.m7d
    }

    /// Mode 7 centre X (13-bit sign-extended).
    pub fn m7x(&self) -> u16 {
        self.m7x
    }

    /// Mode 7 centre Y.
    pub fn m7y(&self) -> u16 {
        self.m7y
    }

    /// BG H scroll per index `0..=3`. Panics on out-of-range.
    pub fn bg_hoffset(&self, bg: usize) -> u16 {
        self.bg_hoffset[bg]
    }

    /// BG V scroll per index `0..=3`. Panics on out-of-range.
    pub fn bg_voffset(&self, bg: usize) -> u16 {
        self.bg_voffset[bg]
    }

    /// 15-bit BGR fixed colour from `$2132` writes.
    pub fn fixed_color(&self) -> u16 {
        self.fixed_color
    }

    /// Overscan bit (`$2133` bit 2), affects VBlank line gating.
    pub fn overscan(&self) -> bool {
        self.overscan
    }

    /// Host display preference: see [`Self::show_overscan`]. When
    /// `false` (default) overscan frames are cropped to the 224-line
    /// NTSC TV window; when `true` the full 240-line frame is exposed.
    pub fn set_show_overscan(&mut self, show: bool) {
        self.show_overscan = show;
    }

    /// Whether the full overscan frame is exposed (vs cropped to 224).
    pub fn show_overscan_enabled(&self) -> bool {
        self.show_overscan
    }

    /// Whether the renderer is currently in active draw (i.e.
    /// the access gates close VRAM/OAM/CGRAM unless
    /// `display_disable` is set). Considers `overscan` so the
    /// 225 (off) vs 240 (on) boundary matches real hardware.
    pub(crate) fn in_active_scanline(&self, v_scanline: u16) -> bool {
        let last_visible: u16 = if self.overscan { 240 } else { 225 };
        v_scanline < last_visible
    }

    // -- Top-level dispatch -------------------------------------

    /// Dispatch a write to one of the 64 PPU registers. `reg` is
    /// the bottom 6 bits of the A-bus address (`addr - 0x2100`,
    /// masked to `0x3F` because the upper register window is
    /// PPU2-side and uses the same dispatch).
    pub fn write_reg(&mut self, reg: u8, value: u8, ctx: &PpuAccessCtx) {
        match reg & 0x3F {
            0x00 => self.write_2100(value, ctx),
            0x01 => self.write_2101(value),
            0x02 => self.write_2102(value),
            0x03 => self.write_2103(value),
            0x04 => self.write_2104(value, ctx),
            0x05 => self.write_2105(value),
            0x06 => self.write_2106(value),
            0x07 => self.write_2107(value),
            0x08 => self.write_2108(value),
            0x09 => self.write_2109(value),
            0x0A => self.write_210a(value),
            0x0B => self.write_210b(value),
            0x0C => self.write_210c(value),
            0x0D => self.write_210d(value),
            0x0E => self.write_210e(value),
            0x0F => self.write_210f(value),
            0x10 => self.write_2110(value),
            0x11 => self.write_2111(value),
            0x12 => self.write_2112(value),
            0x13 => self.write_2113(value),
            0x14 => self.write_2114(value),
            0x15 => self.write_2115(value),
            0x16 => self.write_2116(value, ctx),
            0x17 => self.write_2117(value, ctx),
            0x18 => self.write_2118(value, ctx),
            0x19 => self.write_2119(value, ctx),
            0x1A => self.write_211a(value),
            0x1B => self.write_211b(value),
            0x1C => self.write_211c(value),
            0x1D => self.write_211d(value),
            0x1E => self.write_211e(value),
            0x1F => self.write_211f(value),
            0x20 => self.write_2120(value),
            0x21 => self.write_2121(value),
            0x22 => self.write_2122(value, ctx),
            0x23 => self.write_2123(value),
            0x24 => self.write_2124(value),
            0x25 => self.write_2125(value),
            0x26 => self.write_2126(value),
            0x27 => self.write_2127(value),
            0x28 => self.write_2128(value),
            0x29 => self.write_2129(value),
            0x2A => self.write_212a(value),
            0x2B => self.write_212b(value),
            0x2C => self.write_212c(value),
            0x2D => self.write_212d(value),
            0x2E => self.write_212e(value),
            0x2F => self.write_212f(value),
            0x30 => self.write_2130(value),
            0x31 => self.write_2131(value),
            0x32 => self.write_2132(value),
            0x33 => self.write_2133(value),
            // $2134-$213F are read-only on real hardware, writes
            // are silently dropped.
            0x34..=0x3F => {}
            _ => unreachable!("masked above"),
        }
    }

    /// Dispatch a read from one of the 64 PPU registers. Returns
    /// MDR for write-only registers (open bus). The handler is also
    /// responsible for updating the relevant MDR latch.
    pub fn read_reg(&mut self, reg: u8, ctx: &PpuAccessCtx) -> u8 {
        match reg & 0x3F {
            // PPU1 MDR-only bytes: the write-only registers on the PPU1 side.
            // These registers are write-only, reading them surfaces
            // the PPU1 MDR latch.
            0x04 | 0x05 | 0x06 | 0x08 | 0x09 | 0x0A | 0x14 | 0x15 | 0x16 | 0x18 | 0x19 | 0x1A
            | 0x24 | 0x25 | 0x26 | 0x28 | 0x29 | 0x2A => self.ppu1_mdr,
            0x34 => self.read_2134(),
            0x35 => self.read_2135(),
            0x36 => self.read_2136(),
            // $2137 SLHV is read in the bus, not here, the H/V
            // latch lives in SystemRegs, so this branch is a
            // documented "should never be called" fallback to MDR.
            0x37 => ctx.mdr,
            0x38 => self.read_2138(ctx),
            0x39 => self.read_2139(ctx),
            0x3A => self.read_213a(ctx),
            0x3B => self.read_213b(ctx),
            // $213C / $213D / $213F live in SystemRegs; the bus
            // intercepts them before reaching this dispatcher.
            // Fall through to MDR if anyone calls them directly.
            0x3C | 0x3D | 0x3F => ctx.mdr,
            0x3E => self.read_213e(ctx),
            // Every other register in $00-$33 is write-only,
            // reads return MDR.
            _ => ctx.mdr,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppu_constructs_in_reset_state() {
        let ppu = Ppu::new();
        assert!(ppu.display_disable);
        assert_eq!(ppu.display_brightness, 0);
        assert_eq!(ppu.vram_incsize, 1);
        assert_eq!(ppu.vram_addr, 0);
        assert_eq!(ppu.bgmode, 0);
        assert_eq!(ppu.mosaic_size, 1);
    }

    #[test]
    fn ppu_memories_are_zeroed_on_construct() {
        let ppu = Ppu::new();
        assert!(ppu.vram().iter().all(|&w| w == 0));
        assert!(ppu.oam().iter().all(|&b| b == 0));
        assert!(ppu.cgram().iter().all(|&w| w == 0));
    }

    #[test]
    fn reset_clears_registers_but_preserves_memories() {
        let mut ppu = Ppu::new();
        // Stir some state.
        ppu.vram[0x1234] = 0xBEEF;
        ppu.oam[0x10] = 0xAA;
        ppu.cgram[5] = 0x7FFF;
        ppu.display_disable = false;
        ppu.bgmode = 7;

        ppu.reset();

        assert!(ppu.display_disable, "reset re-asserts forced blank");
        assert_eq!(ppu.bgmode, 0, "reset zeroes bgmode");
        // Memories are preserved across reset, the boot ROM
        // re-initialises them on its own.
        assert_eq!(ppu.vram[0x1234], 0xBEEF);
        assert_eq!(ppu.oam[0x10], 0xAA);
        assert_eq!(ppu.cgram[5], 0x7FFF);
    }

    #[test]
    fn write_only_registers_read_as_mdr() {
        let mut ppu = Ppu::new();
        let ctx = PpuAccessCtx {
            mdr: 0x5A,
            h_dot: 0,
            v_scanline: 0,
        };
        // $2100, $2101, $2102, $2103 are write-only and read as MDR.
        assert_eq!(ppu.read_reg(0x00, &ctx), 0x5A);
        assert_eq!(ppu.read_reg(0x01, &ctx), 0x5A);
        assert_eq!(ppu.read_reg(0x02, &ctx), 0x5A);
        assert_eq!(ppu.read_reg(0x03, &ctx), 0x5A);
    }

    #[test]
    fn registers_above_3f_dispatch_to_same_window() {
        // The bus passes `reg = addr - 0x2100`. Even if the upper
        // bits make it through, `& 0x3F` masks them down. $2100
        // (write-only INIDISP) reads as A-bus MDR; the same
        // alias at $2140 must resolve to the same value.
        let mut ppu = Ppu::new();
        let ctx = PpuAccessCtx {
            mdr: 0xC3,
            h_dot: 0,
            v_scanline: 0,
        };
        assert_eq!(ppu.read_reg(0x00, &ctx), 0xC3);
        assert_eq!(ppu.read_reg(0x40, &ctx), 0xC3);
    }

    #[test]
    fn writes_to_read_only_registers_are_dropped() {
        let mut ppu = Ppu::new();
        let ctx = PpuAccessCtx::ZERO;
        // $2134-$213F are read-only on real hardware; the
        // dispatcher should not panic on writes.
        for reg in 0x34u8..=0x3F {
            ppu.write_reg(reg, 0xAA, &ctx);
        }
        // STAT77 sprite flags must stay clear, writes should
        // not flip internal state.
        assert!(!ppu.sprite_time_over);
        assert!(!ppu.sprite_range_over);
    }

    #[test]
    fn in_active_scanline_respects_overscan_boundary() {
        let mut ppu = Ppu::new();
        // Default overscan = false -> active region is 0..224
        // (gate flips at 225).
        assert!(ppu.in_active_scanline(224));
        assert!(!ppu.in_active_scanline(225));
        // With overscan on the active region extends to 239
        // (gate flips at 240).
        ppu.overscan = true;
        assert!(ppu.in_active_scanline(225));
        assert!(ppu.in_active_scanline(239));
        assert!(!ppu.in_active_scanline(240));
    }

    #[test]
    fn tilemap_row_cache_matches_only_on_identical_key() {
        let mut cache = BgTilemapRowCache::EMPTY;
        // Empty slot never matches.
        assert!(!cache.matches(0, 0, 0, 0));

        let entries = [tile::TilemapEntry::default(); 64];
        cache.store(7, 0x0400, 1, 3, entries);
        // Exact key hits.
        assert!(cache.matches(7, 0x0400, 1, 3));
        // Any single differing field misses.
        assert!(!cache.matches(8, 0x0400, 1, 3), "generation differs");
        assert!(!cache.matches(7, 0x0800, 1, 3), "screen_addr differs");
        assert!(!cache.matches(7, 0x0400, 2, 3), "screen_size differs");
        assert!(!cache.matches(7, 0x0400, 1, 4), "ty differs");
    }

    #[test]
    fn tilemap_row_cache_store_keeps_entries() {
        let mut cache = BgTilemapRowCache::EMPTY;
        let mut entries = [tile::TilemapEntry::default(); 64];
        entries[5] = tile::TilemapEntry::decode(0x1234);
        cache.store(1, 0, 0, 0, entries);
        assert_eq!(cache.entries()[5], tile::TilemapEntry::decode(0x1234));
    }

    #[test]
    fn reset_invalidates_tilemap_caches_and_rewinds_generation() {
        let mut ppu = Ppu::new();
        ppu.bg_tilemap_row_cache[0].store(9, 0x0400, 1, 2, [tile::TilemapEntry::default(); 64]);
        ppu.vram_generation = 42;

        ppu.reset();

        assert!(!ppu.bg_tilemap_row_cache[0].matches(9, 0x0400, 1, 2));
        assert_eq!(ppu.vram_generation, 0);
    }

    #[test]
    fn invalidate_tilemap_caches_forces_miss() {
        let mut ppu = Ppu::new();
        ppu.bg_tilemap_row_cache[1].store(0, 0, 0, 0, [tile::TilemapEntry::default(); 64]);
        assert!(ppu.bg_tilemap_row_cache[1].matches(0, 0, 0, 0));
        ppu.invalidate_tilemap_caches();
        assert!(!ppu.bg_tilemap_row_cache[1].matches(0, 0, 0, 0));
    }

    #[test]
    fn vram_write_byte_bumps_generation_when_gate_open() {
        let mut ppu = Ppu::new();
        // display_disable defaults to true -> gate open at any scanline.
        let before = ppu.vram_generation;
        ppu.vram_write_byte(0, 0xAB, 0);
        assert_eq!(ppu.vram_generation, before.wrapping_add(1));
    }

    #[test]
    fn vram_write_byte_dropped_during_active_does_not_bump_generation() {
        let mut ppu = Ppu::new();
        ppu.display_disable = false; // active-scan gate now applies
        let before = ppu.vram_generation;
        // Scanline 50 is inside the active region -> write drops.
        ppu.vram_write_byte(0, 0xAB, 50);
        assert_eq!(ppu.vram_generation, before, "dropped write must not bump");
    }
}
