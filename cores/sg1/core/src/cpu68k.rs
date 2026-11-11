//! Motorola 68000 CPU emulation.

pub mod addressing;
pub mod decode;
pub mod disasm;
pub mod exceptions;
pub mod instructions;
pub mod prefetch;
use addressing::Size;
use decode::bit_extract;
use instructions::InstructionResult;
use prefetch::Prefetch;
use retro_core::cpu::Addressable;
use std::cell::Cell;

/// Test-only: build a zeroed ROM of `size` bytes (minimum 0x200) with
/// just enough of a valid Genesis header to satisfy
/// `build_genesis_cartridge`. The header at `0x100..0x110` is set to
/// `"SEGA GENESIS    "`. Reset vectors and other bytes are left at 0;
/// callers are free to overwrite them before passing the ROM to the
/// cartridge factory.
#[cfg(test)]
pub(crate) fn make_test_rom_with_header(size: usize) -> Vec<u8> {
    let mut rom = vec![0u8; size.max(0x200)];
    rom[0x100..0x110].copy_from_slice(b"SEGA GENESIS    ");
    rom
}

/// Information about an address error that occurred.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AddressErrorInfo {
    /// The address that caused the error (misaligned access)
    pub access_addr: u32,
    /// The PC at the moment the error was detected
    pub pc_at_error: u32,
    /// True if this was a write access, false for read
    pub is_write: bool,
    /// Function code (default: 5 = supervisor data, 6 = supervisor program for branches)
    pub function_code: u8,
    /// I/N bit (false = instruction fetch, true = not instruction fetch)
    pub in_bit: bool,
    /// Optional SR value to push into the Address Error stack frame (instead of current `self.sr`).
    /// Needed for cases like `RTE` where the CPU remains on supervisor stack, but the SR pushed is
    /// the one restored from the exception frame.
    pub sr_for_frame: Option<u16>,
    /// Force using the current supervisor stack (i.e., skip USP<->SSP swap) when entering the
    /// Address Error exception, even if the SR value being pushed indicates user mode.
    pub force_supervisor_stack: bool,
}

/// 68000 CPU.
pub struct M68k {
    // Data registers (D0-D7)
    pub d: [u32; 8],
    // Address registers (A0-A7)
    pub a: [u32; 8],
    // Program counter
    pub pc: u32,
    // Status register
    pub sr: u16,
    // Stack pointers
    pub usp: u32, // User stack pointer
    pub ssp: u32, // Supervisor stack pointer

    // Cycle counter
    cycles_executed: u64,

    // Address error tracking: stores both the faulting address and PC at error
    // Used to detect word/long access at odd addresses
    pub(crate) address_error: Option<AddressErrorInfo>,

    // Tracks how many extension word bytes have been read during the current instruction.
    // This is used to calculate the correct PC for Address Error exceptions.
    // Reset to 0 at the start of each instruction in step().
    pub(crate) extension_bytes_read: u32,

    // Pending interrupt request level (0 = no pending interrupt, 1-7 = IRQ level)
    // This is set by external hardware (VDP, etc.) and checked between instructions.
    pub pending_irq_level: u8,

    // Flag set when an instruction encounters an invalid addressing mode or opcode encoding.
    // Checked in step() after decode_and_execute returns to take the Illegal Instruction exception.
    // Uses Cell<bool> for interior mutability (EA decoders only have &M68k).
    pub(crate) illegal_instruction_pending: Cell<bool>,

    // PC/SR before the instruction, stored on every step; nothing reads them back
    pre_exec_snapshot_pc: u32,
    pre_exec_snapshot_sr: u16,

    // STOP instruction halts CPU until an interrupt with priority > current IPL fires
    pub stopped: bool,
}

impl M68k {
    /// Create a new 68000 CPU.
    pub fn new() -> Self {
        Self {
            d: [0; 8],
            a: [0; 8],
            pc: 0,
            sr: 0x2700, // Supervisor mode, interrupts disabled
            usp: 0,
            ssp: 0,
            cycles_executed: 0,
            address_error: None,
            extension_bytes_read: 0,
            pending_irq_level: 0,
            illegal_instruction_pending: Cell::new(false),
            pre_exec_snapshot_pc: 0,
            pre_exec_snapshot_sr: 0,
            stopped: false,
        }
    }

    /// Check if an address is aligned for the given size.
    /// Word and Long accesses must be at even addresses on the 68000.
    ///
    /// If misaligned, stores both the faulting address and the current prefetch position.
    #[inline]
    pub fn check_address_alignment_with_prefetch(
        &mut self,
        addr: u32,
        size: Size,
        prefetch: &Prefetch,
        is_write: bool,
    ) -> bool {
        if size != Size::Byte && (addr & 1) != 0 {
            self.address_error = Some(AddressErrorInfo {
                access_addr: addr,
                // Use the exact position in the prefetch queue
                // This automatically accounts for extension words read so far
                pc_at_error: prefetch.pc_for_stack(),
                is_write,
                function_code: 0x05, // Supervisor data (default)
                in_bit: false,       // Instruction fetch (default)
                sr_for_frame: None,
                force_supervisor_stack: false,
            });
            false
        } else {
            true
        }
    }

    /// Check address alignment with write-only PC adjustment.
    /// Used by write_with_adjust for write-only operations like MOVE destination.
    #[inline]
    pub fn check_address_alignment_write_only(
        &mut self,
        addr: u32,
        size: Size,
        prefetch: &Prefetch,
    ) -> bool {
        if size != Size::Byte && (addr & 1) != 0 {
            self.address_error = Some(AddressErrorInfo {
                access_addr: addr,
                // For write-only operations, cursor=0 means we're at instruction_pc + 2
                pc_at_error: prefetch.pc_for_stack_write_only(),
                is_write: true,
                function_code: 0x05,
                in_bit: false,
                sr_for_frame: None,
                force_supervisor_stack: false,
            });
            false
        } else {
            true
        }
    }

    /// Check address alignment for -(An) write destination (needs +2 PC adjustment).
    /// This is specific to pre-decrement mode where PC should be instruction_pc + 2.
    #[inline]
    pub fn check_address_alignment_predec_write(
        &mut self,
        addr: u32,
        size: Size,
        prefetch: &Prefetch,
    ) -> bool {
        if size != Size::Byte && (addr & 1) != 0 {
            let mut adjusted_addr = addr;
            if size == Size::Long {
                adjusted_addr = addr.wrapping_add(2);
            }

            self.address_error = Some(AddressErrorInfo {
                access_addr: adjusted_addr,
                // For -(An) write destinations, PC needs +2 adjustment
                pc_at_error: prefetch.pc_for_stack_write_only().wrapping_add(2),
                is_write: true,
                function_code: 0x05,
                in_bit: false,
                sr_for_frame: None,
                force_supervisor_stack: false,
            });
            false
        } else {
            true
        }
    }

