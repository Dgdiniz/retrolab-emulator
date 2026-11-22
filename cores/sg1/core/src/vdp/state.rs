//! The VDP into and out of the savestate's layout, and into and out of the
//! savestate's extension the state within a frame the layout has no field
//! for.

use super::sprites::{Sprite, SpriteCache};
use super::{timing, FillState, Vdp};
use crate::savestate::{
    VdpBeamState, VdpExtension, VdpFillState, VdpSpriteState, VdpState, VdpTimingState,
};

impl Vdp {
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

        // The layout has no field for the vertical interrupt's delay or the
        // first frame after a reset: they take what a state with nothing
        // after its payload holds (the delay elapsed, the first frame past),
        // and the extension's, when it holds them (`restore_extension`).
        // Left as the loading machine had them, a new machine, whose first
        // frame is still ahead, would swallow the first vertical interrupt
        // after the load.
        self.timing.vint_request_delay_mclks = -1;
        self.timing.first_frame_after_reset = false;
        // Nor for the lines the frame draws and its interlace mode: a load
        // takes them from registers 1 and 12, as the next frame's first
        // line does.
        self.timing.latch_lines_drawn(self.registers[1]);
        self.latch_interlace();

        self.dma_fill_value = s.dma_fill_value;
        self.dma_pending = s.dma_pending;
        self.dma_busy = s.dma_busy;
        self.dma_remaining_clocks = s.dma_remaining_clocks;
        // A CRAM transfer in active display tints the picture line by line
        // (CRAM itself holds its end already): the picture's, not carried.
        self.progressive_cram = None;
        self.sprite_overflow = s.sprite_overflow;
        self.sprite_collision = s.sprite_collision;
        self.sprite_mask_overflow = s.sprite_mask_overflow;
        self.odd_frame = s.odd_frame;
        self.field_drawn = u8::from(self.odd_frame);
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

        // Rebuild derived state. The layout has no field for the VDP's copy
        // of the sprite table's first words: a load takes it from the table
        // at its base, which holds the same words unless the base moved
        // after they were written.
        self.sprite_cache_valid = false;
        self.take_sprite_heads_from_table();
        self.sprite_list_stale = false;
        self.rebuild_cram_rgb();
        // What was recorded described the machine before the load; what is
        // armed stays armed.
        self.clear_recorded_frames();
    }
}

impl Vdp {
    /// What the VDP holds that the layout has no field for: a fill in
    /// flight, the vertical interrupt held off, the first frame after a
    /// reset, the beam part of the way through its line, the sprite list
    /// the frame's next line is drawn with. What is drawn into the picture,
    /// and the tables a CRAM transfer in active display tints it with, are
    /// the picture's, not the machine's.
    pub fn extension(&self) -> VdpExtension {
        let beam = VdpBeamState {
            into_line: self.beam_cycles_into_line,
            line_length: self.beam_cycles_per_line,
        };
        VdpExtension {
            fill: self.fill_state.map(|fill| VdpFillState {
                remaining: fill.remaining,
                carry: fill.cell_accum,
            }),
            vint_delay: u32::try_from(self.timing.vint_request_delay_mclks).ok(),
            first_frame_after_reset: self.timing.first_frame_after_reset,
            beam: (!beam.at_line_end()).then_some(beam),
            sprites: self.sprite_cache_valid.then(|| {
                self.sprite_cache
                    .sprites()
                    .iter()
                    .map(|sprite| VdpSpriteState {
                        x: sprite.x,
                        y: sprite.y,
                        width_tiles: sprite.width_tiles,
                        height_tiles: sprite.height_tiles,
                        pattern_base: sprite.pattern_base,
                        palette: sprite.palette,
                        priority: sprite.priority,
                        flip_h: sprite.flip_h,
                        flip_v: sprite.flip_v,
                        link: sprite.link,
                    })
                    .collect()
            }),
        }
    }

    /// Make the VDP's state within a frame `extension`'s, after
    /// [`Self::from_savestate`]; the beam stands at `line_end` when the
    /// extension holds none (a line's end, which the payload gives).
    pub fn restore_extension(&mut self, extension: VdpExtension, line_end: VdpBeamState) {
        self.fill_state = extension.fill.map(|fill| FillState {
            remaining: fill.remaining,
            cell_accum: fill.carry,
        });
        self.timing.vint_request_delay_mclks = extension
            .vint_delay
            .and_then(|clocks| i32::try_from(clocks).ok())
            .unwrap_or(-1);
        self.timing.first_frame_after_reset = extension.first_frame_after_reset;
        let beam = extension.beam.unwrap_or(line_end);
        self.beam_cycles_into_line = beam.into_line;
        self.beam_cycles_per_line = beam.line_length;
        match extension.sprites {
            Some(sprites) => {
                self.sprite_cache
                    .replace(sprites.into_iter().map(|sprite| Sprite {
                        x: sprite.x,
                        y: sprite.y,
                        width_tiles: sprite.width_tiles,
                        height_tiles: sprite.height_tiles,
                        pattern_base: sprite.pattern_base,
                        palette: sprite.palette,
                        priority: sprite.priority,
                        flip_h: sprite.flip_h,
                        flip_v: sprite.flip_v,
                        link: sprite.link,
                    }));
                self.sprite_cache_valid = true;
                // A list the copy and the table no longer give is one that
                // a change made in the table, while the line the state was
                // saved in ran, made stale: the next line draws from it, and
                // that line's end builds the list again.
                let mut now = SpriteCache::new();
                now.build(
                    self.sprite_heads(),
                    &self.vram,
                    self.sat_base(),
                    self.sprite_table_slots(),
                    self.interlace,
                );
                self.sprite_list_stale = now.sprites() != self.sprite_cache.sprites();
            }
            None => self.sprite_cache_valid = false,
        }
    }
}
