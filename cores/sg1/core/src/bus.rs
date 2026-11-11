//! Address bus implementation for Genesis.
//!
//! Routes reads and writes to the appropriate hardware components based on
//! the address being accessed.
//!
//! Uses a 256-entry page table for O(1) address region dispatch,
//! indexed by the top 8 bits of the 24-bit address.

use crate::cartridge::composable::GenesisCartridge;
use crate::io::GenesisIo;
use crate::memory::GenesisMemory;
use crate::psg::Psg;
use crate::timing::VideoRegion;
use crate::vdp::dma::{DmaTarget, MemoryDma};
use crate::vdp::Vdp;
use crate::ym2612::Ym2612;
use retro_core::cpu::{Addressable, CyclesTrackable};

/// 68K cycles the BUSACK ($A11100) read keeps reporting "busy" after BUSREQ is
/// asserted, modelling the time the Z80 takes to finish its current bus cycle and
/// release the bus. Real hardware grants after the in-flight Z80 instruction
/// completes; SG1 runs the Z80 instruction-atomically, so we approximate that
/// release latency with a fixed delay (~one maximal Z80 instruction, 23 Z80
/// cycles ~ 11 68K cycles). The actual 68K access is still served immediately
/// (only the BUSACK status bit is delayed), so games that skip the poll are
/// unaffected.
const Z80_BUS_GRANT_DELAY_68K: u64 = 11;

/// Memory region identifiers for the page table.
/// Each 64KB page maps to one of these regions.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum MemRegion {
    Cartridge, // 0x000000-0x7FFFFF
    Reserved,  // 0x800000-0x9FFFFF, 0xA12000-0xBFFFFF
    Z80,       // 0xA00000-0xA0FFFF
    IoControl, // 0xA10000-0xA11FFF (subdivided internally)
    Vdp,       // 0xC00000-0xDFFFFF
    WorkRam,   // 0xE00000-0xFFFFFF
}

/// Build the 256-entry page table at compile time.
/// Index = top byte of 24-bit address (addr >> 16).
const fn build_page_table() -> [MemRegion; 256] {
    let mut table = [MemRegion::Reserved; 256];
    let mut i: usize = 0;
    // 0x00-0x7F: Cartridge
    while i < 0x80 {
        table[i] = MemRegion::Cartridge;
        i += 1;
    }
    // 0x80-0x9F: Reserved (already set)
    // 0xA0: Z80
    table[0xA0] = MemRegion::Z80;
    // 0xA1: I/O + Control (subdivided in handler)
    table[0xA1] = MemRegion::IoControl;
    // 0xA2-0xBF: Reserved (already set)
    // 0xC0-0xDF: VDP
    i = 0xC0;
    while i < 0xE0 {
        table[i] = MemRegion::Vdp;
        i += 1;
    }
    // 0xE0-0xFF: Work RAM
    i = 0xE0;
    while i < 0x100 {
        table[i] = MemRegion::WorkRam;
        i += 1;
    }
    table
}

static PAGE_TABLE: [MemRegion; 256] = build_page_table();

/// Genesis address bus.
///
/// Routes memory accesses to the correct component:
/// - Cartridge (ROM/SRAM/mappers)
/// - RAM
/// - VDP
/// - PSG
/// - YM2612 (FM sound chip)
/// - I/O ports
/// - Z80 memory space
pub struct AddressBus {
    cartridge: Option<Box<dyn GenesisCartridge>>,
    memory: GenesisMemory,
    vdp: Vdp,
    psg: Psg,
    ym2612: Ym2612,
    io: GenesisIo,
    z80_bus_req: bool,
    /// Z80 reset line. When true, Z80 is held in reset and cannot execute.
    /// Controlled by 68K writes to 0xA11200 (bit 0: 0=reset, 1=run).
    z80_reset: bool,
    /// Set to true when Z80 reset transitions from asserted to released.
    /// The system loop should check this flag, reset the Z80 CPU state
    /// (PC=0, iff1=false, etc.), and clear the flag.
    z80_reset_release_pending: bool,
    /// Z80 bank register (9-bit, shifted left by 15 to get 68K address)
    /// Built up one bit at a time via writes to 0x6000-0x60FF
    z80_bank_register: u32,
    /// Bit counter for bank register writes (0-8, reset after 9th write)
    z80_bank_bit_count: u8,
    pub cycles_executed: u64,
    /// Wait cycles imposed by VDP FIFO stalls (CPU halted when FIFO is full).
    pub vdp_wait_cycles: u32,
    /// Extra 68K cycles charged this instruction for accessing the Z80 bus
    /// ($A00000-$A0FFFF). The 68000 stalls one cycle per byte while the slower
    /// Z80-side bus completes the transfer (Genesis Plus GX and PicoDrive
    /// charge the same cycle). The
    /// step functions fold this into the instruction's total cycle count.
    pub z80_bus_wait_cycles: u32,
    /// Absolute 68K master-cycle count, mirrored from the system before each
    /// instruction. Used to time the BUSREQ -> BUSACK grant latency without
    /// reaching back into the system from a bus access.
    pub m68k_cycle: u64,
    /// 68K master cycle at which a pending BUSREQ grant completes. On hardware the
    /// Z80 finishes its current bus cycle before releasing the bus, so BUSACK
    /// ($A11100) reads "busy" for a short window after BUSREQ is asserted. We gate
    /// the grant on the *68K* position (which always advances as the CPU polls),
    /// which is deadlock free, unlike gating on the Z80 (which stops the instant
    /// BUSREQ is set). PicoDrive times the grant against the 68000 too.
    z80_busack_grant_cycle: u64,
    /// Address of the 68000 instruction-prefetch word (the next word the CPU
    /// would fetch). The VDP status register leaves bits 10-15 undriven, so on
    /// real hardware they float to the last value on the 68000 data bus, the
    /// prefetched instruction word (checked against Genesis Plus GX). Every
    /// ROM/work-RAM word fetch (opcode +
    /// extension words, all routed through `read_word`) advances this to
    /// `addr + 2`, so at a status read it holds the true prefetch pointer; the
    /// word status read in `read_word` merges `mem[open_bus_pc] & 0xFC00`.
    pub open_bus_pc: u32,
    /// When set, every 68K bus read/write is logged into `watch_events`
    /// so the system can match memory watchpoints after the instruction.
    /// Off on the fast path (no per-access cost beyond one predictable
    /// branch); the system arms it only while a read/write watchpoint is
    /// active and disarms it before any sub-CPU bus traffic so those
    /// accesses are never attributed to the 68K.
    debug_armed: bool,
    /// `(addr, size_in_bytes, is_write, value)` accesses recorded during
    /// the current 68K instruction while `debug_armed`. `value` carries
    /// the byte (size 1) or big-endian word (size 2) read/written, so
    /// value-matched watchpoints can compare the observed bytes. Drained
    /// per step.
    watch_events: Vec<(u32, u8, bool, u16)>,
    /// The DMA transfers from 68000 memory started while `debug_armed`: the
    /// VDP reads their source straight from the cartridge and work RAM, so
    /// those reads never reach `watch_events`. Drained per step.
    watch_dmas: Vec<MemoryDma>,
}