    /// Check address alignment for absolute long write destination.
    /// The PC pushed depends on the source mode: with `decrement` set (MOVE from
    /// a memory source) it is two bytes below the write-only position.
    #[inline]
    pub fn check_address_alignment_abslong_write(
        &mut self,
        addr: u32,
        size: Size,
        prefetch: &Prefetch,
        decrement: bool,
    ) -> bool {
        let mut value_to_sub = 0;

        if decrement {
            value_to_sub = 2;
        }

        if size != Size::Byte && (addr & 1) != 0 {
            self.address_error = Some(AddressErrorInfo {
                access_addr: addr,
                // The write-only position already counts the address's two
                // extension words; with `decrement` the PC is two bytes lower
                pc_at_error: prefetch
                    .pc_for_stack_write_only()
                    .wrapping_sub(value_to_sub),
                is_write: true,
                function_code: 0x05,
                in_bit: false,
                sr_for_frame: None,
                force_supervisor_stack: false,
            });
            false
        } else {
            true
        }
    }

    /// Check if an address is aligned (legacy version without the prefetch queue).
    /// Used by `read_memory_legacy` / `write_memory_legacy` in `addressing.rs`.
    #[inline]
    pub fn check_address_alignment(&mut self, addr: u32, size: Size) -> bool {
        if size != Size::Byte && (addr & 1) != 0 {
            self.address_error = Some(AddressErrorInfo {
                access_addr: addr,
                pc_at_error: self.pc.wrapping_sub(2),
                is_write: false, // Legacy code doesn't track read/write
                function_code: 0x05,
                in_bit: false,
                sr_for_frame: None,
                force_supervisor_stack: false,
            });
            false
        } else {
            true
        }
    }

    /// Clear any pending address error.
    #[inline]
    pub fn clear_address_error(&mut self) {
        self.address_error = None;
    }

    /// Check if there's a pending address error.
    #[inline]
    pub fn has_address_error(&self) -> bool {
        self.address_error.is_some()
    }

    /// Check if CPU is in supervisor mode.
    #[inline]
    pub fn is_supervisor(&self) -> bool {
        (self.sr & 0x2000) != 0
    }

    /// Get the current stack pointer value.
    /// In supervisor mode, returns SSP. In user mode, returns USP.
    #[inline]
    pub fn sp(&self) -> u32 {
        self.a[7]
    }

    /// Set the current stack pointer and sync with SSP/USP.
    /// This should be used instead of directly setting a[7].
    #[inline]
    pub fn set_sp(&mut self, value: u32) {
        self.a[7] = value;
        // Sync with the appropriate stack pointer register
        if self.is_supervisor() {
            self.ssp = value;
        } else {
            self.usp = value;
        }
    }

    /// Sync a[7] with SSP/USP based on current mode.
    /// Call this after changing the S bit in SR.
    pub fn sync_sp_from_mode(&mut self) {
        if self.is_supervisor() {
            self.a[7] = self.ssp;
        } else {
            self.a[7] = self.usp;
        }
    }

    /// Sync SSP/USP from a[7] based on current mode.
    /// Call this before reading ssp/usp directly.
    pub fn sync_sp_to_registers(&mut self) {
        if self.is_supervisor() {
            self.ssp = self.a[7];
        } else {
            self.usp = self.a[7];
        }
    }

    /// Request an interrupt at the given level (1-7).
    /// The interrupt will be processed before the next instruction if the level
    /// is higher than the current interrupt mask in the SR.
    /// Level 7 is non-maskable (always accepted).
    pub fn request_interrupt(&mut self, level: u8) {
        if level > self.pending_irq_level {
            self.pending_irq_level = level;
        }
    }

    /// Check and process pending interrupt from the pending_irq_level field.
    /// Called between instructions. Returns the number of cycles consumed
    /// by the interrupt processing, or 0 if no interrupt was processed.
    pub fn process_pending_interrupts(&mut self, bus: &mut dyn Addressable) -> u32 {
        if self.pending_irq_level == 0 {
            return 0;
        }

        let irq_level = self.pending_irq_level;
        let current_mask = ((self.sr >> 8) & 0x07) as u8;

        // Level 7 is non-maskable and always accepted.
        // Other levels must be strictly greater than the current mask.
        if irq_level == 7 || irq_level > current_mask {
            self.stopped = false;
            self.pending_irq_level = 0;

            let vector = 24 + irq_level;
            return self.exception(bus, vector);
        }

        0
    }

    /// Reset the CPU to initial state.
    pub fn reset(&mut self, bus: &mut dyn Addressable) {
        // Read initial SSP from 0x000000
        let ssp_high = bus.read_word(0x000000) as u32;
        let ssp_low = bus.read_word(0x000002) as u32;
        self.ssp = (ssp_high << 16) | ssp_low;
        self.a[7] = self.ssp;

        // Read initial PC from 0x000004
        let pc_high = bus.read_word(0x000004) as u32;
        let pc_low = bus.read_word(0x000006) as u32;
        self.pc = (pc_high << 16) | pc_low;

        // Reset status register
        self.sr = 0x2700; // Supervisor mode, interrupts disabled

        // Clear registers
        self.d = [0; 8];
        self.a[0..7].fill(0);

        self.cycles_executed = 0;
        self.pending_irq_level = 0;
        self.stopped = false;
    }

    /// Helper: Extract bus cycles from an Addressable.
    ///
    /// For buses that track cycles (via CyclesTrackable trait), returns the cycle count.
    /// For buses that don't track cycles, returns 0.
    #[inline]
    fn get_bus_cycles(bus: &dyn Addressable) -> u32 {
        bus.cycles().unwrap_or(0) as u32
    }

