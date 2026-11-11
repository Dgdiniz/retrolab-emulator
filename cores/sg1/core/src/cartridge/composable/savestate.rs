//! Typed coprocessor savestate for the composable Genesis cartridge.
//!
//! The generic [`Coprocessor`](retro_core::cartridge::Coprocessor) trait
//! serializes to opaque `Vec<u8>` so the `retro-core` layer stays
//! console-agnostic. Each Genesis chip `bincode`-encodes one variant of
//! [`GenesisCoprocessorState`] inside that blob, and
//! [`ComposableCartridge`](super::ComposableCartridge) decodes it back,
//! exactly the bridge the SNES core uses with its `CoprocessorState`.
//!
//! The enum is backend-tagged: a blob saved by one chip never silently
//! restores into another (a J-Cart blob won't load into the SVP). New
//! variants are appended so existing indices stay stable and older
//! savestates keep decoding.

use serde::{Deserialize, Serialize};

/// Which dynamic banking scheme a bank-mapper chip implements. The chip
/// owns the cartridge ROM and translates 68000 ROM-window reads through a
/// per-slot bank table; this tag records which register protocol drives
/// that table so a restore rebinds the right one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BankMapperKind {
    /// SEGA mapper (`0xA130F0..0xA130F7` bank registers, 8 x 512 KiB slots).
    Sega,
    /// Super Street Fighter II (`0xA130F1` toggles bank 7/8 into slot 7).
    Ssf2,
    /// Codemasters (bank selects written into ROM-window addresses).
    Codemasters,
}

/// Volatile state of a bank-switching mapper chip: the per-slot bank
/// selection. SEGA carries 8 entries (one per 512 KiB slot), Codemasters
/// 4, SSF2 a single entry (the high bank latched into slot 7). The chip
/// re-derives `bank_count` from the ROM it is handed on restore, so only
/// the live selection is persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BankMapperState {
    pub kind: BankMapperKind,
    pub banks: Vec<u8>,
}

/// Serial-EEPROM protocol phase (I2C-like state machine). Mirror of the
/// chip's internal enum so the exact handshake position round-trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EepromPhase {
    Idle,
    DeviceAddress,
    DeviceAck,
    MemoryAddress,
    MemoryAck,
    WriteData,
    WriteAck,
    ReadData,
    ReadAck,
    Stop,
}

/// Volatile state of a serial EEPROM chip (X24C01/X24C02). `data` is the
/// battery-backed contents (also surfaced as the cartridge's save RAM);
/// the remaining fields are the mid-transfer protocol state so a savestate
/// taken during a read/write handshake resumes cleanly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EepromChipState {
    pub data: Vec<u8>,
    pub phase: EepromPhase,
    pub sda: bool,
    pub scl: bool,
    pub sda_prev: bool,
    pub scl_prev: bool,
    pub bit_count: u8,
    pub current_byte: u8,
    pub device_addr: u8,
    pub rw_bit: bool,
    pub memory_addr: u8,
}

/// Volatile state of a J-Cart controller-port expander. Only the latched
/// port/TH registers persist; the connected controllers' button state is
/// driven live by the input layer each frame, not saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct JCartState {
    pub port3_data: u8,
    pub port4_data: u8,
    pub port3_th: bool,
    pub port4_th: bool,
}

