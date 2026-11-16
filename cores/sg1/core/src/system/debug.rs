//! The debugger's side of the machine: the CPUs' disassembly, register
//! snapshots and trace entries, and the checks the debugger makes around a
//! step (execute, watch, call stack, events).

use super::GenesisEmulator;
#[cfg(doc)]
use crate::bus::AddressBus;
use crate::cpu68k::calls::{call_step, CallStep};
use crate::debugger::{video_event_watch, GenesisDebugger, GenesisEvent, MAIN_CPU, SOUND_CPU};
use crate::expr_ctx::m68k_register;
use crate::vdp::dma::MemoryDma;
use retro_core::cpu::Addressable;
use retro_core::debug::{Access, ByteOrder, CpuExprContext};

use retro_core::*;

/// Plausible instruction-length offsets `disassemble_window` probes
/// when walking backward from PC. 68K instructions are 2-10 bytes and
/// always even; Z80 instructions are 1-4 bytes (1 byte for plain
/// opcodes through 4 bytes for `DD/FD CB` prefixed instructions).
/// Offsets are tried smallest-first so the shortest matching
/// predecessor wins.
fn probe_offsets(cpu_id: usize) -> &'static [u32] {
    if cpu_id == SOUND_CPU {
        &[1, 2, 3, 4]
    } else {
        &[2, 4, 6, 8, 10]
    }
}

/// Decode a single instruction at `addr` for the given CPU into a
/// `Code`-kind `DisasmRow`. Shared by the `disassemble_window` driver
/// and `GenesisEmulator::decode_at`.
fn decode_one(emu: &mut GenesisEmulator, cpu_id: usize, addr: u32) -> DisasmRow {
    if cpu_id == SOUND_CPU {
        // Z80: reuse the trace-format-aware decoder.
        let z80_bus = emu.bus.create_z80_bus();
        let (text, size) =
            crate::z80::disasm::disassemble_at(&mut z80_bus.peek_view(), addr as u16);
        let trace_text = crate::z80::disasm::to_trace_format(&text);
        let (mnemonic, operands) = if let Some(pos) = trace_text.find(' ') {
            (
                trace_text[..pos].to_string(),
                trace_text[pos + 1..].trim().to_string(),
            )
        } else {
            (trace_text.clone(), String::new())
        };
        let mut bytes = Vec::with_capacity(size as usize);
        for i in 0..size {
            bytes.push(z80_bus.peek((addr as u16).wrapping_add(i) as u32));
        }
        DisasmRow {
            address: addr,
            kind: DisasmRowKind::Code,
            bytes,
            mnemonic,
            operands,
            comment: None,
        }
    } else {
        let parts = crate::cpu68k::disasm::disassemble_one_parts(&mut emu.bus.peek_view(), addr);
        DisasmRow {
            address: addr,
            kind: DisasmRowKind::Code,
            bytes: parts.bytes,
            mnemonic: parts.mnemonic,
            operands: parts.operands,
            comment: parts.comment,
        }
    }
}

/// The bus accesses one stepped 68000 instruction made, kept while
/// [`GenesisEmulator::set_instruction_access_logging`] is on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstructionAccessLog {
    /// Address of the instruction that made the accesses, 24 bits wide like
    /// the access addresses (the CPU's PC can carry high bits the bus drops).
    pub pc: u32,
    /// `(address, size_in_bytes, is_write, value)` in the order the bus saw
    /// them; `value` is the byte (size 1) or the big-endian word (size 2).
    /// The fetches of the instruction's own opcode and extension words are
    /// among the reads: the bus does not tell them from data reads.
    pub accesses: Vec<(u32, u8, bool, u16)>,
    /// The DMA transfers from 68000 memory the instruction started (its
    /// write to the VDP control port completed a DMA command). The VDP reads
    /// their source itself, so those bytes are not among `accesses`.
    pub dmas: Vec<MemoryDma>,
}

