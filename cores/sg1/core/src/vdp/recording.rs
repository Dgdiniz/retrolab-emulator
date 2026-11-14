//! What the VDP records of each frame while a host asks for it, so that a
//! capture can read how each line of the picture was drawn and draw the
//! picture again the way it was drawn.
//!
//! Nothing is recorded until a host arms it ([`Vdp::set_capture_recording`]):
//! until then `Vdp::recording` is `None`, and each place that records costs
//! one check of it. None of it is in a savestate, and drawing a picture
//! again from it moves nothing a savestate holds.
//!
//! The line table keeps, for each line of the picture, what drawing the
//! line read besides video memory: the 24 registers, the horizontal scroll
//! of the two planes, the vertical scroll memory, the palette as the line
//! used it, the sprite mask carried in from the line above, every palette
//! write made while the line was on screen with what the write coloured
//! again, and, for a line a CRAM transfer painted with the display off, the
//! colours it painted. Video memory is not copied for each line: a journal
//! keeps each VRAM word the frame wrote, with the value the word held
//! before, so going back through it puts VRAM back as any line read it.

use std::collections::HashMap;

use retro_core::video::LineStateTable;

use super::sprites::SpriteCache;
use super::Vdp;

/// What the core records while frames run, for a capture to read. All off
/// by default; never part of a savestate.
///
/// This core records the line states; the other four read false whatever a
/// host asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CaptureRecording {
    /// The settings each line of the picture was drawn with, and what
    /// drawing the picture again from them needs.
    pub line_states: bool,
    /// Where each pixel of the picture came from.
    pub pixel_sources: bool,
    /// The sprites each line held and what became of each.
    pub line_sprites: bool,
    /// What wrote each word of video memory.
    pub video_memory_sources: bool,
    /// The transfers and the palette and sprite table writes of each frame.
    pub frame_writes: bool,
}

/// How a line of the picture was drawn: the `kind` column of the line
/// table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Nothing was recorded of the line: it was drawn before the table was
    /// armed, or the table was cleared since.
    NotRecorded = 0,
    /// Drawn from the planes, the window and the sprites.
    Drawn = 1,
    /// The display was off: the whole line is the backdrop colour.
    Blank = 2,
    /// The display was off and a CRAM transfer was running: the line shows
    /// the colours the transfer moved, two pixels each.
    DirectColour = 3,
}

/// The `palette` column, and `LinePalettes::palette_of_line`, of a line
/// nothing was recorded of.
pub const NO_PALETTE: u16 = 0xFFFF;

/// The column a palette write coloured its line again from, in
/// `LinePalettes::changes`, when it coloured nothing again.
pub const NO_COLUMN: u16 = 0xFFFF;

/// The palettes the lines of the picture were drawn with, as the line table
/// names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinePalettes {
    /// The colours of each palette: 64, the four lines of sixteen of CRAM.
    pub colours_per_palette: u32,
    /// Each palette a line began with, once, in the order the lines first
    /// used it: `colours_per_palette` CRAM words (`0000 BBB0 GGG0 RRR0`) each.
    pub palettes: Vec<u16>,
    /// For each row of the line table, the palette its line began with, an
    /// index into `palettes`, or [`NO_PALETTE`] for a line nothing was
    /// recorded of.
    pub palette_of_line: Vec<u16>,
    /// The palette writes made while each line was on screen, row by row
    /// and in order, five values each: the row; the column the write
    /// coloured the line again from, or [`NO_COLUMN`]; the colour index; the
    /// colour word; and 1 when the write coloured the whole line again (the
    /// display was off and it changed the backdrop colour), else 0.
    pub changes: Vec<u16>,
}

/// The most lines a picture has: 240, the PAL console's 30-cell lines.
const MOST_LINES: usize = 240;

/// The words of vertical scroll memory the renderer reads: two for each of
/// the 20 columns of 16 pixels, words 38 and 39 also serving the partial
/// column at the left edge.
const VERTICAL_SCROLL_WORDS: usize = 40;

