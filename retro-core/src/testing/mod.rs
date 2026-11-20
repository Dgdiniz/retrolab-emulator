//! The harnesses that run a part of a core against published test data,
//! written once for every core and CPU crate.
//!
//! - `singlestep` (feature `singlestep`): the runner of CPU test vectors,
//!   the processor's state before and after one instruction, case by case
//!   (Tom Harte's 68000 vectors and the SingleStepTests of the Z80, the
//!   65C816 and the SPC700). A CPU crate or a core supplies its CPU under
//!   test, the runner does the rest.

#[cfg(feature = "singlestep")]
pub mod singlestep;