impl AddressBus {
    /// Create a new address bus.
    pub fn new() -> Self {
        Self::with_region(VideoRegion::Ntsc)
    }

    /// Create a new address bus with a specific region.
    pub fn with_region(region: VideoRegion) -> Self {
        // Audio clocks
        let psg_clock = (region.master_clock() / 15) as u32; // ~3.58 MHz (same as Z80)
        let ym2612_clock = (region.master_clock() / 7) as u32; // ~7.67 MHz (same as 68K)
        let sample_rate = 44100; // Standard audio sample rate

        Self {
            cartridge: None,
            memory: GenesisMemory::new(),
            vdp: Vdp::new(),
            psg: Psg::new(psg_clock, sample_rate),
            ym2612: Ym2612::new(ym2612_clock, sample_rate),
            io: GenesisIo::new(),
            z80_bus_req: false,
            z80_reset: true, // Z80 starts in reset
            z80_reset_release_pending: false,
            z80_bank_register: 0,
            z80_bank_bit_count: 0,
            cycles_executed: 0,
            vdp_wait_cycles: 0,
            z80_bus_wait_cycles: 0,
            m68k_cycle: 0,
            z80_busack_grant_cycle: 0,
            open_bus_pc: 0,
            debug_armed: false,
            watch_events: Vec::new(),
            watch_dmas: Vec::new(),
        }
    }

    /// Arm or disarm recording of 68K bus accesses for memory watchpoints.
    /// Arming clears any leftover log; the system arms before stepping the
    /// 68K and disarms after draining so sub-CPU traffic is never recorded.
    pub fn set_watch_armed(&mut self, armed: bool) {
        self.debug_armed = armed;
        self.watch_events.clear();
        self.watch_dmas.clear();
    }

    /// Take the `(addr, size, is_write, value)` accesses recorded since
    /// the last arm/drain, leaving the log empty for the next instruction.
    pub fn take_watch_events(&mut self) -> Vec<(u32, u8, bool, u16)> {
        std::mem::take(&mut self.watch_events)
    }

    /// Take the DMA transfers from 68000 memory started since the last
    /// arm/drain, leaving the list empty for the next instruction.
    pub fn take_watch_dmas(&mut self) -> Vec<MemoryDma> {
        std::mem::take(&mut self.watch_dmas)
    }

    /// Note a DMA from 68000 memory that is about to run, while recording.
    fn record_memory_dma(&mut self, source: u32, words: u32) {
        if self.debug_armed {
            let (destination, code) = self.vdp.current_access();
            self.watch_dmas.push(MemoryDma {
                source,
                words,
                target: DmaTarget::from_access_code(code),
                destination,
            });
        }
    }

    /// Read the word at `addr` as 68000 open-bus data (the instruction prefetch
    /// that floats onto undriven VDP status bits). Reads ROM/work-RAM only, the
    /// regions code executes from, and adds no bus cycles or side effects. Other
    /// regions return 0 (`open_bus_pc` is only ever set past a ROM or work-RAM
    /// word read).
    fn open_bus_word(&mut self, addr: u32) -> u16 {
        let a = addr & 0x00FFFFFF;
        let mc = self.m68k_cycle;
        match a {
            0x000000..=0x7FFFFF => {
                if let Some(ref mut cart) = self.cartridge {
                    ((cart.read_bus(a, mc) as u16) << 8)
                        | (cart.read_bus(a.wrapping_add(1), mc) as u16)
                } else {
                    0
                }
            }
            0xE00000..=0xFFFFFF => {
                let ram = self.memory.work_ram.as_slice();
                let mask = ram.len() - 1;
                let ra = (a & 0xFFFF) as usize;
                ((ram[ra & mask] as u16) << 8) | (ram[(ra.wrapping_add(1)) & mask] as u16)
            }
            _ => 0,
        }
    }

    /// Side-effect-free single-byte peek for the debugger: the memory views
    /// and the multi-byte value of a sized watchpoint. Unlike
    /// [`Addressable::read`] it charges no bus cycles, logs no watch event
    /// and changes no port's state. Reads the cartridge, work RAM and Z80 RAM
    /// (`$A00000-$A01FFF`, repeated at `$A02000`, whoever holds the Z80's
    /// bus); every other address returns 0, since a read of the I/O or VDP
    /// ports would change them. The VDP's memories have calls of their own.
    pub fn peek_byte(&mut self, addr: u32) -> u8 {
        let a = addr & 0x00FFFFFF;
        let mc = self.m68k_cycle;
        match a {
            0x000000..=0x7FFFFF => self
                .cartridge
                .as_mut()
                .map(|cart| cart.read_bus(a, mc))
                .unwrap_or(0xFF),
            0xA00000..=0xA03FFF => self.memory.z80_ram.read(a & 0x1FFF),
            0xE00000..=0xFFFFFF => {
                let ram = self.memory.work_ram.as_slice();
                let mask = ram.len() - 1;
                ram[(a & 0xFFFF) as usize & mask]
            }
            _ => 0,
        }
    }

