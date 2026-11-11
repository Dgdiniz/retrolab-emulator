//! Swapping cartridges is a power cycle, not a reset-button press.
//!
//! `EmulatorCore::reset` faithfully emulates the Genesis reset *button*,
//! which leaves work RAM, Z80 RAM, the VDP register file and the audio
//! chips holding whatever the running game left behind. That is correct
//! for a reset, and wrong for a cartridge swap: the host loads a second
//! ROM into the same live emulator, so without an explicit power cycle
//! the incoming game boots on top of the outgoing one's state. A title
//! that does not rewrite every VDP register (plane / sprite-table base
//! addresses in particular) then comes up with corrupt graphics, but
//! only when another game ran first, which is exactly what makes the
//! symptom vanish on a page reload.
//!
//! The invariant both tests pin: after `load_rom(b)` on an emulator that
//! has been running `a`, the machine must be byte-identical to a
//! freshly-constructed emulator that only ever loaded `b`.

mod helpers;

use helpers::{M68kRomBuilder, MiniCartridgeBuilder};
use retro_core::EmulatorCore;
use sg1_core::GenesisEmulator;

/// A tiny valid Genesis ROM whose body is distinguishable per `tag`.
fn mini_rom(tag: u16) -> Vec<u8> {
    let mut b = M68kRomBuilder::new(0x400);
    // Enough to make the two ROM images differ, and harmless to execute.
    b.move_w_imm_to_d0(tag);
    b.build()
}

/// Run the machine and scribble over the volatile state a reset preserves,
/// mimicking a game that has been playing for a while.
fn dirty(emu: &mut GenesisEmulator) {
    for _ in 0..8 {
        emu.run_frame_fast();
    }
    // Work RAM (0xFF0000-0xFFFFFF), never cleared by `reset()`.
    emu.write_memory_debug(0xFF0000, &[0xA5; 256]);
    // VDP registers, through the real control port: plane A / plane B /
    // sprite-attribute-table bases, the three that decide where the
    // renderer reads its tilemaps and sprites from.
    for (reg, value) in [(2u16, 0x30u16), (4, 0x07), (5, 0x7C)] {
        emu.vdp_mut()
            .write_word_port(0x00C00004, 0x8000 | (reg << 8) | value);
    }
    for _ in 0..8 {
        emu.run_frame_fast();
    }
}

#[test]
fn loading_a_different_rom_power_cycles_the_console() {
    let a = mini_rom(0x1111);
    let b = mini_rom(0x2222);

    let mut fresh = GenesisEmulator::new();
    fresh.load_rom(&b).expect("load b");

    let mut reused = GenesisEmulator::new();
    reused.load_rom(&a).expect("load a");
    dirty(&mut reused);
    reused.load_rom(&b).expect("swap to b");

    assert_eq!(
        reused.state_hash(),
        fresh.state_hash(),
        "a cartridge swap must leave the machine in the same state as a cold boot; \
         diverging subsystems: {:?}",
        diverging_parts(&reused, &fresh),
    );

    // And it stays converged once both run.
    for _ in 0..30 {
        reused.run_frame_fast();
        fresh.run_frame_fast();
    }
    assert_eq!(reused.state_hash(), fresh.state_hash());
    assert_eq!(
        reused.get_framebuffer().data,
        fresh.get_framebuffer().data,
        "the first frames after a swap must render the incoming game, not the outgoing one",
    );
}

#[test]
fn reloading_the_same_rom_is_still_a_plain_reset() {
    // The hot-reload path (a Bass rebuild feeding identical bytes back in)
    // deliberately keeps the debugger's coverage bitmap and does not need a
    // power cycle; only a *different* cartridge triggers one.
    let a = mini_rom(0x1111);

    let mut emu = GenesisEmulator::new();
    emu.load_rom(&a).expect("load a");
    emu.write_memory_debug(0xFF0000, &[0xA5; 256]);
    emu.load_rom(&a).expect("reload a");

    assert_eq!(
        emu.read_memory_debug(0xFF0000, 4),
        vec![0xA5; 4],
        "reloading the same ROM must behave like the reset button and keep work RAM",
    );
}

#[test]
fn hard_reset_clears_volatile_state_but_keeps_the_cartridge() {
    let a = mini_rom(0x1111);

    let mut fresh = GenesisEmulator::new();
    fresh.load_rom(&a).expect("load a");

    let mut cycled = GenesisEmulator::new();
    cycled.load_rom(&a).expect("load a");
    dirty(&mut cycled);
    cycled.hard_reset();

    assert_eq!(
        cycled.state_hash(),
        fresh.state_hash(),
        "hard_reset must return the console to its power-on state; \
         diverging subsystems: {:?}",
        diverging_parts(&cycled, &fresh),
    );
    assert!(
        cycled.get_rom_info().is_some(),
        "the cartridge must stay plugged in across a power cycle",
    );
}

/// The three resets differ in exactly one user-visible way: what happens to the
/// battery save. `power_on_reset` blanks it (netplay needs both peers
/// byte-identical regardless of either one's save file), which is why the UI's
/// Reset control routes through `hard_reset` instead: the platform saves
/// whatever SRAM the core reports, so a blank image would overwrite the player's save.
#[test]
fn only_power_on_reset_blanks_the_battery_save() {
    fn save_after(cycle: impl Fn(&mut GenesisEmulator)) -> Option<Vec<u8>> {
        let rom = MiniCartridgeBuilder::new()
            .with_title("SRAM TEST")
            .with_sram(0x2000)
            .build();
        let mut emu = GenesisEmulator::new();
        emu.load_rom(&rom).expect("load");
        for _ in 0..4 {
            emu.run_frame_fast();
        }

        // Written exactly as the platform restores a stored save.
        let mut save = vec![0u8; 0x2000];
        save[..4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        emu.set_save_ram(&save);
        assert_eq!(
            emu.get_save_ram().map(|s| s[..4].to_vec()),
            Some(vec![0xDE, 0xAD, 0xBE, 0xEF]),
            "precondition: the save must read back before the reset",
        );

        cycle(&mut emu);
        emu.get_save_ram().map(|s| s[..4].to_vec())
    }

    let saved = Some(vec![0xDE, 0xAD, 0xBE, 0xEF]);
    assert_eq!(
        save_after(|e| e.reset()),
        saved,
        "the reset button keeps the save"
    );
    assert_eq!(
        save_after(|e| e.hard_reset()),
        saved,
        "hard_reset is the UI's Reset: it MUST keep the save",
    );
    assert_ne!(
        save_after(|e| e.power_on_reset()),
        saved,
        "power_on_reset is netplay's cold boot and blanks the save by design; if this \
         ever starts preserving it, the doc comments claiming otherwise need revisiting",
    );
}

fn diverging_parts(a: &GenesisEmulator, b: &GenesisEmulator) -> Vec<&'static str> {
    a.state_hash_parts()
        .into_iter()
        .zip(b.state_hash_parts())
        .filter(|((_, ha), (_, hb))| ha != hb)
        .map(|((name, _), _)| name)
        .collect()
}
