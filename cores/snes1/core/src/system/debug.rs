//! The debugger's side of the machine: the CPUs' disassembly, register
//! snapshots and trace entries, and the checks the debugger makes around a
//! step (execute, watch, call stack, events).

use retro_core::cartridge::Cartridge;
use retro_core::cpu::Addressable;
use retro_core::debugger::DebuggerCore;
use retro_core::frame::DebugMachine;
use retro_core::{DisasmRow, DisasmRowKind, InstructionTrace, StackFrame};

use wdc65816::Wdc65816;

use retro_core::debug::{ByteOrder, CpuExprContext};

use super::SnesEmulator;
use crate::debugger::{
    cpu_state_65816, snes_events, SnesEvent, TraceEntryBinary65816, CARTRIDGE_CPU, MAIN_CPU,
    SOUND_CPU,
};
use crate::expr_ctx::wdc65816_register;
use crate::trace_format::{effective_address, EffRegs};

use crate::bus::{SnesBus, SnesPeekBus};

impl SnesEmulator {
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

    /// Write `bytes` into VRAM from word `word_address` on, the way a
    /// debugger edits graphics ([`crate::ppu::Ppu::write_vram_debug`]):
    /// straight into memory, so the timing, the CPUs and every register
    /// stay as they were.
    pub fn write_vram_debug(&mut self, word_address: u32, bytes: &[u8]) {
        self.bus.ppu_mut().write_vram_debug(word_address, bytes);
    }

    /// Write `colours` into CGRAM from colour `colour_index` on, the way a
    /// debugger edits a palette ([`crate::ppu::Ppu::write_cgram_debug`]).
    pub fn write_cgram_debug(&mut self, colour_index: u32, colours: &[u16]) {
        self.bus.ppu_mut().write_cgram_debug(colour_index, colours);
    }

    /// Write `bytes` into OAM from `byte_address` on, the way a debugger
    /// edits sprites ([`crate::ppu::Ppu::write_oam_debug`]).
    pub fn write_oam_debug(&mut self, byte_address: u32, bytes: &[u8]) {
        self.bus.ppu_mut().write_oam_debug(byte_address, bytes);
    }

    /// Whether a repaint or a layer render made now draws the frame from
    /// its line table, each line with its own settings and the video
    /// memory it read ([`crate::ppu::Ppu::draws_from_line_states`]), rather
    /// than from the registers as they stand.
    pub fn draws_from_line_states(&self) -> bool {
        self.bus.ppu().draws_from_line_states()
    }

    /// The PPU's settings, structured ([`crate::ppu::PpuSettings`]).
    pub fn ppu_settings(&self) -> crate::ppu::PpuSettings {
        self.bus.ppu().settings()
    }

