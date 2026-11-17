//! DSP-1 coprocessor without its firmware.
//!
//! The DSP-1 is a NEC uPD7725 signal processor with a fixed program. The
//! console sees two registers of it: a data register (`DR`) it writes
//! commands and their operands to and reads results from, and a status
//! register (`SR`). [`Dsp1Hle`] answers those two registers by computing
//! each command's result directly instead of running the chip's program,
//! so it needs no dump of it. The default build installs it for every DSP-1
//! cartridge; the other backend, [`Dsp1Chip`](super::dsp1::Dsp1Chip), runs
//! the program itself and is built only with the `nec-lle` feature.
//!
//! The module is in five parts:
//!
//! - [`rom`]: the chip's data ROM, the one piece of its contents kept here
//!   (constants: powers of two, a reciprocal table, a sine table, pi);
//! - [`number`]: its arithmetic: 1.15 fixed point, mantissa-and-exponent
//!   pairs, reciprocal, sine, cosine, square root;
//! - [`camera`]: the projection commands (`Parameter`, `Raster`,
//!   `Project`, `Target`), which draw Super Mario Kart's and Pilotwings'
//!   ground;
//! - [`attitude`]: the rotation commands;
//! - [`commands`]: the command bytes and the registers the commands share;
//!
//! and this file, the host port: the byte protocol on `DR`.
//!
//! **Where it comes from.** The command set and the meaning of each
//! command's operands are in the console's development documentation and
//! in fullsnes. The arithmetic, which products the chip rounds, where it
//! normalises, which ROM words it reads, is the chip's own, first worked
//! out by Overload, The Dumper, neviksti and Andreas Naive, whose findings
//! the snes9x emulator carries; it was checked against snes9x. What holds
//! it to the chip is the chip: the `nec-lle` build runs the real program
//! on the uPD7725 interpreter, and the two are compared command by
//! command, on chosen vectors and on operands drawn at random, every word
//! equal, and over the whole stream of bytes Super Mario Kart exchanges
//! with the chip.
//!
//! **The host port.** A command is one byte written to `DR`. Its operands
//! follow as 16-bit words, low byte first, and it runs when the last byte
//! of the last word arrives; the result waits to be read from `DR` the same
//! way. `SR` always reports the chip ready, since nothing here takes time.
//! Three details of the exchange matter to games:
//!
//! - a byte that is no command (`0x80`, which Super Mario Kart pads with)
//!   leaves the port waiting for one;
//! - `Raster` does not end: when the host has read one line's four words
//!   the next line's are already waiting, so a game reads line after line
//!   from one command;
//! - while a raster line is waiting, a *write* to `DR` discards one byte of
//!   it instead of starting a command. A game ends a run of lines by
//!   writing until the line is gone, and only then sends its next command.

mod attitude;
mod camera;
mod commands;
mod number;
mod rom;

use retro_core::cartridge::{
    ChipRegister, Coprocessor, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
};

use crate::savestate::{CoprocessorState, Dsp1HleState};
use commands::{Answer, Command, Registers};

/// `SR` as the host reads it: bit 7, "ready for a transfer", always set.
const STATUS_READY: u8 = 0x80;

/// What `DR` reads as when no result is waiting.
const NOTHING_TO_READ: u8 = 0x80;

/// Room for a command's operand bytes. The longest command takes seven
/// words.
const OPERAND_BYTES: usize = 16;

/// Room for a result's bytes. The longest result is four words.
const RESULT_BYTES: usize = 16;

/// DSP-1 backend that computes each command's result itself.
pub struct Dsp1Hle {
    /// The address bit that tells `SR` from `DR`, from the cartridge's
    /// [`Dsp1Layout`](super::dsp1::Dsp1Layout).
    status_address_bit: u32,
    /// `"DSP-1 (HLE)"`, for the CLI's `info` command.
    label: String,
    info: ExpansionChipInfo,
    /// Reads and writes of the two registers since reset.
    accesses: u64,

    /// The last byte taken as a command.
    command_byte: u8,
    /// The next byte written to `DR` is a command, not an operand.
    awaiting_command: bool,
    /// A command has been taken and none of its operand bytes has arrived.
    command_is_fresh: bool,
    /// Operand bytes the command still needs before it runs.
    operand_bytes_due: usize,
    /// Operand bytes received since the command. Bytes past the buffer's
    /// room are counted and dropped.
    operand_bytes_seen: usize,
    operands: [u8; OPERAND_BYTES],
    /// Result bytes not read yet, and where the next one is.
    result_bytes_due: usize,
    result_cursor: usize,
    result: [u8; RESULT_BYTES],
    /// The result is the data ROM itself, read straight from it rather
    /// than from `result`.
    dumping_rom: bool,

