//! The 68000 instruction groups run through the CPU and the bus, the
//! debugger, and the CPU and VDP running together.

mod breakpoint_tests;
mod common;
mod cpu_vdp_sync_integration;
mod debugger_integration;
mod integration_arithmetic;
mod integration_bcd;
mod integration_bit_ops;
mod integration_branches;
mod integration_comparisons;
mod integration_data_movement;
mod integration_decoder_tests;
mod integration_logical;
mod integration_mult_div;
mod integration_shifts;
mod integration_system;
mod mini_rom_tests;
