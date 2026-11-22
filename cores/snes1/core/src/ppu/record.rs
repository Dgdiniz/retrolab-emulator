//! What the PPU records while frames run, for a capture to read: the
//! settings each line of the picture was drawn with, and the changes to
//! video memory made while the frame was drawn, so that the frame can be
//! drawn again as it was drawn the first time.
//!
//! Nothing here is part of a savestate, and nothing records until the host
//! asks for it with a [`CaptureRecording`]. With every recorder off, the
//! one hook is a test of one flag as each line is drawn; the memory ports
//! carry none (the live recorders, in [`super::writes`], are reached through
//! a path the bus chooses before the port).
//!
//! ## The line table
//!
//! As each line is drawn, after the test that drops the lines past the
//! active display, [`LineState`] takes every register the renderer reads,
//! as the PPU stores it. Line 1 starts a new table.
//!
//! The video memory a line read is not copied line by line. The table keeps
//! one copy of VRAM, CGRAM and OAM as the last line drawn read them, and as
//! each line is drawn it compares the memory with that copy (VRAM only when
//! the VRAM generation moved, which every write that lands does): each byte
//! of VRAM, colour of CGRAM or byte of OAM that changed goes into a log of
//! its memory, with the value it replaced and the number of lines drawn
//! before it changed, and the copy takes the new value. Undoing the log,
//! newest first, puts the memory back as it was for the first line;
//! redoing it in order, line by line, gives each line the memory it read.
//! Each undo and each redo swaps the value in memory with the one in the
//! log, so the two passes leave the memory and the log exactly as they
//! found them. Several changes to one place between two lines are one change,
//! which is all a line can see.
//!
//! What changed after the last line, in the vertical blank and up to the
//! next frame's line 1, is compared and logged too when the frame is drawn
//! again, and counted when its palettes are read: a frame drawn again after
//! it ran whole still reads the memory its lines read. A debugger's edit of video memory
//! is no change of the frame's: the copy takes it as it is, so it is never
//! logged and shows in what is drawn again.
//!
//! A log that fills up stops, and is emptied, so it never puts back part
//! of what changed: the table then says its memory is not exact, and the
//! frame is drawn again from the memory as it stands.
//!
//! The table reproduces the picture this emulator drew, not the one the
//! hardware would: a line is drawn whole at one dot, so a change in the
//! middle of a line shows from the next one, as it did the first time.

use retro_core::capture::{
    arm_recorder, distinct_palettes, CaptureRecording, LinePalettes, TableColumns,
};
use retro_core::video::LineStateTable;

use super::writes::LiveRecorders;
use super::{Ppu, WindowEnable};

impl TableColumns for WindowEnable {
    fn push_names(name: &str, names: &mut Vec<String>) {
        for part in ["w1_enable", "w1_invert", "w2_enable", "w2_invert"] {
            names.push(format!("{name}.{part}"));
        }
    }
    fn push_values(&self, values: &mut Vec<u32>) {
        values.extend([
            u32::from(self.w1_enable),
            u32::from(self.w1_invert),
            u32::from(self.w2_enable),
            u32::from(self.w2_invert),
        ]);
    }
}

