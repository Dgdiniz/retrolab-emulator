//! # Retro WASM
//!
//! What the console cores' WebAssembly crates share: the functions that turn
//! a core's values into the JavaScript values a host reads, and the macros
//! that write the `#[wasm_bindgen]` methods every core exports the same way.
//!
//! `#[wasm_bindgen]` cannot export a generic, so a method both crates export
//! is written here once as a `macro_rules!` macro, which each binding crate
//! expands on its own `RetroEmulator`: the macro writes a second
//! `#[wasm_bindgen] impl` block whose methods call a `retro-core` trait on a
//! field of the struct and one of the marshallers.
//!
//! - [`js`]: the values every binding crate builds the same way (plain
//!   objects, typed-array copies, values serialized with maps as objects).
//! - [`capture`]: the capture contract's marshallers, and
//!   [`capture_bindings!`] for its methods.
//! - [`cartridge`]: [`cartridge_bindings!`] for the cartridge's methods.
//! - [`debugger`]: the debugger's values (execution modes and breakpoint
//!   types by name, the coverage figures), and [`debugger_bindings!`] for
//!   its methods.
//! - `test_support` (feature `test-support`): `test_support::shape`, a
//!   description of a JavaScript value a test can compare.
//!
//! A crate that expands a macro of this crate depends on `wasm-bindgen`
//! itself, as every crate declaring a `#[wasm_bindgen]` struct does: the
//! code `#[wasm_bindgen]` writes names that crate.

pub mod capture;
pub mod cartridge;
pub mod debugger;
#[cfg(test)]
mod fake_machine;
pub mod js;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

/// A core's build identity, which the netplay handshake compares:
/// `"{core_id}@{version}+{git_hash}"`, the hash `"unknown"` outside a git
/// checkout. Two peers refuse to connect on a mismatch, since another build
/// can diverge deterministically.
pub fn build_id(core_id: &str, version: &str, git_hash: Option<&str>) -> String {
    format!("{core_id}@{version}+{}", git_hash.unwrap_or("unknown"))
}

#[cfg(test)]
mod build_id_tests {
    #[test]
    fn names_the_core_its_version_and_its_commit() {
        assert_eq!(
            super::build_id("sg1", "0.1.0", Some("ab12cd3")),
            "sg1@0.1.0+ab12cd3"
        );
        assert_eq!(
            super::build_id("snes1", "0.1.0", None),
            "snes1@0.1.0+unknown"
        );
    }
}

/// What the macros of this crate name through `$crate`; not for direct use.
#[doc(hidden)]
pub mod __private {
    pub use js_sys;
    pub use retro_core;
    pub use wasm_bindgen;
}