impl GenesisEmulator {
    /// Keep (`true`) or stop keeping the bus accesses each 68000
    /// instruction makes, for [`Self::take_instruction_access_log`]. The
    /// paths that run one instruction at a time keep them
    /// ([`EmulatorCore::step`], which the full debug frame loop runs too, and
    /// the light debug loop); the fast frame loop never does. Only the
    /// instruction's own accesses are kept, not those of an interrupt entered
    /// before it nor the Z80's; a DMA from 68000 memory the instruction
    /// starts is kept beside them.
    pub fn set_instruction_access_logging(&mut self, enabled: bool) {
        self.instruction_access_logging = enabled;
        self.instruction_access_log = InstructionAccessLog::default();
    }

    /// The bus accesses of the last 68000 instruction stepped while
    /// [`Self::set_instruction_access_logging`] is on, leaving the log
    /// empty, so a step that ran no instruction (the CPU stopped, waiting
    /// for an interrupt) returns nothing.
    pub fn take_instruction_access_log(&mut self) -> InstructionAccessLog {
        std::mem::take(&mut self.instruction_access_log)
    }

    /// Live video debugger over the VDP: per-layer toggles plus
    /// tile/palette/sprite/register inspection through the generic
    /// `retro_core::video::VideoDebugger` interface. Mirrors
    /// `SnesEmulator::video_debugger()` so the host drives both consoles
    /// through the same trait.
    pub fn video_debugger(&mut self) -> &mut dyn retro_core::video::VideoDebugger {
        self.bus.vdp_mut()
    }

    /// Live audio debugger over the primary FM chip (YM2612): channel,
    /// operator, and register inspection through the generic
    /// `retro_core::audio::AudioDebugger` interface. Mirrors
    /// `SnesEmulator::audio_debugger()`. The PSG is a second audio chip
    /// reached via [`Self::psg`]; the generic single-chip trait exposes the
    /// FM synth, which is what the host's primary audio panels drive.
    pub fn audio_debugger(&mut self) -> &mut dyn retro_core::audio::AudioDebugger {
        self.bus.ym2612_mut()
    }