    /// The 68000's address space read through [`AddressBus::peek_byte`], for
    /// the debugger's views of it (the disassembly).
    pub fn peek_view(&mut self) -> PeekView<'_> {
        PeekView(self)
    }

    /// Set the cartridge.
    pub fn set_cartridge(&mut self, cartridge: Box<dyn GenesisCartridge>) {
        self.cartridge = Some(cartridge);
    }

    /// Unplug the cartridge and hand it back to the caller.
    ///
    /// Used by the power-cycle path, which throws the whole bus away
    /// (work RAM, Z80 RAM, VDP, audio chips, I/O and the Z80 latches all
    /// return to their power-on state) but must keep the cartridge:
    /// battery-backed SRAM survives a power cycle on real hardware, and
    /// silently blanking it would destroy the player's save.
    pub fn take_cartridge(&mut self) -> Option<Box<dyn GenesisCartridge>> {
        self.cartridge.take()
    }

    /// Get a reference to the cartridge.
    pub fn cartridge(&self) -> Option<&dyn GenesisCartridge> {
        self.cartridge.as_ref().map(|c| c.as_ref())
    }

    /// Get a mutable reference to the cartridge.
    pub fn cartridge_mut(&mut self) -> Option<&mut (dyn GenesisCartridge + '_)> {
        match &mut self.cartridge {
            Some(cart) => Some(&mut **cart),
            None => None,
        }
    }

    /// Load ROM - Legacy compatibility method (use set_cartridge for new code)
    #[deprecated(note = "Use set_cartridge with build_genesis_cartridge instead")]
    #[allow(dead_code)]
    pub fn load_rom(&mut self, mut rom: Vec<u8>) {
        use crate::cartridge::composable::build_genesis_cartridge;

        // Ensure minimum size for header
        if rom.len() < 512 {
            rom.resize(512, 0);
        }

        // Add header if not present
        if &rom[0x100..0x110] != b"SEGA MEGA DRIVE " {
            rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
            rom[0x120..0x138].copy_from_slice(b"TEST ROM                ");
        }

        if let Ok(cartridge) = build_genesis_cartridge(rom) {
            self.set_cartridge(cartridge);
        }
    }

    /// Get a reference to the memory system.
    pub fn memory(&self) -> &GenesisMemory {
        &self.memory
    }

    /// Get a mutable reference to the memory system.
    pub fn memory_mut(&mut self) -> &mut GenesisMemory {
        &mut self.memory
    }

    /// Get a reference to the VDP.
    pub fn vdp(&self) -> &Vdp {
        &self.vdp
    }

    /// Get a mutable reference to the VDP.
    pub fn vdp_mut(&mut self) -> &mut Vdp {
        &mut self.vdp
    }

    /// Get VDP framebuffer.
    pub fn get_vdp_framebuffer(&self) -> &[u8] {
        self.vdp.framebuffer()
    }

    /// Get VDP display width.
    pub fn get_vdp_display_width(&self) -> u32 {
        self.vdp.display_width()
    }

    /// Get VDP display height.
    pub fn get_vdp_display_height(&self) -> u32 {
        self.vdp.display_height()
    }

    /// Get a reference to the PSG.
    pub fn psg(&self) -> &Psg {
        &self.psg
    }

    /// Get a mutable reference to the PSG.
    pub fn psg_mut(&mut self) -> &mut Psg {
        &mut self.psg
    }

    /// Get a reference to the YM2612.
    pub fn ym2612(&self) -> &Ym2612 {
        &self.ym2612
    }

    /// Get a mutable reference to the YM2612.
    pub fn ym2612_mut(&mut self) -> &mut Ym2612 {
        &mut self.ym2612
    }

    /// Get a reference to the I/O system.
    pub fn io(&self) -> &GenesisIo {
        &self.io
    }

    /// Get a mutable reference to the I/O system.
    pub fn io_mut(&mut self) -> &mut GenesisIo {
        &mut self.io
    }

    /// Check if Z80 bus is requested by 68K (Z80 cannot execute)
    pub fn is_z80_bus_requested(&self) -> bool {
        self.z80_bus_req
    }

    /// Check if Z80 is held in reset
    pub fn is_z80_reset(&self) -> bool {
        self.z80_reset
    }

    /// Check and clear the Z80 reset-release pending flag.
    /// Returns true if the Z80 was just released from reset and needs its
    /// CPU state reset (PC=0, etc.). Clears the flag after reading.
    pub fn take_z80_reset_release(&mut self) -> bool {
        let pending = self.z80_reset_release_pending;
        self.z80_reset_release_pending = false;
        pending
    }

    /// Get a mutable reference to the Z80 RAM in the bus memory
    pub fn z80_ram_mut(&mut self) -> &mut retro_core::memory::Ram {
        &mut self.memory.z80_ram
    }

    /// Create a Z80Bus with access to PSG, YM2612, bank register, cartridge ROM,
    /// and the shared Z80 RAM (inside GenesisMemory).
    ///
    /// The Z80 RAM is part of GenesisMemory, so both the 68K (writing via bus)
    /// and the Z80 (executing via Z80Bus) access the same RAM instance.
    pub fn create_z80_bus(&mut self) -> crate::z80_bus::Z80Bus<'_> {
        crate::z80_bus::Z80Bus::new(
            &mut self.psg,
            &mut self.ym2612,
            &mut self.z80_bank_register,
            &mut self.z80_bank_bit_count,
            self.cartridge.as_deref_mut(),
            &mut self.memory,
        )
    }

    /// Read a byte from the bus (Big Endian for 68000).
    /// Uses page table for O(1) region dispatch.
    #[inline(always)]
    fn read_byte(&mut self, addr: u32) -> u8 {
        let addr = addr & 0x00FFFFFF;
        let mc = self.m68k_cycle;
        let page = (addr >> 16) as usize;
        match PAGE_TABLE[page] {
            MemRegion::Cartridge => {
                if let Some(ref mut cart) = self.cartridge {
                    cart.read_bus(addr, mc)
                } else {
                    0xFF
                }
            }
            MemRegion::WorkRam => {
                let ram_addr = addr & 0xFFFF;
                self.memory.work_ram.read(ram_addr)
            }
            MemRegion::Vdp => self.vdp.read(addr),
            MemRegion::Z80 => {
                if self.z80_bus_req {
                    // The 68000 stalls one cycle while the slower Z80 bus serves
                    // the byte. Only charged when
                    // the bus is actually granted.
                    self.z80_bus_wait_cycles += 1;
                    let z80_addr = (addr & 0xFFFF) as u16;
                    self.z80_read(z80_addr)
                } else {
                    0xFF
                }
            }
            MemRegion::IoControl => {
                // Sub-dispatch within the 0xA1xxxx page
                match addr {
                    0xA10000..=0xA10FFF => self.io.read(addr),
                    0xA11000..=0xA11FFF => {
                        if addr == 0xA11100 {
                            // BUSACK: D0 clear = bus granted to the 68K. Granted only
                            // once BUSREQ is set AND the grant latency has elapsed
                            // (68K reached the target), so while the Z80 runs it does
                            // not read "granted" the same instant BUSREQ is asserted.
                            if self.z80_bus_req && self.m68k_cycle >= self.z80_busack_grant_cycle {
                                0x00
                            } else {
                                0x01
                            }
                        } else if addr == 0xA11200 {
                            0x00
                        } else {
                            0xFF
                        }
                    }
                    // SRAM-enable / /TIME control registers ($A130xx). These
                    // live outside the cartridge map plan (page 0xA1), so the
                    // cartridge reports the enable bit via a dedicated hook;
                    // everything else on this window is open bus.
                    0xA13000..=0xA130FF => self
                        .cartridge
                        .as_ref()
                        .and_then(|cart| cart.read_time_register(addr))
                        .unwrap_or(0xFF),
                    // SVP (and future I/O-page coprocessor) mailbox: the
                    // $A15000-$A1500B window is dispatched to the cartridge,
                    // which routes it to the owning chip via its map plan.
                    0xA15000..=0xA1500B => {
                        if let Some(ref mut cart) = self.cartridge {
                            cart.read_bus(addr, mc)
                        } else {
                            0xFF
                        }
                    }
                    _ => 0xFF,
                }
            }
            MemRegion::Reserved => 0xFF,
        }
    }

    /// Write a byte to the bus (Big Endian for 68000).
    /// Uses page table for O(1) region dispatch.
    #[inline(always)]
    fn write_byte(&mut self, addr: u32, value: u8) {
        self.cycles_executed += 2;
        let addr = addr & 0x00FFFFFF;
        let mc = self.m68k_cycle;
        let page = (addr >> 16) as usize;
        match PAGE_TABLE[page] {
            MemRegion::Cartridge => {
                if let Some(ref mut cart) = self.cartridge {
                    cart.write_bus(addr, value, mc);
                }
            }
            MemRegion::WorkRam => {
                let ram_addr = addr & 0xFFFF;
                self.memory.work_ram.write(ram_addr, value);
            }
            MemRegion::Vdp => {
                // PSG occupies odd addresses with bit 4 set within VDP space
                // (0xC00011, 0xC00013, ..., mirrored every 0x20 bytes)
                if (addr & 0x11) == 0x11 {
                    self.psg.write(value);
                } else {
                    self.vdp.write(addr, value);

                    // Check if a Memory-to-VRAM DMA was triggered
                    if self.vdp.is_dma_pending() {
                        let regs = *self.vdp.registers();
                        let raw_length = ((regs[20] as u16) << 8) | (regs[19] as u16);
                        let length: u32 = if raw_length == 0 {
                            0x10000
                        } else {
                            raw_length as u32
                        };
                        let addr_low = regs[21] as u32;
                        let addr_mid = regs[22] as u32;
                        let addr_high = (regs[23] & 0x7F) as u32;
                        let source = ((addr_high << 16) | (addr_mid << 8) | addr_low) << 1;

                        self.record_memory_dma(source, length);
                        let mut dma_buffer = Vec::with_capacity(length as usize * 2);
                        let ram_slice = self.memory.work_ram.as_slice();
                        // The 68k-bus DMA increments only reg21/22 (the 16-bit word
                        // counter); reg23 (the 128 KB window base) is never touched,
                        // so the source wraps inside its 128 KB block rather than
                        // crossing the boundary linearly (as the post-DMA
                        // source-register writeback does; checked against
                        // Genesis Plus GX).
                        // VDPFIFOTesting "DMA Transfer Source Wrapping" arms a
                        // transfer one word below the boundary.
                        let window_base = source & 0x00FE0000;
                        let start_word = (source & 0x1FFFF) >> 1;
                        for i in 0..(length * 2) {
                            let word_off = start_word.wrapping_add(i >> 1) & 0xFFFF;
                            let src_addr = window_base | (word_off << 1) | (i & 1);
                            let byte = match PAGE_TABLE[(src_addr >> 16) as usize] {
                                MemRegion::Cartridge => {
                                    if let Some(ref mut cart) = self.cartridge {
                                        cart.read_bus(src_addr, mc)
                                    } else {
                                        0xFF
                                    }
                                }
                                MemRegion::WorkRam => {
                                    let ram_addr = (src_addr & 0xFFFF) as usize;
                                    if ram_addr < ram_slice.len() {
                                        ram_slice[ram_addr]
                                    } else {
                                        0xFF
                                    }
                                }
                                _ => 0xFF,
                            };
                            dma_buffer.push(byte);
                        }
                        self.vdp.execute_pending_dma(&dma_buffer);
                    }
                }
            }
            MemRegion::Z80 => {
                if self.z80_bus_req {
                    // As on the read path: one stall cycle per byte written to the
                    // Z80 bus.
                    self.z80_bus_wait_cycles += 1;
                    let z80_addr = (addr & 0xFFFF) as u16;
                    self.z80_write(z80_addr, value);
                }
            }
            MemRegion::IoControl => {
                match addr {
                    0xA10000..=0xA10FFF => self.io.write(addr, value),
                    0xA11000..=0xA11FFF => {
                        if addr == 0xA11100 {
                            let new_req = (value & 0x01) != 0;
                            if new_req && !self.z80_bus_req {
                                // BUSREQ assert edge. The grant (BUSACK low) is
                                // delayed until the Z80 would have released the bus
                                // at an instruction boundary, gated on the 68K cycle
                                // position so the poll loop always makes progress
                                // (PicoDrive times it the same way; Genesis Plus GX
                                // runs the Z80 up to the same edge). While the Z80 is
                                // held in reset it is not driving the bus, so the grant
                                // is immediate: this is the boot path where the 68K
                                // loads the sound driver with the Z80 still in reset.
                                self.z80_busack_grant_cycle = if self.z80_reset {
                                    self.m68k_cycle
                                } else {
                                    self.m68k_cycle + Z80_BUS_GRANT_DELAY_68K
                                };
                            }
                            self.z80_bus_req = new_req;
                        } else if addr == 0xA11200 {
                            let new_reset = (value & 0x01) == 0;
                            let assert_edge = !self.z80_reset && new_reset;
                            let release_edge = self.z80_reset && !new_reset;
                            if release_edge {
                                self.z80_reset_release_pending = true;
                            }
                            // The YM2612 /IC pin follows the Z80 reset line: clear
                            // the FM chip when reset is asserted (silences sound the
                            // way games expect when they reset the Z80 to re-init
                            // the sound driver) and again when it is released so the
                            // restarted driver sees a clean chip (Genesis Plus GX
                            // resets the FM on both transitions too).
                            if assert_edge || release_edge {
                                self.ym2612.ic_reset();
                            }
                            self.z80_reset = new_reset;
                        }
                    }
                    // SRAM-enable / bank registers ($A130xx). These are bus
                    // observers (the SRAM-enable toggle, a SEGA mapper's bank
                    // latches), not cartridge-window accesses, so they route
                    // through the observe hook rather than the map plan.
                    0xA13000..=0xA130FF => {
                        if let Some(ref mut cart) = self.cartridge {
                            cart.observe_bus_write(addr, value);
                        }
                    }
                    // SVP (and future I/O-page coprocessor) mailbox writes.
                    0xA15000..=0xA1500B => {
                        if let Some(ref mut cart) = self.cartridge {
                            cart.write_bus(addr, value, mc);
                        }
                    }
                    _ => {}
                }
            }
            MemRegion::Reserved => {}
        }
    }

    // ===== Z80 Memory Access from the 68000 (while it holds the Z80 bus) =====

    /// Read from Z80 address space for the 68000, through its
    /// `$A00000-$A0FFFF` window (called only while it holds the Z80 bus)
    ///
    /// **Z80 Memory Map (16-bit address space, as the Z80 sees it):**
    /// - 0x0000-0x1FFF: Z80 RAM (8KB)
    /// - 0x2000-0x3FFF: Z80 RAM mirror
    /// - 0x4000-0x5FFF: YM2612 (memory-mapped)
    /// - 0x6000-0x7FFF: Bank register (0x6000-0x60FF), VDP and PSG (0x7F00-0x7FFF)
    /// - 0x8000-0xFFFF: 68K memory window (banked, 32KB)
    fn z80_read(&mut self, addr: u16) -> u8 {
        match addr {
            // Z80 RAM (8KB)
            0x0000..=0x1FFF => {
                let z80_addr = (addr & 0x1FFF) as u32;
                self.memory.z80_ram.read(z80_addr)
            }

            // Z80 RAM mirror, not mirrored on this side: reads $FF
            0x2000..=0x3FFF => 0xFF,

            // YM2612 (memory-mapped at 0x4000-0x5FFF)
            0x4000..=0x5FFF => {
                let port = (addr & 0x03) as u8;
                self.ym2612.read_port(port)
            }

            // Bank register
            0x6000..=0x7FFF => 0xFF, // Write-only, so it reads $FF

            // The Z80's banked 68K window. Genesis Plus GX and PicoDrive give the
            // 68000 no bank window here: they mirror $A00000-$A07FFF.
            0x8000..=0xFFFF => {
                // TODO: mirror the lower half, as they do
                0xFF
            }
        }
    }

    /// Write to Z80 address space for the 68000, through its
    /// `$A00000-$A0FFFF` window (called only while it holds the Z80 bus)
    ///
    /// **Z80 Memory Map (16-bit address space, as the Z80 sees it):**
    /// - 0x0000-0x1FFF: Z80 RAM (8KB)
    /// - 0x2000-0x3FFF: Z80 RAM mirror
    /// - 0x4000-0x5FFF: YM2612 (memory-mapped)
    /// - 0x6000-0x7FFF: Bank register (0x6000-0x60FF), VDP and PSG (0x7F00-0x7FFF)
    /// - 0x8000-0xFFFF: 68K memory window (banked, 32KB)
    fn z80_write(&mut self, addr: u16, value: u8) {
        match addr {
            // Z80 RAM (8KB)
            0x0000..=0x1FFF => {
                let z80_addr = (addr & 0x1FFF) as u32;
                self.memory.z80_ram.write(z80_addr, value);
            }

            // Z80 RAM mirror, not mirrored on this side: writes are dropped
            0x2000..=0x3FFF => {}

            // YM2612 (memory-mapped at 0x4000-0x5FFF)
            0x4000..=0x5FFF => {
                let port = (addr & 0x03) as u8;
                self.ym2612.write_port(port, value);
            }

            // Bank register
            0x6000..=0x7FFF => {
                // TODO: Implement bank switching
            }

            // The Z80's banked 68K window. Genesis Plus GX and PicoDrive give the
            // 68000 no bank window here: they mirror $A00000-$A07FFF.
            0x8000..=0xFFFF => {
                // TODO: mirror the lower half, as they do
            }
        }
    }
}

