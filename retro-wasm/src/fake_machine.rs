//! What the binding tests' fake cores share: a payload, and the machine
//! items no binding under test reaches.

use retro_core::savestate::{SavestateFormat, SavestatePayload};
use serde::{Deserialize, Serialize};

/// The payload of a fake core, which no test seals.
#[derive(Serialize, Deserialize)]
pub(crate) struct FakeSavestate;

impl SavestatePayload for FakeSavestate {
    retro_core::savestate_codec!();

    const FORMAT: SavestateFormat = SavestateFormat {
        core_id: "fake",
        version: 1,
    };
    type Extension = ();
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
    fn hash_parts(&self) -> Vec<(&'static str, u64)> {
        Vec::new()
    }
}

/// The `EmulatorCore` items of the machine (its savestate, resets, clocks,
/// region, audio ring, input and views) that a fake core never reaches.
macro_rules! unreachable_machine_items {
    () => {
        type Savestate = crate::fake_machine::FakeSavestate;
        fn rom_identity(&self) -> Option<retro_core::savestate::RomIdentity<'_>> {
            unreachable!()
        }
        fn savestate_payload(
            &self,
            _purpose: retro_core::savestate::PayloadUse,
        ) -> Self::Savestate {
            unreachable!()
        }
        fn savestate_extension(&self) {}
        fn run_frame_headless(&mut self) {
            unreachable!()
        }
        fn restore_savestate(&mut self, _payload: Self::Savestate, _extension: ()) {
            unreachable!()
        }
        fn power_cycle(&mut self) {
            unreachable!()
        }
        fn frame_count(&self) -> u64 {
            unreachable!()
        }
        fn master_cycles(&self) -> u64 {
            unreachable!()
        }
        fn master_clock_hz(&self) -> u64 {
            unreachable!()
        }
        fn region(&self) -> retro_core::types::VideoRegion {
            unreachable!()
        }
        fn apply_region(&mut self, _region: retro_core::types::VideoRegion) {
            unreachable!()
        }
        fn host_audio(&self) -> &retro_core::audio::HostAudioRing {
            unreachable!()
        }
        fn host_audio_mut(&mut self) -> &mut retro_core::audio::HostAudioRing {
            unreachable!()
        }
        fn input_manager(&self) -> &retro_core::input::GenericInputManager {
            unreachable!()
        }
        fn input_manager_mut(&mut self) -> &mut retro_core::input::GenericInputManager {
            unreachable!()
        }
        fn video_debugger(&mut self) -> &mut dyn retro_core::video::VideoDebugger {
            unreachable!()
        }
        fn audio_chip_count(&self) -> usize {
            unreachable!()
        }
        fn audio_debugger(
            &mut self,
            _chip: usize,
        ) -> Option<&mut dyn retro_core::audio::AudioDebugger> {
            unreachable!()
        }
        fn cartridge_debugger(&mut self) -> Box<dyn retro_core::cartridge::CartridgeDebugger + '_> {
            unreachable!()
        }
        fn write_memory_debug(&mut self, _addr: u32, _data: &[u8]) {
            unreachable!()
        }
        fn disassemble_window(
            &mut self,
            _cpu_id: usize,
            _pc: u32,
            _before_count: usize,
            _after_count: usize,
        ) -> Vec<retro_core::types::DisasmRow> {
            unreachable!()
        }
    };
}
pub(crate) use unreachable_machine_items;