    /// A [`CartridgeDebugger`](retro_core::cartridge::CartridgeDebugger) over
    /// the installed cartridge: memory-map / ROM / SRAM regions, the mapper
    /// state, the per-chip coprocessor snapshots, and ROM/SRAM breakpoints
    /// (which append to the emulator's shared breakpoint list). Mirrors
    /// `SnesEmulator::cartridge_debugger()`. With no cartridge loaded it
    /// binds an empty plan so the surface stays non-`Option`.
    pub fn cartridge_debugger(
        &mut self,
    ) -> crate::cartridge::composable::GenesisCartridgeDebugger<'_> {
        let (plan, rom_size, sram_size, chip_states) = match self.bus.cartridge() {
            Some(cart) => (
                cart.map_plan().clone(),
                cart.info().rom_size as u32,
                cart.sram().len() as u32,
                cart.coprocessors_debug_state(),
            ),
            None => (
                crate::cartridge::composable::MapPlan::default(),
                0,
                0,
                Vec::new(),
            ),
        };
        crate::cartridge::composable::GenesisCartridgeDebugger::new(
            plan,
            rom_size,
            sram_size,
            chip_states,
            self.debugger.breakpoints_mut(),
        )
    }

    /// Live debug snapshot of the primary (lowest-id) coprocessor (the SVP's
    /// SSP1601 registers on Virtua Racing), or `None` on a plain cartridge.
    pub fn coprocessor_debug_state(&self) -> Option<retro_core::cartridge::ExpansionChipState> {
        self.bus
            .cartridge()
            .and_then(|cart| cart.coprocessor_debug_state())
    }

    /// Live debug snapshots of every coprocessor, keyed by `chip_id`. Empty
    /// on a plain cartridge.
    pub fn coprocessors_debug_state(&self) -> Vec<(u8, retro_core::cartridge::ExpansionChipState)> {
        self.bus
            .cartridge()
            .map(|cart| cart.coprocessors_debug_state())
            .unwrap_or_default()
    }

    /// Human-readable label of every coprocessor, keyed by `chip_id` (e.g.
    /// `[(0, "SVP")]`). Empty on a plain cartridge.
    pub fn coprocessor_labels(&self) -> Vec<(u8, String)> {
        self.bus
            .cartridge()
            .map(|cart| cart.coprocessor_labels())
            .unwrap_or_default()
    }

    /// MMIO access count of the primary coprocessor (proves the game is
    /// driving it). Zero when there is no coprocessor.
    pub fn coprocessor_accesses(&self) -> u64 {
        self.bus
            .cartridge()
            .map(|cart| cart.coprocessor_accesses())
            .unwrap_or(0)
    }

    /// Get mutable reference to the debugger.
    pub fn debugger(&mut self) -> &mut GenesisDebugger {
        &mut self.debugger
    }

    /// Get immutable reference to the debugger (for checking state without borrowing mutably).
    pub fn debugger_ref(&self) -> &GenesisDebugger {
        &self.debugger
    }

    /// Read memory for debugging, with no side effects: the cartridge, work
    /// RAM and Z80 RAM as [`AddressBus::peek_byte`] gives them, 0 for the I/O
    /// and VDP ports (a read of theirs would change the running game). VRAM,
    /// CRAM and VSRAM are not on the 68000's bus; the VDP's own calls read
    /// them.
    pub fn read_memory_debug(&mut self, addr: u32, size: u32) -> Vec<u8> {
        (0..size)
            .map(|i| self.bus.peek_byte(addr.wrapping_add(i)))
            .collect()
    }

    /// Write memory for debugging, a byte at a time through the bus's CPU path:
    /// a write to an I/O or VDP port acts as a CPU write would.
    pub fn write_memory_debug(&mut self, addr: u32, data: &[u8]) {
        for (i, &byte) in data.iter().enumerate() {
            self.bus.write(addr + i as u32, byte);
        }
    }

    /// Disassemble instructions starting at address.
    pub fn disassemble_at(&mut self, addr: u32, count: u32) -> Vec<Instruction> {
        use crate::cpu68k::disasm;

        let mut instructions = Vec::with_capacity(count as usize);
        let mut current_addr = addr;

        for _ in 0..count {
            let parts = disasm::disassemble_one_parts(&mut self.bus.peek_view(), current_addr);

            instructions.push(Instruction {
                address: current_addr,
                bytes: parts.bytes,
                mnemonic: parts.mnemonic,
                operands: parts.operands,
                comment: parts.comment,
            });

            current_addr += parts.bytes_consumed;
        }

        instructions
    }

    /// Disassemble Z80 instructions starting at address.
    ///
    /// Returns instructions with lowercase formatting for trace output:
    /// mnemonics and register names are lowercase, hex values stay uppercase,
    /// no space after commas. Example: "ld bc,$1FD9" instead of "LD BC, $1FD9".
    pub fn disassemble_z80_at(&mut self, addr: u32, count: u32) -> Vec<Instruction> {
        use crate::z80::disasm;

        let mut instructions = Vec::with_capacity(count as usize);
        let mut current_addr = addr as u16;

        let z80_bus = self.bus.create_z80_bus();
        for _ in 0..count {
            let (text, size) = disasm::disassemble_at(&mut z80_bus.peek_view(), current_addr);

            // Convert to lowercase trace format:
            // "LD BC, $1FD9" -> "ld bc,$1FD9"
            let trace_text = disasm::to_trace_format(&text);

            // Split mnemonic and operands
            let (mnemonic, operands) = if let Some(pos) = trace_text.find(' ') {
                (
                    trace_text[..pos].to_string(),
                    trace_text[pos + 1..].trim().to_string(),
                )
            } else {
                (trace_text.clone(), String::new())
            };

            // Collect raw bytes
            let mut bytes = Vec::with_capacity(size as usize);
            for i in 0..size {
                bytes.push(z80_bus.peek(current_addr.wrapping_add(i) as u32));
            }

            instructions.push(Instruction {
                address: current_addr as u32,
                bytes,
                mnemonic,
                operands,
                comment: None,
            });

            current_addr = current_addr.wrapping_add(size);
        }

        instructions
    }

    /// Get known instruction boundaries before an address.
    /// Returns addresses in ascending order (oldest to newest).
    pub fn get_boundaries_before(&self, addr: u32, count: usize) -> Vec<u32> {
        self.debugger.console().get_boundaries_before(addr, count)
    }

    /// Disassemble using known instruction boundaries (bsnes-style).
    /// For addresses that are known boundaries, disassemble the actual instruction.
    /// For addresses between known boundaries, show DC.W with the raw bytes.
    /// `before_count`: how many instructions to show before `pc`
    /// `after_count`: how many instructions to show after (including) `pc`
    ///
    /// Deprecated in favour of `disassemble_window`, which uses the
    /// in-memory coverage bitmap instead of a global HashSet of
    /// observed instruction starts. Kept for the `disassembleAroundPc`
    /// WASM binding.
    pub fn disassemble_around_pc(
        &mut self,
        pc: u32,
        before_count: usize,
        after_count: usize,
    ) -> Vec<Instruction> {
        use crate::cpu68k::disasm;

        let mut instructions = Vec::new();

        // Get known boundaries before PC
        let boundaries_before = self
            .debugger
            .console()
            .get_boundaries_before(pc, before_count);

        // Disassemble each known boundary before PC
        for &addr in &boundaries_before {
            let parts = disasm::disassemble_one_parts(&mut self.bus.peek_view(), addr);
            instructions.push(Instruction {
                address: addr,
                bytes: parts.bytes,
                mnemonic: parts.mnemonic,
                operands: parts.operands,
                comment: parts.comment,
            });
        }

        // Disassemble forward from PC
        let mut current_addr = pc;
        for _ in 0..after_count {
            let parts = disasm::disassemble_one_parts(&mut self.bus.peek_view(), current_addr);
            instructions.push(Instruction {
                address: current_addr,
                bytes: parts.bytes,
                mnemonic: parts.mnemonic,
                operands: parts.operands,
                comment: parts.comment,
            });
            current_addr += parts.bytes_consumed;
        }

        instructions
    }

    /// Bitmap-aware disassembly window around `pc` for the given CPU.
    ///
    /// Thin wrapper that builds a [`GenesisDisasmDriver`] and delegates
    /// to [`retro_core::debug::disassemble_window`]. The algorithm
    /// (backward probe + forward decode) lives in `retro-core` so
    /// other cores share it; only the CPU-specific decoding and the
    /// 68K-vs-Z80 probe-offset table stay here.
    ///
    /// `cpu_id`: 0 = 68K, 1 = Z80.
    pub fn disassemble_window(
        &mut self,
        cpu_id: usize,
        pc: u32,
        before_count: usize,
        after_count: usize,
    ) -> Vec<DisasmRow> {
        let mut driver = GenesisDisasmDriver { emu: self, cpu_id };
        retro_core::disassemble_window(&mut driver, pc, before_count, after_count)
    }
}