/// Volatile state of the SVP coprocessor.
///
/// The SVP shares a block of DRAM with the 68000 and runs an SSP1601 DSP out
/// of internal RAM. This snapshot persists that shared DRAM plus the entire
/// SSP1601 register/RAM/stack/PMAC file, including the mailbox registers
/// (`SSP_XST`/`SSP_PM0` live in `gr`) and the wait-loop flags (`emu_status`),
/// so a savestate taken mid-handshake or mid-render resumes cleanly. The
/// program ROM half of the SSP's instruction memory is *not* saved: it is
/// re-copied from the cartridge on restore, so only the writable `iram`
/// (words `0x000..0x400`) is persisted. `last_master_cycle`/`ssp_accum` are
/// the coprocessor's catch-up anchor (in 68K cycles) and its carry. Kept
/// `Default` so the chip round-trips an empty snapshot with no special-casing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SvpState {
    /// Shared 68000/SSP DRAM (`0x300000-0x31FFFF`), big-endian bytes.
    pub dram: Vec<u8>,
    /// SSP1601 general register file (`gr[16]`; 16-bit regs in the high half,
    /// `A`/`P`/`PMC` full 32-bit). Carries `SSP_XST`/`SSP_PM0`/`SSP_PC`.
    pub gr: [u32; 16],
    /// Two internal RAM banks (512 words).
    pub ram: Vec<u16>,
    /// Eight 8-bit pointer registers.
    pub ptr: [u8; 8],
    /// Six-level hardware stack.
    pub stack: [u16; 6],
    /// PMAC read/write address+mode file for PM0-PM5.
    pub pmac: [[u32; 6]; 2],
    /// PMC programming state + wait-loop flags (`SSP_WAIT_*`).
    pub emu_status: u32,
    /// Writable IRAM (instruction-memory words `0x000..0x400`); the program
    /// ROM half is reconstructed from the cartridge on restore.
    pub iram: Vec<u16>,
    /// SSP1601 cycle counter (diagnostic).
    pub cycles: u64,
    /// 68K-cycle anchor of the last catch-up (`None` before the first).
    pub last_master_cycle: Option<u64>,
    /// Fractional master-cycle->SSP-cycle carry across catch-ups.
    pub ssp_accum: u64,
}

/// Typed, backend-tagged coprocessor state for a Genesis savestate. Each
/// composable-cartridge chip serializes exactly one of these; the
/// cartridge keys them by `chip_id` in the multi-chip snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GenesisCoprocessorState {
    BankMapper(BankMapperState),
    Eeprom(EepromChipState),
    JCart(JCartState),
    Svp(SvpState),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bank_mapper_state_round_trips_via_bincode() {
        let state = GenesisCoprocessorState::BankMapper(BankMapperState {
            kind: BankMapperKind::Sega,
            banks: vec![0, 1, 2, 3, 4, 5, 6, 7],
        });
        let bytes = bincode::serialize(&state).unwrap();
        let back: GenesisCoprocessorState = bincode::deserialize(&bytes).unwrap();
        assert_eq!(state, back);
    }

    #[test]
    fn eeprom_state_round_trips_via_bincode() {
        let state = GenesisCoprocessorState::Eeprom(EepromChipState {
            data: vec![0xAB; 128],
            phase: EepromPhase::WriteData,
            sda: false,
            scl: true,
            sda_prev: true,
            scl_prev: false,
            bit_count: 3,
            current_byte: 0x5A,
            device_addr: 0x50,
            rw_bit: true,
            memory_addr: 0x10,
        });
        let bytes = bincode::serialize(&state).unwrap();
        assert_eq!(
            bincode::deserialize::<GenesisCoprocessorState>(&bytes).unwrap(),
            state
        );
    }

    #[test]
    fn jcart_and_svp_states_round_trip() {
        let jcart = GenesisCoprocessorState::JCart(JCartState {
            port3_data: 0x40,
            port4_data: 0x00,
            port3_th: true,
            port4_th: false,
        });
        let svp = GenesisCoprocessorState::Svp(SvpState {
            dram: vec![1, 2, 3],
            gr: [0x1234_5678; 16],
            ram: vec![0xAAAA; 512],
            ptr: [1, 2, 3, 4, 5, 6, 7, 0],
            stack: [10, 20, 30, 40, 50, 60],
            pmac: [[1, 2, 3, 4, 5, 6], [7, 8, 9, 10, 11, 12]],
            emu_status: 0x2000,
            iram: vec![0x0F0F; 0x400],
            cycles: 999,
            last_master_cycle: Some(123_456),
            ssp_accum: 42,
        });
        for state in [jcart, svp] {
            let bytes = bincode::serialize(&state).unwrap();
            assert_eq!(
                bincode::deserialize::<GenesisCoprocessorState>(&bytes).unwrap(),
                state
            );
        }
    }

    #[test]
    fn variants_are_backend_tagged_distinctly() {
        // A J-Cart blob must not decode as (or equal) an SVP state, the
        // tag keeps chips from cross-loading each other's bytes.
        let jcart = GenesisCoprocessorState::JCart(JCartState::default());
        let svp = GenesisCoprocessorState::Svp(SvpState::default());
        assert_ne!(jcart, svp);
    }
}