retro_core::line_state! {
    /// The settings one line of the picture was drawn with: every register
    /// the renderer reads, as the PPU stores it, taken when the line is
    /// drawn, and put back and written as columns from this one list. A
    /// register the renderer reads must be listed here, or a frame drawn
    /// again from its lines would draw it from the register as it stands.
    ///
    /// Left out, because drawing the lines in order from line 1 builds them
    /// again: the frame-wide hires and interlace state, the rows vertical
    /// mosaic repeats, and the caches of decoded VRAM. Left out too, as the
    /// host's and not the game's: the debugger's layer mask, the single
    /// layer a layer render draws and the overscan crop the host shows.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
    pub struct LineState for Ppu {
        /// Forced blank (INIDISP bit 7).
        display_disable: bool = display_disable,
        /// Master brightness `0..=15` (INIDISP bits 0-3).
        display_brightness: u8 = display_brightness,
        /// The sprite size pair selector (OBSEL bits 5-7).
        obj_base_size: u8 = obj_base_size,
        /// The gap between the two sprite character tables (OBSEL bits 3-4).
        obj_name_select: u8 = obj_name_select,
        /// Byte address of the first sprite character table (OBSEL bits 0-2).
        obj_tiledata_addr: u32 = obj_tiledata_addr,
        /// The sprite the evaluator starts from (the priority rotation of
        /// OAMADDH bit 7).
        oam_first_sprite: u8 = oam_first_sprite,
        /// Background mode `0..=7` (BGMODE bits 0-2).
        bgmode: u8 = bgmode,
        /// Mode 1's BG3 priority lift (BGMODE bit 3).
        bg3_priority: bool = bg3_priority,
        /// 16x16 tiles, per background (BGMODE bits 4-7).
        bg_tile_size: [bool; 4] = bg_tile_size,
        /// Mosaic block size in pixels, `1..=16` (MOSAIC bits 4-7, plus 1).
        mosaic_size: u8 = mosaic_size,
        /// Mosaic on, per background (MOSAIC bits 0-3).
        bg_mosaic_enable: [bool; 4] = bg_mosaic_enable,
        /// Tilemap byte address, per background (BGnSC).
        bg_screen_addr: [u32; 4] = bg_screen_addr,
        /// Tilemap size `0..=3`, per background (BGnSC bits 0-1).
        bg_screen_size: [u8; 4] = bg_screen_size,
        /// Character byte address, per background (BG12NBA, BG34NBA).
        bg_tiledata_addr: [u32; 4] = bg_tiledata_addr,
        /// Horizontal scroll, per background, as written (BGnHOFS).
        bg_hoffset: [u16; 4] = bg_hoffset,
        /// Vertical scroll, per background, as written (BGnVOFS).
        bg_voffset: [u16; 4] = bg_voffset,
        /// Mode 7 screen over (M7SEL bits 6-7).
        mode7_repeat: u8 = mode7_repeat,
        /// Mode 7 vertical flip (M7SEL bit 1).
        mode7_vflip: bool = mode7_vflip,
        /// Mode 7 horizontal flip (M7SEL bit 0).
        mode7_hflip: bool = mode7_hflip,
        /// Mode 7 matrix A, signed 8.8 fixed point as written (M7A).
        mode7_matrix_a: u16 = m7a,
        /// Mode 7 matrix B (M7B).
        mode7_matrix_b: u16 = m7b,
        /// Mode 7 matrix C (M7C).
        mode7_matrix_c: u16 = m7c,
        /// Mode 7 matrix D (M7D).
        mode7_matrix_d: u16 = m7d,
        /// Mode 7 centre X, as written (M7X).
        mode7_centre_x: u16 = m7x,
        /// Mode 7 centre Y, as written (M7Y).
        mode7_centre_y: u16 = m7y,
        /// Mode 7 horizontal scroll, as written (BG1HOFS through Mode 7's
        /// latch).
        mode7_hoffset: u16 = mode7_hoffset,
        /// Mode 7 vertical scroll, as written.
        mode7_voffset: u16 = mode7_voffset,
        /// Mode 7's second background (SETINI bit 6).
        mode7_extbg: bool = mode7_extbg,
        /// Window enables and inversions per layer: BG1, BG2, BG3, BG4,
        /// sprites, colour (W12SEL, W34SEL, WOBJSEL).
        window_enable: [WindowEnable; 6] = window_enable,
        /// Left edge of window 1 (WH0).
        window_one_left: u8 = window_one_left,
        /// Right edge of window 1 (WH1).
        window_one_right: u8 = window_one_right,
        /// Left edge of window 2 (WH2).
        window_two_left: u8 = window_two_left,
        /// Right edge of window 2 (WH3).
        window_two_right: u8 = window_two_right,
        /// How the two windows combine, per layer in the same order (WBGLOG,
        /// WOBJLOG).
        window_combine: [u8; 6] = window_combine,
        /// Backgrounds on the main screen (TM bits 0-3).
        bg_main_enable: [bool; 4] = bg_main_enable,
        /// Sprites on the main screen (TM bit 4).
        obj_main_enable: bool = obj_main_enable,
        /// Backgrounds on the sub screen (TS bits 0-3).
        bg_sub_enable: [bool; 4] = bg_sub_enable,
        /// Sprites on the sub screen (TS bit 4).
        obj_sub_enable: bool = obj_sub_enable,
        /// The windows clip these backgrounds on the main screen (TMW bits
        /// 0-3).
        bg_main_window_disable: [bool; 4] = bg_main_window_disable,
        /// The windows clip the sprites on the main screen (TMW bit 4).
        obj_main_window_disable: bool = obj_main_window_disable,
        /// The windows clip these backgrounds on the sub screen (TSW bits 0-3).
        bg_sub_window_disable: [bool; 4] = bg_sub_window_disable,
        /// The windows clip the sprites on the sub screen (TSW bit 4).
        obj_sub_window_disable: bool = obj_sub_window_disable,
        /// Where the main screen shows its pixels (CGWSEL bits 6-7).
        col_main_mask: u8 = col_main_mask,
        /// Where colour math may run (CGWSEL bits 4-5).
        col_sub_mask: u8 = col_sub_mask,
        /// Colour math takes the sub screen instead of the fixed colour
        /// (CGWSEL bit 1).
        addsub_mode: bool = addsub_mode,
        /// Direct colour for the 256-colour backgrounds (CGWSEL bit 0).
        direct_color: bool = direct_color,
        /// Colour math subtracts (CGADSUB bit 7).
        color_math_subtract: bool = color_math_subtract,
        /// Colour math halves its result (CGADSUB bit 6).
        color_math_halve: bool = color_math_halve,
        /// Colour math on the backdrop (CGADSUB bit 5).
        color_math_back_enable: bool = color_math_back_enable,
        /// Colour math on sprites (CGADSUB bit 4).
        color_math_oam_enable: bool = color_math_oam_enable,
        /// Colour math on each background (CGADSUB bits 0-3).
        color_math_bg_enable: [bool; 4] = color_math_bg_enable,
        /// The fixed colour, BGR555 (COLDATA).
        fixed_color: u16 = fixed_color,
        /// Pseudo-hires (SETINI bit 3).
        pseudo_hires: bool = pseudo_hires,
        /// Overscan, 239 visible lines (SETINI bit 2).
        overscan: bool = overscan,
        /// Sprite interlace (SETINI bit 1).
        obj_interlace: bool = obj_interlace,
        /// Screen interlace (SETINI bit 0), latched for the frame at line 1.
        interlace: bool = interlace,
        /// The interlace field the line was drawn in.
        field: bool = field,
    }
}

/// One logged change to video memory: where it is, the value it replaced,
/// and the number of lines drawn before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemoryChange {
    /// How many lines of the table were drawn before the change: the index
    /// of the first line that read the new value.
    pub(crate) before_line: u16,
    /// The VRAM byte, the CGRAM colour or the OAM byte that changed.
    pub(crate) address: u16,
    /// The value the change replaced; while memory is wound back, the value
    /// the change put there.
    pub(crate) value: u16,
}

/// The changes one video memory took while the table was kept, oldest
/// first.
#[derive(Debug)]
pub(crate) struct MemoryLog {
    changes: Vec<MemoryChange>,
    /// The most changes the log keeps for a frame.
    cap: usize,
    /// The log filled up this frame: it was emptied and stopped.
    overflowed: bool,
}

impl MemoryLog {
    fn new(cap: usize) -> Self {
        Self {
            changes: Vec::new(),
            cap,
            overflowed: false,
        }
    }

    fn clear(&mut self) {
        self.changes.clear();
        self.overflowed = false;
    }

    /// Log a change that replaced `value` at `address`, after `before_line`
    /// lines were drawn.
    fn log(&mut self, before_line: usize, address: u16, value: u16) {
        if self.overflowed {
            return;
        }
        if self.changes.len() == self.cap {
            // Part of a log would put back part of what changed: empty it,
            // and the frame is drawn again from the memory as it stands.
            self.changes.clear();
            self.overflowed = true;
            return;
        }
        self.changes.push(MemoryChange {
            before_line: before_line as u16,
            address,
            value,
        });
    }
}

/// The most VRAM byte changes logged for a frame (768 KiB of log): two whole
/// VRAMs, more than the DMA can move in a frame.
const VRAM_LOG_CAP: usize = 131_072;
/// The most CGRAM colour changes logged for a frame: sixteen whole palettes.
const PALETTE_LOG_CAP: usize = 4_096;
/// The most OAM byte changes logged for a frame: some fifteen whole tables.
const SPRITE_TABLE_LOG_CAP: usize = 8_192;
/// The most lines a frame draws: 239 with overscan.
const MOST_LINES: usize = 240;

/// The three video memories, borrowed together while a log is wound back
/// or forward.
pub(crate) struct VideoMemory<'a> {
    pub(crate) vram: &'a mut [u16; 0x8000],
    pub(crate) cgram: &'a mut [u16; 0x100],
    pub(crate) oam: &'a mut [u8; 0x220],
}

/// The three video memories as they stand, read while the table compares
/// them with its copy, and the VRAM generation that says whether VRAM moved.
pub(crate) struct VideoMemoryNow<'a> {
    pub(crate) vram: &'a [u16; 0x8000],
    pub(crate) cgram: &'a [u16; 0x100],
    pub(crate) oam: &'a [u8; 0x220],
    pub(crate) vram_generation: u64,
}