impl Default for AddressBus {
    fn default() -> Self {
        Self::new()
    }
}

// Savestate support
/// The 68000's address space as the debugger reads it: every read is an
/// [`AddressBus::peek_byte`] and every write is dropped, so a disassembly
/// that runs over the I/O or VDP ports reads 0 there and changes nothing.
pub struct PeekView<'a>(&'a mut AddressBus);

impl Addressable for PeekView<'_> {
    fn read(&mut self, addr: u32) -> u8 {
        self.0.peek_byte(addr)
    }

    fn write(&mut self, _addr: u32, _value: u8) {}

    /// Big-endian, as the 68000 reads a word.
    fn read_word(&mut self, addr: u32) -> u16 {
        ((self.0.peek_byte(addr) as u16) << 8) | self.0.peek_byte(addr.wrapping_add(1)) as u16
    }
}

use crate::system::savestate::BusState;

impl AddressBus {
    pub fn to_savestate(&self) -> BusState {
        BusState {
            z80_bus_req: self.z80_bus_req,
            z80_reset: self.z80_reset,
            z80_reset_release_pending: self.z80_reset_release_pending,
            z80_bank_register: self.z80_bank_register,
            z80_bank_bit_count: self.z80_bank_bit_count,
            cycles_executed: self.cycles_executed,
        }
    }

