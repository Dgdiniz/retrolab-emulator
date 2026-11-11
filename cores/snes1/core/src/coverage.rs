//! Execution coverage tracking.
//!
//! This module is empty: the debugger (`crate::debugger`) records which
//! addresses the 5A22 and the SPC700 have executed, one
//! `retro_core::debug::CoverageBitmap` per CPU, and the disassembly
//! window decodes the covered addresses as instructions and shows the
//! rest as unknown bytes.