/// Adapter wiring [`GenesisEmulator`] into the generic
/// [`retro_core::DisasmDriver`] contract for one CPU at a time. Owned
/// briefly inside [`GenesisEmulator::disassemble_window`] so we can
/// borrow the emulator mutably across the (back + forward) walk.
struct GenesisDisasmDriver<'a> {
    emu: &'a mut GenesisEmulator,
    cpu_id: usize,
}

impl<'a> retro_core::DisasmDriver for GenesisDisasmDriver<'a> {
    fn probe_offsets(&self) -> &'static [u32] {
        probe_offsets(self.cpu_id)
    }

    fn is_covered(&self, addr: u32) -> bool {
        self.emu.debugger.is_covered(self.cpu_id, addr)
    }

    fn decode_at(&mut self, addr: u32) -> DisasmRow {
        decode_one(self.emu, self.cpu_id, addr)
    }
}

impl GenesisEmulator {
    /// Disassemble a single instruction for the given CPU and wrap it
    /// in a `Code`-kind `DisasmRow`. Decoder dispatch follows the
    /// `cpu_id` convention used everywhere else in this file
    /// (0 = 68K, 1 = Z80).
    #[allow(dead_code)] // kept for potential ad-hoc callers; window goes through GenesisDisasmDriver
    fn decode_at(&mut self, cpu_id: usize, addr: u32) -> DisasmRow {
        decode_one(self, cpu_id, addr)
    }

