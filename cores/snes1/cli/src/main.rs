//! Command-line entry point for the SNES1 emulator: the commands both
//! tools share come from `retro-cli`, over what the SNES gives them
//! (`console.rs`: its CPUs, the parsers of their bsnes-plus-format lines in
//! `trace_ref.rs`, its start-up flags); the SNES's own commands are under
//! `cmd/`.

mod cmd;
mod console;
mod trace_ref;

fn main() -> anyhow::Result<()> {
    retro_cli::run::<console::Snes>()
}
