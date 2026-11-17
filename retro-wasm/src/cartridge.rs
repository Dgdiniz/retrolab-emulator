//! [`cartridge_bindings!`], which writes the cartridge methods every
//! binding crate exports the same way.
//!
//! [`cartridge_bindings!`]: crate::cartridge_bindings

/// Writes the cartridge methods on `$emulator`, over the core in its field
/// `$core`: `getCartridgeHash(): string | undefined`, the SHA-256 of the
/// ROM file the core's last successful `loadRom` received, in lowercase
/// hex, `undefined` before one (the core's `get_cartridge_hash`). Its Rust
/// name is the JavaScript name in snake case, which no other method of the
/// struct may have. The crate expanding the macro depends on `wasm-bindgen`
/// itself, as every crate declaring a `#[wasm_bindgen]` struct does.
#[macro_export]
macro_rules! cartridge_bindings {
    ($emulator:ident, $core:ident $(,)?) => {
        #[$crate::__private::wasm_bindgen::prelude::wasm_bindgen]
        impl $emulator {
            /// The SHA-256 of the ROM file the last successful `loadRom`
            /// received, in lowercase hex; `undefined` before one.
            #[wasm_bindgen(js_name = getCartridgeHash)]
            pub fn get_cartridge_hash(&self) -> ::core::option::Option<::std::string::String> {
                self.$core.get_cartridge_hash()
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use wasm_bindgen::prelude::wasm_bindgen;
    use wasm_bindgen_test::*;

    /// A core that answers the hash it was given.
    struct FakeCore {
        hash: Option<String>,
    }

    impl FakeCore {
        fn get_cartridge_hash(&self) -> Option<String> {
            self.hash.clone()
        }
    }

    #[wasm_bindgen]
    pub struct CartridgeEmulator {
        core: FakeCore,
    }

    crate::cartridge_bindings!(CartridgeEmulator, core);

    #[wasm_bindgen_test]
    fn the_hash_is_the_cores_and_undefined_before_a_load() {
        let loaded = CartridgeEmulator {
            core: FakeCore {
                hash: Some("ab12".to_string()),
            },
        };
        assert_eq!(loaded.get_cartridge_hash().as_deref(), Some("ab12"));
        let empty = CartridgeEmulator {
            core: FakeCore { hash: None },
        };
        assert_eq!(empty.get_cartridge_hash(), None);
    }
}