    /// The 68000's and the Z80's registers as they are now, the CPU states
    /// the debugger reports.
    pub(super) fn cpu_states(&self) -> Vec<CpuState> {
        let m68k_state = CpuState {
            cpu_id: MAIN_CPU,
            registers: vec![
                // Data registers (D0-D7)
                RegisterValue {
                    name: "D0".to_string(),
                    value: self.cpu.d[0] as u64,
                },
                RegisterValue {
                    name: "D1".to_string(),
                    value: self.cpu.d[1] as u64,
                },
                RegisterValue {
                    name: "D2".to_string(),
                    value: self.cpu.d[2] as u64,
                },
                RegisterValue {
                    name: "D3".to_string(),
                    value: self.cpu.d[3] as u64,
                },
                RegisterValue {
                    name: "D4".to_string(),
                    value: self.cpu.d[4] as u64,
                },
                RegisterValue {
                    name: "D5".to_string(),
                    value: self.cpu.d[5] as u64,
                },
                RegisterValue {
                    name: "D6".to_string(),
                    value: self.cpu.d[6] as u64,
                },
                RegisterValue {
                    name: "D7".to_string(),
                    value: self.cpu.d[7] as u64,
                },
                // Address registers (A0-A7)
                RegisterValue {
                    name: "A0".to_string(),
                    value: self.cpu.a[0] as u64,
                },
                RegisterValue {
                    name: "A1".to_string(),
                    value: self.cpu.a[1] as u64,
                },
                RegisterValue {
                    name: "A2".to_string(),
                    value: self.cpu.a[2] as u64,
                },
                RegisterValue {
                    name: "A3".to_string(),
                    value: self.cpu.a[3] as u64,
                },
                RegisterValue {
                    name: "A4".to_string(),
                    value: self.cpu.a[4] as u64,
                },
                RegisterValue {
                    name: "A5".to_string(),
                    value: self.cpu.a[5] as u64,
                },
                RegisterValue {
                    name: "A6".to_string(),
                    value: self.cpu.a[6] as u64,
                },
                RegisterValue {
                    name: "A7".to_string(),
                    value: self.cpu.a[7] as u64,
                },
                // Special registers
                RegisterValue {
                    name: "PC".to_string(),
                    value: self.cpu.pc as u64,
                },
                RegisterValue {
                    name: "SR".to_string(),
                    value: self.cpu.sr as u64,
                },
            ],
            pc: self.cpu.pc,
            flags: self.cpu.sr as u32, // Status register contains flags
        };

        let z80_state = CpuState {
            cpu_id: SOUND_CPU,
            registers: vec![
                RegisterValue {
                    name: "A".to_string(),
                    value: self.z80.a as u64,
                },
                RegisterValue {
                    name: "F".to_string(),
                    value: self.z80.f as u64,
                },
                RegisterValue {
                    name: "B".to_string(),
                    value: self.z80.b as u64,
                },
                RegisterValue {
                    name: "C".to_string(),
                    value: self.z80.c as u64,
                },
                RegisterValue {
                    name: "D".to_string(),
                    value: self.z80.d as u64,
                },
                RegisterValue {
                    name: "E".to_string(),
                    value: self.z80.e as u64,
                },
                RegisterValue {
                    name: "H".to_string(),
                    value: self.z80.h as u64,
                },
                RegisterValue {
                    name: "L".to_string(),
                    value: self.z80.l as u64,
                },
                RegisterValue {
                    name: "PC".to_string(),
                    value: self.z80.pc as u64,
                },
                RegisterValue {
                    name: "SP".to_string(),
                    value: self.z80.sp as u64,
                },
                RegisterValue {
                    name: "IX".to_string(),
                    value: self.z80.ix as u64,
                },
                RegisterValue {
                    name: "IY".to_string(),
                    value: self.z80.iy as u64,
                },
            ],
            pc: self.z80.pc as u32,
            flags: self.z80.f as u32,
        };

        vec![m68k_state, z80_state]
    }