    registers: Registers,
}

impl Dsp1Hle {
    /// A chip for a cartridge whose `SR` is selected by `status_address_bit`.
    pub fn new(status_address_bit: u32) -> Self {
        Self {
            status_address_bit,
            label: "DSP-1 (HLE)".to_string(),
            info: ExpansionChipInfo {
                name: "DSP-1".to_string(),
                chip_type: ExpansionChipType::DSP1,
                description: "high-level emulation (firmware-free)".to_string(),
                features: vec!["hle".to_string()],
            },
            accesses: 0,
            command_byte: 0,
            awaiting_command: true,
            command_is_fresh: false,
            operand_bytes_due: 0,
            operand_bytes_seen: 0,
            operands: [0; OPERAND_BYTES],
            result_bytes_due: 0,
            result_cursor: 0,
            result: [0; RESULT_BYTES],
            dumping_rom: false,
            registers: Registers::default(),
        }
    }

    /// Everything a reset returns to its power-on value.
    fn power_on(&mut self) {
        *self = Self::new(self.status_address_bit);
    }

    /// Whether the last command taken streams raster lines.
    fn in_a_raster_run(&self) -> bool {
        Command::of(self.command_byte) == Some(Command::Raster)
    }

    /// Queue an answer's words as the bytes the host will read.
    fn queue(&mut self, answer: Answer) {
        let mut bytes = 0;
        for word in answer.words() {
            self.result[bytes..bytes + 2].copy_from_slice(&word.to_le_bytes());
            bytes += 2;
        }
        self.result_cursor = 0;
        self.result_bytes_due = bytes;
    }

    /// The host reads `DR`.
    fn read_data(&mut self) -> u8 {
        if self.result_bytes_due == 0 {
            return NOTHING_TO_READ;
        }
        let byte = if self.dumping_rom {
            rom::dump_byte(self.result_cursor)
        } else {
            self.result[self.result_cursor]
        };
        self.result_cursor += 1;
        self.result_bytes_due -= 1;

        if self.result_bytes_due == 0 && self.in_a_raster_run() {
            // The line is read: the next one is ready before the host asks.
            let next = self.registers.next_raster();
            self.queue(next);
        }
        // Reading a result byte ends whatever the host was sending: the
        // next write is a command.
        self.awaiting_command = true;
        byte
    }

    /// The host writes `DR`.
    fn write_data(&mut self, byte: u8) {
        if self.in_a_raster_run() && self.result_bytes_due != 0 {
            // A raster line is waiting: the write throws one byte of it
            // away.
            self.result_bytes_due -= 1;
            self.result_cursor += 1;
        } else if self.awaiting_command {
            self.take_command(byte);
        } else {
            self.take_operand(byte);
        }
    }

    fn take_command(&mut self, byte: u8) {
        self.command_byte = byte;
        self.operand_bytes_seen = 0;
        match Command::of(byte) {
            Some(command) => {
                self.awaiting_command = false;
                self.command_is_fresh = true;
                self.operand_bytes_due = 2 * command.words_in();
            }
            None => {
                // Not a command: keep waiting for one.
                self.command_is_fresh = false;
                self.operand_bytes_due = 0;
            }
        }
    }

    fn take_operand(&mut self, byte: u8) {
        if let Some(slot) = self.operands.get_mut(self.operand_bytes_seen) {
            *slot = byte;
        }
        self.operand_bytes_seen += 1;
        self.command_is_fresh = false;
        if self.operand_bytes_due != 0 {
            self.operand_bytes_due -= 1;
            if self.operand_bytes_due == 0 {
                self.run_command();
            }
        }
    }

    /// The last operand byte is in: run the command and queue its answer.
    fn run_command(&mut self) {
        self.awaiting_command = true;
        self.result_cursor = 0;
        let Some(command) = Command::of(self.command_byte) else {
            self.dumping_rom = false;
            self.result_bytes_due = 0;
            return;
        };
        if command == Command::MemoryDump {
            self.dumping_rom = true;
            self.result_bytes_due = 2 * rom::WORDS;
            return;
        }
        self.dumping_rom = false;
        let received = &self.operands[..self.operand_bytes_seen.min(OPERAND_BYTES)];
        // Whole words only: a lone trailing byte is not an operand.
        let (pairs, _) = received.as_chunks::<2>();
        let words: Vec<u16> = pairs.iter().map(|&pair| u16::from_le_bytes(pair)).collect();
        let answer = self.registers.run(command, &words);
        self.queue(answer);
    }

