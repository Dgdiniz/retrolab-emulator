//! [`machine_bindings!`], which writes the machine's methods every binding
//! crate exports the same way (its information, resets, frames, region,
//! savestates and battery save), and the functions it calls.
//!
//! [`machine_bindings!`]: crate::machine_bindings

use retro_core::types::{CpuArchitecture, CpuLocation};
use retro_core::EmulatorCore;
use serde::Serialize;
use wasm_bindgen::JsValue;

use crate::js::unsupported_chip_error;

/// The constructor's refusal of a system name its console does not answer
/// to: `"Unknown system: '{system}'. Supported: {aliases}"`, the aliases
/// joined by commas.
pub fn unknown_system(system: &str, aliases: &[&str]) -> JsValue {
    JsValue::from_str(&format!(
        "Unknown system: '{}'. Supported: {}",
        system,
        aliases.join(", ")
    ))
}

/// Load `data` into `core`. A refusal for a chip this build has no module
/// for is an `Error` named `UnsupportedChip` ([`unsupported_chip_error`]);
/// any other refusal is its reason as text. With `retry_detected_mapping`
/// and no manifest staged, a refused load is tried once more with the
/// manifest detection gives the file, staged for that load only: a ROM
/// whose header scores too low for detection to trust it still boots, the
/// way a command line's forced detection does, while a manifest the user
/// staged is never overridden and its refusal is reported as it is.
pub fn load_rom<E: EmulatorCore>(
    core: &mut E,
    data: &[u8],
    retry_detected_mapping: bool,
) -> Result<(), JsValue> {
    let Err(err) = core.load_rom(data) else {
        return Ok(());
    };
    // No retry for a missing chip: the detected mapping needs the same chip.
    if let Some(chip) = core.last_unsupported_chip() {
        return Err(unsupported_chip_error(chip, &err));
    }
    if !retry_detected_mapping || core.cartridge_manifest_override().is_some() {
        return Err(JsValue::from_str(&err));
    }
    let detected = core.detected_manifest(data);
    core.set_cartridge_manifest_override(Some(detected));
    let retry = core.load_rom(data);
    core.set_cartridge_manifest_override(None);
    retry.map_err(|e| JsValue::from_str(&e))
}

/// The per-part hashes of a state as a JSON object of hex strings,
/// `{"name":"hash",...}` in the order the parts come.
pub fn state_hash_parts_json(parts: &[(&str, u64)]) -> String {
    let mut json = String::from("{");
    for (i, (name, hash)) in parts.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!("\"{name}\":\"{hash:x}\""));
    }
    json.push('}');
    json
}

/// The core's id, the first part of its build id (`"sg1"`, `"snes1"`).
pub fn core_id<E: EmulatorCore>(_core: &E) -> &'static str {
    E::CORE_ID
}

/// One CPU of the machine as `getCpus` describes it: what the system
/// information says of it, and what the debugger does with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CpuDescription {
    /// Its id, the one every per-CPU method takes.
    pub id: usize,
    /// Its name.
    pub name: String,
    /// Its architecture.
    pub architecture: CpuArchitecture,
    /// Where it is: on the console's board or in the cartridge.
    pub location: CpuLocation,
    /// A host can step it an instruction at a time.
    pub steppable: bool,
    /// The emulator records its coverage as it runs.
    pub records_coverage: bool,
}

/// The machine's CPUs, in the system information's order, each with the
/// debugger's two answers.
pub fn cpus<E: EmulatorCore>(core: &E) -> Vec<CpuDescription> {
    let debugger = core.debugger_ref();
    core.system_info()
        .cpus
        .iter()
        .map(|cpu| CpuDescription {
            id: cpu.id,
            name: cpu.name.clone(),
            architecture: cpu.architecture,
            location: cpu.location,
            steppable: debugger.cpu_steppable(cpu.id),
            records_coverage: debugger.cpu_records_coverage(cpu.id),
        })
        .collect()
}

/// `false`, or the value given: what an optional `true`/`false` argument of
/// a binding macro reads as when it is left out.
#[doc(hidden)]
#[macro_export]
macro_rules! __flag_or_false {
    () => {
        false
    };
    ($value:literal) => {
        $value
    };
}