    /// Arm 68K bus-access recording when a memory watchpoint is active or the
    /// instruction access log is on, so the just-stepped instruction's
    /// reads/writes are logged. Returns whether arming happened, which the
    /// caller passes to [`Self::drain_watchpoints`]. Otherwise the bus stays on
    /// its zero-overhead path.
    pub(super) fn arm_watchpoints(&mut self) -> bool {
        let armed = self.debugger.has_watchpoints_or_events() || self.instruction_access_logging;
        if armed {
            self.bus.set_watch_armed(true);
        }
        armed
    }

    /// Drain the 68K bus accesses recorded during the just-executed instruction
    /// at `pc`, keep them in the instruction access log when it is on, and
    /// match them against memory watchpoints, disarming recording so later
    /// bus traffic (audio, DMA bookkeeping, sub-CPU) isn't attributed to the 68K.
    /// Returns whether any read/write watchpoint fired.
    pub(super) fn drain_watchpoints(&mut self, armed: bool, pc: u32) -> bool {
        if !armed {
            return false;
        }
        let events = self.bus.take_watch_events();
        let dmas = self.bus.take_watch_dmas();
        let hit = self.debugger.has_watchpoints_or_events() && self.watchpoint_hit(&events);
        self.bus.set_watch_armed(false);
        if self.instruction_access_logging {
            self.instruction_access_log = InstructionAccessLog {
                pc: pc & 0x00FF_FFFF,
                accesses: events,
                dmas,
            };
        }
        hit
    }

    /// Whether the accesses one instruction made fire a read or write
    /// watchpoint.
    ///
    /// A word access is expanded to its two bytes, the high one at its
    /// address, so a watch on either byte fires. Read watchpoints also see
    /// instruction/operand fetches because the bus cannot distinguish a
    /// prefetch from a data read; write watchpoints are exact (a fetch never
    /// writes). Prefer execute breakpoints for code and write/RAM watches
    /// for data. A sized watch's value and a condition's memory are read
    /// through the bus's peek, after the instruction.
    fn watchpoint_hit(&mut self, events: &[(u32, u8, bool, u16)]) -> bool {
        self.watch_accesses.clear();
        for &(address, size, write, value) in events {
            if size == 2 {
                self.watch_accesses
                    .push_word_big_endian(address, write, value);
            } else {
                self.watch_accesses.push(Access {
                    address,
                    write,
                    value: value as u8,
                });
            }
        }
        let (cpu, bus) = (&self.cpu, &self.bus);
        let mut context = CpuExprContext::new(
            |name| m68k_register(cpu, name),
            |address| bus.peek_byte(address),
            ByteOrder::BigEndian,
        );
        self.debugger
            .check_accesses(MAIN_CPU, self.watch_accesses.as_slice(), &mut context)
    }

    /// Check the 68000's execute breakpoints before the instruction at `pc`
    /// runs ([`retro_core::debug::DebuggerState::check_execute`]: the
    /// instruction a stop left the CPU at runs unchecked), a condition read
    /// from the registers and, through the bus's peek, memory as they are.
    pub(super) fn check_execute_breakpoint(&mut self, pc: u32) -> bool {
        let (cpu, bus) = (&self.cpu, &self.bus);
        let mut context = CpuExprContext::new(
            |name| m68k_register(cpu, name),
            |address| bus.peek_byte(address),
            ByteOrder::BigEndian,
        );
        self.debugger
            .check_execute(MAIN_CPU, pc, Some(&mut context))
    }