/// The table's copy of the video memory as the last line drawn read it,
/// which the logs are worked out from.
#[derive(Debug)]
struct MemoryCopy {
    vram: Box<[u16; 0x8000]>,
    cgram: [u16; 0x100],
    oam: [u8; 0x220],
    /// The VRAM generation the VRAM copy matches.
    vram_generation: u64,
    /// The copy holds the memory as some line read it; until then nothing
    /// can be compared with it.
    taken: bool,
}

impl MemoryCopy {
    fn new() -> Self {
        Self {
            vram: vec![0u16; 0x8000]
                .into_boxed_slice()
                .try_into()
                .expect("vram copy alloc 32K words"),
            cgram: [0; 0x100],
            oam: [0; 0x220],
            vram_generation: 0,
            taken: false,
        }
    }

    /// Take the memory as it stands, logging nothing.
    fn take(&mut self, now: &VideoMemoryNow) {
        if !self.taken || self.vram_generation != now.vram_generation {
            self.vram.copy_from_slice(&now.vram[..]);
            self.vram_generation = now.vram_generation;
        }
        self.cgram = *now.cgram;
        self.oam = *now.oam;
        self.taken = true;
    }
}

/// VRAM words compared at once while looking for what changed.
const VRAM_CHUNK: usize = 64;

/// Swap a VRAM byte with the low byte of `value`.
fn swap_vram_byte(vram: &mut [u16; 0x8000], address: u16, value: &mut u16) {
    let word = &mut vram[usize::from(address >> 1)];
    let mut bytes = word.to_le_bytes();
    let slot = &mut bytes[usize::from(address & 1)];
    let held = *slot;
    *slot = *value as u8;
    *value = u16::from(held);
    *word = u16::from_le_bytes(bytes);
}

/// Swap a CGRAM colour with `value`.
fn swap_colour(cgram: &mut [u16; 0x100], index: u16, value: &mut u16) {
    core::mem::swap(&mut cgram[usize::from(index & 0xff)], value);
}

/// Swap an OAM byte with the low byte of `value`.
fn swap_sprite_byte(oam: &mut [u8; 0x220], address: u16, value: &mut u16) {
    let byte = &mut oam[usize::from(address) % oam.len()];
    let held = *byte;
    *byte = *value as u8;
    *value = u16::from(held);
}

/// How far each log has been redone while a frame is drawn again.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ReplayCursor {
    vram: usize,
    palette: usize,
    sprite_table: usize,
}

/// The line table: the settings of every line of the frame drawn last, and
/// the changes to video memory made while it was kept.
#[derive(Debug)]
pub(crate) struct LineRecorder {
    /// The number of the frame the lines belong to.
    frame: u64,
    /// The table begins with the frame's line 1.
    from_first_line: bool,
    /// The frame's picture is finished.
    finished: bool,
    /// The debugger wrote a PPU register since line 1: a repaint then
    /// draws from the registers as they stand, which show the edit.
    registers_edited: bool,
    /// The debugger wrote video memory straight in since line 1.
    memory_edited: bool,
    /// The overscan bit the picture was finished with.
    finished_overscan: bool,
    /// Rows of the finished picture each line gives.
    rows_per_line: u32,
    /// The line in the finished picture's first row.
    first_row_line: u32,
    /// Each line drawn, with its settings, in the order drawn.
    lines: Vec<(u16, LineState)>,
    vram: MemoryLog,
    palette: MemoryLog,
    sprite_table: MemoryLog,
    /// The memory as the last line drawn read it.
    copy: Box<MemoryCopy>,
}

impl LineRecorder {
    fn new(frame: u64) -> Self {
        Self {
            frame,
            from_first_line: false,
            finished: false,
            registers_edited: false,
            memory_edited: false,
            finished_overscan: false,
            rows_per_line: 1,
            first_row_line: 1,
            lines: Vec::with_capacity(MOST_LINES),
            vram: MemoryLog::new(VRAM_LOG_CAP),
            palette: MemoryLog::new(PALETTE_LOG_CAP),
            sprite_table: MemoryLog::new(SPRITE_TABLE_LOG_CAP),
            copy: Box::new(MemoryCopy::new()),
        }
    }

    /// Forget everything, the copy of the memory too (a reset or a load
    /// may have replaced it): the next lines recorded start no frame of
    /// their own until line 1 comes.
    fn clear(&mut self) {
        self.forget_lines();
        self.copy.taken = false;
    }

    /// Forget the lines and the logs, keeping the copy of the memory.
    fn forget_lines(&mut self) {
        self.from_first_line = false;
        self.finished = false;
        self.registers_edited = false;
        self.memory_edited = false;
        self.lines.clear();
        self.vram.clear();
        self.palette.clear();
        self.sprite_table.clear();
    }

    /// Record a line drawn with `state`, video memory standing as `now`;
    /// line 1 starts a new table for frame `frame`, from the memory as it
    /// stands.
    fn record(&mut self, line: u16, state: LineState, frame: u64, now: &VideoMemoryNow) {
        if line == 1 {
            self.forget_lines();
            self.from_first_line = true;
            self.frame = frame;
            self.copy.take(now);
        } else {
            if self.lines.is_empty() {
                self.frame = frame;
            }
            self.catch_up(now);
        }
        self.lines.push((line, state));
    }

    /// Log every change between the copy and the memory as it stands, as
    /// made before the next line, and bring the copy up to date. With no
    /// copy taken yet (a table started in the middle of a frame) the copy
    /// is taken and nothing is logged.
    pub(crate) fn catch_up(&mut self, now: &VideoMemoryNow) {
        if !self.copy.taken {
            self.copy.take(now);
            return;
        }
        let before = self.lines.len();
        let copy = &mut *self.copy;
        if copy.vram_generation != now.vram_generation {
            // VRAM's 32768 words are a whole number of chunks: nothing is
            // left over.
            let (current_chunks, _) = now.vram.as_chunks::<VRAM_CHUNK>();
            let (kept_chunks, _) = copy.vram.as_chunks_mut::<VRAM_CHUNK>();
            let chunks = current_chunks.iter().zip(kept_chunks.iter_mut());
            for (chunk, (current, kept)) in chunks.enumerate() {
                if current == kept {
                    continue;
                }
                for (offset, (&word, held)) in current.iter().zip(kept.iter_mut()).enumerate() {
                    if word == *held {
                        continue;
                    }
                    let address = ((chunk * VRAM_CHUNK + offset) * 2) as u16;
                    let [new_low, new_high] = word.to_le_bytes();
                    let [old_low, old_high] = held.to_le_bytes();
                    if new_low != old_low {
                        self.vram.log(before, address, u16::from(old_low));
                    }
                    if new_high != old_high {
                        self.vram.log(before, address + 1, u16::from(old_high));
                    }
                    *held = word;
                }
            }
            copy.vram_generation = now.vram_generation;
        }
        if copy.cgram != *now.cgram {
            for (index, (&colour, held)) in now.cgram.iter().zip(copy.cgram.iter_mut()).enumerate()
            {
                if colour != *held {
                    self.palette.log(before, index as u16, *held);
                    *held = colour;
                }
            }
        }
        if copy.oam != *now.oam {
            for (index, (&byte, held)) in now.oam.iter().zip(copy.oam.iter_mut()).enumerate() {
                if byte != *held {
                    self.sprite_table
                        .log(before, index as u16, u16::from(*held));
                    *held = byte;
                }
            }
        }
    }