    pub fn from_savestate(&mut self, s: &BusState) {
        self.z80_bus_req = s.z80_bus_req;
        self.z80_reset = s.z80_reset;
        self.z80_reset_release_pending = s.z80_reset_release_pending;
        self.z80_bank_register = s.z80_bank_register;
        self.z80_bank_bit_count = s.z80_bank_bit_count;
        self.cycles_executed = s.cycles_executed;
    }
}

impl Addressable for AddressBus {
    fn read(&mut self, addr: u32) -> u8 {
        self.cycles_executed += 4;
        let value = self.read_byte(addr);
        if self.debug_armed {
            self.watch_events
                .push((addr & 0x00FFFFFF, 1, false, value as u16));
        }
        value
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.cycles_executed += 4;
        if self.debug_armed {
            self.watch_events
                .push((addr & 0x00FFFFFF, 1, true, value as u16));
        }
        self.write_byte(addr, value);
    }

    /// Read a 16-bit word (Big Endian - 68000 byte order).
    fn read_word(&mut self, addr: u32) -> u16 {
        self.cycles_executed += 4;
        let addr = addr & 0x00FFFFFF; // 24-bit addressing

        // Optimized word read: single address dispatch for common regions.
        // Avoids calling read_byte() twice (which would do 2 match chains + 2 cycle adds).
        let value = match addr {
            // Cartridge area -- most frequent (instruction fetches, data reads)
            0x000000..=0x7FFFFF => {
                // Track the prefetch pointer for the VDP status open bus: the
                // next word past this fetch is what floats onto undriven status
                // bits (see `open_bus_pc`).
                self.open_bus_pc = addr.wrapping_add(2);
                let mc = self.m68k_cycle;
                if let Some(ref mut cart) = self.cartridge {
                    let hi = cart.read_bus(addr, mc) as u16;
                    let lo = cart.read_bus(addr.wrapping_add(1), mc) as u16;
                    (hi << 8) | lo
                } else {
                    0xFFFF
                }
            }
            // Work RAM -- second most frequent
            0xE00000..=0xFFFFFF => {
                self.open_bus_pc = addr.wrapping_add(2);
                let ram_addr = (addr & 0xFFFF) as usize;
                let data = self.memory.work_ram.as_slice();
                let mask = data.len() - 1;
                let hi = data[ram_addr & mask] as u16;
                let lo = data[(ram_addr + 1) & mask] as u16;
                (hi << 8) | lo
            }
            // VDP -- needs word-level access to avoid double-clearing status flags
            0xC00000..=0xDFFFFF => {
                let v = self.vdp.read_word_port(addr);
                // Control/status port (0xC00004-7 within the mirror): the VDP does
                // not drive status bits 10-15, so they float to the last word on
                // the 68000 bus, the instruction prefetch (checked against
                // Genesis Plus GX).
                // Data/HV ports keep their own open-bus (FIFO) handling.
                if (addr & 0x1C) == 0x04 {
                    let ob = self.open_bus_word(self.open_bus_pc);
                    (v & 0x03FF) | (ob & 0xFC00)
                } else {
                    v
                }
            }
            // All other regions: fallback to 2x read_byte
            _ => {
                let high = self.read_byte(addr) as u16;
                let low = self.read_byte(addr.wrapping_add(1)) as u16;
                (high << 8) | low
            }
        };
        if self.debug_armed {
            self.watch_events.push((addr, 2, false, value));
        }
        value
    }