/// The most VRAM writes the journal keeps for one picture. Past it the
/// lines can no longer be given back the video memory they read: they are
/// drawn again from video memory as it is at the end, each with its own
/// horizontal scroll.
pub(super) const JOURNAL_CAPACITY: usize = 65_536;

/// What the VDP keeps while a host has armed something, in
/// `Vdp::recording`.
pub(super) struct Recording {
    /// The line table, while line states are armed.
    pub(super) lines: Option<LineTable>,
}

/// The line table: one row for each line of the picture, the journal of
/// VRAM writes, and the sprite lists the renderer parsed.
pub(super) struct LineTable {
    /// One row for each line, 240 of them; a picture uses its first 224 or
    /// 240.
    pub(super) rows: Vec<LineRecord>,
    /// The frame the journal follows: the one being drawn when the table
    /// was armed or when it last drew line 0. Its rows can be given back
    /// exactly the video memory they read.
    pub(super) frame: u64,
    /// Each VRAM word written since the journal began, oldest first, with
    /// the value it held before.
    pub(super) journal: Vec<MemoryUndo>,
    /// The journal ran out of room.
    pub(super) journal_full: bool,
    /// The sprite lists the renderer parsed, oldest first: those of the
    /// frame the journal follows and of the frame before it.
    pub(super) sprite_lists: Vec<RecordedSpriteList>,
}

/// What drawing one line read, besides video memory.
pub(super) struct LineRecord {
    /// The frame the line was drawn in.
    pub(super) frame: u64,
    /// How it was drawn.
    pub(super) kind: LineKind,
    /// The sprite mask the line above carried into it.
    pub(super) sprite_mask_carried_in: bool,
    /// The 24 registers.
    pub(super) registers: [u8; 24],
    /// The horizontal scroll of plane A and of plane B, as the line read
    /// them from the scroll table (10 bits each).
    pub(super) horizontal_scroll: [u16; 2],
    /// The 40 words of vertical scroll memory the renderer reads.
    pub(super) vertical_scroll: [u16; VERTICAL_SCROLL_WORDS],
    /// The palette as the line began with it, 64 CRAM words: what the
    /// renderer coloured the line with, which is not CRAM while a CRAM
    /// transfer is shown line by line.
    pub(super) palette: [u16; 64],
    /// The palette writes made while the line was on screen, in order.
    pub(super) palette_changes: Vec<PaletteChange>,
    /// For a line a CRAM transfer painted: the colours it painted, one for
    /// each two pixels, from the left; the rest of the line is the backdrop.
    pub(super) direct_colours: Vec<u16>,
}

/// One palette write made while a line was on screen.
#[derive(Debug, Clone, Copy)]
pub(super) struct PaletteChange {
    /// The colour index, 0 to 63.
    pub(super) index: u8,
    /// The colour word written (`0000 BBB0 GGG0 RRR0`).
    pub(super) colour: u16,
    /// What the write coloured again.
    pub(super) redraw: PaletteRedraw,
}

/// What a palette write coloured again of the line it was made on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaletteRedraw {
    /// Nothing: the beam had passed the point where a write still shows on
    /// its line.
    Nothing,
    /// The columns `from..to`, the beam's column to the line's end, with
    /// shadow and highlight as they were then.
    Columns {
        from: u16,
        to: u16,
        shadow_highlight: bool,
    },
    /// The whole line, blanked again in the backdrop colour: the display
    /// was off and the write changed that colour. `width` and `backdrop`
    /// are the line's width and the backdrop's colour index then.
    WholeLine { width: u16, backdrop: u8 },
}

/// A VRAM word a write changed, and what it held before.
#[derive(Debug, Clone, Copy)]
pub(super) struct MemoryUndo {
    /// The line the beam was on when the word was written.
    pub(super) line: u16,
    /// The word's index, its byte address halved.
    pub(super) word: u16,
    /// The word before the write, its high byte the one at the even address.
    pub(super) before: u16,
}