    /// Check them after an instruction, at `next_pc`, for the instruction
    /// that runs next ([`retro_core::debug::DebuggerState::check_execute_next`]:
    /// the check before that instruction then passes, so each instruction
    /// is checked once).
    pub(super) fn check_next_execute_breakpoint(&mut self, next_pc: u32) -> bool {
        let (cpu, bus) = (&self.cpu, &self.bus);
        let mut context = CpuExprContext::new(
            |name| m68k_register(cpu, name),
            |address| bus.peek_byte(address),
            ByteOrder::BigEndian,
        );
        self.debugger
            .check_execute_next(MAIN_CPU, next_pc, Some(&mut context))
    }

    /// Keep the 68000's call stack for the instruction at `pc`, whose first
    /// word is `opcode`, once it has run ([`call_step`]): a call pushes a
    /// frame whose return address is the address after the call, a return
    /// takes the innermost frame off.
    pub(super) fn maintain_call_stack(&mut self, pc: u32, opcode: u16) {
        match call_step(opcode) {
            CallStep::Call { length } => self.debugger.push_call(
                MAIN_CPU,
                StackFrame {
                    call_address: pc,
                    return_address: pc.wrapping_add(length),
                    function_name: None,
                },
            ),
            CallStep::Return => self.debugger.pop_call(MAIN_CPU),
            CallStep::Other => {}
        }
    }

    /// The 68000 took an interrupt at `interrupted`, the address it was
    /// about to run: keep a frame for it, its call and its return address
    /// both that address, which the handler's `RTE` takes off.
    pub(super) fn push_interrupt_frame(&mut self, interrupted: u32) {
        self.debugger.push_call(
            MAIN_CPU,
            StackFrame {
                call_address: interrupted,
                return_address: interrupted,
                function_name: None,
            },
        );
    }

    /// Whether an event breakpoint stops on the interrupt of `level` the
    /// 68000 has just taken: the vertical interrupt's at level 6, the
    /// horizontal interrupt's at level 4. The stepping paths ask this when
    /// they take an interrupt, so the event is the interrupt delivered, part
    /// of the way into a line too; the stop then lands after the handler's
    /// first instruction, as one on an execute breakpoint at its entry does.
    /// Nothing is checked while no event or watch breakpoint is armed.
    pub(super) fn interrupt_event_hit(&mut self, level: u8) -> bool {
        if !self.debugger.has_watchpoints_or_events() {
            return false;
        }
        match level {
            6 => self.debugger.check_event(GenesisEvent::VInt),
            4 => self.debugger.check_event(GenesisEvent::HInt),
            _ => false,
        }
    }

    /// Whether an event breakpoint stops on a write to video memory made
    /// during the step just run: a transfer that finished, a palette colour
    /// written, a transfer into the sprite table, a tile rewritten. The VDP
    /// raises only what the watch [`Self::arm_video_events`] armed asks
    /// for, so with no such breakpoint this is one check of nothing raised.
    /// The stop lands after the instruction that made the write, part of
    /// the way into a line, as a watchpoint's does; a fill finishing as the
    /// 68000 runs on stops after the instruction it finished during.
    pub(super) fn video_event_hit(&mut self) -> bool {
        match self.bus.vdp_mut().raised_events_mut() {
            Some(raised) if !raised.is_empty() => self.debugger.check_raised(raised),
            _ => false,
        }
    }

    /// Arm the VDP's watch on writes to video memory from the enabled event
    /// breakpoints ([`video_event_watch`]), or clear it when none asks. The
    /// debugger loops do this as each pass begins, and a step the host asks
    /// for before it runs.
    pub(super) fn arm_video_events(&mut self) {
        let watch = if self.debugger.has_watchpoints_or_events() {
            video_event_watch(self.debugger.breakpoints())
        } else {
            None
        };
        self.bus.vdp_mut().set_event_watch(watch);
    }
}
