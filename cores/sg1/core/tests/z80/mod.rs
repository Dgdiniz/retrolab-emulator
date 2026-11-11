//! Z80 integration tests module

mod common;
mod integration_arithmetic;
mod integration_arithmetic16; // 16-bit arithmetic
mod integration_bit_ops; // Bit Operations
mod integration_exceptions; // Exception handling: NMI, IM 0/1/2, RETI/RETN
mod integration_extended; // Extended ED (LDI/LDIR, CPI/CPIR, LD extended, IM, RETI/RETN)
mod integration_index; // Index operations (IX/IY)
mod integration_io; // I/O operations (IN/OUT)
mod integration_jumps;
mod integration_jumps_extended; // Extended jumps (PO/PE/P/M, RST)
mod integration_load;
mod integration_logical;
mod integration_misc;
mod integration_shifts; // Rotate & Shift
mod integration_stack;
mod mini_rom_tests;
