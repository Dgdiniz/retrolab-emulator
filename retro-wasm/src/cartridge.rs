//! [`cartridge_bindings!`], which writes the cartridge methods every binding
//! crate exports the same way (its hash, what it declares, its battery
//! save's flag, its mapper and chips, its map, and the manifest detection
//! gives or a host stages), and the values they hand a host.
//!
//! [`cartridge_bindings!`]: crate::cartridge_bindings

use std::collections::HashMap;

use retro_core::cartridge::{ExpansionChipState, MapRegionSummary, MapperId, MapperState};
use retro_core::EmulatorCore;
use serde::Serialize;
use wasm_bindgen::JsValue;

use crate::js::to_js;

/// Read and check a manifest a host stages, for the core `core` is; the
/// refusal is the text that says what is wrong with it.
pub fn parse_manifest_for<E: EmulatorCore>(_core: &E, json: &str) -> Result<E::Manifest, JsValue> {
    E::parse_manifest(json).map_err(|e| JsValue::from_str(&e))
}

/// A manifest as the JSON text a host edits.
pub fn manifest_text<M: Serialize>(manifest: &M) -> Result<String, JsValue> {
    serde_json::to_string(manifest).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// The label a mapper viewer shows for a mapper: a named mapper's name, any
/// other id as written.
pub fn default_mapper_label(id: &MapperId) -> String {
    match id {
        MapperId::Custom(name) => name.clone(),
        other => format!("{other:?}"),
    }
}

/// `{ mapper_type, registers, banks }`, the flat object a mapper viewer
/// reads (retro-core keeps the banks under `bank_config` and names the
/// mapper by an enum): `mapper_type` is `label`'s, and a cartridge with no
/// mapper state reads `"None"` with no register and no bank, which a viewer
/// takes to hide its tables.
pub fn mapper_state_value(
    state: Option<MapperState>,
    label: fn(&MapperId) -> String,
) -> serde_json::Value {
    match state {
        Some(m) => serde_json::json!({
            "mapper_type": label(&m.mapper_id),
            "registers": m.registers,
            "banks": m.bank_config.banks,
        }),
        None => serde_json::json!({
            "mapper_type": "None",
            "registers": [],
            "banks": [],
        }),
    }
}

/// `[{ chipId, label, state }]`, one row a chip in `states`' order, its
/// label from `labels` (empty when it has none).
pub fn chip_state_rows(
    states: Vec<(u8, ExpansionChipState)>,
    labels: Vec<(u8, String)>,
) -> JsValue {
    let labels: HashMap<u8, String> = labels.into_iter().collect();
    let rows: Vec<serde_json::Value> = states
        .into_iter()
        .map(|(chip_id, state)| {
            serde_json::json!({
                "chipId": chip_id,
                "label": labels.get(&chip_id).cloned().unwrap_or_default(),
                "state": state,
            })
        })
        .collect();
    to_js(&rows)
}

/// `{ regions: [{ start, end, label, detail }] }`, the cartridge's address
/// space in the order addresses resolve through it.
pub fn memory_map_object(regions: Vec<MapRegionSummary>) -> JsValue {
    to_js(&serde_json::json!({ "regions": regions }))
}

/// The mapper label a binding crate names, or [`default_mapper_label`].
#[doc(hidden)]
#[macro_export]
macro_rules! __mapper_label {
    () => {
        $crate::cartridge::default_mapper_label
    };
    ($label:path) => {
        $label
    };
}

/// Writes the cartridge methods on `$emulator`, over the `EmulatorCore` in
/// its field `$core` and its `cartridge_debugger()`:
///
/// - `getCartridgeHash()`: the SHA-256 of the ROM file the last successful
///   `loadRom` received, in lowercase hex, `undefined` before one;
/// - `getCartridgeInfo()`, `getCartridgeTitle()`, `isCartridgeSramDirty()`
///   and `clearCartridgeSramDirty()`;
/// - `getMapperState()`, `getChipState()`, `getChipStates()` and
///   `getMemoryMap()`;
/// - `detectManifest(data)`, `getDetectedManifest()`,
///   `setCartridgeManifest(json)` and `clearCartridgeManifest()`.
///
/// `mapper_label = f` names the function that labels a mapper for the
/// mapper viewer (`fn(&MapperId) -> String`), [`default_mapper_label`] when
/// left out.
///
/// Each method's Rust name is its JavaScript name in snake case, which no
/// other method of the struct may have. The crate expanding the macro
/// depends on `wasm-bindgen` itself, as every crate declaring a
/// `#[wasm_bindgen]` struct does.
#[macro_export]
macro_rules! cartridge_bindings {
    ($emulator:ident, $core:ident $(, mapper_label = $label:path)? $(,)?) => {
        #[$crate::__private::wasm_bindgen::prelude::wasm_bindgen]
        impl $emulator {
            /// The SHA-256 of the ROM file the last successful `loadRom`
            /// received, in lowercase hex; `undefined` before one.
            #[wasm_bindgen(js_name = getCartridgeHash)]
            pub fn get_cartridge_hash(&self) -> ::core::option::Option<::std::string::String> {
                $crate::__private::retro_core::EmulatorCore::get_cartridge_hash(&self.$core)
            }

            /// What the loaded ROM's header says, as `getRomInfo`; refused
            /// with `"No cartridge loaded"` before a load.
            #[wasm_bindgen(js_name = getCartridgeInfo)]
            pub fn get_cartridge_info(
                &self,
            ) -> ::core::result::Result<
                $crate::__private::wasm_bindgen::JsValue,
                $crate::__private::wasm_bindgen::JsValue,
            > {
                match $crate::__private::retro_core::EmulatorCore::get_rom_info(&self.$core) {
                    ::core::option::Option::Some(info) => {
                        ::core::result::Result::Ok($crate::js::to_js(&info))
                    }
                    ::core::option::Option::None => ::core::result::Result::Err(
                        $crate::__private::wasm_bindgen::JsValue::from_str("No cartridge loaded"),
                    ),
                }
            }

            /// The cartridge's title; `undefined` with no cartridge.
            #[wasm_bindgen(js_name = getCartridgeTitle)]
            pub fn get_cartridge_title(&self) -> ::core::option::Option<::std::string::String> {
                $crate::__private::retro_core::EmulatorCore::cartridge_info(&self.$core)
                    .map(|info| info.title.clone())
            }

            /// Whether the game wrote its battery save since the flag was
            /// last cleared, so the host knows to keep it.
            #[wasm_bindgen(js_name = isCartridgeSramDirty)]
            pub fn is_cartridge_sram_dirty(&self) -> bool {
                $crate::__private::retro_core::EmulatorCore::is_cartridge_sram_dirty(&self.$core)
            }

            /// Clear the battery save's flag, once the host has kept it.
            #[wasm_bindgen(js_name = clearCartridgeSramDirty)]
            pub fn clear_cartridge_sram_dirty(&mut self) {
                $crate::__private::retro_core::EmulatorCore::clear_cartridge_sram_dirty(
                    &mut self.$core,
                );
            }

            /// The cartridge's mapper as `{ mapper_type, registers, banks }`;
            /// a cartridge with no mapper reads `mapper_type: "None"`.
            #[wasm_bindgen(js_name = getMapperState)]
            pub fn get_mapper_state(
                &mut self,
            ) -> ::core::result::Result<
                $crate::__private::wasm_bindgen::JsValue,
                $crate::__private::wasm_bindgen::JsValue,
            > {
                let state =
                    $crate::__private::retro_core::EmulatorCore::cartridge_debugger(&mut self.$core)
                        .get_mapper_state();
                ::core::result::Result::Ok($crate::js::to_js(
                    &$crate::cartridge::mapper_state_value(state, $crate::__mapper_label!($($label)?)),
                ))
            }

            /// The first expansion chip's state (`chip_type`, `registers`,
            /// `internal_ram`, `program_counter`, `cycle_count`), the
            /// lowest chip id's; `null` on a cartridge with no chip.
            #[wasm_bindgen(js_name = getChipState)]
            pub fn get_chip_state(
                &mut self,
            ) -> ::core::result::Result<
                $crate::__private::wasm_bindgen::JsValue,
                $crate::__private::wasm_bindgen::JsValue,
            > {
                match $crate::__private::retro_core::EmulatorCore::cartridge_debugger(&mut self.$core)
                    .get_chip_state()
                {
                    ::core::option::Option::Some(state) => {
                        ::core::result::Result::Ok($crate::js::to_js(&state))
                    }
                    ::core::option::Option::None => {
                        ::core::result::Result::Ok($crate::__private::wasm_bindgen::JsValue::NULL)
                    }
                }
            }

            /// Every expansion chip's state, keyed by its id, as
            /// `[{ chipId, label, state }]`; empty on a cartridge with no
            /// chip.
            #[wasm_bindgen(js_name = getChipStates)]
            pub fn get_chip_states(
                &mut self,
            ) -> ::core::result::Result<
                $crate::__private::wasm_bindgen::JsValue,
                $crate::__private::wasm_bindgen::JsValue,
            > {
                let debugger =
                    $crate::__private::retro_core::EmulatorCore::cartridge_debugger(&mut self.$core);
                ::core::result::Result::Ok($crate::cartridge::chip_state_rows(
                    debugger.get_chip_states(),
                    debugger.get_chip_labels(),
                ))
            }

            /// The cartridge's address space as `{ regions: [{ start, end,
            /// label, detail }] }`, in the order addresses resolve through
            /// it; no region with no cartridge.
            #[wasm_bindgen(js_name = getMemoryMap)]
            pub fn get_memory_map(
                &mut self,
            ) -> ::core::result::Result<
                $crate::__private::wasm_bindgen::JsValue,
                $crate::__private::wasm_bindgen::JsValue,
            > {
                ::core::result::Result::Ok($crate::cartridge::memory_map_object(
                    $crate::__private::retro_core::EmulatorCore::cartridge_debugger(&mut self.$core)
                        .map_regions(),
                ))
            }

            /// The `cartridge-manifest-v1` detection resolves for the ROM
            /// `data`, as JSON text, for a manifest editor to start from;
            /// nothing is loaded and no staged manifest is applied.
            #[wasm_bindgen(js_name = detectManifest)]
            pub fn detect_manifest(
                &self,
                data: &[u8],
            ) -> ::core::result::Result<::std::string::String, $crate::__private::wasm_bindgen::JsValue>
            {
                $crate::cartridge::manifest_text(
                    &$crate::__private::retro_core::EmulatorCore::detected_manifest(&self.$core, data),
                )
            }

            /// The manifest detection resolves for the ROM loaded now, as JSON
            /// text; `"null"` before a load.
            #[wasm_bindgen(js_name = getDetectedManifest)]
            pub fn get_detected_manifest(
                &self,
            ) -> ::core::result::Result<::std::string::String, $crate::__private::wasm_bindgen::JsValue>
            {
                let rom = $crate::__private::retro_core::EmulatorCore::last_rom(&self.$core);
                if rom.is_empty() {
                    return ::core::result::Result::Ok(::std::string::String::from("null"));
                }
                $crate::cartridge::manifest_text(
                    &$crate::__private::retro_core::EmulatorCore::detected_manifest(&self.$core, rom),
                )
            }

            /// Stage a manifest (JSON text) for the next `loadRom`, in place
            /// of detection; the cartridge loaded now is left as it is. A
            /// manifest that does not read is refused with the reason.
            #[wasm_bindgen(js_name = setCartridgeManifest)]
            pub fn set_cartridge_manifest(
                &mut self,
                json: &str,
            ) -> ::core::result::Result<(), $crate::__private::wasm_bindgen::JsValue> {
                let manifest = $crate::cartridge::parse_manifest_for(&self.$core, json)?;
                $crate::__private::retro_core::EmulatorCore::set_cartridge_manifest_override(
                    &mut self.$core,
                    ::core::option::Option::Some(manifest),
                );
                ::core::result::Result::Ok(())
            }

            /// Drop a staged manifest: the next `loadRom` detects the mapping
            /// again.
            #[wasm_bindgen(js_name = clearCartridgeManifest)]
            pub fn clear_cartridge_manifest(&mut self) {
                $crate::__private::retro_core::EmulatorCore::set_cartridge_manifest_override(
                    &mut self.$core,
                    ::core::option::Option::None,
                );
            }
        }
    };
}
