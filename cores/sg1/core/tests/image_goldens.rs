//! The Mega Drive core's golden images (`retro_core::capture::image_goldens`):
//! the programs the capture conformance suite runs, as their image sets
//! hold them (`program.bin`, which `capture_conformance.rs` holds to the
//! program it builds), each run from power-on, each listed frame held at
//! its capture boundary, and the images a host reads of it held to the set
//! in `tests/golden/images/<set>/`: the picture, each layer alone, each
//! layer's whole map, each sprite the frame drew and those sprites
//! together, the tiles in each palette line in use, and the palette.
//!
//! `conformance` is the program as the suite runs it first; `interlace` is
//! the same program in interlace mode 2, whose picture is two rows a line.
//! In both, frame 4 shows the four sprites and frame 10 none (the program
//! points the sprite table at an empty one in one frame of four).
//! `RETRO_UPDATE_GOLDEN=1` writes the sets again.

retro_core::image_golden_tests!(sg1_core::GenesisEmulator;
    conformance => [4, 5, 10],
    interlace => [4, 10],
);
