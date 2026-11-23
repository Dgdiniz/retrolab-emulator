//! The SNES's own sound files: the SPC, the APU's state as a player runs
//! it, and the BRR sample with its WAV and its description.

pub mod brr;
pub mod spc;

pub use brr::{brr_blocks, brr_with_loop_header, sample_wav, SampleFiles, SampleInfo};
pub use spc::{build_spc, register_page, Id666, SPC_LEN};
