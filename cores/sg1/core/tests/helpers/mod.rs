//! Helper modules for testing
//!
//! Every integration-test binary under `tests/` compiles this module
//! independently, so each binary uses a different subset of these
//! re-exports. That makes any one of them look "unused" to a per-binary
//! lint while another binary depends on it, hence the explicit allow:
//! deleting an apparently-unused line here breaks a sibling test target.

pub mod m68k_rom_builder;
pub mod mini_cartridge;
pub mod z80_rom_builder;
pub mod z80_test_runner;

#[allow(unused_imports)]
pub use m68k_rom_builder::M68kRomBuilder;
#[allow(unused_imports)]
pub use mini_cartridge::MiniCartridgeBuilder;
#[allow(unused_imports)]
pub use z80_rom_builder::Z80RomBuilder;
#[allow(unused_imports)]
pub use z80_test_runner::{Z80GenesisTestRunner, Z80TestRunner};