    /// A [`CartridgeDebugger`](retro_core::cartridge::CartridgeDebugger) over
    /// the installed cartridge, bound to the live breakpoint list:
    /// memory-map / ROM / SRAM regions from the installed
    /// [`crate::cartridge::MapPlan`], the mapper state (the preset's
    /// [`crate::cartridge::PresetKind::mapper_id`]), the per-chip coprocessor
    /// snapshots, and ROM/SRAM breakpoints delegating into the emulator's
    /// own breakpoint list. The same surface as the Mega Drive's;
    /// [`EmulatorCore::cartridge_debugger`] is it boxed.
    pub fn map_plan_debugger(
        &mut self,
    ) -> retro_core::cartridge::MapPlanDebugger<'_, crate::cartridge::MapSlot> {
        let plan = self.bus.cartridge_plan().cloned().unwrap_or_default();
        let rom_size = self.rom_size as u32;
        let sram_size = self.bus.cartridge_sram().map(<[u8]>::len).unwrap_or(0) as u32;
        let mapper_id = self.preset_kind.map(|kind| kind.mapper_id(&plan.name));
        // Snapshot every coprocessor's state and label up front (owned), so
        // the immutable cartridge borrow ends before we borrow the
        // breakpoint list mutably below.
        let (chip_states, chip_labels) = self
            .bus
            .cartridge()
            .map(|c| (c.coprocessors_debug_state(), c.coprocessor_labels()))
            .unwrap_or_default();
        retro_core::cartridge::MapPlanDebugger::new(
            plan,
            rom_size,
            sram_size,
            mapper_id,
            chip_states,
            self.debugger.breakpoints_mut(),
        )
        .with_chip_labels(chip_labels)
    }

    /// Human-readable label of the primary (lowest-id) coprocessor (e.g.
    /// `"DSP-1B (LLE)"`), or `None` for a plain cartridge. Used by the
    /// CLI `info` command.
    pub fn coprocessor_label(&self) -> Option<&str> {
        self.bus.cartridge()?.coprocessor_label()
    }

    /// Count of CPU accesses to the coprocessor's register window since
    /// the last reset (`0` when no coprocessor is installed or the game has
    /// not touched it yet). A positive value proves the running game is
    /// actively driving the chip.
    pub fn coprocessor_accesses(&self) -> u64 {
        self.bus
            .cartridge()
            .map(|c| c.coprocessor_accesses())
            .unwrap_or(0)
    }

    /// Live state of the primary (lowest-id) coprocessor (the DSP-1's
    /// uPD7725 registers + data RAM), or `None` for a plain cartridge.
    /// Read-only path for the CLI `info` command and the WASM debugger; it
    /// does not borrow the breakpoint list like
    /// [`Self::map_plan_debugger`] does. See
    /// [`Self::coprocessors_debug_state`] for the full set.
    pub fn coprocessor_debug_state(&self) -> Option<retro_core::cartridge::ExpansionChipState> {
        self.bus.cartridge()?.coprocessor_debug_state()
    }

    /// Live state of every installed coprocessor, keyed by `chip_id`. Empty
    /// for a plain cartridge.
    pub fn coprocessors_debug_state(&self) -> Vec<(u8, retro_core::cartridge::ExpansionChipState)> {
        self.bus
            .cartridge()
            .map(|c| c.coprocessors_debug_state())
            .unwrap_or_default()
    }

    /// Human-readable label of every installed coprocessor, keyed by
    /// `chip_id` (e.g. `[(1, "DSP-1B (LLE)")]`). Empty for a plain
    /// cartridge. Used by the CLI `info` command to enumerate the chips.
    pub fn coprocessor_labels(&self) -> Vec<(u8, String)> {
        self.bus
            .cartridge()
            .map(|c| c.coprocessor_labels())
            .unwrap_or_default()
    }

    /// Snapshot the live 65C816 state into a binary trace entry, resolving
    /// the effective-address bracket against memory (indirect modes need
    /// it), read through the bus's side-effect-free peek. Called **before**
    /// the instruction executes, matching bsnes-plus, so the registers and
    /// raster position describe the state the instruction sees.
    pub(super) fn capture_trace_65816(&self) -> TraceEntryBinary65816 {
        let pbr = self.cpu.pbr;
        let pc = self.cpu.pc;
        let pbr_pc = ((pbr as u32) << 16) | pc as u32;
        let p = self.cpu.p;
        let emulation = self.cpu.emulation;
        // Operand width follows the M (memory/accumulator) and X (index)
        // flags; in emulation mode both are forced 8-bit.
        let m8 = emulation || (p & 0x20) != 0;
        let x8 = emulation || (p & 0x10) != 0;

        let mut bus = SnesPeekBus(&self.bus);
        let opcode = bus.read(pbr_pc);
        let info = &wdc65816::decode::OPCODE_TABLE[opcode as usize];
        let size = (1 + info.mode.operand_size(m8, x8)).min(4);

        let mut bytes = [0u8; 4];
        bytes[0] = opcode;
        for i in 1..size {
            // Operand bytes are fetched within the program bank, wrapping
            // the 16-bit PC, exactly as the CPU's own fetch does.
            let addr = ((pbr as u32) << 16) | (pc.wrapping_add(i as u16) as u32);
            bytes[i as usize] = bus.read(addr);
        }

        let regs = EffRegs {
            d: self.cpu.d,
            x: self.cpu.x,
            y: self.cpu.y,
            s: self.cpu.sp,
            db: self.cpu.dbr,
            pbr_pc,
        };
        let eff = effective_address(&mut bus, info.mode, &bytes[..size as usize], &regs);

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

    /// Mirror the debugger's SPC700 tracing flag into the APU so
    /// [`crate::apu::Apu::catch_up_to`] captures a trace entry before each
    /// SPC700 step. The APU runs via lazy catch-up deep inside mailbox
    /// accesses where the debugger isn't reachable, so this flag is the
    /// bridge, kept fresh at every debug entry point.
    pub(super) fn sync_spc_trace_enable(&mut self) {
        let on = self.debugger.tracing(SOUND_CPU);
        self.bus.apu_mut().set_spc_trace_enabled(on);
    }

    /// Move the SPC700 trace entries the APU captured this frame into the
    /// debugger's SPC700 trace, preserving SPC execution order. Draining
    /// empties the APU buffer for the next catch-up window.
    pub(super) fn drain_spc_trace(&mut self) {
        let entries = self.bus.apu_mut().take_spc_trace();
        for entry in &entries {
            self.debugger.record_trace(SOUND_CPU, entry);
        }
    }

    /// Mirror the debugger's tracing flag for the cartridge's CPU into the
    /// cartridge so the chip's `catch_up` burst captures a trace entry
    /// before each step of its CPU, the Super FX's GSU or the Cx4. That CPU
    /// runs deep inside the chip (mailbox catch-up + the per-instruction
    /// burst hook) where the debugger isn't reachable, so this flag is the
    /// bridge, kept fresh at every debug entry point. A cartridge with no
    /// CPU that traces (a plain one, an SA-1) never has the flag set: its
    /// slot takes no trace.
    pub(super) fn sync_cartridge_trace_enable(&mut self) {
        let on = self.debugger.tracing(CARTRIDGE_CPU);
        if let Some(cart) = self.bus.cartridge_mut() {
            cart.set_secondary_trace_enabled(on);
        }
    }

    /// Move the trace entries the cartridge's CPU captured this frame into
    /// the debugger's trace for it, preserving execution order. Draining
    /// empties the chip's buffer for the next catch-up window. The slot's
    /// entry size is the installed chip's (the GSU's or the Cx4's), set
    /// when the ROM loaded.
    pub(super) fn drain_cartridge_trace(&mut self) {
        let raw = self
            .bus
            .cartridge_mut()
            .map(|c| c.take_secondary_trace())
            .unwrap_or_default();
        if !raw.is_empty() {
            self.debugger.record_trace_bytes(CARTRIDGE_CPU, &raw);
        }
    }

    /// Mirror the debugger's "has watch/event breakpoints" state into the
    /// emulator flag, the bus (CPU access recording) and the APU (DSP
    /// register-write recording). Called at every debug entry so the
    /// recorders are armed exactly when an event/watchpoint could fire,
    /// and disarmed (clearing their buffers) otherwise.
    pub(super) fn sync_debug_arm(&mut self) {
        let armed = self.debugger.has_watchpoints_or_events();
        self.arm_debug_recorders(armed);
    }

    /// Arm or disarm what the watchpoints and the event breakpoints read:
    /// the emulator flag that gates the event checks, the bus's record of
    /// the CPU's accesses, the APU's record of S-DSP register writes and
    /// the PPU's palette and tile watch. Disarming drops what they hold.
    fn arm_debug_recorders(&mut self, armed: bool) {
        self.debug_armed = armed;
        self.bus.set_debug_armed(armed);
        self.bus.apu_mut().set_dsp_event_recording(armed);
        let watch = if armed {
            self.video_memory_watch()
        } else {
            None
        };
        self.bus.ppu_mut().set_event_watch(watch);
    }

    /// Set the debugger's hooks as a frame call with no breakpoint and no
    /// trace has them: what the APU and the cartridge's CPU traced so far
    /// moved to the debugger and their trace capture off, and nothing armed
    /// for a watchpoint or an event, so a frame run now leaves nothing
    /// behind for a later check to find. The next debug entry point arms
    /// them again from the debugger's state.
    pub(super) fn disarm_debugger_hooks(&mut self) {
        self.drain_spc_trace();
        self.drain_cartridge_trace();
        self.bus.apu_mut().set_spc_trace_enabled(false);
        if let Some(cart) = self.bus.cartridge_mut() {
            cart.set_secondary_trace_enabled(false);
        }
        self.arm_debug_recorders(false);
    }

    /// The palette colours and the tiles the enabled `PaletteWrite` and
    /// `TileChange` event breakpoints watch, or `None` when they watch
    /// nothing: the PPU then tests one bit per write instead of the
    /// breakpoint list.
    fn video_memory_watch(&self) -> Option<crate::ppu::EventWatch> {
        let breakpoints = self.debugger.breakpoints();
        let colours = breakpoints.watched_params(snes_events::PALETTE_WRITE);
        let tiles = breakpoints.watched_params(snes_events::TILE_CHANGE);
        (colours.is_some() || tiles.is_some()).then(|| crate::ppu::EventWatch {
            colours: colours.unwrap_or_default(),
            tiles: tiles.unwrap_or_default(),
        })
    }

    /// Drain the palette and tile events the PPU raised and match them
    /// against their event breakpoints. Returns `true` when any matched;
    /// every event is checked, so the hit counts stay right.
    pub(super) fn drain_ppu_debug_events(&mut self) -> bool {
        match self.bus.ppu_mut().raised_debug_events() {
            Some(raised) if !raised.is_empty() => self.debugger.check_raised(raised),
            _ => false,
        }
    }

    /// Check the instruction's recorded CPU accesses against the
    /// watchpoints (each watch at most once, reading its whole value after
    /// the instruction) and its PPU and APU writes against their events,
    /// then drop the records. Returns `true` when any breakpoint matched;
    /// every check runs (no early exit) so per-breakpoint hit counts stay
    /// accurate.
    pub(super) fn drain_bus_debug_events(&mut self) -> bool {
        // The palette and tile events the instruction's stores and its
        // general DMA raised in the PPU.
        let mut hit = self.drain_ppu_debug_events();
        if !self.bus.accesses().is_empty() {
            let mut context = expr_context(&self.cpu, &self.bus);
            hit |= self
                .debugger
                .check_accesses(MAIN_CPU, self.bus.accesses(), &mut context);
        }
        hit |= self.debugger.check_raised(self.bus.raised_debug_events());
        self.bus.clear_debug_events();
        hit
    }

    /// Drain the S-DSP register-write indices the APU captured (via the
    /// `$F3` DSPDATA path) and match them against `DspRegisterWrite`
    /// event breakpoints. Returns `true` when any matched.
    pub(super) fn drain_apu_dsp_events(&mut self) -> bool {
        let regs = self.bus.apu_mut().take_dsp_events();
        let mut hit = false;
        for reg in regs {
            hit |= self.debugger.check_event(SnesEvent::DspRegisterWrite(reg));
        }
        hit
    }

    /// What the call stack needs to know of a 65C816 instruction before it
    /// runs: its address and opcode (read through the peek).
    pub(super) fn step_start_65816(&self, pc24: u32) -> StepStart {
        StepStart {
            pc24,
            opcode: self.bus.peek(pc24),
        }
    }

    /// Keep the 65C816 call stack after the instruction that began as
    /// `start`: push a frame on a subroutine or software-interrupt call
    /// (JSR `$20` / JSR(abs,x) `$FC` / JSL `$22` / BRK `$00` / COP `$02`)
    /// and pop one on a return (RTS `$60` / RTL `$6B` / RTI `$40`). An
    /// interrupt the CPU takes instead of an instruction pushes its frame
    /// through the frame driver's `interrupt_taken`, the interrupted
    /// address as both the call and the return, which the handler's RTI
    /// pops. The return address is the next instruction, wrapped inside the
    /// program bank like the CPU's own PC; the `function_name` stays `None`
    /// (the core resolves no symbols).
    pub(super) fn maintain_call_stack_65816(&mut self, start: StepStart) {
        let pc24 = start.pc24;
        let bank = pc24 & 0x00FF_0000;
        let call = |size: u16| StackFrame {
            call_address: pc24,
            return_address: bank | (pc24 as u16).wrapping_add(size) as u32,
            function_name: None,
        };
        match start.opcode {
            0x20 | 0xFC => self.debugger.push_call(MAIN_CPU, call(3)),
            0x22 => self.debugger.push_call(MAIN_CPU, call(4)),
            0x00 | 0x02 => self.debugger.push_call(MAIN_CPU, call(2)),
            0x60 | 0x6B | 0x40 => self.debugger.pop_call(MAIN_CPU),
            _ => {}
        }
    }

    /// The history entry of the instruction at `pc24`, made before it runs:
    /// its disassembly (read through the peek) and the registers it starts
    /// from. Made only on the steps a host asks for, where building the
    /// disassembly string and a register snapshot per instruction is
    /// affordable.
    pub(super) fn history_entry_65816(&self, pc24: u32) -> InstructionTrace {
        let state = wdc65816::disasm::DisasmState {
            emulation: self.cpu.emulation,
            flag_m: self.cpu.p & 0x20 != 0,
            flag_x: self.cpu.p & 0x10 != 0,
        };
        let mut bus = SnesPeekBus(&self.bus);
        let (instruction, _len) = wdc65816::disasm::disassemble_at(&mut bus, pc24, &state);
        let opcode = self.bus.peek(pc24);
        let cycle_count = wdc65816::decode::OPCODE_TABLE[opcode as usize].base_cycles as u32;
        InstructionTrace {
            address: pc24,
            instruction,
            registers_before: cpu_state_65816(&self.cpu).registers,
            cycle_count,
        }
    }
}