/// A sprite list the renderer parsed from the sprite table.
#[derive(Clone)]
pub(super) struct RecordedSpriteList {
    /// The frame it was parsed in.
    pub(super) frame: u64,
    /// The line it was parsed on: the lines from there on draw from it.
    pub(super) line: u16,
    /// Where the sprite table was, and how many slots it had.
    pub(super) table: u16,
    pub(super) slots: u8,
    /// The list.
    pub(super) sprites: SpriteCache,
}

impl LineRecord {
    fn new() -> Self {
        Self {
            frame: 0,
            kind: LineKind::NotRecorded,
            sprite_mask_carried_in: false,
            registers: [0; 24],
            horizontal_scroll: [0; 2],
            vertical_scroll: [0; VERTICAL_SCROLL_WORDS],
            palette: [0; 64],
            palette_changes: Vec::new(),
            direct_colours: Vec::new(),
        }
    }

    /// The line was drawn and its record is the one drawing it again uses.
    fn recorded(&self) -> bool {
        self.kind != LineKind::NotRecorded
    }
}

impl LineTable {
    fn new(frame: u64) -> Self {
        Self {
            rows: (0..MOST_LINES).map(|_| LineRecord::new()).collect(),
            frame,
            journal: Vec::with_capacity(4096),
            journal_full: false,
            sprite_lists: Vec::new(),
        }
    }

    /// Forget every row, the journal and the sprite lists: the machine
    /// changed under them (a reset, a savestate).
    fn clear(&mut self, frame: u64) {
        for row in &mut self.rows {
            row.kind = LineKind::NotRecorded;
            row.palette_changes.clear();
            row.direct_colours.clear();
        }
        self.frame = frame;
        self.journal.clear();
        self.journal_full = false;
        self.sprite_lists.clear();
    }

    /// Line 0 of frame `frame` is about to be drawn: the journal begins
    /// again, and the sprite lists older than the frame before are dropped.
    fn begin_picture(&mut self, frame: u64) {
        self.frame = frame;
        self.journal.clear();
        self.journal_full = false;
        self.sprite_lists
            .retain(|list| list.frame.saturating_add(1) >= frame);
    }

    /// Note that VRAM word `word` is about to be written on line `line`,
    /// holding `before`. A word written again on the same line right after
    /// needs no second entry: the first one already gives its value from
    /// before both.
    fn journal_write(&mut self, line: u16, word: u16, before: u16) {
        if self.journal_full {
            return;
        }
        if let Some(last) = self.journal.last() {
            if last.line == line && last.word == word {
                return;
            }
        }
        if self.journal.len() >= JOURNAL_CAPACITY {
            self.journal_full = true;
            return;
        }
        self.journal.push(MemoryUndo { line, word, before });
    }

    /// The row drawing is about to write, when it belongs to `frame`.
    fn row_of_frame(&mut self, line: u16, frame: u64) -> Option<&mut LineRecord> {
        self.rows
            .get_mut(line as usize)
            .filter(|row| row.recorded() && row.frame == frame)
    }

    /// The sprite list line `line` of frame `frame` was drawn from: the
    /// last one parsed in that frame at or before the line.
    pub(super) fn sprite_list_for(&self, frame: u64, line: u16) -> Option<usize> {
        self.sprite_lists
            .iter()
            .rposition(|list| list.frame == frame && list.line <= line)
    }

    /// The row can be given back exactly the video memory its line read.
    pub(super) fn row_is_exact(&self, row: &LineRecord) -> bool {
        row.recorded() && row.frame == self.frame && !self.journal_full
    }

    /// Each distinct palette the rows began with, in the order the rows
    /// first used it, and the index of each row's (or [`NO_PALETTE`]) for
    /// the first `rows` rows.
    fn distinct_palettes(&self, rows: usize) -> (Vec<[u16; 64]>, Vec<u16>) {
        let mut seen: HashMap<[u16; 64], u16> = HashMap::new();
        let mut palettes = Vec::new();
        let mut of_line = Vec::with_capacity(rows);
        for row in &self.rows[..rows] {
            if !row.recorded() {
                of_line.push(NO_PALETTE);
                continue;
            }
            let next = palettes.len() as u16;
            let index = *seen.entry(row.palette).or_insert_with(|| {
                palettes.push(row.palette);
                next
            });
            of_line.push(index);
        }
        (palettes, of_line)
    }
}