    /// Write a 16-bit word (Big Endian - 68000 byte order).
    /// Optimized to avoid double dispatch for common regions.
    fn write_word(&mut self, addr: u32, value: u16) {
        self.cycles_executed += 4;
        let addr = addr & 0x00FFFFFF;
        if self.debug_armed {
            self.watch_events.push((addr, 2, true, value));
        }
        let page = (addr >> 16) as usize;
        match PAGE_TABLE[page] {
            // Work RAM - direct write, no double dispatch
            MemRegion::WorkRam => {
                let ram_addr = (addr & 0xFFFF) as usize;
                let data = self.memory.work_ram.as_mut_slice();
                let mask = data.len() - 1;
                data[ram_addr & mask] = (value >> 8) as u8;
                data[(ram_addr + 1) & mask] = (value & 0xFF) as u8;
                return;
            }
            // Cartridge - direct write
            MemRegion::Cartridge => {
                let mc = self.m68k_cycle;
                if let Some(ref mut cart) = self.cartridge {
                    cart.write_bus(addr, (value >> 8) as u8, mc);
                    cart.write_bus(addr.wrapping_add(1), (value & 0xFF) as u8, mc);
                }
                return;
            }
            _ => {}
        }

        // VDP needs word-level access for proper command handling
        match addr {
            0xC00000..=0xDFFFFF => {
                // PSG occupies addresses with bit 4 set within VDP space.
                // For 68K word writes (always even-aligned), the low byte (D0-D7)
                // reaches the PSG at the odd address (addr+1).
                if (addr & 0x10) != 0 {
                    self.psg.write((value & 0xFF) as u8);
                } else {
                    self.vdp.write_word_port(addr, value);

                    // Check if a Memory-to-VRAM DMA was triggered
                    if self.vdp.is_dma_pending() {
                        let regs = *self.vdp.registers();
                        let raw_length = ((regs[20] as u16) << 8) | (regs[19] as u16);
                        // DMA length 0 means 65536 words (pre-decrementing counter)
                        let length: u32 = if raw_length == 0 {
                            0x10000
                        } else {
                            raw_length as u32
                        };
                        let addr_low = regs[21] as u32;
                        let addr_mid = regs[22] as u32;
                        // Use 7 bits: bit 6 of R23 is A23 (selects RAM vs ROM space)
                        let addr_high = (regs[23] & 0x7F) as u32;
                        let source = ((addr_high << 16) | (addr_mid << 8) | addr_low) << 1;

                        self.record_memory_dma(source, length);
                        let mut dma_buffer = Vec::with_capacity(length as usize * 2);
                        let mc = self.m68k_cycle;
                        let ram_slice = self.memory.work_ram.as_slice();
                        // Source wraps within its 128 KB window (reg23 base fixed);
                        // see the matching comment in `write_byte`'s DMA path.
                        let window_base = source & 0x00FE0000;
                        let start_word = (source & 0x1FFFF) >> 1;
                        for i in 0..(length * 2) {
                            let word_off = start_word.wrapping_add(i >> 1) & 0xFFFF;
                            let src_addr = window_base | (word_off << 1) | (i & 1);
                            let byte = match src_addr {
                                0x000000..=0x7FFFFF => {
                                    if let Some(ref mut cart) = self.cartridge {
                                        cart.read_bus(src_addr, mc)
                                    } else {
                                        0xFF
                                    }
                                }
                                0xE00000..=0xFFFFFF => {
                                    let ram_addr = (src_addr & 0xFFFF) as usize;
                                    if ram_addr < ram_slice.len() {
                                        ram_slice[ram_addr]
                                    } else {
                                        0xFF
                                    }
                                }
                                _ => 0xFF,
                            };
                            dma_buffer.push(byte);
                        }
                        self.vdp.execute_pending_dma(&dma_buffer);

                        // Note: Memory-to-VRAM DMA halts the 68K while the bus is occupied,
                        // but MAME's totalcycles counter does NOT include the halt time.
                        // The DMA timing is handled by MAME's scheduler/screen device separately.
                        // Therefore we do NOT add DMA halt cycles to the CPU cycle count.
                    }

                    // Collect any FIFO wait cycles from data port writes
                    let fifo_wait = self.vdp.pending_cpu_wait_cycles;
                    self.vdp.pending_cpu_wait_cycles = 0;
                    self.vdp_wait_cycles += fifo_wait;
                }
            }
            _ => {
                self.write_byte(addr, (value >> 8) as u8);
                self.write_byte(addr.wrapping_add(1), (value & 0xFF) as u8);
            }
        }
    }