    /// The host-visible state, for a savestate.
    pub fn savestate(&self) -> Option<CoprocessorState> {
        let operands = self.operands[..self.operand_bytes_seen.min(OPERAND_BYTES)].to_vec();
        let result = if self.dumping_rom {
            Vec::new()
        } else {
            let from = self.result_cursor.min(RESULT_BYTES);
            let to = (self.result_cursor + self.result_bytes_due).min(RESULT_BYTES);
            self.result[from..to].to_vec()
        };
        Some(CoprocessorState::Dsp1Hle(Dsp1HleState {
            command: self.command_byte,
            awaiting_command: self.awaiting_command,
            command_is_fresh: self.command_is_fresh,
            operand_bytes_due: self.operand_bytes_due as u16,
            operands,
            result,
            dumping_rom: self.dumping_rom,
            rom_dump_position: if self.dumping_rom {
                self.result_cursor as u32
            } else {
                0
            },
            registers: self.registers.words(),
            accesses: self.accesses,
        }))
    }

    /// Take the state back from a savestate. A state of another chip is
    /// ignored.
    pub fn restore_savestate(&mut self, state: &CoprocessorState, _master_clock_hz: u64) {
        let CoprocessorState::Dsp1Hle(saved) = state else {
            return;
        };
        self.power_on();
        self.command_byte = saved.command;
        self.accesses = saved.accesses;
        self.awaiting_command = saved.awaiting_command;
        self.command_is_fresh = saved.command_is_fresh;
        self.operand_bytes_due = usize::from(saved.operand_bytes_due);
        self.registers = Registers::from_words(&saved.registers);

        let kept = saved.operands.len().min(OPERAND_BYTES);
        self.operands[..kept].copy_from_slice(&saved.operands[..kept]);
        self.operand_bytes_seen = kept;

        if saved.dumping_rom {
            self.dumping_rom = true;
            self.result_cursor = saved.rom_dump_position as usize;
            self.result_bytes_due = (2 * rom::WORDS).saturating_sub(self.result_cursor);
        } else {
            let kept = saved.result.len().min(RESULT_BYTES);
            self.result[..kept].copy_from_slice(&saved.result[..kept]);
            self.result_cursor = 0;
            self.result_bytes_due = kept;
        }
    }
}

impl Coprocessor for Dsp1Hle {
    fn info(&self) -> &ExpansionChipInfo {
        &self.info
    }

    fn read(&mut self, offset: u32, _master_cycle: u64) -> u8 {
        self.accesses += 1;
        if offset & self.status_address_bit != 0 {
            STATUS_READY
        } else {
            self.read_data()
        }
    }

    fn write(&mut self, offset: u32, value: u8, _master_cycle: u64) {
        self.accesses += 1;
        // The host cannot write `SR`.
        if offset & self.status_address_bit == 0 {
            self.write_data(value);
        }
    }

    /// Nothing runs between accesses, so there is nothing to catch up.
    fn catch_up(&mut self, _master_cycle: u64) {}