    /// The memory was put back as the copy holds it after a frame was drawn
    /// again, while the VRAM generation moved on: the copy still matches.
    pub(crate) fn memory_put_back(&mut self, vram_generation: u64) {
        if self.copy.taken {
            self.copy.vram_generation = vram_generation;
        }
    }

    /// The table holds a whole frame: from its line 1 to its finished
    /// picture.
    pub(crate) fn holds_whole_frame(&self) -> bool {
        self.from_first_line && self.finished
    }

    /// A repaint or a layer render draws from this table: it holds a whole
    /// frame and no register was edited since.
    pub(crate) fn redraws_frame(&self) -> bool {
        self.holds_whole_frame() && !self.registers_edited
    }

    /// Video memory can be put back as each line read it: no log filled up
    /// and the debugger edited none of it.
    fn memory_exact(&self) -> bool {
        !(self.vram.overflowed
            || self.palette.overflowed
            || self.sprite_table.overflowed
            || self.memory_edited)
    }

    /// The lines recorded, with their settings.
    pub(crate) fn lines(&self) -> &[(u16, LineState)] {
        &self.lines
    }

    /// The overscan bit the picture was finished with.
    pub(crate) fn finished_overscan(&self) -> bool {
        self.finished_overscan
    }

    /// Undo every logged change, newest first, leaving the memory as it was
    /// for the first line recorded. Returns whether VRAM moved, so the
    /// caches of decoded VRAM are dropped.
    pub(crate) fn rewind(&mut self, memory: &mut VideoMemory) -> bool {
        for change in self.vram.changes.iter_mut().rev() {
            swap_vram_byte(memory.vram, change.address, &mut change.value);
        }
        for change in self.palette.changes.iter_mut().rev() {
            swap_colour(memory.cgram, change.address, &mut change.value);
        }
        for change in self.sprite_table.changes.iter_mut().rev() {
            swap_sprite_byte(memory.oam, change.address, &mut change.value);
        }
        !self.vram.changes.is_empty()
    }

    /// Redo, in order, every change not redone yet that came before line
    /// `index` of the table was drawn (`index` past the last line redoes
    /// the rest). Returns whether VRAM moved.
    pub(crate) fn replay_through(
        &mut self,
        index: usize,
        cursor: &mut ReplayCursor,
        memory: &mut VideoMemory,
    ) -> bool {
        let vram_moved = replay_log(&mut self.vram, index, &mut cursor.vram, |address, value| {
            swap_vram_byte(memory.vram, address, value)
        });
        replay_log(
            &mut self.palette,
            index,
            &mut cursor.palette,
            |address, value| swap_colour(memory.cgram, address, value),
        );
        replay_log(
            &mut self.sprite_table,
            index,
            &mut cursor.sprite_table,
            |address, value| swap_sprite_byte(memory.oam, address, value),
        );
        vram_moved
    }
}

impl LineRecorder {
    /// OAM as each line of the table read it, worked out on a copy (see
    /// [`ReplayedMemory`]). `now` is OAM as it stands, the start when the
    /// table never took its copy.
    pub(crate) fn sprite_table_by_line(&self, now: &[u8; 0x220]) -> ReplayedMemory<[u8; 0x220]> {
        let start = if self.copy.taken { &self.copy.oam } else { now };
        ReplayedMemory::new(
            Box::new(*start),
            self.sprite_table.changes.clone(),
            swap_sprite_byte,
        )
    }

    /// VRAM as each line of the table read it, worked out on a copy (see
    /// [`ReplayedMemory`]). `now` is VRAM as it stands, the start when the
    /// table never took its copy.
    pub(crate) fn vram_by_line(&self, now: &[u16; 0x8000]) -> ReplayedMemory<[u16; 0x8000]> {
        let start = if self.copy.taken {
            &*self.copy.vram
        } else {
            now
        };
        let mut memory: Box<[u16; 0x8000]> = vec![0u16; 0x8000]
            .into_boxed_slice()
            .try_into()
            .expect("vram copy alloc 32K words");
        memory.copy_from_slice(start);
        ReplayedMemory::new(memory, self.vram.changes.clone(), swap_vram_byte)
    }
}

/// One video memory as each line of a line table read it, worked out on a
/// copy, so the PPU's own memory is not touched and a reader that walks the
/// frame line by line (its sprite lists, its offset-per-tile columns) needs
/// no more than a shared borrow.
///
/// It starts from the table's copy of the memory as the last line read it
/// and undoes the memory's log, newest first; [`Self::advance_to`] then
/// redoes it line by line, as a frame drawn again does in place. A log that
/// filled up was emptied, so such a memory stays as the last line read it.
pub(crate) struct ReplayedMemory<M: 'static> {
    memory: Box<M>,
    changes: Vec<MemoryChange>,
    next: usize,
    swap: fn(&mut M, u16, &mut u16),
}

impl<M: 'static> ReplayedMemory<M> {
    /// Wind `memory` back through `changes` with `swap`, newest first.
    fn new(
        mut memory: Box<M>,
        mut changes: Vec<MemoryChange>,
        swap: fn(&mut M, u16, &mut u16),
    ) -> Self {
        for change in changes.iter_mut().rev() {
            swap(&mut memory, change.address, &mut change.value);
        }
        Self {
            memory,
            changes,
            next: 0,
            swap,
        }
    }

    /// A memory as it stands, the same for every line: the frame read from
    /// the registers and the memory as they stand.
    pub(crate) fn as_it_stands(memory: Box<M>, swap: fn(&mut M, u16, &mut u16)) -> Self {
        Self::new(memory, Vec::new(), swap)
    }

    /// Redo every change made before line `index` of the table was drawn,
    /// so the memory is as that line read it. Lines are visited in order.
    pub(crate) fn advance_to(&mut self, index: usize) {
        while let Some(change) = self.changes.get_mut(self.next) {
            if usize::from(change.before_line) > index {
                break;
            }
            (self.swap)(&mut self.memory, change.address, &mut change.value);
            self.next += 1;
        }
    }

    /// The memory as the line last advanced to read it.
    pub(crate) fn memory(&self) -> &M {
        &self.memory
    }
}