/// The CRAM word a colour of the renderer's table came from. The table
/// holds each three-bit level with its bits repeated
/// ([`Vdp::convert_genesis_color`]), so its top three bits give the level
/// back exactly.
pub(super) fn colour_word((red, green, blue): (u8, u8, u8)) -> u16 {
    (u16::from(blue >> 5) << 9) | (u16::from(green >> 5) << 5) | (u16::from(red >> 5) << 1)
}

/// The names of the line table's columns, `line` first.
fn line_state_columns() -> Vec<String> {
    let mut columns: Vec<String> = ["line", "frame", "kind", "exact"]
        .into_iter()
        .map(String::from)
        .collect();
    columns.extend((0..24).map(|i| format!("register_{i}")));
    columns.push("plane_a_horizontal_scroll".to_string());
    columns.push("plane_b_horizontal_scroll".to_string());
    columns.extend((0..VERTICAL_SCROLL_WORDS).map(|i| format!("vertical_scroll_{i}")));
    columns.push("sprite_mask_carried_in".to_string());
    columns.push("palette".to_string());
    columns
}

impl Vdp {
    /// Arm or disarm what is recorded while frames run, for a capture to
    /// read, and return what is now armed: only the line states are
    /// recorded by this core, so the other flags come back false.
    ///
    /// Arming allocates the line table (about 100 KiB) and nothing more
    /// until frames run; disarming everything drops it, after which the
    /// renderer pays one check per place that records. Arming in the middle
    /// of a frame records the lines drawn from then on; the lines above
    /// stay unrecorded until the next frame draws them.
    pub fn set_capture_recording(&mut self, wanted: CaptureRecording) -> CaptureRecording {
        let armed = CaptureRecording {
            line_states: wanted.line_states,
            ..CaptureRecording::default()
        };
        if armed.line_states {
            let frame = self.frame_being_drawn;
            let current_list = self
                .sprite_cache_valid
                .then(|| self.current_sprite_list(frame, 0));
            let recording = self
                .recording
                .get_or_insert_with(|| Box::new(Recording { lines: None }));
            if recording.lines.is_none() {
                let mut table = LineTable::new(frame);
                // The lines drawn from now on in this frame draw from the
                // list the renderer already parsed.
                table.sprite_lists.extend(current_list);
                recording.lines = Some(table);
            }
        } else if let Some(recording) = self.recording.as_deref_mut() {
            recording.lines = None;
        }
        if armed == CaptureRecording::default() {
            self.recording = None;
        }
        armed
    }

    /// What is recorded while frames run.
    pub fn capture_recording(&self) -> CaptureRecording {
        CaptureRecording {
            line_states: self.line_table().is_some(),
            ..CaptureRecording::default()
        }
    }

    /// Set the number of the frame being drawn, which every record of it
    /// carries: the frames finished, plus one (the system sets it as each
    /// frame starts).
    pub fn set_frame_being_drawn(&mut self, frame: u64) {
        self.frame_being_drawn = frame;
    }

    /// The line table, while line states are armed.
    pub(super) fn line_table(&self) -> Option<&LineTable> {
        self.recording.as_deref()?.lines.as_ref()
    }

    fn line_table_mut(&mut self) -> Option<&mut LineTable> {
        self.recording.as_deref_mut()?.lines.as_mut()
    }

    /// Forget what was recorded of the frames so far, keeping what is
    /// armed: the machine changed under it (a reset, a savestate load).
    pub(super) fn clear_recorded_frames(&mut self) {
        let frame = self.frame_being_drawn;
        if let Some(table) = self.line_table_mut() {
            table.clear(frame);
        }
    }

    /// The sprite list the renderer holds now, as a record parsed on line
    /// `line` of frame `frame`.
    fn current_sprite_list(&self, frame: u64, line: u16) -> RecordedSpriteList {
        RecordedSpriteList {
            frame,
            line,
            table: self.sat_base(),
            slots: self.sprite_table_slots(),
            sprites: self.sprite_cache.clone(),
        }
    }