    fn reset(&mut self, _master_clock_hz: u64) {
        self.power_on();
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn accesses(&self) -> u64 {
        self.accesses
    }

    /// The savestate as bytes: a `bincode`-encoded
    /// [`CoprocessorState::Dsp1Hle`].
    fn serialize(&self) -> Vec<u8> {
        match self.savestate() {
            Some(state) => bincode::serialize(&state).unwrap_or_default(),
            None => Vec::new(),
        }
    }

    fn restore(&mut self, data: &[u8], master_clock_hz: u64) {
        if let Ok(state) = bincode::deserialize::<CoprocessorState>(data) {
            self.restore_savestate(&state, master_clock_hz);
        }
    }

    fn debug_state(&self) -> ExpansionChipState {
        let idle = self.awaiting_command && self.result_bytes_due == 0;
        // 2 while a result waits, 1 while operands are due, 0 otherwise.
        let phase = if self.result_bytes_due != 0 {
            2
        } else if !self.awaiting_command {
            1
        } else {
            0
        };
        let rows: [(&str, u32, &str); 9] = [
            (
                "CMD",
                if idle {
                    0xFFFF_FFFF
                } else {
                    u32::from(self.command_byte)
                },
                "Active command opcode (0xFFFFFFFF = idle)",
            ),
            (
                "PHASE",
                phase,
                "Handshake phase (0 idle / 1 input / 2 output)",
            ),
            (
                "IN_LEFT",
                self.operand_bytes_due as u32,
                "Input bytes still expected",
            ),
            (
                "IN_HAVE",
                self.operand_bytes_seen as u32,
                "Input bytes collected",
            ),
            (
                "OUT_LEFT",
                self.result_bytes_due as u32,
                "Output bytes queued",
            ),
            ("OUT_INDEX", self.result_cursor as u32, "Output read cursor"),
            ("SR", u32::from(STATUS_READY), "Status register (host view)"),
            (
                "RASTER_VS",
                u32::from(self.registers.raster_line as u16),
                "Next Mode-7 raster scanline index",
            ),
            (
                "ACCESSES",
                self.accesses as u32,
                "CPU DR/SR accesses since reset (diagnostic, truncated)",
            ),
        ];
        let registers = rows
            .iter()
            .zip(0u32..)
            .map(|(&(name, value, description), address)| ChipRegister {
                address,
                name: name.to_string(),
                value,
                description: description.to_string(),
            })
            .collect();
        // The shared registers stand in for the chip's RAM in the debugger,
        // as little-endian words.
        let internal_ram = self
            .registers
            .words()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        ExpansionChipState {
            chip_type: ExpansionChipType::DSP1,
            registers,
            internal_ram: Some(internal_ram),
            program_counter: None,
            cycle_count: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DR: u32 = 0x0000;
    const SR: u32 = 0x4000;

    fn new_chip() -> Dsp1Hle {
        Dsp1Hle::new(SR)
    }

    fn write_word(chip: &mut Dsp1Hle, word: u16) {
        for byte in word.to_le_bytes() {
            chip.write(DR, byte, 0);
        }
    }

    fn read_word(chip: &mut Dsp1Hle) -> u16 {
        u16::from_le_bytes([chip.read(DR, 0), chip.read(DR, 0)])
    }

    /// One whole exchange, the way the console does it: the command byte,
    /// its words, then `answers` words read back.
    fn exchange(chip: &mut Dsp1Hle, command: u8, words: &[u16], answers: usize) -> Vec<u16> {
        assert_eq!(chip.read(SR, 0) & STATUS_READY, STATUS_READY);
        chip.write(DR, command, 0);
        for &word in words {
            write_word(chip, word);
        }
        (0..answers).map(|_| read_word(chip)).collect()
    }

    /// A camera high over the ground, looking down at a slant.
    const CAMERA: [u16; 7] = [0, 0, 0x40, 0x20, 0x100, 0, 0x2000];

    #[test]
    fn a_new_chip_is_ready_and_names_itself() {
        let mut chip = new_chip();
        assert_eq!(chip.label(), "DSP-1 (HLE)");
        assert_eq!(chip.read(SR, 0), STATUS_READY);
        assert_eq!(chip.accesses(), 1);
    }

    #[test]
    fn a_command_runs_when_its_last_byte_arrives() {
        let mut chip = new_chip();
        assert_eq!(exchange(&mut chip, 0x00, &[0x4000, 0x4000], 1), [0x2000]);
        assert_eq!(exchange(&mut chip, 0x00, &[0x8000, 0x4000], 1), [0xc000]);
        // 1 / 1/2: the format's "1", one place up.
        assert_eq!(
            exchange(&mut chip, 0x10, &[0x4000, 0x0000], 2),
            [0x7fff, 0x0001]
        );
    }

    #[test]
    fn nothing_waiting_reads_as_0x80() {
        let mut chip = new_chip();
        for _ in 0..8 {
            assert_eq!(chip.read(DR, 0), NOTHING_TO_READ);
        }
    }

    #[test]
    fn a_byte_that_is_no_command_leaves_the_port_waiting() {
        let mut chip = new_chip();
        chip.write(DR, 0x80, 0);
        assert!(chip.awaiting_command);
        assert_eq!(chip.result_bytes_due, 0);
        assert_eq!(exchange(&mut chip, 0x00, &[0x4000, 0x4000], 1), [0x2000]);
    }

    #[test]
    fn raster_goes_on_from_line_to_line() {
        let mut chip = new_chip();
        exchange(&mut chip, 0x02, &CAMERA, 4);
        chip.write(DR, 0x0a, 0);
        write_word(&mut chip, 0);
        let mut line = || [(); 4].map(|()| read_word(&mut chip));
        let lines = [(); 6].map(|()| line());
        assert_ne!(
            lines[0], lines[5],
            "the matrix changes down the screen: {lines:x?}"
        );
        // Six lines read, and the seventh made ready as the sixth ended.
        assert_eq!(chip.registers.raster_line, 7);
    }

    #[test]
    fn a_write_during_a_raster_run_discards_a_byte_of_the_line() {
        let mut chip = new_chip();
        exchange(&mut chip, 0x02, &CAMERA, 4);
        exchange(&mut chip, 0x0a, &[0], 4);

        // The next line is already waiting, so a write is not a command.
        let waiting = chip.result_bytes_due;
        assert_eq!(waiting, 8);
        chip.write(DR, 0x00, 0);
        assert_eq!(chip.result_bytes_due, waiting - 1);
        assert_eq!(chip.command_byte, 0x0a);

        // Writing the line away ends the run; the next byte is a command.
        for _ in 0..chip.result_bytes_due {
            chip.write(DR, 0x00, 0);
        }
        assert_eq!(chip.result_bytes_due, 0);
        assert_eq!(exchange(&mut chip, 0x00, &[0x4000, 0x4000], 1), [0x2000]);
    }

    #[test]
    fn an_attitude_answers_nothing_and_its_vectors_three_words() {
        let mut chip = new_chip();
        assert!(exchange(&mut chip, 0x01, &[0x7fff, 0, 0, 0], 0).is_empty());
        assert_eq!(chip.result_bytes_due, 0);
        let turned = exchange(&mut chip, 0x0d, &[0x0100, 0x0080, 0x0200], 3);
        assert_eq!(turned.len(), 3);
        assert_eq!(chip.result_bytes_due, 0);
    }

    #[test]
    fn the_memory_dump_streams_the_data_rom() {
        let mut chip = new_chip();
        chip.write(DR, 0x1f, 0);
        write_word(&mut chip, 0);
        assert_eq!(chip.result_bytes_due, 2 * rom::WORDS);
        let words: Vec<u16> = (0..rom::WORDS).map(|_| read_word(&mut chip)).collect();
        assert_eq!(words[0x22], 0x0001);
        assert_eq!(words[rom::PI as usize], 0x6488);
        assert_eq!(words[rom::WORDS - 1], 0xffff);
        assert_eq!(chip.read(DR, 0), NOTHING_TO_READ);
    }

    #[test]
    fn a_savestate_brings_back_the_registers_and_the_exchange() {
        let mut chip = new_chip();
        exchange(&mut chip, 0x02, &CAMERA, 4);
        // Stop half way through a raster line.
        chip.write(DR, 0x0a, 0);
        write_word(&mut chip, 7);
        let first = read_word(&mut chip);

        let saved = chip.savestate().expect("the chip has a state");
        let mut restored = new_chip();
        restored.restore_savestate(&saved, 0);
        assert_eq!(restored.registers, chip.registers);
        assert_eq!(restored.serialize(), chip.serialize());

        // Both go on with the same bytes.
        let rest: Vec<u16> = (0..7).map(|_| read_word(&mut chip)).collect();
        let rest_restored: Vec<u16> = (0..7).map(|_| read_word(&mut restored)).collect();
        assert_eq!(rest, rest_restored);
        assert_ne!(first, rest[3], "the next line differs from the first");
    }

    #[test]
    fn a_reset_clears_the_exchange_and_the_registers() {
        let mut chip = new_chip();
        exchange(&mut chip, 0x01, &[1, 2, 3, 4], 0);
        exchange(&mut chip, 0x0a, &[9], 1);
        chip.write(DR, 0x08, 0);
        chip.reset(0);
        assert_eq!(chip.accesses(), 0);
        assert_eq!(chip.registers, Registers::default());
        assert!(chip.awaiting_command);
        assert_eq!(chip.operand_bytes_due, 0);
        assert_eq!(chip.result_bytes_due, 0);
    }

    #[test]
    fn the_debugger_sees_the_port_and_the_registers() {
        let state = new_chip().debug_state();
        assert_eq!(state.chip_type, ExpansionChipType::DSP1);
        assert_eq!(state.program_counter, None);
        assert_eq!(
            state.internal_ram.as_ref().map(Vec::len),
            Some(commands::STATE_WORDS * 2)
        );
        let names: Vec<&str> = state.registers.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "CMD",
                "PHASE",
                "IN_LEFT",
                "IN_HAVE",
                "OUT_LEFT",
                "OUT_INDEX",
                "SR",
                "RASTER_VS",
                "ACCESSES"
            ]
        );
        assert_eq!(state.registers[0].value, 0xFFFF_FFFF);
    }
}