    fn cycles(&self) -> Option<u64> {
        Some(self.cycles_executed())
    }
}

impl CyclesTrackable for AddressBus {
    fn cycles_executed(&self) -> u64 {
        self.cycles_executed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::composable::build_genesis_cartridge;

    #[test]
    fn test_bus_creation() {
        let bus = AddressBus::new();
        assert!(!bus.z80_bus_req);
    }

    #[test]
    fn test_rom_access() {
        let mut bus = AddressBus::new();

        // Create minimal ROM
        let mut rom = vec![0; 512];
        rom[0..8].copy_from_slice(&[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0]);
        rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");

        let cartridge = build_genesis_cartridge(rom).unwrap();
        bus.set_cartridge(cartridge);

        // Test byte reads
        assert_eq!(bus.read(0x000000), 0x12);
        assert_eq!(bus.read(0x000001), 0x34);
        assert_eq!(bus.read(0x000007), 0xF0);

        // Test word reads (Big Endian)
        assert_eq!(bus.read_word(0x000000), 0x1234);
        assert_eq!(bus.read_word(0x000002), 0x5678);
    }

    #[test]
    fn test_work_ram_access() {
        let mut bus = AddressBus::new();

        // Write to Work RAM
        bus.write(0xE00000, 0x42);
        bus.write(0xE00001, 0x43);

        // Read back
        assert_eq!(bus.read(0xE00000), 0x42);
        assert_eq!(bus.read(0xE00001), 0x43);
    }

    #[test]
    fn test_work_ram_mirroring() {
        let mut bus = AddressBus::new();

        // Write at base address
        bus.write(0xE00100, 0xAA);

        // Read from mirrored addresses
        assert_eq!(bus.read(0xE10100), 0xAA); // +64KB
        assert_eq!(bus.read(0xE20100), 0xAA); // +128KB
        assert_eq!(bus.read(0xFF0100), 0xAA); // High mirror
    }

    #[test]
    fn test_z80_ram_access() {
        let mut bus = AddressBus::new();

        // Request Z80 bus first (68K needs control to access Z80 space)
        // Z80 is in reset by default, so bus is granted immediately (no delay)
        bus.write(0xA11100, 0x01);
        assert_eq!(bus.read(0xA11100), 0x00); // Bus granted immediately (Z80 in reset)

        // Write to Z80 RAM
        bus.write(0xA00000, 0x55);
        bus.write(0xA00100, 0x66);

        // Read back
        assert_eq!(bus.read(0xA00000), 0x55);
        assert_eq!(bus.read(0xA00100), 0x66);

        // Release bus
        bus.write(0xA11100, 0x00);
    }

    #[test]
    fn test_z80_bus_request() {
        let mut bus = AddressBus::new();

        // Initially, Z80 has the bus - reading returns 0x01 (bus held by Z80)
        assert_eq!(bus.read(0xA11100), 0x01);

        // Z80 RAM returns 0xFF when 68K doesn't have the bus
        assert_eq!(bus.read(0xA00000), 0xFF);

        // Release Z80 from reset so it is "running", a BUSREQ now has to wait
        // for the Z80 to reach a bus boundary before BUSACK reports granted.
        bus.write(0xA11200, 0x01);

        // Request Z80 bus (68K wants control).
        bus.write(0xA11100, 0x01);

        // BUSACK reports "busy" (D0 set) until the 68K advances past the grant
        // latency, this is the spin the BTST/BNE poll loop performs on hardware.
        assert_eq!(bus.read(0xA11100), 0x01);

        // Advance the 68K position past the grant delay; BUSACK now reports
        // "granted" (D0 clear).
        bus.m68k_cycle += 32;
        assert_eq!(bus.read(0xA11100), 0x00);

        // Now 68K can access Z80 RAM
        bus.write(0xA00000, 0x11);
        assert_eq!(bus.read(0xA00000), 0x11);

        // Release bus (give it back to Z80)
        bus.write(0xA11100, 0x00);
        // Reading returns 0x01: bus held by Z80 again
        assert_eq!(bus.read(0xA11100), 0x01);

        // Z80 RAM returns 0xFF again when 68K doesn't have the bus
        assert_eq!(bus.read(0xA00000), 0xFF);
    }

    #[test]
    fn test_z80_bus_access_charges_wait_cycles() {
        let mut bus = AddressBus::new();

        // Z80 in reset by default -> BUSREQ is granted immediately.
        bus.write(0xA11100, 0x01);
        assert_eq!(bus.z80_bus_wait_cycles, 0);

        // Each 68K access to the Z80 bus ($A00000-$A0FFFF) charges one stall cycle.
        // The system step folds and
        // clears this into the instruction's total_cycles.
        bus.read(0xA00000);
        assert_eq!(bus.z80_bus_wait_cycles, 1);
        bus.write(0xA00001, 0x55);
        assert_eq!(bus.z80_bus_wait_cycles, 2);

        // No penalty once the bus is handed back: the access is rejected (0xFF) and
        // must not accrue stall cycles.
        bus.write(0xA11100, 0x00);
        assert_eq!(bus.read(0xA00000), 0xFF);
        assert_eq!(bus.z80_bus_wait_cycles, 2);
    }

    #[test]
    fn test_z80_reset_line_resets_ym2612() {
        let mut bus = AddressBus::new();

        // Arm the FM chip with audible state.
        bus.ym2612_mut().write_register(0, 0x2B, 0x80); // DAC enable
        bus.ym2612_mut().set_dac_value(100);
        assert!(bus.ym2612().is_dac_enabled());

        // Releasing the Z80 reset line ($A11200 bit0 = 1) pulses /IC on the way out
        // (the FM chip's reset pin follows the Z80 reset line).
        bus.write(0xA11200, 0x01);
        assert!(
            !bus.ym2612().is_dac_enabled(),
            "release edge should reset FM"
        );
        assert_eq!(bus.ym2612().get_dac_value(), 0);

        // Re-arm, then assert reset ($A11200 bit0 = 0): the chip is cleared again.
        bus.ym2612_mut().write_register(0, 0x2B, 0x80);
        bus.ym2612_mut().set_dac_value(100);
        assert!(bus.ym2612().is_dac_enabled());

        bus.write(0xA11200, 0x00);
        assert!(
            !bus.ym2612().is_dac_enabled(),
            "assert edge should reset FM"
        );
        assert_eq!(bus.ym2612().get_dac_value(), 0);
    }

    #[test]
    fn test_endianness() {
        let mut bus = AddressBus::new();

        // Write word (Big Endian)
        bus.write_word(0xE00000, 0x1234);

        // Check byte order
        assert_eq!(bus.read(0xE00000), 0x12); // High byte first
        assert_eq!(bus.read(0xE00001), 0x34); // Low byte second

        // Read word back
        assert_eq!(bus.read_word(0xE00000), 0x1234);
    }

    #[test]
    fn test_rom_writes_ignored() {
        let mut bus = AddressBus::new();
        // The cartridge factory validates the Genesis header, so a
        // raw zero buffer is rejected. Build a minimal ROM that
        // passes header validation: "SEGA" at offset 0x100. We need
        // the buffer to extend past 0x200 so the bus read of that
        // address has a non-header byte to inspect.
        let mut rom = vec![0x00; 0x400];
        rom[0x100..0x110].copy_from_slice(b"SEGA GENESIS    ");
        rom[0x200] = 0x12; // sentinel byte we'll try to overwrite

        let cartridge = build_genesis_cartridge(rom).unwrap();
        bus.set_cartridge(cartridge);

        // Read original value from a non-header location
        let original = bus.read(0x000200);

        // Try to write to ROM
        bus.write(0x000200, 0xFF);

        // Should still be original value (write ignored)
        assert_eq!(bus.read(0x000200), original);
    }

    #[test]
    fn test_open_bus() {
        let mut bus = AddressBus::new();
        // No ROM loaded

        // Reading from ROM area should return 0xFF (open bus)
        assert_eq!(bus.read(0x000000), 0xFF);
        assert_eq!(bus.read(0x100000), 0xFF);
    }

    #[test]
    fn test_peek_byte_reads_memories_without_side_effects() {
        let mut bus = AddressBus::new();

        // Z80 RAM written while the 68000 holds the bus reads back through
        // the peek once the bus is handed back, and from its mirror too.
        bus.write(0xA11100, 0x01);
        bus.write(0xA00010, 0x5A);
        bus.write(0xA11100, 0x00);
        let stalls = bus.z80_bus_wait_cycles;
        assert_eq!(bus.peek_byte(0xA00010), 0x5A);
        assert_eq!(bus.peek_byte(0xA02010), 0x5A);
        assert_eq!(bus.z80_bus_wait_cycles, stalls, "a peek charges no stall");

        bus.write(0xFF0020, 0x33);
        assert_eq!(bus.peek_byte(0xFF0020), 0x33);

        // Ports read 0: the I/O version register, the VDP's status.
        assert_eq!(bus.peek_byte(0xA10001), 0);
        assert_eq!(bus.peek_byte(0xC00004), 0);
    }
}
