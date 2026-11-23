//! The Mega Drive's own sound files. The YM2612 and the PSG live in
//! `ym2612` and `psg`; `system/audio.rs` mixes them and keeps their log.

pub mod gym;

pub use gym::{encode_gym, GymHeader, GYMX_HEADER_LEN};