    /// Record line `line`, about to be drawn as `kind`: what the renderer is
    /// about to read besides video memory. Line 0 begins a new picture.
    #[inline(never)]
    pub(super) fn record_line(&mut self, line: u16, kind: LineKind) {
        let frame = self.frame_being_drawn;
        let mode = self.registers[11] & 0x03;
        let scroll_table = usize::from(self.registers[13] & 0x3F) << 10;
        let horizontal_scroll = [
            self.read_hscroll(scroll_table, 0, mode, line),
            self.read_hscroll(scroll_table, 2, mode, line),
        ];
        let Some(table) = self.recording.as_deref_mut().and_then(|r| r.lines.as_mut()) else {
            return;
        };
        if line == 0 {
            table.begin_picture(frame);
        }
        let Some(row) = table.rows.get_mut(line as usize) else {
            return;
        };
        row.frame = frame;
        row.kind = kind;
        row.sprite_mask_carried_in = self.sprite_mask_overflow;
        row.registers = self.registers;
        row.horizontal_scroll = horizontal_scroll;
        row.vertical_scroll
            .copy_from_slice(&self.vsram[..VERTICAL_SCROLL_WORDS]);
        for (word, &colour) in row.palette.iter_mut().zip(self.cram_rgb.iter()) {
            *word = colour_word(colour);
        }
        row.palette_changes.clear();
        row.direct_colours.clear();
    }

    /// Note that the VRAM word at `byte_address` is about to be written.
    #[inline(always)]
    pub(super) fn note_vram_write(&mut self, byte_address: usize) {
        if self.recording.is_some() {
            self.journal_vram_write(byte_address);
        }
    }

    #[inline(never)]
    fn journal_vram_write(&mut self, byte_address: usize) {
        let line = self.timing.scanline;
        let word = (byte_address >> 1) & 0x7FFF;
        let before = u16::from_be_bytes([self.vram[word * 2], self.vram[word * 2 + 1]]);
        if let Some(table) = self.recording.as_deref_mut().and_then(|r| r.lines.as_mut()) {
            table.journal_write(line, word as u16, before);
        }
    }

    /// Note that `count` VRAM words are about to be written, the first at
    /// `first` and each next one `step` bytes on: a transfer, before it
    /// writes any of them.
    #[inline(always)]
    pub(super) fn note_vram_transfer(&mut self, first: u16, step: u16, count: usize) {
        if self.recording.is_some() {
            let mut address = first;
            for _ in 0..count {
                self.journal_vram_write(usize::from(address));
                address = address.wrapping_add(step);
            }
        }
    }

    /// Note a palette write: colour `index` became `colour`, and the write
    /// coloured again what `redraw` says of the line the beam is on.
    #[inline(always)]
    pub(super) fn note_palette_change(&mut self, index: usize, colour: u16, redraw: PaletteRedraw) {
        if self.recording.is_some() {
            self.record_palette_change(index, colour, redraw);
        }
    }

    #[inline(never)]
    fn record_palette_change(&mut self, index: usize, colour: u16, redraw: PaletteRedraw) {
        let line = self.timing.scanline;
        if self.timing.in_vblank || line >= self.timing.visible_scanlines() {
            return;
        }
        let frame = self.frame_being_drawn;
        if let Some(row) = self
            .line_table_mut()
            .and_then(|table| table.row_of_frame(line, frame))
        {
            row.palette_changes.push(PaletteChange {
                index: index as u8,
                colour,
                redraw,
            });
        }
    }

    /// Note the colours a CRAM transfer paints line `line` with: `words`,
    /// one for each two pixels from the left.
    #[inline(never)]
    pub(super) fn record_direct_colours(&mut self, line: u16, words: impl Iterator<Item = u16>) {
        let frame = self.frame_being_drawn;
        if let Some(row) = self
            .line_table_mut()
            .and_then(|table| table.row_of_frame(line, frame))
        {
            row.direct_colours.extend(words);
        }
    }