/// Writes the machine's methods on `$emulator`, over the `EmulatorCore` in
/// its field `$core`:
///
/// - `getSystemInfo()`, `getRomInfo()` (`undefined` before a load),
///   `getBuildId()` (`"{core}@{version}+{commit}"`), `getFrameNumber()`,
///   `getRegion()`, `getStateHash()` (a `bigint`) and
///   `getStateHashParts()`;
/// - the resets `reset()` (the console's reset button), `hardReset()` (the
///   app's Reset: a power cycle that keeps the save) and `powerOnReset()`
///   (netplay's cold start);
/// - `loadRom(data)` ([`load_rom`](crate::machine::load_rom)),
///   `setRegion(region)`;
/// - the frames `runFrame()`, `runUntilVBlank()` and `runFrameHeadless()`;
/// - `saveState()`, `loadState(data)` and `loadStateAnyRom(data)` (a state
///   of another ROM refused as an `Error` named `SaveStateOtherRom`);
/// - `getSaveRam()` and `setSaveRam(data)`, the battery save;
/// - `setShowOverscan(show)` and `isShowOverscanEnabled()`;
/// - `getCpus()`, the machine's CPUs ([`cpus`](crate::machine::cpus)).
///
/// `after_frame = m` names a method of `$emulator` run after each call that
/// can change the picture (`reset`, `hardReset`, `powerOnReset`,
/// `runFrame`, `runUntilVBlank`, `saveState`, and `loadRom`, `setRegion`,
/// `loadState` and `loadStateAnyRom` when they succeed), never after
/// `runFrameHeadless`, whose picture nobody sees.
/// `retry_detected_mapping = true` makes `loadRom` retry a refused load
/// with the detected mapping.
///
/// Each method's Rust name is its JavaScript name in snake case, which no
/// other method of the struct may have. The crate expanding the macro
/// depends on `wasm-bindgen` itself, as every crate declaring a
/// `#[wasm_bindgen]` struct does.
#[macro_export]
macro_rules! machine_bindings {
    (
        $emulator:ident, $core:ident
        $(, after_frame = $after_frame:ident)?
        $(, retry_detected_mapping = $retry:literal)?
        $(,)?
    ) => {
        #[$crate::__private::wasm_bindgen::prelude::wasm_bindgen]
        impl $emulator {
            /// The console's description: its name, CPUs, display, sound
            /// and input.
            #[wasm_bindgen(js_name = getSystemInfo)]
            pub fn get_system_info(&self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js($crate::__private::retro_core::EmulatorCore::system_info(
                    &self.$core,
                ))
            }

            /// What the loaded ROM's header says (`title`, `region`, `size`,
            /// `checksum`, `mapper`, `title_confidence`); `undefined`
            /// before a load.
            #[wasm_bindgen(js_name = getRomInfo)]
            pub fn get_rom_info(&self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js(&$crate::__private::retro_core::EmulatorCore::get_rom_info(
                    &self.$core,
                ))
            }

            /// The console's reset button: the CPUs start again from their
            /// reset vectors, and the memories the button does not clear
            /// keep what the game left there. The cartridge and its save
            /// are kept.
            pub fn reset(&mut self) {
                $crate::__private::retro_core::EmulatorCore::reset(&mut self.$core);
                $(self.$after_frame();)?
            }

            /// The app's Reset: the power cycle of `powerOnReset` that keeps
            /// the battery save, and the debugger's breakpoints and traces.
            /// `reset` is the console's reset button, which keeps the
            /// memories it does not clear.
            #[wasm_bindgen(js_name = hardReset)]
            pub fn hard_reset(&mut self) {
                $crate::__private::retro_core::EmulatorCore::hard_reset(&mut self.$core);
                $(self.$after_frame();)?
            }

            /// Netplay's power cycle, not the reset button: every chip as a
            /// fresh load of the cartridge leaves it, in the region the
            /// machine is in, the save a fresh image (a save resumes
            /// through the seeded savestate transfer). The host's settings
            /// and the debugger's mode are kept; every breakpoint,
            /// watchpoint and event breakpoint is removed, every trace
            /// switched off, and nothing the input holds stays pressed. Two
            /// peers that ran the game for different lengths reach the same
            /// frame-0 state hash.
            #[wasm_bindgen(js_name = powerOnReset)]
            pub fn power_on_reset(&mut self) {
                $crate::__private::retro_core::EmulatorCore::power_on_reset(&mut self.$core);
                $(self.$after_frame();)?
            }

            /// Load a ROM file. A refusal is its reason as text, or an
            /// `Error` named `UnsupportedChip` (with the chip's name in
            /// `chip`) for a cartridge whose chip this build has no module
            /// for. A manifest staged with `setCartridgeManifest` is applied
            /// in place of detection.
            #[wasm_bindgen(js_name = loadRom)]
            pub fn load_rom(
                &mut self,
                data: &[u8],
            ) -> ::core::result::Result<(), $crate::__private::wasm_bindgen::JsValue> {
                $crate::machine::load_rom(
                    &mut self.$core,
                    data,
                    $crate::__flag_or_false!($($retry)?),
                )?;
                $(self.$after_frame();)?
                ::core::result::Result::Ok(())
            }

            /// Run one frame on the tier the debugger's state selects: the
            /// play tier with nothing armed, the light debug tier with
            /// breakpoints or traces while running, and the paused or
            /// stepping tier otherwise.
            #[wasm_bindgen(js_name = runFrame)]
            pub fn run_frame(&mut self) {
                $crate::__private::retro_core::EmulatorCore::run_frame(&mut self.$core);
                $(self.$after_frame();)?
            }

            /// Run to the start of vertical blanking and stop there: the
            /// capture boundary, as `runToCaptureBoundary`.
            #[wasm_bindgen(js_name = runUntilVBlank)]
            pub fn run_until_vblank(&mut self) {
                $crate::__private::retro_core::EmulatorCore::run_until_vblank(&mut self.$core);
                $(self.$after_frame();)?
            }

            /// Run one frame headlessly, for netplay's rollback and catch-up:
            /// the whole deterministic machine, with the host's picture and
            /// the mix of its sound skipped (each chip's sound is still
            /// made, since it is state a netplay hash covers). Never for a
            /// frame the player has not seen and heard yet.
            #[wasm_bindgen(js_name = runFrameHeadless)]
            pub fn run_frame_headless(&mut self) {
                $crate::__private::retro_core::EmulatorCore::run_frame_headless(&mut self.$core);
            }

            /// The canonical hash of the whole deterministic machine state:
            /// two peers in the same state hash to the same value, so a
            /// mismatch is a desync. A JavaScript `bigint`.
            #[wasm_bindgen(js_name = getStateHash)]
            pub fn get_state_hash(&self) -> u64 {
                $crate::__private::retro_core::EmulatorCore::state_hash(&self.$core)
            }

            /// The hash of each part of the canonical savestate, as a JSON
            /// object of hex strings (`{"name":"hash",...}`), for desync
            /// triage: when two peers disagree on `getStateHash` this names
            /// the part whose state diverged. Diagnostic only.
            #[wasm_bindgen(js_name = getStateHashParts)]
            pub fn get_state_hash_parts(&self) -> ::std::string::String {
                $crate::machine::state_hash_parts_json(
                    &$crate::__private::retro_core::EmulatorCore::state_hash_parts(&self.$core),
                )
            }

            /// The core's frame counter (the serialized frame count), not a
            /// host's. A JavaScript `bigint`.
            #[wasm_bindgen(js_name = getFrameNumber)]
            pub fn get_frame_number(&self) -> u64 {
                $crate::__private::retro_core::EmulatorCore::frame_count(&self.$core)
            }

            /// Run in the region `"ntsc"` or `"pal"` (any case): a power cycle
            /// in it that keeps the save; the region the machine is in
            /// already changes nothing. Both netplay peers agree on the
            /// region in the session's handshake. An unknown name is refused.
            #[wasm_bindgen(js_name = setRegion)]
            pub fn set_region(
                &mut self,
                region: &str,
            ) -> ::core::result::Result<(), $crate::__private::wasm_bindgen::JsValue> {
                $crate::__private::retro_core::EmulatorCore::set_region_str(&mut self.$core, region)
                    .map_err(|e| $crate::__private::wasm_bindgen::JsValue::from_str(&e))?;
                $(self.$after_frame();)?
                ::core::result::Result::Ok(())
            }

            /// The region the machine runs in, `"ntsc"` or `"pal"`.
            #[wasm_bindgen(js_name = getRegion)]
            pub fn get_region(&self) -> ::std::string::String {
                ::std::string::ToString::to_string(
                    $crate::__private::retro_core::EmulatorCore::region_str(&self.$core),
                )
            }

            /// The core's build identity, which the netplay handshake
            /// compares: `"{core}@{version}+{commit}"`, the commit
            /// `"unknown"` outside a git checkout. Two peers refuse to
            /// connect on a mismatch, since another build can diverge
            /// deterministically.
            #[wasm_bindgen(js_name = getBuildId)]
            pub fn get_build_id(&self) -> ::std::string::String {
                $crate::build_id($crate::machine::core_id(&self.$core))
            }

            /// Save the machine's state. A frame held at the capture boundary
            /// is finished first, so a savestate never holds it; a frame
            /// stopped anywhere else is saved where it stands.
            #[wasm_bindgen(js_name = saveState)]
            pub fn save_state(&mut self) -> ::std::vec::Vec<u8> {
                let state = $crate::__private::retro_core::EmulatorCore::save_state(&mut self.$core);
                $(self.$after_frame();)?
                state
            }

            /// Load a savestate. A savestate made on another ROM is refused
            /// with an `Error` named `SaveStateOtherRom`; any other refusal
            /// is its reason as text.
            #[wasm_bindgen(js_name = loadState)]
            pub fn load_state(
                &mut self,
                data: &[u8],
            ) -> ::core::result::Result<(), $crate::__private::wasm_bindgen::JsValue> {
                $crate::__private::retro_core::EmulatorCore::load_state(
                    &mut self.$core,
                    data,
                    $crate::__private::retro_core::savestate::RomCheck::SameRom,
                )
                .map_err($crate::js::load_state_refusal)?;
                $(self.$after_frame();)?
                ::core::result::Result::Ok(())
            }

            /// Load a savestate whatever ROM it was made on: for a player
            /// who chose to carry a state to another build of the game.
            #[wasm_bindgen(js_name = loadStateAnyRom)]
            pub fn load_state_any_rom(
                &mut self,
                data: &[u8],
            ) -> ::core::result::Result<(), $crate::__private::wasm_bindgen::JsValue> {
                $crate::__private::retro_core::EmulatorCore::load_state(
                    &mut self.$core,
                    data,
                    $crate::__private::retro_core::savestate::RomCheck::AnyRom,
                )
                .map_err($crate::js::load_state_refusal)?;
                $(self.$after_frame();)?
                ::core::result::Result::Ok(())
            }

            /// The battery save's bytes; `undefined` for a cartridge with
            /// none.
            #[wasm_bindgen(js_name = getSaveRam)]
            pub fn get_save_ram(&self) -> ::core::option::Option<::std::vec::Vec<u8>> {
                $crate::__private::retro_core::EmulatorCore::get_save_ram(&self.$core)
                    .map(<[u8]>::to_vec)
            }

            /// Put a battery save into the cartridge.
            #[wasm_bindgen(js_name = setSaveRam)]
            pub fn set_save_ram(&mut self, data: &[u8]) {
                $crate::__private::retro_core::EmulatorCore::set_save_ram(&mut self.$core, data);
            }

            /// A host's display preference, never part of a savestate: show
            /// the lines a console draws past its picture's usual height
            /// (the SNES's overscan frames, 239 lines; `false`, the default,
            /// crops them to the 224-line window). It survives a reset and a
            /// savestate's load; a console that draws no such lines ignores
            /// it.
            #[wasm_bindgen(js_name = setShowOverscan)]
            pub fn set_show_overscan(&mut self, show: bool) {
                $crate::__private::retro_core::EmulatorCore::set_show_overscan(
                    &mut self.$core,
                    show,
                );
            }

            /// Whether those lines are shown; `false` on a console that draws
            /// none.
            #[wasm_bindgen(js_name = isShowOverscanEnabled)]
            pub fn is_show_overscan_enabled(&self) -> bool {
                $crate::__private::retro_core::EmulatorCore::show_overscan_enabled(&self.$core)
            }

            /// The machine's CPUs, `CpuDescription[]`: `{ id, name,
            /// architecture, location, steppable, records_coverage }` each,
            /// in the order and with the ids every per-CPU method takes.
            /// `steppable` says whether `stepCpu(id)` steps it,
            /// `records_coverage` whether its coverage fills as it runs. A
            /// cartridge with a CPU of its own adds it at its load.
            #[wasm_bindgen(js_name = getCpus)]
            pub fn get_cpus(&self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js(&$crate::machine::cpus(&self.$core))
            }
        }
    };
}
