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
//!   objects, typed-array copies, values serialized with maps as objects,
//!   pictures, the refusals a host tells apart by name).
//! - [`machine`]: [`machine_bindings!`] for the machine's methods (its
//!   information, resets, frames, region, savestates and battery save).
//! - [`display`]: [`display_bindings!`] for the picture's methods, and
//!   [`display::PresentBuffer`], a picture packed out of a wider
//!   framebuffer.
//! - [`audio`]: [`audio_bindings!`] for the host's sound.
//! - [`input`]: [`input_bindings!`] for the keys, pads and ports.
//! - [`video`]: [`video_bindings!`] for the video chip's debugger.
//! - [`capture`]: the capture contract's marshallers, and
//!   [`capture_bindings!`] for its methods.
//! - [`cartridge`]: [`cartridge_bindings!`] for the cartridge's methods.
//! - [`debugger`]: the debugger's values (execution modes and breakpoint
//!   types by name, the coverage figures), and [`debugger_bindings!`] for
//!   its methods.
//! - [`module_start!`]: the module's start function.
//! - `test_support` (feature `test-support`): `test_support::shape`, a
//!   description of a JavaScript value a test can compare.
//!
//! A binding crate expands every macro, each on its emulator struct, in one
//! order, and writes only what its console makes different: its system
//! names, its own chips' methods, and the methods it keeps under older
//! names.
//!
//! A crate that expands a macro of this crate depends on `wasm-bindgen`
//! itself, as every crate declaring a `#[wasm_bindgen]` struct does: the
//! code `#[wasm_bindgen]` writes names that crate.

pub mod audio;
pub mod capture;
pub mod cartridge;
pub mod debugger;
pub mod display;
#[cfg(test)]
mod fake_machine;
pub mod input;
pub mod js;
pub mod machine;
#[cfg(test)]
mod macro_tests;
pub mod video;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

/// A core's build identity, which the netplay handshake compares:
/// `"{core_id}@{version}+{git_hash}"`, the workspace's version and the
/// commit `build.rs` reads, `"unknown"` outside a git checkout. Two peers
/// refuse to connect on a mismatch, since another build can diverge
/// deterministically.
pub fn build_id(core_id: &str) -> String {
    format!(
        "{core_id}@{}+{}",
        env!("CARGO_PKG_VERSION"),
        option_env!("GIT_HASH").unwrap_or("unknown")
    )
}

#[cfg(test)]
mod build_id_tests {
    #[test]
    fn names_the_core_the_version_and_the_commit() {
        let id = super::build_id("sg1");
        let commit = id
            .strip_prefix(concat!("sg1@", env!("CARGO_PKG_VERSION"), "+"))
            .expect("the core and the version first");
        assert!(!commit.is_empty());
        assert_eq!(option_env!("GIT_HASH").unwrap_or("unknown"), commit);
    }
}

/// Writes the module's start function, `init`, which installs the panic hook
/// so a Rust panic reaches the browser's console as a readable message.
/// Each binding crate expands it once.
#[macro_export]
macro_rules! module_start {
    () => {
        /// Install the panic hook, so a Rust panic reaches the browser's
        /// console as a readable message instead of an opaque
        /// `RuntimeError: unreachable`.
        #[$crate::__private::wasm_bindgen::prelude::wasm_bindgen(start)]
        pub fn init() {
            $crate::__private::console_error_panic_hook::set_once();
        }
    };
}

/// What the macros of this crate name through `$crate`; not for direct use.
#[doc(hidden)]
pub mod __private {
    pub use console_error_panic_hook;
    pub use js_sys;
    pub use retro_core;
    pub use wasm_bindgen;
}