    /// Note that the renderer parsed its sprite list for line `line`.
    #[inline(never)]
    pub(super) fn record_sprite_list(&mut self, line: u16) {
        let frame = self.frame_being_drawn;
        let list = self.current_sprite_list(frame, line);
        if let Some(table) = self.line_table_mut() {
            table.sprite_lists.push(list);
        }
    }

    /// A debugger wrote colour `index`: every recorded line, and every
    /// palette write recorded of it, takes the new colour, so the picture
    /// drawn again shows it wherever the colour is used.
    pub(super) fn patch_recorded_colour(&mut self, index: usize, colour: u16) {
        if let Some(table) = self.line_table_mut() {
            for row in &mut table.rows {
                row.palette[index] = colour;
                for change in &mut row.palette_changes {
                    if usize::from(change.index) == index {
                        change.colour = colour;
                    }
                }
            }
        }
    }

    /// A debugger wrote word `index` of the vertical scroll memory: every
    /// recorded line takes it.
    pub(super) fn patch_recorded_vertical_scroll(&mut self, index: usize, value: u16) {
        if index >= VERTICAL_SCROLL_WORDS {
            return;
        }
        if let Some(table) = self.line_table_mut() {
            for row in &mut table.rows {
                row.vertical_scroll[index] = value;
            }
        }
    }

    /// A debugger wrote into the sprite table: parse every recorded sprite
    /// list again, each from video memory as it was on the line it was
    /// parsed on, with the debugger's bytes in place where the frame did
    /// not write over them later.
    pub(super) fn reparse_recorded_sprite_lists(&mut self) {
        let Some(table) = self.recording.as_deref_mut().and_then(|r| r.lines.as_mut()) else {
            return;
        };
        if table.sprite_lists.is_empty() {
            return;
        }
        let mut memory = self.vram.clone();
        let mut cursor = table.journal.len();
        let followed = table.frame;
        let exact = !table.journal_full;
        for list in table.sprite_lists.iter_mut().rev() {
            if exact {
                let back_to = if list.frame == followed { list.line } else { 0 };
                cursor = undo_journal(&table.journal, cursor, back_to, &mut memory);
            }
            list.sprites.build(&memory, list.table, list.slots);
        }
    }

    /// The line table as a table of values, one row for each line of the
    /// picture: the columns are listed on the `VideoDebugger::get_line_states`
    /// of [`Vdp`]. A line nothing was recorded of holds 0 in every column
    /// but `line` and `palette` ([`NO_PALETTE`]).
    pub(super) fn line_state_table(&self) -> Option<LineStateTable> {
        let table = self.line_table()?;
        let rows = usize::from(self.timing.visible_scanlines()).min(table.rows.len());
        let columns = line_state_columns();
        let (_, palette_of_line) = table.distinct_palettes(rows);
        let mut values = Vec::with_capacity(rows * columns.len());
        for (line, row) in table.rows[..rows].iter().enumerate() {
            values.push(line as u32);
            if !row.recorded() {
                values.resize(values.len() + columns.len() - 2, 0);
                values.push(u32::from(NO_PALETTE));
                continue;
            }
            values.push(row.frame as u32);
            values.push(row.kind as u32);
            values.push(u32::from(table.row_is_exact(row)));
            values.extend(row.registers.iter().map(|&value| u32::from(value)));
            values.extend(row.horizontal_scroll.iter().map(|&value| u32::from(value)));
            values.extend(row.vertical_scroll.iter().map(|&value| u32::from(value)));
            values.push(u32::from(row.sprite_mask_carried_in));
            values.push(u32::from(palette_of_line[line]));
        }
        let rows_of_frame = || {
            table.rows[..rows]
                .iter()
                .filter(|row| row.recorded() && row.frame == table.frame)
        };
        let earlier = table.rows[..rows]
            .iter()
            .any(|row| row.recorded() && row.frame != table.frame);
        Some(LineStateTable {
            frame: table.frame,
            from_first_line: table.rows[0].recorded() && table.rows[0].frame == table.frame,
            complete: rows_of_frame().count() == rows,
            memory_exact: !table.journal_full && !earlier,
            rows_per_line: 1,
            first_row_line: 0,
            columns,
            values,
        })
    }