    /// Execute one instruction.
    /// Returns the number of cycles consumed.
    pub fn step(&mut self, bus: &mut dyn Addressable) -> u32 {
        if self.stopped {
            return 4;
        }

        // Clear any pending address error from previous instruction
        self.clear_address_error();
        self.illegal_instruction_pending.set(false);

        // Reset extension bytes counter for this instruction
        self.extension_bytes_read = 0;

        // Save only PC/SR before execution (cheap: 6 bytes); no full register
        // snapshot is taken, on any path.
        let instruction_pc = self.pc;
        let instruction_sr = self.sr;

        // Fetch instruction word (costs 4 cycles, already tracked by bus)
        let instruction = bus.read_word(self.pc);

        // Create prefetch queue (always, for all instructions)
        let mut prefetch = Prefetch::new(instruction_pc);

        // Keep PC/SR in the CPU's pre-execution snapshot fields too. Written for
        // every instruction and read by nothing: the illegal-instruction path
        // below restores from the locals, and no D/A/USP/SSP copy is taken.
        self.pre_exec_snapshot_pc = instruction_pc;
        self.pre_exec_snapshot_sr = instruction_sr;

        // Decode and execute
        let result = self.decode_and_execute(bus, instruction, &mut prefetch);

        // Check for illegal instruction signaled by EA decoder or instruction handler
        if self.illegal_instruction_pending.get() {
            self.illegal_instruction_pending.set(false);
            // Restore PC and SR. For D/A registers, we can't restore them since
            // we didn't save them. However, illegal instructions in practice
            // are either truly unimplemented opcodes (where the CPU state before
            // decode is unchanged) or occur immediately during decode before any
            // register modification. Reset PC/SR which is the critical state.
            self.pc = instruction_pc;
            self.sr = instruction_sr;
            let cycles = self.exception(bus, exceptions::ExceptionVector::IllegalInstruction as u8);
            self.sync_sp_to_registers();
            self.cycles_executed += cycles as u64;
            return cycles;
        }

        // Check for Address Error exception
        if let Some(error_info) = self.address_error.take() {
            self.pc = error_info.pc_at_error;

            // NOTE: Do NOT restore instruction_sr here!
            // Each instruction is responsible for managing flags correctly:
            // - MOVE updates flags BEFORE write (based on source), so they're preserved
            // - ADD checks for Address Error and doesn't update flags if error occurred

            // Address Error exception processing
            // The bus has already tracked all cycles up to and including the exception handling
            // (prefetch, EA decoding, operand accesses, stack writes)
            self.address_error_exception_with_fc(
                bus,
                error_info.access_addr,
                instruction,
                error_info.is_write,
                error_info.function_code as u16,
                error_info.in_bit,
                error_info.sr_for_frame,
                error_info.force_supervisor_stack,
            );

            self.sync_sp_to_registers();

            // Address Error exception processing
            // Bus has tracked all memory accesses (stack frame + vector fetch);
            // the total is `result.cycles` plus those bus cycles, with no
            // internal cycles added for the exception.
            let bus_cycles = Self::get_bus_cycles(bus);
            let total_cycles = result.cycles + bus_cycles;

            self.cycles_executed += total_cycles as u64;
            return total_cycles;
        }

        // Update PC based on how the instruction handled it:
        // - pc_set = true: PC was already set by instruction (branch taken, JMP, RTS, etc.)
        // - uses_prefetch = true: use prefetch.consumed()
        // - Legacy: use result.pc_increment (already includes opcode size)
        if result.pc_set {
            // PC was already set by the instruction, don't modify it
        } else if result.uses_prefetch {
            // Prefetch-based: instruction_pc + 2 (opcode) + consumed bytes
            self.pc = instruction_pc
                .wrapping_add(2)
                .wrapping_add(prefetch.consumed());
        } else {
            // Legacy: instruction_pc + pc_increment (pc_increment already includes the 2-byte opcode)
            self.pc = instruction_pc.wrapping_add(result.pc_increment);
        }

        // Sync a[7] to SSP/USP based on current mode.
        // This ensures that if the instruction modified a[7], the correct
        // stack pointer register (ssp or usp) is updated.
        self.sync_sp_to_registers();

        // Most instructions return their total cycles (including bus accesses) in result.cycles
        // The cycle tables for MOVE, etc. already include memory access times
        // So we return result.cycles directly - no need to add bus_cycles separately
        //
        // Note: bus.cycles_executed tracks accesses for debugging but is NOT added to timing
        // to avoid double-counting with instruction cycle tables
        self.cycles_executed += result.cycles as u64;
        result.cycles
    }