/// What [`SnesEmulator::maintain_call_stack_65816`] needs to know of a
/// 65C816 instruction before it runs.
#[derive(Clone, Copy)]
pub struct StepStart {
    /// The address of the instruction.
    pc24: u32,
    /// Its opcode, read through the peek.
    opcode: u8,
}

/// The context a breakpoint's condition and a watch's value are read in on
/// the 65C816: its registers by name ([`wdc65816_register`]) and memory
/// through the bus's side-effect-free peek, little-endian.
pub(super) fn expr_context<'a>(
    cpu: &'a Wdc65816,
    bus: &'a SnesBus,
) -> CpuExprContext<impl FnMut(&str) -> Option<i64> + 'a, impl FnMut(u32) -> u8 + 'a> {
    CpuExprContext::new(
        move |name| wdc65816_register(cpu, name),
        move |address| bus.peek(address),
        ByteOrder::LittleEndian,
    )
}

/// Side-effect-free [`Addressable`] view of the APU's SPC700 memory for
/// disassembly. Reads resolve only the IPL-ROM window + ARAM (never a
/// DSP data port or a read-clearing timer), and writes are dropped, so
/// rendering a disassembly window can't perturb the audio program.
pub(super) struct SpcPeekBus<'a> {
    pub(super) apu: &'a crate::apu::Apu,
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
            .cartridge()
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
        let res = wdc65816::disasm::disassemble_one(&mut SnesPeekBus(&emu.bus), addr24, &state);
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
/// briefly inside [`EmulatorCore::disassemble_window`] so the emulator
/// can be borrowed mutably across the (backward + forward) walk.
pub(super) struct SnesDisasmDriver<'a> {
    pub(super) emu: &'a mut SnesEmulator,
    pub(super) cpu_id: usize,
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
            // The slot-2 CPU (the SA-1's 65816, the GSU or the Cx4) has no
            // coverage bitmap, so every row decodes as `Code` rather than an
            // `Unknown` placeholder.
            CARTRIDGE_CPU => true,
            SOUND_CPU => DebuggerCore::is_covered(&self.emu.debugger, SOUND_CPU, addr),
            _ => DebuggerCore::is_covered(&self.emu.debugger, MAIN_CPU, addr),
        }
    }

    fn decode_at(&mut self, addr: u32) -> DisasmRow {
        decode_one_snes(self.emu, self.cpu_id, addr)
    }
}

