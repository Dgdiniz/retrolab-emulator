//! The VDP into and out of the savestate's layout.

use super::{timing, Vdp};
use crate::savestate::{VdpState, VdpTimingState};

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
        // What was recorded described the machine before the load; what is
        // armed stays armed.
        self.clear_recorded_frames();
    }
}