/// OAM as it stands, as a [`ReplayedMemory`] with nothing to replay.
pub(crate) fn sprite_table_as_it_stands(oam: &[u8; 0x220]) -> ReplayedMemory<[u8; 0x220]> {
    ReplayedMemory::as_it_stands(Box::new(*oam), swap_sprite_byte)
}

/// VRAM as it stands, as a [`ReplayedMemory`] with nothing to replay.
pub(crate) fn vram_as_it_stands(vram: &[u16; 0x8000]) -> ReplayedMemory<[u16; 0x8000]> {
    let mut memory: Box<[u16; 0x8000]> = vec![0u16; 0x8000]
        .into_boxed_slice()
        .try_into()
        .expect("vram copy alloc 32K words");
    memory.copy_from_slice(vram);
    ReplayedMemory::as_it_stands(memory, swap_vram_byte)
}

/// Redo the changes of `log` from `*next` on that came before line `index`
/// was drawn, swapping each into memory with `swap`. Returns whether any
/// was redone.
fn replay_log(
    log: &mut MemoryLog,
    index: usize,
    next: &mut usize,
    mut swap: impl FnMut(u16, &mut u16),
) -> bool {
    let start = *next;
    while let Some(change) = log.changes.get_mut(*next) {
        if usize::from(change.before_line) > index {
            break;
        }
        swap(change.address, &mut change.value);
        *next += 1;
    }
    *next != start
}

/// The recorders the host armed, kept beside the PPU's state and never in a
/// savestate.
#[derive(Debug)]
pub(crate) struct Recorders {
    /// What the host asked to record.
    recording: CaptureRecording,
    /// The line table, while the host asks for it.
    pub(crate) lines: Option<Box<LineRecorder>>,
    /// The number of the frame the lines drawn next belong to: the frames
    /// finished, plus one.
    frame: u64,
    /// A live recorder or a debugger watch is armed: the bus then sends
    /// the PPU's writes through the path that notes what the ports took
    /// (`Ppu::notes_port_writes`). The same as `live.is_some()`, kept as a
    /// flag for a one-load test.
    pub(crate) on: bool,
    /// A live recorder is armed: the writers name themselves.
    pub(crate) recording_writes: bool,
    /// The live recorders and the debugger's watch, while any is armed.
    pub(crate) live: Option<Box<LiveRecorders>>,
}

impl Default for Recorders {
    fn default() -> Self {
        Self {
            recording: CaptureRecording::default(),
            lines: None,
            // No frame finished yet: the lines drawn next belong to frame 1.
            frame: 1,
            on: false,
            recording_writes: false,
            live: None,
        }
    }
}

impl Recorders {
    /// The number of the frame being built, which every recorded write
    /// carries.
    pub(crate) fn frame(&self) -> u64 {
        self.frame
    }
}

impl Ppu {
    /// Arm or disarm the recorders ([`CaptureRecording`]). Turning the line
    /// table on mid-frame keeps the lines still to come, but such a table
    /// does not hold its frame from line 1 and is not drawn from; the next
    /// frame's is. Turning it off drops it. The two live recorders start
    /// empty when turned on and are dropped when turned off.
    pub fn set_capture_recording(&mut self, recording: CaptureRecording) {
        self.recorders.recording = recording;
        let frame = self.recorders.frame;
        arm_recorder(
            &mut self.recorders.lines,
            recording.wants_line_table(),
            || Box::new(LineRecorder::new(frame)),
        );
        self.arm_write_recorders(recording);
    }

    /// What the recorders were asked to record.
    pub fn capture_recording(&self) -> CaptureRecording {
        self.recorders.recording
    }

    /// The number of the frame the lines drawn next belong to: the frames
    /// finished, plus one. The emulator sets it as each frame ends.
    pub fn set_recording_frame(&mut self, frame: u64) {
        self.recorders.frame = frame;
    }

    /// The frame's picture is finished: its line table, when one is kept,
    /// now holds the whole frame. The emulator calls this as it finishes
    /// each frame's picture; a host that drives the PPU line by line calls
    /// it after the frame's last line.
    pub fn note_picture_finished(&mut self) {
        let overscan = self.overscan;
        let rows_per_line = if self.frame_hires { 2 } else { 1 };
        let first_row_line = self.first_row_line();
        if let Some(table) = self.recorders.lines.as_deref_mut() {
            table.finished = true;
            table.finished_overscan = overscan;
            table.rows_per_line = rows_per_line;
            table.first_row_line = first_row_line;
        }
    }

    /// Forget the frame the line table holds (a reset, a savestate load):
    /// the recorders stay armed.
    pub(crate) fn drop_recorded_frame(&mut self) {
        if let Some(table) = self.recorders.lines.as_deref_mut() {
            table.clear();
        }
    }

    /// The debugger wrote a PPU register: a repaint draws from the
    /// registers as they stand until the next frame's line 1.
    pub(crate) fn note_registers_edited(&mut self) {
        if let Some(table) = self.recorders.lines.as_deref_mut() {
            table.registers_edited = true;
        }
    }