    /// Decode and execute an instruction.
    fn decode_and_execute(
        &mut self,
        bus: &mut dyn Addressable,
        opcode: u16,
        prefetch: &mut Prefetch,
    ) -> InstructionResult {
        // Extract common bit fields
        let bits_15_12 = bit_extract::bits_15_12(opcode);
        let bits_11_9 = bit_extract::bits_11_9(opcode);
        let bits_8_6 = bit_extract::bits_8_6(opcode);
        let bits_7_6 = bit_extract::bits_7_6(opcode);
        let bits_5_3 = bit_extract::bits_5_3(opcode);
        let bits_2_0 = bit_extract::bits_2_0(opcode);

        // Decode based on top nibble and other fields
        match bits_15_12 {
            0x0 => {
                // Group 0: Immediate operations + Bit operations
                // Special cases for CCR/SR operations first
                if opcode == 0x003C {
                    return self.ori_to_ccr(bus, prefetch);
                }
                if opcode == 0x007C {
                    return self.ori_to_sr(bus, prefetch);
                }
                if opcode == 0x023C {
                    return self.andi_to_ccr(bus, prefetch);
                }
                if opcode == 0x027C {
                    return self.andi_to_sr(bus, prefetch);
                }
                if opcode == 0x0A3C {
                    return self.eori_to_ccr(bus, prefetch);
                }
                if opcode == 0x0A7C {
                    return self.eori_to_sr(bus, prefetch);
                }

                // MOVEP
                // Pattern: 0000 ddd1 sm001 aaa
                // - bit 8 is always 1
                // - bits 5-3 are always 001
                // - bit 7 = direction, bit 6 = size (0=word, 1=long)
                //
                // IMPORTANT: This must be checked BEFORE dynamic bit operations,
                // since MOVEP shares the broader 0x0100 prefix with them.
                if (opcode & 0xF138) == 0x0108 {
                    let data_reg = bits_11_9;
                    let addr_reg = bits_2_0;
                    let to_memory = (opcode & 0x0080) != 0;
                    let size = if (opcode & 0x0040) != 0 {
                        Size::Long
                    } else {
                        Size::Word
                    };
                    // MOVEP uses a 16-bit displacement extension word -> use prefetch.
                    return self.movep(bus, prefetch, size, data_reg, addr_reg, to_memory);
                }

                // Bit operations (dynamic - register)
                // Pattern: 0000 rrr 1 xx mmm rrr (bit 8 = 1, bits 7-6 = operation)
                if (opcode & 0xF100) == 0x0100 {
                    let bit_reg = bits_11_9;
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    let op = bits_7_6;
                    // Mode 1 (An direct) is never valid for bit operations
                    if mode == 1 {
                        return self.illegal_instruction(bus, opcode);
                    }
                    // For BCHG/BCLR/BSET (write ops), PC-relative and immediate are invalid
                    // For all ops (incl BTST), mode 7 reg >= 5 doesn't exist
                    if mode == 7 && ((op != 0 && reg >= 2) || reg >= 5) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return match op {
                        0 => self.btst(bus, prefetch, bit_reg, mode, reg, false),
                        1 => self.bchg(bus, prefetch, bit_reg, mode, reg, false),
                        2 => self.bclr(bus, prefetch, bit_reg, mode, reg, false),
                        3 => self.bset(bus, prefetch, bit_reg, mode, reg, false),
                        _ => unreachable!(),
                    };
                }

                // Bit operations (static - immediate)
                // Pattern: 0000 1000 xx mmm rrr (bits 7-6 = operation)
                if (opcode & 0xFF00) == 0x0800 {
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    let op = bits_7_6;
                    // Mode 1 (An direct) is never valid for bit operations
                    if mode == 1 {
                        return self.illegal_instruction(bus, opcode);
                    }
                    // For static bit ops, mode 7 reg >= 2 is invalid:
                    // - reg 2-3 (PC-relative) invalid for BCHG/BCLR/BSET (write ops)
                    // - reg 4 (#immediate) invalid for all static bit ops
                    // - reg 5-7 don't exist
                    if mode == 7 && ((op != 0 && reg >= 2) || reg >= 4) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return match op {
                        0 => self.btst(bus, prefetch, 0, mode, reg, true),
                        1 => self.bchg(bus, prefetch, 0, mode, reg, true),
                        2 => self.bclr(bus, prefetch, 0, mode, reg, true),
                        3 => self.bset(bus, prefetch, 0, mode, reg, true),
                        _ => unreachable!(),
                    };
                }

                // Immediate instructions by bits_11_9
                let mode = bits_5_3;
                let reg = bits_2_0;

                // Size bits 7-6: 00=Byte, 01=Word, 10=Long, 11=Invalid
                let size = match decode::decode_size(bits_7_6) {
                    Some(s) => s,
                    None => return self.illegal_instruction(bus, opcode),
                };

                // Validate addressing modes for immediate operations:
                // Mode 1 (An direct) is always invalid for immediate ops.
                // Mode 7 with reg 2-4 (PC-relative, immediate) are also invalid destinations.
                if mode == 1 || (mode == 7 && reg >= 2) {
                    return self.illegal_instruction(bus, opcode);
                }

                match bits_11_9 {
                    0x0 => self.ori(bus, prefetch, size, mode, reg),
                    0x1 => self.andi(bus, prefetch, size, mode, reg),
                    0x2 => self.subi(bus, prefetch, size, mode, reg),
                    0x3 => self.addi(bus, prefetch, size, mode, reg),
                    0x5 => self.eori(bus, prefetch, size, mode, reg),
                    0x6 => self.cmpi(bus, prefetch, size, mode, reg),
                    _ => self.illegal_instruction(bus, opcode),
                }
            }
            0x1..=0x3 => {
                // MOVE.B (0x1), MOVE.L (0x2), MOVE.W (0x3)
                let size = match bits_15_12 {
                    0x1 => Size::Byte,
                    0x2 => Size::Long,
                    0x3 => Size::Word,
                    _ => unreachable!(),
                };

                let dst_mode = bits_8_6;
                let dst_reg = bits_11_9;
                let src_mode = bits_5_3;
                let src_reg = bits_2_0;

                // Validate addressing modes:
                // 1. MOVE.B with An source (mode 1) is illegal - byte operations
                //    don't support address register direct as source.
                // 2. MOVEA.B doesn't exist - MOVEA only supports Word and Long.
                // 3. Invalid destination modes: mode 7 with reg >= 2.
                // 4. Invalid source modes: mode 7 with reg >= 5.
                if (size == Size::Byte && (src_mode == 1 || dst_mode == 1))
                    || (dst_mode == 7 && dst_reg >= 2)
                    || (src_mode == 7 && src_reg >= 5)
                {
                    return self.illegal_instruction(bus, opcode);
                }

                // Check for MOVEA (destination is address register)
                if dst_mode == 1 {
                    self.movea(bus, prefetch, size, dst_reg, src_mode, src_reg)
                } else {
                    self.move_instr(bus, prefetch, size, dst_mode, dst_reg, src_mode, src_reg)
                }
            }
            0x4 => {
                // Miscellaneous: NEGX, CLR, NEG, NOT, EXT, NBCD, SWAP, PEA, ILLEGAL, TAS, TST, TRAP, LINK, UNLK, MOVE USP, RESET, NOP, STOP, RTE, RTD, RTS, TRAPV, RTR, JSR, JMP

                // System Control Instructions
                if opcode == 0x4E70 {
                    return self.reset_devices(bus);
                }
                if opcode == 0x4E71 {
                    return self.nop();
                }
                if opcode == 0x4E72 {
                    return self.stop(bus, prefetch);
                }
                if opcode == 0x4E73 {
                    return self.rte(bus, prefetch);
                }
                if opcode == 0x4E75 {
                    return self.rts(bus);
                }
                if opcode == 0x4E76 {
                    return self.trapv(bus, prefetch);
                }
                if opcode == 0x4E77 {
                    return self.rtr(bus);
                }
                if opcode == 0x4AFC {
                    return self.illegal(bus);
                }

                // TRAP
                if (opcode & 0xFFF0) == 0x4E40 {
                    let vector = (opcode & 0xF) as u8;
                    return self.trap(bus, prefetch, vector);
                }

                // MOVE USP
                if (opcode & 0xFFF0) == 0x4E60 {
                    let addr_reg = (opcode & 0x7) as u8;
                    // MC68000 encoding:
                    // bit3 = 0 => An -> USP
                    // bit3 = 1 => USP -> An
                    let to_usp = (opcode & 0x0008) == 0;
                    return self.move_usp(bus, addr_reg, to_usp);
                }

                // CHK - data EA: mode 1 (An) invalid, mode 7 reg >= 5 invalid
                if (opcode & 0xF1C0) == 0x4180 {
                    let data_reg = bits_11_9;
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    if mode == 1 || (mode == 7 && reg >= 5) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return self.chk(bus, prefetch, data_reg, mode, reg);
                }

                // MOVE from SR - data alterable: mode 1 (An) invalid, mode 7 reg >= 2 invalid
                if (opcode & 0xFFC0) == 0x40C0 {
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    if mode == 1 || (mode == 7 && reg >= 2) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return self.move_from_sr(bus, prefetch, mode, reg);
                }

                // MOVE to CCR - data EA: mode 1 (An) invalid, mode 7 reg >= 5 invalid
                if (opcode & 0xFFC0) == 0x44C0 {
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    if mode == 1 || (mode == 7 && reg >= 5) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return self.move_to_ccr(bus, prefetch, mode, reg);
                }

                // MOVE to SR - data EA: mode 1 (An) invalid, mode 7 reg >= 5 invalid
                if (opcode & 0xFFC0) == 0x46C0 {
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    if mode == 1 || (mode == 7 && reg >= 5) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return self.move_to_sr(bus, prefetch, mode, reg);
                }

                // Other Group 4 instructions
                if (opcode & 0xFFC0) == 0x4EC0 {
                    // JMP - control addressing modes only
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    if mode == 0 || mode == 1 || mode == 3 || mode == 4 || (mode == 7 && reg >= 4) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    self.jmp(bus, prefetch, mode, reg)
                } else if (opcode & 0xFFC0) == 0x4E80 {
                    // JSR - control addressing modes only
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    if mode == 0 || mode == 1 || mode == 3 || mode == 4 || (mode == 7 && reg >= 4) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    self.jsr(bus, prefetch, mode, reg)
                } else if (opcode & 0xFF00) == 0x4600 {
                    // NOT - data alterable: mode 1 (An) is invalid, mode 7 reg >= 2 invalid
                    let size = match decode::decode_size(bits_7_6) {
                        Some(s) => s,
                        None => return self.illegal_instruction(bus, opcode),
                    };
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    if mode == 1 || (mode == 7 && reg >= 2) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    self.not(bus, prefetch, size, mode, reg)
                } else if (opcode & 0xFFC0) == 0x4AC0 {
                    // TAS (must be before TST!) - data alterable destination
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    if mode == 1 || (mode == 7 && reg >= 2) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    self.tas(bus, prefetch, mode, reg)
                } else if (opcode & 0xFF00) == 0x4A00 {
                    // TST - data alterable on 68000: mode 1 (An) invalid, mode 7 reg >= 2 invalid
                    let size = match decode::decode_size(bits_7_6) {
                        Some(s) => s,
                        None => return self.illegal_instruction(bus, opcode),
                    };
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    if mode == 1 || (mode == 7 && reg >= 2) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    self.tst(bus, prefetch, size, mode, reg)
                } else if (opcode & 0xFF00) == 0x4400 {
                    // NEG - data alterable: mode 1 (An) invalid, mode 7 reg >= 2 invalid
                    let size = match decode::decode_size(bits_7_6) {
                        Some(s) => s,
                        None => return self.illegal_instruction(bus, opcode),
                    };
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    if mode == 1 || (mode == 7 && reg >= 2) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    self.neg(bus, prefetch, size, mode, reg)
                } else if (opcode & 0xFF00) == 0x4000 {
                    // NEGX - data alterable: mode 1 (An) invalid, mode 7 reg >= 2 invalid
                    let size = match decode::decode_size(bits_7_6) {
                        Some(s) => s,
                        None => return self.illegal_instruction(bus, opcode),
                    };
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    if mode == 1 || (mode == 7 && reg >= 2) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    self.negx(bus, prefetch, size, mode, reg)
                } else if (opcode & 0xF1C0) == 0x41C0 {
                    // LEA (0100rrr111mmmrrr) - control addressing modes only
                    let dst_reg = bits_11_9;
                    let src_mode = bits_5_3;
                    let src_reg = bits_2_0;
                    if src_mode == 0
                        || src_mode == 1
                        || src_mode == 3
                        || src_mode == 4
                        || (src_mode == 7 && src_reg >= 4)
                    {
                        return self.illegal_instruction(bus, opcode);
                    }
                    self.lea(bus, prefetch, dst_reg, src_mode, src_reg)
                } else if (opcode & 0xFFC0) == 0x4800 {
                    // NBCD - data alterable: mode 1 (An) invalid, mode 7 reg >= 2 invalid
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    if mode == 1 || (mode == 7 && reg >= 2) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    self.nbcd(bus, prefetch, mode, reg)
                } else if (opcode & 0xFF00) == 0x4200 {
                    // CLR - data alterable: mode 1 (An) invalid, mode 7 reg >= 2 invalid
                    let size = match decode::decode_size(bits_7_6) {
                        Some(s) => s,
                        None => return self.illegal_instruction(bus, opcode),
                    };
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    if mode == 1 || (mode == 7 && reg >= 2) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    self.clr(bus, prefetch, size, mode, reg)
                } else if (opcode & 0xFFF8) == 0x4840 {
                    // SWAP
                    let reg = (opcode & 0x7) as u8;
                    self.swap(reg)
                } else if (opcode & 0xFFC0) == 0x4840 {
                    // PEA - control addressing modes only
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    if mode == 0 || mode == 1 || mode == 3 || mode == 4 || (mode == 7 && reg >= 4) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    self.pea(bus, prefetch, mode, reg)
                } else if (opcode & 0xFFF8) == 0x4E50 {
                    // LINK
                    let reg = (opcode & 0x7) as u8;
                    self.link(bus, prefetch, reg)
                } else if (opcode & 0xFFF8) == 0x4E58 {
                    // UNLK
                    let reg = (opcode & 0x7) as u8;
                    self.unlk(bus, prefetch, reg)
                } else if (opcode & 0xFFF8) == 0x4880 {
                    // EXT.W - Sign extend byte to word
                    let reg = bits_2_0;
                    self.ext(reg, Size::Word)
                } else if (opcode & 0xFFF8) == 0x48C0 {
                    // EXT.L - Sign extend word to long
                    let reg = bits_2_0;
                    self.ext(reg, Size::Long)
                } else if (opcode & 0xFB80) == 0x4880 {
                    // MOVEM
                    let to_memory = (opcode & 0x0400) == 0;
                    let size = if (opcode & 0x0040) != 0 {
                        Size::Long
                    } else {
                        Size::Word
                    };
                    let mode = bits_5_3;
                    let reg = bits_2_0;
                    // MOVEM: validate addressing modes
                    if to_memory {
                        // To memory: control alterable + -(An), NOT Dn/An/(An)+/PC-rel/imm
                        if mode == 0 || mode == 1 || mode == 3 || (mode == 7 && reg >= 2) {
                            return self.illegal_instruction(bus, opcode);
                        }
                    } else {
                        // From memory: control + (An)+, NOT Dn/An/-(An)/imm
                        if mode == 0 || mode == 1 || mode == 4 || (mode == 7 && reg >= 4) {
                            return self.illegal_instruction(bus, opcode);
                        }
                    }
                    self.movem(bus, prefetch, size, mode, reg, to_memory)
                } else {
                    self.illegal_instruction(bus, opcode)
                }
            }
            0x5 => {
                // ADDQ, SUBQ, Scc, DBcc
                let data = bits_11_9;
                let mode = bits_5_3;
                let reg = bits_2_0;

                // DBcc
                if (opcode & 0xF0F8) == 0x50C8 {
                    let condition = bit_extract::bits_11_8(opcode);
                    return self.dbcc(bus, prefetch, condition, reg);
                }

                // Scc
                if (opcode & 0xF0C0) == 0x50C0 {
                    let condition = bit_extract::bits_11_8(opcode);
                    // Scc: data alterable destination only (no An, no PC-relative/immediate)
                    if mode == 1 || (mode == 7 && reg >= 2) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return self.scc(bus, prefetch, condition, mode, reg);
                }

                // ADDQ/SUBQ
                if (opcode & 0x00C0) != 0x00C0 {
                    // ADDQ or SUBQ
                    let size = match decode::decode_size(bits_7_6) {
                        Some(s) => s,
                        None => return self.illegal_instruction(bus, opcode),
                    };
                    // ADDQ/SUBQ: byte to An is illegal, mode 7 reg >= 2 invalid dest
                    if (size == Size::Byte && mode == 1) || (mode == 7 && reg >= 2) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    let is_sub = (opcode & 0x0100) != 0;

                    if is_sub {
                        // SUBQ
                        self.subq(bus, prefetch, size, data, mode, reg)
                    } else {
                        // ADDQ
                        self.addq(bus, prefetch, size, data, mode, reg)
                    }
                } else {
                    // Unknown in this group
                    self.illegal_instruction(bus, opcode)
                }
            }
            0x6 => {
                // Bcc, BSR, BRA
                let condition = bit_extract::bits_11_8(opcode);
                let displacement = (opcode & 0xFF) as i8;

                // BSR - Branch to Subroutine
                if condition == 0x1 {
                    return self.bsr(bus, prefetch, displacement);
                }

                // Bcc/BRA
                self.branch(bus, prefetch, condition, displacement)
            }
            0x7 => {
                // MOVEQ
                if (opcode & 0x0100) == 0 {
                    let reg = bits_11_9;
                    let immediate = (opcode & 0xFF) as i8;
                    self.moveq(reg, immediate)
                } else {
                    self.illegal_instruction(bus, opcode)
                }
            }
            0x8 => {
                // OR, DIV, SBCD
                let reg = bits_11_9;

                // SBCD
                if (opcode & 0xF1F0) == 0x8100 {
                    let dst_reg = bits_11_9;
                    let src_reg = bits_2_0;
                    let memory_mode = (opcode & 0x0008) != 0;
                    return self.sbcd(bus, prefetch, dst_reg, src_reg, memory_mode);
                }

                // DIVU
                if (opcode & 0xF1C0) == 0x80C0 {
                    let mode = bits_5_3;
                    let ea_reg = bits_2_0;
                    // DIVU: data addressing modes only (no An direct)
                    if mode == 1 || (mode == 7 && ea_reg >= 5) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return self.divu(bus, prefetch, reg, mode, ea_reg);
                }

                // DIVS
                if (opcode & 0xF1C0) == 0x81C0 {
                    let mode = bits_5_3;
                    let ea_reg = bits_2_0;
                    // DIVS: data addressing modes only (no An direct)
                    if mode == 1 || (mode == 7 && ea_reg >= 5) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return self.divs(bus, prefetch, reg, mode, ea_reg);
                }

                // OR
                {
                    // OR
                    let size = match decode::decode_size(bits_7_6) {
                        Some(s) => s,
                        None => return self.illegal_instruction(bus, opcode),
                    };
                    let to_ea = (opcode & 0x0100) != 0;
                    let ea_mode = bits_5_3;
                    let ea_reg = bits_2_0;
                    // OR: data source (no An), memory alterable destination
                    if to_ea {
                        if ea_mode <= 1 || (ea_mode == 7 && ea_reg >= 2) {
                            return self.illegal_instruction(bus, opcode);
                        }
                    } else if ea_mode == 1 || (ea_mode == 7 && ea_reg >= 5) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    self.or(bus, prefetch, size, reg, ea_mode, ea_reg, to_ea)
                }
            }
            0x9 => {
                // SUB, SUBX, SUBA
                let reg = bits_11_9;

                // SUBA (check before SUBX to avoid overlap)
                if (opcode & 0xF0C0) == 0x90C0 {
                    let size = if (opcode & 0x0100) != 0 {
                        Size::Long
                    } else {
                        Size::Word
                    };
                    let mode = bits_5_3;
                    let ea_reg = bits_2_0;
                    // SUBA: all addressing modes, only mode 7 reg >= 5 invalid
                    if mode == 7 && ea_reg >= 5 {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return self.suba(bus, prefetch, size, reg, mode, ea_reg);
                }

                // SUBX
                // Pattern: 1001xxx1xx00yxxx (bit8=1, bit4=0, y=memory_mode)
                if (opcode & 0xF130) == 0x9100 {
                    let dst_reg = bits_11_9;
                    let src_reg = bits_2_0;
                    let memory_mode = (opcode & 0x0008) != 0;
                    let size = match decode::decode_size(bits_7_6) {
                        Some(s) => s,
                        None => return self.illegal_instruction(bus, opcode),
                    };
                    return self.subx(bus, prefetch, size, dst_reg, src_reg, memory_mode);
                }

                // SUB
                let size = match decode::decode_size(bits_7_6) {
                    Some(s) => s,
                    None => return self.illegal_instruction(bus, opcode),
                };
                let to_ea = (opcode & 0x0100) != 0;
                let ea_mode = bits_5_3;
                let ea_reg = bits_2_0;
                // SUB: all source modes (byte excludes An), memory alterable destination
                if to_ea {
                    if ea_mode <= 1 || (ea_mode == 7 && ea_reg >= 2) {
                        return self.illegal_instruction(bus, opcode);
                    }
                } else if (size == Size::Byte && ea_mode == 1) || (ea_mode == 7 && ea_reg >= 5) {
                    return self.illegal_instruction(bus, opcode);
                }
                self.sub(bus, prefetch, size, reg, ea_mode, ea_reg, to_ea)
            }
            0xB => {
                // CMP, CMPA, CMPM, EOR
                let reg = bits_11_9;

                // CMPA - Compare Address (check FIRST, before CMPM!)
                if (opcode & 0xF0C0) == 0xB0C0 {
                    let size = if (opcode & 0x0100) != 0 {
                        Size::Long
                    } else {
                        Size::Word
                    };
                    let mode = bits_5_3;
                    let ea_reg = bits_2_0;
                    // CMPA: all addressing modes, only mode 7 reg >= 5 invalid
                    if mode == 7 && ea_reg >= 5 {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return self.cmpa(bus, prefetch, size, reg, mode, ea_reg);
                }

                // CMPM - Compare Memory to Memory ((Ay)+, (Ax)+) - CHECK BEFORE EOR!
                if (opcode & 0xF138) == 0xB108 {
                    let dst_reg = bits_11_9;
                    let src_reg = bits_2_0;
                    let size = match decode::decode_size(bits_7_6) {
                        Some(s) => s,
                        None => return self.illegal_instruction(bus, opcode),
                    };
                    return self.cmpm(bus, prefetch, size, dst_reg, src_reg);
                }

                // EOR (when bit 8 is 1 and not CMPA/CMPM)
                if (opcode & 0x0100) != 0 && (opcode & 0x0038) != 0x0008 {
                    let size = match decode::decode_size(bits_7_6) {
                        Some(s) => s,
                        None => return self.illegal_instruction(bus, opcode),
                    };
                    let ea_mode = bits_5_3;
                    let ea_reg = bits_2_0;
                    // EOR: data alterable destination (mode 1 already excluded by bit pattern)
                    if ea_mode == 7 && ea_reg >= 2 {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return self.eor(bus, prefetch, size, reg, ea_mode, ea_reg);
                }

                // CMP - Compare
                let size = match decode::decode_size(bits_7_6) {
                    Some(s) => s,
                    None => return self.illegal_instruction(bus, opcode),
                };
                let ea_mode = bits_5_3;
                let ea_reg = bits_2_0;
                // CMP: all source modes (byte excludes An), mode 7 reg >= 5 invalid
                if (size == Size::Byte && ea_mode == 1) || (ea_mode == 7 && ea_reg >= 5) {
                    return self.illegal_instruction(bus, opcode);
                }
                self.cmp(bus, prefetch, size, reg, ea_mode, ea_reg)
            }
            0xC => {
                // AND, MUL, ABCD, EXG
                let reg = bits_11_9;

                // ABCD
                if (opcode & 0xF1F0) == 0xC100 {
                    let dst_reg = bits_11_9;
                    let src_reg = bits_2_0;
                    let memory_mode = (opcode & 0x0008) != 0;
                    return self.abcd(bus, prefetch, dst_reg, src_reg, memory_mode);
                }

                // MULU
                if (opcode & 0xF1C0) == 0xC0C0 {
                    let mode = bits_5_3;
                    let ea_reg = bits_2_0;
                    // MULU: data addressing modes only (no An direct)
                    if mode == 1 || (mode == 7 && ea_reg >= 5) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return self.mulu(bus, prefetch, reg, mode, ea_reg);
                }

                // MULS
                if (opcode & 0xF1C0) == 0xC1C0 {
                    let mode = bits_5_3;
                    let ea_reg = bits_2_0;
                    // MULS: data addressing modes only (no An direct)
                    if mode == 1 || (mode == 7 && ea_reg >= 5) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return self.muls(bus, prefetch, reg, mode, ea_reg);
                }

                // EXG - only valid opmodes: 01000 (8), 01001 (9), 10001 (17)
                if (opcode & 0xF130) == 0xC100 {
                    let rx = bits_11_9;
                    let ry = bits_2_0;
                    let opmode = ((opcode >> 3) & 0x1F) as u8;
                    if opmode == 0b01000 || opmode == 0b01001 || opmode == 0b10001 {
                        return self.exg(rx, ry, opmode);
                    }
                    // Invalid opmode falls through to AND or illegal
                }

                // AND
                {
                    // AND
                    let size = match decode::decode_size(bits_7_6) {
                        Some(s) => s,
                        None => return self.illegal_instruction(bus, opcode),
                    };
                    let to_ea = (opcode & 0x0100) != 0;
                    let ea_mode = bits_5_3;
                    let ea_reg = bits_2_0;
                    // AND: data source (no An), memory alterable destination
                    if to_ea {
                        if ea_mode <= 1 || (ea_mode == 7 && ea_reg >= 2) {
                            return self.illegal_instruction(bus, opcode);
                        }
                    } else if ea_mode == 1 || (ea_mode == 7 && ea_reg >= 5) {
                        return self.illegal_instruction(bus, opcode);
                    }
                    self.and(bus, prefetch, size, reg, ea_mode, ea_reg, to_ea)
                }
            }
            0xD => {
                // ADD, ADDX, ADDA
                let reg = bits_11_9;

                // ADDA
                if (opcode & 0xF0C0) == 0xD0C0 {
                    let size = if (opcode & 0x0100) != 0 {
                        Size::Long
                    } else {
                        Size::Word
                    };
                    let mode = bits_5_3;
                    let ea_reg = bits_2_0;
                    // ADDA: all addressing modes, only mode 7 reg >= 5 invalid
                    if mode == 7 && ea_reg >= 5 {
                        return self.illegal_instruction(bus, opcode);
                    }
                    return self.adda(bus, prefetch, size, reg, mode, ea_reg);
                }

                // ADDX
                // Pattern: 1101xxx1xx00yxxx (bit8=1, bit4=0, y=memory_mode)
                if (opcode & 0xF130) == 0xD100 {
                    let dst_reg = bits_11_9;
                    let src_reg = bits_2_0;
                    let memory_mode = (opcode & 0x0008) != 0;
                    let size = match decode::decode_size(bits_7_6) {
                        Some(s) => s,
                        None => return self.illegal_instruction(bus, opcode),
                    };
                    return self.addx(bus, prefetch, size, dst_reg, src_reg, memory_mode);
                }

                // ADD
                let size = match decode::decode_size(bits_7_6) {
                    Some(s) => s,
                    None => return self.illegal_instruction(bus, opcode),
                };
                let to_ea = (opcode & 0x0100) != 0;
                let ea_mode = bits_5_3;
                let ea_reg = bits_2_0;
                // ADD: all source modes (byte excludes An), memory alterable destination
                if to_ea {
                    if ea_mode <= 1 || (ea_mode == 7 && ea_reg >= 2) {
                        return self.illegal_instruction(bus, opcode);
                    }
                } else if (size == Size::Byte && ea_mode == 1) || (ea_mode == 7 && ea_reg >= 5) {
                    return self.illegal_instruction(bus, opcode);
                }
                self.add(bus, prefetch, size, reg, ea_mode, ea_reg, to_ea)
            }
            0xE => {
                // Shift/Rotate instructions have two encodings:
                // 1. Register form: bits 7-6 = size (00=byte, 01=word, 10=long)
                // 2. Memory form: bits 7-6 = 11, always word, count=1, operates on <ea>
                let direction = (opcode >> 8) & 1; // 0=right, 1=left

                if bits_7_6 == 3 {
                    // Memory form: shift/rotate on <ea>, always word, count=1
                    // Bits 11-9 encode shift type: 000=AS, 001=LS, 010=ROX, 011=RO
                    // Values 100-111 (bit 11 set) are illegal
                    let shift_type_full = (opcode >> 9) & 0x7;
                    if shift_type_full > 3 {
                        return self.illegal_instruction(bus, opcode);
                    }
                    let shift_type = shift_type_full;
                    let ea_mode = bits_5_3;
                    let ea_reg = bits_2_0;
                    // Memory form shifts: memory alterable destination only
                    if ea_mode <= 1 || (ea_mode == 7 && ea_reg >= 2) {
                        return self.illegal_instruction(bus, opcode);
                    }

                    match shift_type {
                        0x0 => {
                            if direction == 1 {
                                self.asl(bus, prefetch, Size::Word, 1, ea_reg, false, Some(ea_mode))
                            } else {
                                self.asr(bus, prefetch, Size::Word, 1, ea_reg, false, Some(ea_mode))
                            }
                        }
                        0x1 => {
                            if direction == 1 {
                                self.lsl(bus, prefetch, Size::Word, 1, ea_reg, false, Some(ea_mode))
                            } else {
                                self.lsr(bus, prefetch, Size::Word, 1, ea_reg, false, Some(ea_mode))
                            }
                        }
                        0x2 => {
                            if direction == 1 {
                                self.roxl(
                                    bus,
                                    prefetch,
                                    Size::Word,
                                    1,
                                    ea_reg,
                                    false,
                                    Some(ea_mode),
                                )
                            } else {
                                self.roxr(
                                    bus,
                                    prefetch,
                                    Size::Word,
                                    1,
                                    ea_reg,
                                    false,
                                    Some(ea_mode),
                                )
                            }
                        }
                        0x3 => {
                            if direction == 1 {
                                self.rol(bus, prefetch, Size::Word, 1, ea_reg, false, Some(ea_mode))
                            } else {
                                self.ror(bus, prefetch, Size::Word, 1, ea_reg, false, Some(ea_mode))
                            }
                        }
                        _ => self.illegal_instruction(bus, opcode),
                    }
                } else {
                    // Register form: shift/rotate on Dn
                    let count = bits_11_9;
                    let size = match decode::decode_size(bits_7_6) {
                        Some(s) => s,
                        None => return self.illegal_instruction(bus, opcode),
                    };
                    let is_register = (opcode & 0x0020) != 0;
                    let shift_type = (opcode >> 3) & 0x3;
                    let reg = bits_2_0;

                    match shift_type {
                        0x0 => {
                            if direction == 1 {
                                self.asl(bus, prefetch, size, count, reg, is_register, None)
                            } else {
                                self.asr(bus, prefetch, size, count, reg, is_register, None)
                            }
                        }
                        0x1 => {
                            if direction == 1 {
                                self.lsl(bus, prefetch, size, count, reg, is_register, None)
                            } else {
                                self.lsr(bus, prefetch, size, count, reg, is_register, None)
                            }
                        }
                        0x2 => {
                            if direction == 1 {
                                self.roxl(bus, prefetch, size, count, reg, is_register, None)
                            } else {
                                self.roxr(bus, prefetch, size, count, reg, is_register, None)
                            }
                        }
                        0x3 => {
                            if direction == 1 {
                                self.rol(bus, prefetch, size, count, reg, is_register, None)
                            } else {
                                self.ror(bus, prefetch, size, count, reg, is_register, None)
                            }
                        }
                        _ => self.illegal_instruction(bus, opcode),
                    }
                }
            }
            0xA => {
                // Line-A (1010) emulator trap - generates Line 1010 exception (vector 10)
                let cycles =
                    self.exception(bus, exceptions::ExceptionVector::Line1010Emulator as u8);
                InstructionResult::branch_taken(cycles)
            }
            0xF => {
                // Line-F (1111) emulator trap - generates Line 1111 exception (vector 11)
                let cycles =
                    self.exception(bus, exceptions::ExceptionVector::Line1111Emulator as u8);
                InstructionResult::branch_taken(cycles)
            }
            _ => self.illegal_instruction(bus, opcode),
        }
    }

    /// Handle illegal/unimplemented instruction by taking the Illegal Instruction exception.
    fn illegal_instruction(&mut self, bus: &mut dyn Addressable, opcode: u16) -> InstructionResult {
        eprintln!("ILLEGAL INSTRUCTION: {:04X} at PC={:08X}", opcode, self.pc);
        let cycles = self.exception(bus, exceptions::ExceptionVector::IllegalInstruction as u8);
        InstructionResult::branch_taken(cycles)
    }

    /// Execute for a number of cycles.
    /// Returns the actual number of cycles executed.
    pub fn run_cycles(&mut self, bus: &mut dyn Addressable, target_cycles: u32) -> u32 {
        let mut cycles_run = 0;
        while cycles_run < target_cycles {
            let instruction_cycles = self.step(bus);
            cycles_run += instruction_cycles;
        }
        cycles_run
    }

    /// Get total cycles executed since creation/reset.
    pub fn total_cycles(&self) -> u64 {
        self.cycles_executed
    }
}

impl Default for M68k {
    fn default() -> Self {
        Self::new()
    }
}

// Savestate support
impl M68k {
    pub fn to_savestate(&self) -> crate::system::savestate::Cpu68kState {
        crate::system::savestate::Cpu68kState {
            d: self.d,
            a: self.a,
            pc: self.pc,
            sr: self.sr,
            ssp: self.ssp,
            usp: self.usp,
            cycles: self.cycles_executed,
            stopped: self.stopped,
            pending_irq_level: self.pending_irq_level,
        }
    }

    pub fn from_savestate(&mut self, state: &crate::system::savestate::Cpu68kState) {
        self.d = state.d;
        self.a = state.a;
        self.pc = state.pc;
        self.sr = state.sr;
        self.ssp = state.ssp;
        self.usp = state.usp;
        self.cycles_executed = state.cycles;
        self.stopped = state.stopped;
        self.pending_irq_level = state.pending_irq_level;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::composable::build_genesis_cartridge;
    use retro_core::memory::Ram;

    #[test]
    fn test_cpu_creation() {
        let cpu = M68k::new();
        assert_eq!(cpu.pc, 0);
        assert_eq!(cpu.sr, 0x2700);
        assert_eq!(cpu.cycles_executed, 0);
    }

    #[test]
    fn test_cpu_reset() {
        let mut cpu = M68k::new();
        // Use a bus instead of raw RAM to get proper Big Endian reads
        use crate::bus::AddressBus;
        let mut bus = AddressBus::new();

        // Setup reset vector
        let mut rom = make_test_rom_with_header(512);
        // SSP at 0x000000: 0x00FF0000 (Big Endian)
        rom[0] = 0x00;
        rom[1] = 0xFF;
        rom[2] = 0x00;
        rom[3] = 0x00;

        // PC at 0x000004: 0x00000400 (Big Endian)
        rom[4] = 0x00;
        rom[5] = 0x00;
        rom[6] = 0x04;
        rom[7] = 0x00;

        let cartridge = build_genesis_cartridge(rom).unwrap();
        bus.set_cartridge(cartridge);

        cpu.reset(&mut bus);

        assert_eq!(cpu.ssp, 0x00FF0000);
        assert_eq!(cpu.pc, 0x00000400);
        assert_eq!(cpu.sr, 0x2700);
    }

    #[test]
    fn test_cpu_step() {
        let mut cpu = M68k::new();
        let mut ram = Ram::new(0x1000);
        cpu.pc = 0x400;

        // Set up a NOP instruction (0x4E71)
        ram.write_word(0x400, 0x4E71);

        let cycles = cpu.step(&mut ram);
        assert_eq!(cycles, 4); // NOP takes 4 cycles
        assert_eq!(cpu.pc, 0x402); // PC advanced by 2
        assert_eq!(cpu.total_cycles(), 4);
    }

    #[test]
    fn test_run_cycles() {
        let mut cpu = M68k::new();
        use crate::bus::AddressBus;
        let mut bus = AddressBus::new();

        // Load dummy ROM (valid header so the cart factory accepts it)
        let rom = make_test_rom_with_header(512);

        let cartridge = build_genesis_cartridge(rom).unwrap();
        bus.set_cartridge(cartridge);

        cpu.pc = 0x400;
        let initial_cycles = cpu.total_cycles();

        let cycles_run = cpu.run_cycles(&mut bus, 20);

        assert!(cycles_run >= 20); // Should execute at least 20 cycles
        assert!(cpu.total_cycles() >= initial_cycles + 20); // Total cycles should increase
        assert_ne!(cpu.pc, 0x400); // PC should have advanced
    }
}