    /// The palettes the lines of the picture began with, each once, and
    /// the palette writes made while each was on screen ([`LinePalettes`]);
    /// `None` while line states are not armed.
    pub fn line_palettes(&self) -> Option<LinePalettes> {
        let table = self.line_table()?;
        let rows = usize::from(self.timing.visible_scanlines()).min(table.rows.len());
        let (palettes, palette_of_line) = table.distinct_palettes(rows);
        let mut changes = Vec::new();
        for (line, row) in table.rows[..rows].iter().enumerate() {
            if !row.recorded() {
                continue;
            }
            for change in &row.palette_changes {
                let (from, whole_line) = match change.redraw {
                    PaletteRedraw::Nothing => (NO_COLUMN, 0),
                    PaletteRedraw::Columns { from, .. } => (from, 0),
                    PaletteRedraw::WholeLine { .. } => (0, 1),
                };
                changes.extend([
                    line as u16,
                    from,
                    u16::from(change.index),
                    change.colour,
                    whole_line,
                ]);
            }
        }
        Some(LinePalettes {
            colours_per_palette: 64,
            palettes: palettes.concat(),
            palette_of_line,
            changes,
        })
    }
}

/// Put back into `memory` the journal's entries before `cursor` whose line
/// is `back_to` or later, newest first, and return where that stopped: the
/// entries are in the order the beam made them, so their lines never go
/// down, and those of the lines before `back_to` come first.
pub(super) fn undo_journal(
    journal: &[MemoryUndo],
    mut cursor: usize,
    back_to: u16,
    memory: &mut [u8],
) -> usize {
    while cursor > 0 && journal[cursor - 1].line >= back_to {
        let entry = journal[cursor - 1];
        let at = usize::from(entry.word) * 2;
        memory[at..at + 2].copy_from_slice(&entry.before.to_be_bytes());
        cursor -= 1;
    }
    cursor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_of_the_renderer_s_table_gives_its_word_back() {
        for word in (0..0x1000u16).filter(|word| word & !0x0EEE == 0) {
            assert_eq!(colour_word(Vdp::convert_genesis_color(word)), word);
        }
    }

    #[test]
    fn the_journal_keeps_one_entry_for_a_word_written_twice_on_a_line_and_stops_when_full() {
        let mut table = LineTable::new(1);
        table.journal_write(3, 10, 0x1111);
        table.journal_write(3, 10, 0x2222);
        table.journal_write(4, 10, 0x3333);
        assert_eq!(table.journal.len(), 2);
        assert_eq!(table.journal[0].before, 0x1111);
        assert_eq!(table.journal[1].before, 0x3333);

        for word in 0..JOURNAL_CAPACITY as u32 {
            table.journal_write(5, (word % 0x8000) as u16, 0);
        }
        assert!(table.journal_full);
        assert_eq!(table.journal.len(), JOURNAL_CAPACITY);
        table.begin_picture(2);
        assert!(!table.journal_full && table.journal.is_empty());
    }

    #[test]
    fn undoing_the_journal_goes_back_line_by_line() {
        let journal = [
            MemoryUndo {
                line: 2,
                word: 0,
                before: 0x0102,
            },
            MemoryUndo {
                line: 5,
                word: 0,
                before: 0x0304,
            },
            MemoryUndo {
                line: 5,
                word: 1,
                before: 0x0506,
            },
        ];
        let mut memory = vec![0xAA; 4];
        let cursor = undo_journal(&journal, journal.len(), 5, &mut memory);
        assert_eq!(cursor, 1);
        assert_eq!(memory, [0x03, 0x04, 0x05, 0x06]);
        let cursor = undo_journal(&journal, cursor, 3, &mut memory);
        assert_eq!(cursor, 1, "nothing written from line 3 to line 4");
        let cursor = undo_journal(&journal, cursor, 0, &mut memory);
        assert_eq!(cursor, 0);
        assert_eq!(memory, [0x01, 0x02, 0x05, 0x06]);
    }
}