    /// Video memory as it stands, for the line table to compare.
    pub(crate) fn video_memory_now(&self) -> VideoMemoryNow<'_> {
        VideoMemoryNow {
            vram: &self.vram,
            cgram: &self.cgram,
            oam: &self.oam,
            vram_generation: self.vram_generation,
        }
    }

    /// The debugger is about to write video memory straight in, through no
    /// port: the line table logs what the frame changed since its last
    /// line first, so the edit is not taken for a change of the frame's.
    pub(crate) fn before_video_memory_edit(&mut self) {
        let now = VideoMemoryNow {
            vram: &self.vram,
            cgram: &self.cgram,
            oam: &self.oam,
            vram_generation: self.vram_generation,
        };
        if let Some(table) = self.recorders.lines.as_deref_mut() {
            table.catch_up(&now);
        }
    }

    /// The debugger wrote video memory straight in: the table's copy takes
    /// the edit as it is, so the edit is never logged and shows in a frame
    /// drawn again, and the table's memory is no longer the memory each
    /// line read.
    pub(crate) fn after_video_memory_edit(&mut self) {
        let now = VideoMemoryNow {
            vram: &self.vram,
            cgram: &self.cgram,
            oam: &self.oam,
            vram_generation: self.vram_generation,
        };
        if let Some(table) = self.recorders.lines.as_deref_mut() {
            if table.copy.taken {
                table.copy.take(&now);
            }
            table.memory_edited = true;
        }
    }

    /// Whether a repaint or a layer render made now draws from the line
    /// table, each line with its own settings and the video memory it
    /// read: the table holds the frame from its line 1 to its finished
    /// picture, and no register was edited by the debugger since. When it
    /// does not, they draw from the registers as they stand.
    pub fn draws_from_line_states(&self) -> bool {
        self.recorders
            .lines
            .as_deref()
            .is_some_and(LineRecorder::redraws_frame)
    }

    /// The line drawn into the picture's first row: 8 when the overscan
    /// lines above the TV window are cropped, else 1.
    pub(super) fn first_row_line(&self) -> u32 {
        if self.overscan && !self.show_overscan {
            8
        } else {
            1
        }
    }

    /// The three video memories, borrowed together.
    pub(crate) fn video_memory(&mut self) -> VideoMemory<'_> {
        VideoMemory {
            vram: &mut self.vram,
            cgram: &mut self.cgram,
            oam: &mut self.oam,
        }
    }

    /// Record the line about to be drawn, when the line table is kept.
    #[cold]
    #[inline(never)]
    pub(crate) fn record_line_state(&mut self, line: u16) {
        let state = LineState::capture(self);
        let frame = self.recorders.frame;
        let now = VideoMemoryNow {
            vram: &self.vram,
            cgram: &self.cgram,
            oam: &self.oam,
            vram_generation: self.vram_generation,
        };
        if let Some(table) = self.recorders.lines.as_deref_mut() {
            table.record(line, state, frame, &now);
        }
    }

    /// The line table as columns: `line`, then every setting of
    /// [`LineState`], one row per line drawn. `None` when no table is kept
    /// or no line was drawn since it was started.
    pub(crate) fn line_state_table(&self) -> Option<LineStateTable> {
        let table = self.recorders.lines.as_deref()?;
        if table.lines.is_empty() {
            return None;
        }
        let mut columns = vec!["line".to_string()];
        columns.extend(LineState::column_names());
        let mut values = Vec::with_capacity(columns.len() * table.lines.len());
        for (line, state) in &table.lines {
            values.push(u32::from(*line));
            state.push_row(&mut values);
        }
        let (rows_per_line, first_row_line) = if table.finished {
            (table.rows_per_line, table.first_row_line)
        } else {
            (if self.frame_hires { 2 } else { 1 }, self.first_row_line())
        };
        Some(LineStateTable {
            frame: table.frame,
            from_first_line: table.from_first_line,
            complete: table.holds_whole_frame(),
            memory_exact: table.memory_exact(),
            rows_per_line,
            first_row_line,
            columns,
            values,
        })
    }

    /// The palettes the lines of the table were drawn with: CGRAM wound
    /// back to the first line and moved forward as the frame moved it, a
    /// palette pushed once for each distinct CGRAM a line read. `None` when
    /// no table is kept or no line was drawn since it was started.
    pub fn line_palettes(&self) -> Option<LinePalettes> {
        let table = self.recorders.lines.as_deref()?;
        if table.lines.is_empty() {
            return None;
        }
        let mut cgram = *self.cgram;
        let mut log = MemoryLog {
            changes: table.palette.changes.clone(),
            cap: usize::MAX,
            overflowed: false,
        };
        // What changed since the last line drawn, not compared yet.
        if table.copy.taken {
            let before = table.lines.len();
            for (index, (&colour, &held)) in cgram.iter().zip(table.copy.cgram.iter()).enumerate() {
                if colour != held {
                    log.log(before, index as u16, held);
                }
            }
        }
        for change in log.changes.iter_mut().rev() {
            swap_colour(&mut cgram, change.address, &mut change.value);
        }
        let mut next = 0usize;
        Some(distinct_palettes((0..table.lines.len()).map(|line| {
            replay_log(&mut log, line, &mut next, |address, value| {
                swap_colour(&mut cgram, address, value)
            });
            Some(cgram)
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ppu::PpuAccessCtx;

    /// A small deterministic generator, so a failure repeats.
    struct Numbers(u64);

    impl Numbers {
        fn next(&mut self) -> u8 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 24) as u8
        }
    }

    /// The registers of `$2100-$2133` a test changes freely: every one but
    /// the three data ports, whose changes go to video memory.
    fn plain_registers() -> impl Iterator<Item = u8> {
        (0x00u8..=0x33).filter(|reg| !matches!(reg, 0x04 | 0x18 | 0x19 | 0x22))
    }

    /// Write every plain register twice with numbers from `numbers`, so the
    /// registers written through a latch take both halves.
    fn scramble(ppu: &mut Ppu, numbers: &mut Numbers) {
        let ctx = PpuAccessCtx::ZERO;
        for _ in 0..2 {
            for reg in plain_registers() {
                ppu.write_reg(reg, numbers.next(), &ctx);
            }
        }
        ppu.field = numbers.next() & 1 != 0;
    }

    #[test]
    fn apply_after_capture_restores_every_register() {
        for seed in 1..=64u64 {
            let mut numbers = Numbers(0x9E37_79B9_7F4A_7C15 ^ seed);
            let mut source = Ppu::new();
            let mut target = Ppu::new();
            scramble(&mut source, &mut numbers);
            scramble(&mut target, &mut numbers);
            LineState::capture(&source).apply(&mut target);

            // What a line does not read: the ports' addresses, latches and
            // buffers, the open-bus latches and the sprite flags a drawn line
            // sets. Every other field of the savestate is a setting the
            // renderer reads, and must come back with the line.
            let wanted = source.to_savestate();
            let mut got = target.to_savestate();
            got.oam_baseaddr = wanted.oam_baseaddr;
            got.oam_addr = wanted.oam_addr;
            got.oam_priority = wanted.oam_priority;
            got.oam_iaddr = wanted.oam_iaddr;
            got.oam_latchdata = wanted.oam_latchdata;
            got.mosaic_vcounter = wanted.mosaic_vcounter;
            got.bgofs_ppu1_latch = wanted.bgofs_ppu1_latch;
            got.bgofs_ppu2_latch = wanted.bgofs_ppu2_latch;
            got.vmain_byte = wanted.vmain_byte;
            got.vram_incmode = wanted.vram_incmode;
            got.vram_mapping = wanted.vram_mapping;
            got.vram_incsize = wanted.vram_incsize;
            got.vram_addr = wanted.vram_addr;
            got.vram_readbuffer = wanted.vram_readbuffer;
            got.mode7_latchdata = wanted.mode7_latchdata;
            got.cgram_addr = wanted.cgram_addr;
            got.cgram_iaddr = wanted.cgram_iaddr;
            got.cgram_latchdata = wanted.cgram_latchdata;
            got.ppu1_mdr = wanted.ppu1_mdr;
            got.ppu2_mdr = wanted.ppu2_mdr;
            got.sprite_time_over = wanted.sprite_time_over;
            got.sprite_range_over = wanted.sprite_range_over;
            assert_eq!(
                bincode::serialize(&got).unwrap(),
                bincode::serialize(&wanted).unwrap(),
                "seed {seed}"
            );
        }
    }

    #[test]
    fn columns_match_the_row_length() {
        let names = LineState::column_names();
        let mut values = Vec::new();
        LineState::capture(&Ppu::new()).push_row(&mut values);
        assert_eq!(names.len(), values.len());
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "every column has its own name");
        assert!(names.contains(&"bg_hoffset[0]".to_string()));
        assert!(names.contains(&"window_enable[2].w1_enable".to_string()));
        assert!(names.contains(&"mode7_matrix_d".to_string()));
    }

    #[test]
    fn a_row_reads_back_the_registers() {
        let mut ppu = Ppu::new();
        let ctx = PpuAccessCtx::ZERO;
        ppu.write_reg(0x0D, 0x34, &ctx); // BG1HOFS low, then high
        ppu.write_reg(0x0D, 0x01, &ctx);
        ppu.write_reg(0x23, 0x02, &ctx); // window 1 on for BG1
        let names = LineState::column_names();
        let mut values = Vec::new();
        LineState::capture(&ppu).push_row(&mut values);
        let value = |name: &str| values[names.iter().position(|n| n == name).unwrap()];
        assert_eq!(value("bg_hoffset[0]"), 0x0134);
        assert_eq!(value("window_enable[0].w1_enable"), 1);
        assert_eq!(value("window_enable[0].w1_invert"), 0);
        assert_eq!(
            value("display_disable"),
            1,
            "the reset state is forced blank"
        );
    }

    /// Three memories with known contents, and a VRAM generation that
    /// moves with every VRAM write, as the port's does.
    struct Memories {
        vram: Box<[u16; 0x8000]>,
        cgram: Box<[u16; 0x100]>,
        oam: Box<[u8; 0x220]>,
        vram_generation: u64,
    }

    impl Memories {
        fn new() -> Self {
            let mut memories = Self {
                vram: vec![0u16; 0x8000].into_boxed_slice().try_into().unwrap(),
                cgram: vec![0u16; 0x100].into_boxed_slice().try_into().unwrap(),
                oam: vec![0u8; 0x220].into_boxed_slice().try_into().unwrap(),
                vram_generation: 0,
            };
            for (i, word) in memories.vram.iter_mut().enumerate() {
                *word = (i as u16).wrapping_mul(0x9E37);
            }
            for (i, colour) in memories.cgram.iter_mut().enumerate() {
                *colour = (i as u16).wrapping_mul(0x0421) & 0x7fff;
            }
            for (i, byte) in memories.oam.iter_mut().enumerate() {
                *byte = i as u8;
            }
            memories
        }

        fn view(&mut self) -> VideoMemory<'_> {
            VideoMemory {
                vram: &mut self.vram,
                cgram: &mut self.cgram,
                oam: &mut self.oam,
            }
        }

        fn now(&self) -> VideoMemoryNow<'_> {
            VideoMemoryNow {
                vram: &self.vram,
                cgram: &self.cgram,
                oam: &self.oam,
                vram_generation: self.vram_generation,
            }
        }

        fn copy(&self) -> (Vec<u16>, Vec<u16>, Vec<u8>) {
            (self.vram.to_vec(), self.cgram.to_vec(), self.oam.to_vec())
        }

        /// Write into memory, as a port would: the table finds the change
        /// when it next compares.
        fn write(&mut self, kind: u8, address: u16, value: u8) {
            match kind {
                0 => {
                    let word = &mut self.vram[usize::from(address >> 1)];
                    let mut bytes = word.to_le_bytes();
                    bytes[usize::from(address & 1)] = value;
                    *word = u16::from_le_bytes(bytes);
                    self.vram_generation += 1;
                }
                1 => self.cgram[usize::from(address & 0xff)] = u16::from(value) * 3,
                _ => self.oam[usize::from(address) % 0x220] = value,
            }
        }
    }

    #[test]
    fn rewind_and_replay_give_each_line_its_memory_and_leave_everything_as_it_was() {
        let ppu = Ppu::new();
        let state = LineState::capture(&ppu);
        let mut table = LineRecorder::new(1);
        let mut memories = Memories::new();
        let mut numbers = Numbers(0x1234_5678);
        let mut seen = Vec::new();
        // Twenty lines, with changes before, between and after them, several
        // to the same places, some putting back what was there.
        for line in 1..=20u16 {
            table.record(line, state, 1, &memories.now());
            seen.push(memories.copy());
            for _ in 0..(numbers.next() % 6) {
                let kind = numbers.next() % 3;
                let address = u16::from(numbers.next() & 0x1f) | (u16::from(kind) << 8);
                let value = numbers.next();
                memories.write(kind, address, value);
            }
        }
        // And a write after the last line, as a game uploads in the
        // vertical blank, compared as a redraw compares before it winds
        // memory back.
        let held = memories.vram[1].to_le_bytes()[1];
        memories.write(0, 3, !held);
        table.catch_up(&memories.now());
        let after = memories.copy();
        let log = (
            table.vram.changes.clone(),
            table.palette.changes.clone(),
            table.sprite_table.changes.clone(),
        );
        assert!(
            !log.0.is_empty() && !log.1.is_empty() && !log.2.is_empty(),
            "every memory was written"
        );
        assert!(
            log.0
                .iter()
                .any(|change| usize::from(change.before_line) == seen.len()),
            "the changes after the last line are logged"
        );

        table.rewind(&mut memories.view());
        let mut cursor = ReplayCursor::default();
        for (index, memory) in seen.iter().enumerate() {
            table.replay_through(index, &mut cursor, &mut memories.view());
            assert_eq!(&memories.copy(), memory, "line {}", index + 1);
        }
        table.replay_through(usize::MAX, &mut cursor, &mut memories.view());
        assert_eq!(memories.copy(), after);
        assert_eq!(
            (
                table.vram.changes.clone(),
                table.palette.changes.clone(),
                table.sprite_table.changes.clone()
            ),
            log,
            "the log comes back as it was"
        );

        // Memory stands as the copy holds it: comparing again finds
        // nothing, whatever the generation says.
        memories.vram_generation += 1;
        table.memory_put_back(memories.vram_generation);
        table.catch_up(&memories.now());
        assert_eq!(table.vram.changes, log.0);
        assert_eq!(table.palette.changes, log.1);
        assert_eq!(table.sprite_table.changes, log.2);
    }

    #[test]
    fn a_log_that_fills_up_is_emptied_and_says_so() {
        let mut log = MemoryLog::new(3);
        for address in 0..3 {
            log.log(0, address, 0);
        }
        assert_eq!(log.changes.len(), 3);
        log.log(0, 3, 0);
        assert!(log.overflowed);
        assert!(log.changes.is_empty(), "no part of what changed is kept");
        log.log(0, 4, 0);
        assert!(log.changes.is_empty(), "and nothing more is logged");
        log.clear();
        log.log(0, 5, 0);
        assert_eq!(log.changes.len(), 1, "a new frame logs again");
    }

    #[test]
    fn the_line_recorders_keep_the_table_and_the_others_do_not() {
        let mut ppu = Ppu::new();
        let line_states = CaptureRecording {
            line_states: true,
            ..Default::default()
        };
        let pixel_sources = CaptureRecording {
            pixel_sources: true,
            ..Default::default()
        };
        let line_sprites = CaptureRecording {
            line_sprites: true,
            ..Default::default()
        };
        let memory_only = CaptureRecording {
            frame_writes: true,
            video_memory_sources: true,
            ..Default::default()
        };
        for (recording, kept) in [
            (line_states, true),
            (pixel_sources, true),
            (line_sprites, true),
            (memory_only, false),
            (CaptureRecording::default(), false),
        ] {
            ppu.set_capture_recording(recording);
            assert_eq!(ppu.recorders.lines.is_some(), kept, "{recording:?}");
            assert_eq!(ppu.capture_recording(), recording);
        }
    }

    /// Write colour `index` through the CGRAM port.
    fn write_colour(ppu: &mut Ppu, index: u8, colour: u16) {
        let ctx = PpuAccessCtx::ZERO;
        let [low, high] = colour.to_le_bytes();
        ppu.write_reg(0x21, index, &ctx);
        ppu.write_reg(0x22, low, &ctx);
        ppu.write_reg(0x22, high, &ctx);
    }

    #[test]
    fn port_writes_between_lines_are_logged_at_the_next_line() {
        let mut ppu = Ppu::new(); // forced blank: the ports are open
        let ctx = PpuAccessCtx::ZERO;
        write_colour(&mut ppu, 5, 0x001f);
        ppu.set_capture_recording(CaptureRecording {
            line_states: true,
            ..Default::default()
        });
        ppu.record_line_state(1);
        write_colour(&mut ppu, 5, 0x03e0);
        ppu.write_reg(0x15, 0x80, &ctx); // VRAM: the high half moves the address
        ppu.write_reg(0x16, 0x10, &ctx);
        ppu.write_reg(0x17, 0x00, &ctx);
        ppu.write_reg(0x18, 0xaa, &ctx);
        ppu.write_reg(0x19, 0xbb, &ctx);
        ppu.write_reg(0x02, 0x00, &ctx); // OAM: one pair into sprite 0
        ppu.write_reg(0x03, 0x00, &ctx);
        ppu.write_reg(0x04, 0x11, &ctx);
        ppu.write_reg(0x04, 0x22, &ctx);
        let table = ppu.recorders.lines.as_deref().unwrap();
        assert!(
            table.vram.changes.is_empty()
                && table.palette.changes.is_empty()
                && table.sprite_table.changes.is_empty(),
            "the changes are found when the next line is drawn"
        );

        ppu.record_line_state(2);
        let table = ppu.recorders.lines.as_deref().unwrap();
        assert_eq!(
            table.palette.changes,
            vec![MemoryChange {
                before_line: 1,
                address: 5,
                value: 0x001f,
            }]
        );
        let logged = |log: &MemoryLog| {
            log.changes
                .iter()
                .map(|change| (change.before_line, change.address, change.value))
                .collect::<Vec<_>>()
        };
        assert_eq!(logged(&table.vram), vec![(1, 0x20, 0), (1, 0x21, 0)]);
        assert_eq!(logged(&table.sprite_table), vec![(1, 0, 0), (1, 1, 0)]);
        assert!(ppu.line_state_table().unwrap().memory_exact);
    }

    #[test]
    fn a_debugger_edit_is_not_logged_and_what_the_frame_wrote_before_it_is() {
        let mut ppu = Ppu::new(); // forced blank: the ports are open
        ppu.set_capture_recording(CaptureRecording {
            line_states: true,
            ..Default::default()
        });
        ppu.record_line_state(1);
        write_colour(&mut ppu, 5, 0x03e0);
        ppu.write_cgram_debug(6, &[0x7c00]);
        ppu.write_vram_debug(0x10, &[0x12, 0x34]);
        ppu.write_oam_debug(0, &[0x56]);
        ppu.record_line_state(2);
        let table = ppu.recorders.lines.as_deref().unwrap();
        assert_eq!(
            table.palette.changes,
            vec![MemoryChange {
                before_line: 1,
                address: 5,
                value: 0,
            }],
            "the frame's write is logged, the edit is not"
        );
        assert!(table.vram.changes.is_empty());
        assert!(table.sprite_table.changes.is_empty());
        assert!(!ppu.line_state_table().unwrap().memory_exact);

        // An empty edit edits nothing.
        ppu.record_line_state(1);
        ppu.write_cgram_debug(6, &[]);
        ppu.write_vram_debug(0x10, &[]);
        ppu.write_oam_debug(0, &[]);
        assert!(ppu.line_state_table().unwrap().memory_exact);
    }

    #[test]
    fn line_one_starts_a_table_and_a_line_after_it_does_not() {
        let mut ppu = Ppu::new();
        ppu.set_recording_frame(7);
        ppu.set_capture_recording(CaptureRecording {
            line_states: true,
            ..Default::default()
        });
        ppu.record_line_state(40);
        let table = ppu.line_state_table().unwrap();
        assert!(!table.from_first_line);
        assert_eq!(table.frame, 7);
        ppu.set_recording_frame(8);
        ppu.record_line_state(1);
        ppu.record_line_state(2);
        let table = ppu.line_state_table().unwrap();
        assert!(table.from_first_line);
        assert!(!table.complete);
        assert_eq!(table.frame, 8);
        assert_eq!(table.values.len(), 2 * table.columns.len());
        assert_eq!(table.columns[0], "line");
        assert_eq!(table.values[0], 1);
        assert_eq!(table.values[table.columns.len()], 2);
        ppu.note_picture_finished();
        assert!(ppu.line_state_table().unwrap().complete);
        assert!(ppu.draws_from_line_states());
        ppu.note_registers_edited();
        assert!(!ppu.draws_from_line_states());
        ppu.drop_recorded_frame();
        assert!(ppu.line_state_table().is_none());
    }
}