/// The 65C816's side of the debugger's tiers: the frame driver runs the
/// debugger's work around a step, and these are the pieces this console
/// does its own way.
impl DebugMachine for SnesEmulator {
    type Debugger = crate::debugger::SnesDebugger;

    fn debug_state(&self) -> &Self::Debugger {
        &self.debugger
    }

    fn debug_state_mut(&mut self) -> &mut Self::Debugger {
        &mut self.debugger
    }

    fn armed(&self) -> bool {
        self.debug_armed
    }

    /// The SPC700's and the cartridge CPU's trace capture as the
    /// debugger traces them, and the event and access recorders as its
    /// breakpoints watch.
    fn arm(&mut self) {
        self.sync_spc_trace_enable();
        self.sync_cartridge_trace_enable();
        self.sync_debug_arm();
    }

    fn disarm(&mut self) {
        self.disarm_debugger_hooks();
    }

    /// The registers before the instruction, and memory through the peek.
    fn check_execute(&mut self, pc: u32) -> bool {
        let mut context = expr_context(&self.cpu, &self.bus);
        self.debugger
            .check_execute(MAIN_CPU, pc, Some(&mut context))
    }

    type CallStart = StepStart;

    fn call_start(&self, pc: u32) -> StepStart {
        self.step_start_65816(pc)
    }

