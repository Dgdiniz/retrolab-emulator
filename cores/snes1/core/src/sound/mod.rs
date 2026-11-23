//! The SNES's own sound files: the SPC, the APU's state as a player runs
//! it.

pub mod spc;

pub use spc::{build_spc, register_page, Id666, SPC_LEN};