    fn call_finish(&mut self, start: StepStart) {
        self.maintain_call_stack_65816(start);
    }

    /// The 65C816's entry is whole before its instruction runs, so it is
    /// recorded there.
    type TraceStart = ();

    fn trace_start(&mut self, _pc: u32) {
        let entry = self.capture_trace_65816();
        self.debugger.record_trace(MAIN_CPU, &entry);
    }

    fn trace_finish(&mut self, _start: (), _cycles: u32) {}

    type HistoryStart = InstructionTrace;

    fn history_start(&self, pc: u32) -> InstructionTrace {
        self.history_entry_65816(pc)
    }

    fn history_finish(&mut self, entry: InstructionTrace, _cycles: u32) {
        self.debugger.record_history(MAIN_CPU, entry);
    }

    /// Drop what was recorded between two instructions (a host's write
    /// while the frame was stopped, an interrupt's stack pushes) so the
    /// watchpoints see only this instruction's own accesses.
    fn arm_access_log(&mut self) -> bool {
        if self.debug_armed {
            self.bus.clear_debug_events();
        }
        self.debug_armed
    }

    fn check_access_log(&mut self, _pc: u32) -> bool {
        self.drain_bus_debug_events()
    }

    /// What the SPC700 and the cartridge's CPU traced, in their order.
    fn flush_traces(&mut self) {
        self.drain_spc_trace();
        self.drain_cartridge_trace();
    }
}
