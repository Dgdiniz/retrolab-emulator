//! The debugger's values as a host reads them, and [`debugger_bindings!`],
//! which writes the debugger methods every binding crate exports the same
//! way over `retro_core`'s `EmulatorCore` and `DebuggerCore`.
//!
//! [`debugger_bindings!`]: crate::debugger_bindings

use retro_core::types::{BreakpointType, ExecutionMode, MatchSpec};
use serde::Serialize;
use wasm_bindgen::JsValue;

use crate::js::{map_as_object, plain_object};

/// The execution mode a host names: `"Running"`, `"Paused"`, `"StepInto"`,
/// `"StepOver"` or `"StepOut"`; any other name is `Running`.
pub fn execution_mode_from_name(name: &str) -> ExecutionMode {
    match name {
        "Paused" => ExecutionMode::Paused,
        "StepInto" => ExecutionMode::StepInto,
        "StepOver" => ExecutionMode::StepOver,
        "StepOut" => ExecutionMode::StepOut,
        _ => ExecutionMode::Running,
    }
}

/// The name a host reads for an execution mode, the one
/// [`execution_mode_from_name`] takes.
pub fn execution_mode_name(mode: ExecutionMode) -> String {
    format!("{mode:?}")
}

/// The breakpoint type a host names: `"read"`, `"write"`, `"readwrite"`;
/// any other name, `"execute"` among them, is an execute breakpoint.
pub fn breakpoint_type_from_name(name: &str) -> BreakpointType {
    match name {
        "read" => BreakpointType::read(),
        "write" => BreakpointType::write(),
        "readwrite" => BreakpointType::read_write(),
        _ => BreakpointType::execute(),
    }
}

/// `{ coveredBytes, percent, totalBytes }`, a plain object: the addresses a
/// CPU ran instructions from, the share of the addresses its coverage spans
/// in percent (a single-precision figure), and how many it spans.
pub fn coverage_stats_object(total: u32, covered: u32) -> JsValue {
    let percent = if total == 0 {
        0.0_f32
    } else {
        (covered as f32 / total as f32) * 100.0
    };
    plain_object(&[
        ("coveredBytes", JsValue::from_f64(f64::from(covered))),
        ("percent", JsValue::from_f64(f64::from(percent))),
        ("totalBytes", JsValue::from_f64(f64::from(total))),
    ])
}

/// A value of the debugger as a host reads it: structs as plain objects,
/// lists as arrays, an absent value as `undefined`.
pub fn debugger_value<T: Serialize + ?Sized>(value: &T) -> JsValue {
    map_as_object(value)
}

/// The match conditions a host sends: a plain object whose keys are
/// `MatchSpec`'s fields, each optional. A value it cannot read is an error
/// naming why.
pub fn match_spec_from_js(spec: JsValue) -> Result<MatchSpec, JsValue> {
    serde_wasm_bindgen::from_value(spec).map_err(|error| JsValue::from_str(&error.to_string()))
}

/// Writes the debugger methods every binding crate exports the same way, as
/// a further `#[wasm_bindgen] impl` block on its emulator struct: each
/// method calls `retro_core`'s `EmulatorCore` or `DebuggerCore` on one field
/// of the struct and turns what it returns into the JavaScript value the
/// helpers of [`debugger`](crate::debugger) make.
///
/// ```text
/// retro_wasm::debugger_bindings!(
///     Emulator,                 // a #[wasm_bindgen] struct
///     core,                     // its field implementing EmulatorCore
///     after_step = refresh,     // optional: a method of Emulator
/// );
/// ```
///
/// `after_step` is a method of the struct taking `&mut self`, run after each
/// step (`step`, `stepCpu`), for a crate that keeps a copy of the picture.
///
/// The methods, by their JavaScript names: `getCpuStates`, `addBreakpoint`,
/// `removeBreakpoint`, `enableBreakpoint`, `setBreakpointAccess`,
/// `setBreakpointMatchSpec`, `getAvailableEvents`, `addEventBreakpoint`,
/// `listBreakpoints`, `getExecutionMode`, `setExecutionMode`,
/// `getMemoryRegions`, `getInstructionHistory`, `getCallStack`,
/// `getFocusedCpu`, `setFocusedCpu`, `step`, `stepCpu`, `isCovered`,
/// `getCoverageStats`, `clearCoverage`, `getBreakpointChecks`, and the main
/// CPU's trace (CPU 0): `setTracingEnabled`, `isTracingEnabled`,
/// `needsTraceFlush` (any CPU's ring), `getTraceFlushData`,
/// `clearTraceFlushed`, `getTraceEntrySize`, `convertTraceChunkToText` and
/// `getTraceStats`. Their Rust names are the JavaScript names in snake case
/// (`get_cpu_states`, and `step` and `step_cpu`), which no other method of
/// the struct may have. The crate expanding the macro depends on
/// `wasm-bindgen` itself, as every crate declaring a `#[wasm_bindgen]`
/// struct does.
#[macro_export]
macro_rules! debugger_bindings {
    (
        $emulator:ident, $core:ident
        $(, after_step = $after_step:ident)?
        $(,)?
    ) => {
        #[$crate::__private::wasm_bindgen::prelude::wasm_bindgen]
        impl $emulator {
            /// The CPU states the debugger last wrote, one a CPU: `[{ cpu_id,
            /// pc, flags, registers: [{ name, value }] }]`. They are written
            /// at a pause (`setExecutionMode("Paused")`, a breakpoint's
            /// stop), at each step and at each frame of the full debugger
            /// loop; a frame run with nothing armed leaves them as they were.
            #[wasm_bindgen(js_name = getCpuStates)]
            pub fn get_cpu_states(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::debugger::debugger_value(
                    &$crate::__private::retro_core::DebuggerCore::get_cpu_states(
                        $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                    ),
                )
            }

            /// Add an address breakpoint and return its id. `bp_type` is
            /// `"execute"`, `"read"`, `"write"` or `"readwrite"` (another
            /// name is `"execute"`); `cpu_id` names the CPU it watches, the
            /// main CPU (0) when left out. `getBreakpointChecks` says what a
            /// CPU checks.
            #[wasm_bindgen(js_name = addBreakpoint)]
            pub fn add_breakpoint(
                &mut self,
                address: u32,
                bp_type: ::std::string::String,
                cpu_id: ::core::option::Option<u32>,
            ) -> u32 {
                $crate::__private::retro_core::DebuggerCore::add_breakpoint(
                    $crate::__private::retro_core::EmulatorCore::debugger(&mut self.$core),
                    $crate::__private::retro_core::types::Breakpoint {
                        address,
                        bp_type: $crate::debugger::breakpoint_type_from_name(&bp_type),
                        cpu_id: ::core::option::Option::Some(cpu_id.unwrap_or(0) as usize),
                        condition: ::core::option::Option::None,
                    },
                )
            }

            /// Remove a breakpoint; an unknown id is ignored.
            #[wasm_bindgen(js_name = removeBreakpoint)]
            pub fn remove_breakpoint(&mut self, id: u32) {
                $crate::__private::retro_core::DebuggerCore::remove_breakpoint(
                    $crate::__private::retro_core::EmulatorCore::debugger(&mut self.$core),
                    id,
                );
            }

            /// Enable or disable a breakpoint; an unknown id is ignored.
            #[wasm_bindgen(js_name = enableBreakpoint)]
            pub fn enable_breakpoint(&mut self, id: u32, enabled: bool) {
                $crate::__private::retro_core::DebuggerCore::enable_breakpoint(
                    $crate::__private::retro_core::EmulatorCore::debugger(&mut self.$core),
                    id,
                    enabled,
                );
            }

            /// Replace what an address breakpoint watches (execute, read,
            /// write) in place, keeping its id, its enabled flag and its hit
            /// count: the R/W/X boxes of a breakpoint's row. An event
            /// breakpoint, or an unknown id, is left alone.
            #[wasm_bindgen(js_name = setBreakpointAccess)]
            pub fn set_breakpoint_access(&mut self, id: u32, exec: bool, read: bool, write: bool) {
                $crate::__private::retro_core::DebuggerCore::set_breakpoint_access(
                    $crate::__private::retro_core::EmulatorCore::debugger(&mut self.$core),
                    id,
                    $crate::__private::retro_core::types::AccessFlags::new(exec, read, write),
                );
            }

            /// Replace a breakpoint's match conditions in place, keeping its
            /// id, type, enabled flag and hit count. `spec` is a plain
            /// object whose keys are optional: `address_end` (the range's
            /// last address), `value`, `value_size` (1-4 bytes, read in the
            /// CPU's byte order), `value_mask`, `value_compare` (`"eq"`,
            /// `"ne"`, `"lt"`, `"le"`, `"gt"`, `"ge"`), `expr` (a condition
            /// over the CPU's registers and memory, such as `"d0 == 5 &&
            /// [0xFF0000].w > 0"`), `hit_skip`, `action` (`"break"` or
            /// `"log"`) and `one_shot`. An unknown id is ignored; an object
            /// that cannot be read is an error.
            #[wasm_bindgen(js_name = setBreakpointMatchSpec)]
            pub fn set_breakpoint_match_spec(
                &mut self,
                id: u32,
                spec: $crate::__private::wasm_bindgen::JsValue,
            ) -> ::core::result::Result<(), $crate::__private::wasm_bindgen::JsValue> {
                let spec = $crate::debugger::match_spec_from_js(spec)?;
                $crate::__private::retro_core::DebuggerCore::set_breakpoint_match_spec(
                    $crate::__private::retro_core::EmulatorCore::debugger(&mut self.$core),
                    id,
                    spec,
                );
                ::core::result::Result::Ok(())
            }

            /// The events this console's breakpoints can stop on, for the
            /// event picker: `[{ code, kind, label, description, param_label,
            /// param_options: [{ value, label }] }]`. `code` is what
            /// `addEventBreakpoint` takes, the console's own number; `kind`
            /// says what the event is in words every console shares
            /// (`"vertical_blank"`, `"palette_write"`, `"tile_write"`,
            /// `"transfer_done"`, ...), so a host finds an event by its kind.
            /// `param_label` names what the param selects (`undefined` for an
            /// event with none) and `param_options` lists its values when
            /// they are few.
            #[wasm_bindgen(js_name = getAvailableEvents)]
            pub fn get_available_events(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::debugger::debugger_value(
                    &$crate::__private::retro_core::DebuggerCore::available_events(
                        $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                    ),
                )
            }

            /// Add a breakpoint on an event of `getAvailableEvents` and
            /// return its id. `param` selects one value of the event's
            /// param; `0xFFFFFFFF` matches any.
            #[wasm_bindgen(js_name = addEventBreakpoint)]
            pub fn add_event_breakpoint(&mut self, code: u32, param: u32) -> u32 {
                $crate::__private::retro_core::DebuggerCore::add_breakpoint(
                    $crate::__private::retro_core::EmulatorCore::debugger(&mut self.$core),
                    $crate::__private::retro_core::types::Breakpoint {
                        address: 0,
                        bp_type: $crate::__private::retro_core::types::BreakpointType::Event {
                            code,
                            param,
                        },
                        cpu_id: ::core::option::Option::None,
                        condition: ::core::option::Option::None,
                    },
                )
            }

            /// Every breakpoint, enabled or not: `[{ id, breakpoint: {
            /// address, bp_type, cpu_id, condition }, enabled, hit_count,
            /// match_spec }]`, `bp_type` `{ Access: { exec, read, write } }`
            /// or `{ Event: { code, param } }`.
            #[wasm_bindgen(js_name = listBreakpoints)]
            pub fn list_breakpoints(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::debugger::debugger_value(
                    &$crate::__private::retro_core::DebuggerCore::list_breakpoints(
                        $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                    ),
                )
            }

            /// The execution mode: `"Running"`, `"Paused"`, `"StepInto"`,
            /// `"StepOver"` or `"StepOut"`.
            #[wasm_bindgen(js_name = getExecutionMode)]
            pub fn get_execution_mode(&mut self) -> ::std::string::String {
                $crate::debugger::execution_mode_name(
                    $crate::__private::retro_core::DebuggerCore::get_execution_mode(
                        $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                    ),
                )
            }

            /// Set the execution mode by the name `getExecutionMode` reads;
            /// a name it does not know is `"Running"`. A pause writes the
            /// CPU states `getCpuStates` returns, wherever the game stopped.
            #[wasm_bindgen(js_name = setExecutionMode)]
            pub fn set_execution_mode(&mut self, mode: ::std::string::String) {
                $crate::__private::retro_core::EmulatorCore::set_execution_mode(
                    &mut self.$core,
                    $crate::debugger::execution_mode_from_name(&mode),
                );
            }

            /// The memory regions a viewer can show: `[{ region, name,
            /// start_address, size, writable }]`, `region` one of
            /// `"MainRam"`, `"VideoRam"`, `"AudioRam"`, `"Rom"`, `"SaveRam"`,
            /// `"PaletteRam"`, `"SpriteTable"`.
            #[wasm_bindgen(js_name = getMemoryRegions)]
            pub fn get_memory_regions(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::debugger::debugger_value(
                    &$crate::__private::retro_core::DebuggerCore::get_memory_regions(
                        $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                    ),
                )
            }

            /// The newest `count` instructions a CPU ran under the debugger
            /// (its steps, and the frames of the full debugger loop), the
            /// newest first: `[{ address, instruction, registers_before: [{
            /// name, value }], cycle_count }]`; `[]` for a CPU that keeps no
            /// history.
            #[wasm_bindgen(js_name = getInstructionHistory)]
            pub fn get_instruction_history(
                &mut self,
                cpu_id: usize,
                count: u32,
            ) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::debugger::debugger_value(
                    &$crate::__private::retro_core::DebuggerCore::get_instruction_history(
                        $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                        cpu_id,
                        count,
                    ),
                )
            }

            /// The calls a CPU made and has not returned from, the outermost
            /// first: `[{ call_address, return_address, function_name }]`;
            /// `[]` for a CPU that keeps none. A reset or a load empties it.
            #[wasm_bindgen(js_name = getCallStack)]
            pub fn get_call_stack(&mut self, cpu_id: usize) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::debugger::debugger_value(
                    &$crate::__private::retro_core::DebuggerCore::get_call_stack(
                        $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                        cpu_id,
                    ),
                )
            }

            /// The CPU the debugger's views and `step` follow.
            #[wasm_bindgen(js_name = getFocusedCpu)]
            pub fn get_focused_cpu(&self) -> usize {
                $crate::__private::retro_core::EmulatorCore::focused_cpu(&self.$core)
            }

            /// Focus a CPU; an id past the CPUs `getSystemInfo` lists is
            /// ignored.
            #[wasm_bindgen(js_name = setFocusedCpu)]
            pub fn set_focused_cpu(&mut self, cpu_id: usize) {
                $crate::__private::retro_core::EmulatorCore::set_focused_cpu(
                    &mut self.$core,
                    cpu_id,
                );
            }

            /// Run one instruction of the focused CPU, as
            /// `stepCpu(getFocusedCpu())` does.
            pub fn step(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                let result = $crate::__private::retro_core::EmulatorCore::step(&mut self.$core);
                $( self.$after_step(); )?
                $crate::debugger::debugger_value(&result)
            }

            /// Run one instruction of a CPU and return `{ cycles,
            /// breakpoint_hit, instruction_executed }`: the cycles it took,
            /// whether a breakpoint, a watchpoint or an event stopped there,
            /// and whether an instruction ran (`false` for a CPU that cannot
            /// be stepped). A CPU that keeps a history records the
            /// instruction and its call stack follows it, and the CPU states
            /// `getCpuStates` returns are written.
            #[wasm_bindgen(js_name = stepCpu)]
            pub fn step_cpu(&mut self, cpu_id: usize) -> $crate::__private::wasm_bindgen::JsValue {
                let result =
                    $crate::__private::retro_core::EmulatorCore::step_cpu(&mut self.$core, cpu_id);
                $( self.$after_step(); )?
                $crate::debugger::debugger_value(&result)
            }

            /// Whether a CPU has run an instruction from `addr` (masked to
            /// its address space) since its coverage was last cleared;
            /// `false` for a CPU with no coverage.
            #[wasm_bindgen(js_name = isCovered)]
            pub fn is_covered(&self, cpu_id: usize, addr: u32) -> bool {
                $crate::__private::retro_core::DebuggerCore::is_covered(
                    $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                    cpu_id,
                    addr,
                )
            }

            /// `{ coveredBytes, percent, totalBytes }`, a plain object: the
            /// addresses a CPU ran instructions from, their share in percent
            /// of the addresses its coverage spans, and how many it spans;
            /// all 0 for a CPU with no coverage.
            #[wasm_bindgen(js_name = getCoverageStats)]
            pub fn get_coverage_stats(&self, cpu_id: usize) -> $crate::__private::wasm_bindgen::JsValue {
                let (total, covered) = $crate::__private::retro_core::DebuggerCore::coverage_stats(
                    $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                    cpu_id,
                );
                $crate::debugger::coverage_stats_object(total, covered)
            }

            /// Forget a CPU's coverage. Loading a different program forgets
            /// every CPU's; a reset keeps it.
            #[wasm_bindgen(js_name = clearCoverage)]
            pub fn clear_coverage(&mut self, cpu_id: usize) {
                $crate::__private::retro_core::DebuggerCore::clear_coverage(
                    $crate::__private::retro_core::EmulatorCore::debugger(&mut self.$core),
                    cpu_id,
                );
            }

            /// What a CPU's breakpoints are checked for: `{ execute, memory,
            /// conditions }`, execute breakpoints, read and write
            /// watchpoints, and conditions; all `false` for a CPU past the
            /// list. A host offers a user only what is checked.
            #[wasm_bindgen(js_name = getBreakpointChecks)]
            pub fn get_breakpoint_checks(&self, cpu_id: usize) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::debugger::debugger_value(
                    &$crate::__private::retro_core::DebuggerCore::breakpoint_checks(
                        $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                        cpu_id,
                    ),
                )
            }

            /// Trace the main CPU (CPU 0), or stop. While on, each
            /// instruction it runs is appended to its trace ring,
            /// `getTraceEntrySize()` bytes an entry; turning it on starts
            /// the ring empty, and the ring is made the first time.
            #[wasm_bindgen(js_name = setTracingEnabled)]
            pub fn set_tracing_enabled(&mut self, enabled: bool) {
                $crate::__private::retro_core::DebuggerCore::set_tracing_enabled(
                    $crate::__private::retro_core::EmulatorCore::debugger(&mut self.$core),
                    0,
                    enabled,
                );
            }

            /// Whether the main CPU is traced.
            #[wasm_bindgen(js_name = isTracingEnabled)]
            pub fn is_tracing_enabled(&self) -> bool {
                $crate::__private::retro_core::DebuggerCore::is_tracing_enabled(
                    $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                    0,
                )
            }

            /// Whether any CPU's trace ring asks to be flushed, the main
            /// CPU's or another's: each is drained with its own
            /// `get...TraceFlushData` and `clear...TraceFlushed`.
            #[wasm_bindgen(js_name = needsTraceFlush)]
            pub fn needs_trace_flush(&self) -> bool {
                $crate::__private::retro_core::DebuggerCore::any_trace_needs_flush(
                    $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                )
            }

            /// The main CPU's traced entries not yet cleared, a copy; empty
            /// when there are none.
            #[wasm_bindgen(js_name = getTraceFlushData)]
            pub fn get_trace_flush_data(&self) -> ::std::vec::Vec<u8> {
                $crate::__private::retro_core::DebuggerCore::get_trace_flush_data(
                    $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                    0,
                )
                .unwrap_or_default()
            }

            /// Empty the main CPU's trace ring, after its entries were
            /// written out.
            #[wasm_bindgen(js_name = clearTraceFlushed)]
            pub fn clear_trace_flushed(&mut self) {
                $crate::__private::retro_core::DebuggerCore::clear_trace_flushed(
                    $crate::__private::retro_core::EmulatorCore::debugger(&mut self.$core),
                    0,
                );
            }

            /// The size in bytes of the main CPU's trace entries.
            #[wasm_bindgen(js_name = getTraceEntrySize)]
            pub fn get_trace_entry_size(&self) -> usize {
                $crate::__private::retro_core::DebuggerCore::trace_entry_size(
                    $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                    0,
                )
                .unwrap_or(0)
            }

            /// A chunk of the main CPU's trace as text: one line per whole
            /// entry, each ending in a line break, in the console's own
            /// format; bytes past the last whole entry make no line.
            #[wasm_bindgen(js_name = convertTraceChunkToText)]
            pub fn convert_trace_chunk_to_text(&mut self, data: &[u8]) -> ::std::string::String {
                $crate::__private::retro_core::EmulatorCore::trace_to_text(&mut self.$core, 0, data)
                    .unwrap_or_default()
            }

            /// `{ total_entries, flushed_entries, buffered_entries,
            /// buffer_bytes, buffer_capacity, enabled }` of the main CPU's
            /// trace ring; all 0 and `enabled` false until its tracing is
            /// first turned on.
            #[wasm_bindgen(js_name = getTraceStats)]
            pub fn get_trace_stats(&self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::debugger::debugger_value(
                    &$crate::__private::retro_core::DebuggerCore::trace_stats(
                        $crate::__private::retro_core::EmulatorCore::debugger_ref(&self.$core),
                        0,
                    )
                    .unwrap_or_default(),
                )
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_execution_mode_is_named_both_ways() {
        for mode in [
            ExecutionMode::Running,
            ExecutionMode::Paused,
            ExecutionMode::StepInto,
            ExecutionMode::StepOver,
            ExecutionMode::StepOut,
        ] {
            assert_eq!(execution_mode_from_name(&execution_mode_name(mode)), mode);
        }
        assert_eq!(execution_mode_name(ExecutionMode::StepOver), "StepOver");
        assert_eq!(execution_mode_from_name("paused"), ExecutionMode::Running);
        assert_eq!(execution_mode_from_name(""), ExecutionMode::Running);
    }

    #[test]
    fn a_breakpoint_type_is_read_from_its_name() {
        assert_eq!(breakpoint_type_from_name("read"), BreakpointType::read());
        assert_eq!(breakpoint_type_from_name("write"), BreakpointType::write());
        assert_eq!(
            breakpoint_type_from_name("readwrite"),
            BreakpointType::read_write()
        );
        assert_eq!(
            breakpoint_type_from_name("execute"),
            BreakpointType::execute()
        );
        assert_eq!(breakpoint_type_from_name("Read"), BreakpointType::execute());
    }
}

#[cfg(test)]
mod binding_tests {
    use crate::test_support::shape;
    use retro_core::debug::{
        trace_to_text, BreakpointChecks, ConsoleDebug, CpuDebugSpec, DebuggerState, EventParam,
        EventSpec, TraceEntry, TraceText,
    };
    use retro_core::types::*;
    use retro_core::{DebuggerCore, EmulatorCore, StepResult};
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_test::*;

    /// A 4-byte trace entry: the address of the instruction.
    struct Entry(u32);

    impl TraceEntry for Entry {
        const SIZE: usize = 4;
        type Bytes = [u8; 4];

        fn to_bytes(&self) -> [u8; 4] {
            self.0.to_le_bytes()
        }

        fn from_bytes(bytes: &[u8]) -> Option<Self> {
            Some(Self(u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?)))
        }
    }

    impl TraceText<()> for Entry {
        fn write_text(&self, _context: &mut (), out: &mut String) {
            out.push_str(&format!("at {:04X}", self.0));
        }
    }

    /// The main CPU: coverage, a 24-bit mask, a trace and a history.
    const MAIN: CpuDebugSpec = CpuDebugSpec {
        coverage_size: 0x0100_0000,
        address_mask: 0x00FF_FFFF,
        trace_entry_size: 4,
        history: true,
        checks: BreakpointChecks::ALL,
    };

    /// The sound CPU: 64 KiB of coverage, a 2-byte trace, no history,
    /// execute breakpoints only.
    const SOUND: CpuDebugSpec = CpuDebugSpec {
        coverage_size: 0x1_0000,
        address_mask: 0xFFFF,
        trace_entry_size: 2,
        history: false,
        checks: BreakpointChecks {
            execute: true,
            memory: false,
            conditions: false,
        },
    };

    fn system() -> SystemInfo {
        SystemInfo {
            name: "Test".to_string(),
            short_name: "test".to_string(),
            manufacturer: "Nobody".to_string(),
            year: 1990,
            cpus: (0..2)
                .map(|id| CpuInfo {
                    id,
                    name: format!("CPU {id}"),
                    architecture: CpuArchitecture::M68000,
                    clock_speed: 1,
                    registers: Vec::new(),
                    location: CpuLocation::Motherboard,
                    address_width: 24,
                })
                .collect(),
            display: DisplayInfo {
                width: 1,
                height: 1,
                aspect_ratio: 1.0,
                refresh_rate: 60.0,
                pixel_format: PixelFormat::RGBA8888,
            },
            audio: AudioInfo {
                sample_rate: 1,
                channels: 1,
                bits_per_sample: 16,
            },
            input: InputInfo {
                max_players: 1,
                button_layout: Vec::new(),
            },
        }
    }

    /// A core of two CPUs that each run two-byte instructions from 0x100;
    /// the main CPU's instruction at 0x102 calls, the one at 0x106 returns.
    struct FakeCore<C: ConsoleDebug> {
        debugger: DebuggerState<2, C>,
        system: SystemInfo,
        pc: [u32; 2],
    }

    impl<C: ConsoleDebug> FakeCore<C> {
        fn new(console: C) -> Self {
            Self {
                debugger: DebuggerState::new([MAIN, SOUND], system(), console),
                system: system(),
                pc: [0x100; 2],
            }
        }
    }

    impl<C: ConsoleDebug> EmulatorCore for FakeCore<C> {
        crate::fake_machine::unreachable_machine_items!();
        fn system_info(&self) -> &SystemInfo {
            &self.system
        }
        fn reset(&mut self) {
            unreachable!("the bindings do not reset")
        }
        fn step_cpu(&mut self, cpu_id: usize) -> StepResult {
            if cpu_id >= 2 {
                return StepResult {
                    cycles: 0,
                    breakpoint_hit: false,
                    instruction_executed: false,
                };
            }
            let pc = self.pc[cpu_id];
            let mut hit = self.debugger.check_execute(cpu_id, pc, None);
            self.debugger.record_coverage(cpu_id, pc);
            if self.debugger.tracing(cpu_id) {
                if cpu_id == 0 {
                    self.debugger.record_trace(0, &Entry(pc));
                } else {
                    self.debugger
                        .record_trace_bytes(1, &(pc as u16).to_le_bytes());
                }
            }
            self.debugger.record_history(
                cpu_id,
                InstructionTrace {
                    address: pc,
                    instruction: format!("op {pc:X}"),
                    registers_before: vec![RegisterValue {
                        name: "PC".to_string(),
                        value: u64::from(pc),
                    }],
                    cycle_count: 4,
                },
            );
            match pc {
                0x102 => self.debugger.push_call(
                    cpu_id,
                    StackFrame {
                        call_address: pc,
                        return_address: pc + 2,
                        function_name: None,
                    },
                ),
                0x106 => self.debugger.pop_call(cpu_id),
                _ => {}
            }
            self.pc[cpu_id] = pc + 2;
            hit |= self
                .debugger
                .check_execute_next(cpu_id, self.pc[cpu_id], None);
            self.refresh_debugger_state();
            StepResult {
                cycles: 4,
                breakpoint_hit: hit,
                instruction_executed: true,
            }
        }
        fn focused_cpu(&self) -> usize {
            self.debugger.focused_cpu()
        }
        fn set_focused_cpu(&mut self, cpu_id: usize) {
            self.debugger.set_focused_cpu(cpu_id);
        }
        fn run_frame(&mut self) {
            unreachable!()
        }
        fn run_until_vblank(&mut self) {
            unreachable!()
        }
        fn load_rom(&mut self, _data: &[u8]) -> Result<(), String> {
            unreachable!()
        }
        fn get_rom_info(&self) -> Option<RomInfo> {
            unreachable!()
        }
        fn get_framebuffer(&self) -> &Framebuffer {
            unreachable!()
        }
        fn get_display_info(&self) -> &DisplayInfo {
            unreachable!()
        }
        fn get_audio_info(&self) -> &AudioInfo {
            unreachable!()
        }
        fn set_audio_enabled(&mut self, _enabled: bool) {
            unreachable!()
        }
        fn is_audio_enabled(&self) -> bool {
            unreachable!()
        }
        fn set_input(&mut self, _player: u8, _input: InputState) {
            unreachable!()
        }
        fn get_input_info(&self) -> &InputInfo {
            unreachable!()
        }
        fn get_save_ram(&self) -> Option<&[u8]> {
            unreachable!()
        }
        fn set_save_ram(&mut self, _data: &[u8]) {
            unreachable!()
        }
        fn debugger(&mut self) -> &mut dyn DebuggerCore {
            &mut self.debugger
        }
        fn debugger_ref(&self) -> &dyn DebuggerCore {
            &self.debugger
        }
        fn refresh_debugger_state(&mut self) {
            let states = (0..2)
                .map(|cpu| CpuState {
                    cpu_id: cpu,
                    registers: vec![RegisterValue {
                        name: "PC".to_string(),
                        value: u64::from(self.pc[cpu]),
                    }],
                    pc: self.pc[cpu],
                    flags: 0,
                })
                .collect();
            self.debugger.set_cpu_states(states);
        }
        fn read_memory_debug(&mut self, _addr: u32, _size: u32) -> Vec<u8> {
            unreachable!()
        }
        fn disassemble_at(&mut self, _addr: u32, _count: u32) -> Vec<Instruction> {
            unreachable!()
        }
        fn disassemble_cpu(&mut self, _cpu_id: usize, _addr: u32, _count: u32) -> Vec<Instruction> {
            unreachable!()
        }
        fn trace_to_text(&mut self, cpu_id: usize, data: &[u8]) -> Option<String> {
            (cpu_id == 0).then(|| trace_to_text::<Entry, ()>(data, &mut ()))
        }
    }

    /// An emulator struct as a binding crate declares one, with a copy of
    /// the picture it refreshes after a step.
    #[wasm_bindgen]
    pub struct SteppedEmulator {
        core: FakeCore<()>,
        refreshes: u32,
    }

    impl SteppedEmulator {
        fn refresh(&mut self) {
            self.refreshes += 1;
        }
    }

    crate::debugger_bindings!(SteppedEmulator, core, after_step = refresh);

    /// A console with a catalogue and a region of its own.
    struct Console;

    impl ConsoleDebug for Console {
        const EVENTS: &'static [EventSpec] = &[
            EventSpec {
                code: 1,
                kind: EventKind::VerticalBlank,
                label: "VBlank",
                description: "The beam entered vertical blank",
                param: EventParam::None,
            },
            EventSpec {
                code: 5,
                kind: EventKind::TransferDone,
                label: "Transfer done",
                description: "A transfer finished",
                param: EventParam::Choice {
                    label: "Memory",
                    options: &[(0, "VRAM"), (1, "CRAM")],
                },
            },
        ];

        fn memory_regions(&self) -> Vec<MemoryRegionInfo> {
            vec![MemoryRegionInfo {
                region: MemoryRegion::PaletteRam,
                name: "CRAM".to_string(),
                start_address: 0,
                size: 128,
                writable: true,
            }]
        }
    }

    /// An emulator struct that keeps no copy of the picture.
    #[wasm_bindgen]
    pub struct CatalogueEmulator {
        core: FakeCore<Console>,
    }

    crate::debugger_bindings!(CatalogueEmulator, core);

    fn emulator() -> SteppedEmulator {
        SteppedEmulator {
            core: FakeCore::new(()),
            refreshes: 0,
        }
    }

    #[wasm_bindgen_test]
    fn a_step_runs_the_focused_cpu_and_then_the_after_step_method() {
        let mut emu = emulator();
        assert_eq!(
            shape(&emu.step()),
            "{breakpoint_hit: false, cycles: 4, instruction_executed: true}"
        );
        assert_eq!(emu.refreshes, 1);
        emu.set_focused_cpu(1);
        assert_eq!(emu.get_focused_cpu(), 1);
        emu.step();
        assert_eq!(emu.core.pc, [0x102, 0x102], "each step ran the focused CPU");
        assert_eq!(
            shape(&emu.step_cpu(5)),
            "{breakpoint_hit: false, cycles: 0, instruction_executed: false}"
        );
        assert_eq!(emu.refreshes, 3);
        emu.set_focused_cpu(2);
        assert_eq!(emu.get_focused_cpu(), 1, "a CPU past the list is ignored");

        let mut bare = CatalogueEmulator {
            core: FakeCore::new(Console),
        };
        bare.step();
        bare.step_cpu(0);
        assert_eq!(bare.core.pc[0], 0x104);
    }

    #[wasm_bindgen_test]
    fn the_cpu_states_are_what_the_core_last_wrote() {
        let mut emu = emulator();
        assert_eq!(shape(&emu.get_cpu_states()), "[]");
        emu.step_cpu(0);
        assert_eq!(
            shape(&emu.get_cpu_states()),
            "[{cpu_id: 0, flags: 0, pc: 258, registers: [{name: \"PC\", value: 258}]}, \
             {cpu_id: 1, flags: 0, pc: 256, registers: [{name: \"PC\", value: 256}]}]"
        );
    }

    #[wasm_bindgen_test]
    fn the_breakpoints_are_added_listed_and_changed() {
        let mut emu = emulator();
        assert_eq!(emu.add_breakpoint(0x104, "execute".to_string(), None), 0);
        assert_eq!(emu.add_breakpoint(0x20, "write".to_string(), Some(1)), 1);
        assert_eq!(emu.add_breakpoint(0x30, "nothing".to_string(), None), 2);
        assert_eq!(emu.add_event_breakpoint(5, 0xFFFF_FFFF), 3);
        emu.add_breakpoint(0x40, "readwrite".to_string(), None);
        emu.add_breakpoint(0x50, "read".to_string(), None);
        emu.remove_breakpoint(4);
        emu.remove_breakpoint(99);
        emu.enable_breakpoint(3, false);
        emu.set_breakpoint_access(2, false, true, true);
        let spec = plain_spec(&[
            ("value", JsValue::from_f64(18.0)),
            ("hit_skip", JsValue::from_f64(1.0)),
            ("value_compare", JsValue::from_str("ge")),
        ]);
        emu.set_breakpoint_match_spec(1, spec).unwrap();
        assert!(emu
            .set_breakpoint_match_spec(1, JsValue::from_str("not a spec"))
            .is_err());
        assert_eq!(
            shape(&emu.list_breakpoints()),
            "[{breakpoint: {address: 260, bp_type: {Access: {exec: true, read: false, write: \
             false}}, condition: undefined, cpu_id: 0}, enabled: true, hit_count: 0, id: 0, \
             match_spec: {action: \"break\", address_end: undefined, expr: undefined, hit_skip: \
             0, one_shot: false, value: undefined, value_compare: \"eq\", value_mask: 0, \
             value_size: 0}}, {breakpoint: {address: 32, bp_type: {Access: {exec: false, read: \
             false, write: true}}, condition: undefined, cpu_id: 1}, enabled: true, hit_count: \
             0, id: 1, match_spec: {action: \"break\", address_end: undefined, expr: undefined, \
             hit_skip: 1, one_shot: false, value: 18, value_compare: \"ge\", value_mask: 0, \
             value_size: 0}}, {breakpoint: {address: 48, bp_type: {Access: {exec: false, read: \
             true, write: true}}, condition: undefined, cpu_id: 0}, enabled: true, hit_count: \
             0, id: 2, match_spec: {action: \"break\", address_end: undefined, expr: undefined, \
             hit_skip: 0, one_shot: false, value: undefined, value_compare: \"eq\", value_mask: \
             0, value_size: 0}}, {breakpoint: {address: 0, bp_type: {Event: {code: 5, param: \
             4294967295}}, condition: undefined, cpu_id: undefined}, enabled: false, hit_count: \
             0, id: 3, match_spec: {action: \"break\", address_end: undefined, expr: undefined, \
             hit_skip: 0, one_shot: false, value: undefined, value_compare: \"eq\", value_mask: \
             0, value_size: 0}}, {breakpoint: {address: 80, bp_type: {Access: {exec: false, \
             read: true, write: false}}, condition: undefined, cpu_id: 0}, enabled: true, \
             hit_count: 0, id: 5, match_spec: {action: \"break\", address_end: undefined, expr: \
             undefined, hit_skip: 0, one_shot: false, value: undefined, value_compare: \"eq\", \
             value_mask: 0, value_size: 0}}]"
        );
    }

    /// A plain object holding these keys.
    fn plain_spec(entries: &[(&str, JsValue)]) -> JsValue {
        crate::js::plain_object(entries)
    }

    #[wasm_bindgen_test]
    fn a_stop_is_left_running_on_from_the_instruction_at_it() {
        let mut emu = emulator();
        emu.add_breakpoint(0x102, "execute".to_string(), None);
        assert_eq!(
            shape(&emu.step_cpu(0)),
            "{breakpoint_hit: true, cycles: 4, instruction_executed: true}",
            "the check after the instruction at 0x100 stops in front of 0x102"
        );
        assert_eq!(
            shape(&emu.step_cpu(0)),
            "{breakpoint_hit: false, cycles: 4, instruction_executed: true}",
            "the instruction at 0x102 runs"
        );
    }

    #[wasm_bindgen_test]
    fn the_event_catalogue_carries_each_event_s_kind() {
        let mut emu = emulator();
        assert_eq!(shape(&emu.get_available_events()), "[]");
        let mut console = CatalogueEmulator {
            core: FakeCore::new(Console),
        };
        assert_eq!(
            shape(&console.get_available_events()),
            "[{code: 1, description: \"The beam entered vertical blank\", kind: \
             \"vertical_blank\", label: \"VBlank\", param_label: undefined, param_options: []}, \
             {code: 5, description: \"A transfer finished\", kind: \"transfer_done\", label: \
             \"Transfer done\", param_label: \"Memory\", param_options: [{label: \"VRAM\", \
             value: 0}, {label: \"CRAM\", value: 1}]}]"
        );
    }

    #[wasm_bindgen_test]
    fn the_memory_regions_are_the_console_s() {
        let mut console = CatalogueEmulator {
            core: FakeCore::new(Console),
        };
        assert_eq!(
            shape(&console.get_memory_regions()),
            "[{name: \"CRAM\", region: \"PaletteRam\", size: 128, start_address: 0, writable: \
             true}]"
        );
        assert_eq!(shape(&emulator().get_memory_regions()), "[]");
    }

    #[wasm_bindgen_test]
    fn the_execution_mode_is_named_and_a_pause_writes_the_states() {
        let mut emu = emulator();
        assert_eq!(emu.get_execution_mode(), "Running");
        emu.set_execution_mode("StepOut".to_string());
        assert_eq!(emu.get_execution_mode(), "StepOut");
        emu.set_execution_mode("no such mode".to_string());
        assert_eq!(emu.get_execution_mode(), "Running");
        assert_eq!(shape(&emu.get_cpu_states()), "[]", "running writes nothing");
        emu.set_execution_mode("Paused".to_string());
        assert_eq!(emu.get_execution_mode(), "Paused");
        assert!(shape(&emu.get_cpu_states()).starts_with("[{cpu_id: 0"));
    }

    #[wasm_bindgen_test]
    fn the_history_is_newest_first_and_the_call_stack_outermost_first() {
        let mut emu = emulator();
        for _ in 0..3 {
            emu.step_cpu(0);
        }
        emu.step_cpu(1);
        assert_eq!(
            shape(&emu.get_instruction_history(0, 2)),
            "[{address: 260, cycle_count: 4, instruction: \"op 104\", registers_before: \
             [{name: \"PC\", value: 260}]}, {address: 258, cycle_count: 4, instruction: \"op \
             102\", registers_before: [{name: \"PC\", value: 258}]}]"
        );
        assert_eq!(shape(&emu.get_instruction_history(0, 0)), "[]");
        assert_eq!(
            shape(&emu.get_instruction_history(1, 8)),
            "[]",
            "a CPU with no history"
        );
        assert_eq!(
            shape(&emu.get_call_stack(0)),
            "[{call_address: 258, function_name: undefined, return_address: 260}]"
        );
        emu.step_cpu(0);
        assert_eq!(shape(&emu.get_call_stack(0)), "[]", "0x106 returned");
        assert_eq!(shape(&emu.get_call_stack(1)), "[]");
        assert_eq!(shape(&emu.get_call_stack(7)), "[]");
    }

    #[wasm_bindgen_test]
    fn the_coverage_is_read_and_cleared_per_cpu() {
        let mut emu = emulator();
        emu.step_cpu(0);
        emu.step_cpu(1);
        assert!(emu.is_covered(0, 0x100));
        assert!(emu.is_covered(0, 0xFF00_0100), "masked to the CPU's space");
        assert!(!emu.is_covered(0, 0x102));
        assert!(emu.is_covered(1, 0x1_0100));
        assert!(!emu.is_covered(2, 0x100));
        assert_eq!(
            shape(&emu.get_coverage_stats(0)),
            "{coveredBytes: 1, percent: 0.0000059604644775390625, totalBytes: 16777216}"
        );
        assert_eq!(
            shape(&emu.get_coverage_stats(1)),
            "{coveredBytes: 1, percent: 0.00152587890625, totalBytes: 65536}"
        );
        assert_eq!(
            shape(&emu.get_coverage_stats(2)),
            "{coveredBytes: 0, percent: 0, totalBytes: 0}"
        );
        emu.clear_coverage(0);
        assert!(!emu.is_covered(0, 0x100));
        assert!(emu.is_covered(1, 0x100));
    }

    #[wasm_bindgen_test]
    fn the_coverage_stats_are_a_plain_object_in_the_order_read_before() {
        let stats = super::coverage_stats_object(16, 4);
        let keys: Vec<String> = js_sys::Object::keys(stats.unchecked_ref())
            .iter()
            .filter_map(|key| key.as_string())
            .collect();
        assert_eq!(keys, ["coveredBytes", "percent", "totalBytes"]);
        assert_eq!(
            shape(&stats),
            "{coveredBytes: 4, percent: 25, totalBytes: 16}"
        );
    }

    #[wasm_bindgen_test]
    fn the_breakpoint_checks_are_each_cpu_s() {
        let emu = emulator();
        assert_eq!(
            shape(&emu.get_breakpoint_checks(0)),
            "{conditions: true, execute: true, memory: true}"
        );
        assert_eq!(
            shape(&emu.get_breakpoint_checks(1)),
            "{conditions: false, execute: true, memory: false}"
        );
        assert_eq!(
            shape(&emu.get_breakpoint_checks(2)),
            "{conditions: false, execute: false, memory: false}"
        );
    }

    #[wasm_bindgen_test]
    fn the_main_cpu_s_trace_is_switched_read_and_cleared() {
        let mut emu = emulator();
        assert!(!emu.is_tracing_enabled());
        assert_eq!(emu.get_trace_entry_size(), 4);
        assert_eq!(
            shape(&emu.get_trace_stats()),
            "{buffer_bytes: 0, buffer_capacity: 0, buffered_entries: 0, enabled: false, \
             flushed_entries: 0, total_entries: 0}",
            "no ring until tracing is first turned on"
        );
        emu.set_tracing_enabled(true);
        emu.step_cpu(0);
        emu.step_cpu(0);
        assert!(emu.is_tracing_enabled());
        let data = emu.get_trace_flush_data();
        assert_eq!(data, [0x00, 0x01, 0x00, 0x00, 0x02, 0x01, 0x00, 0x00]);
        assert_eq!(
            shape(&emu.get_trace_stats()),
            "{buffer_bytes: 8, buffer_capacity: 8388608, buffered_entries: 2, enabled: true, \
             flushed_entries: 0, total_entries: 2}"
        );
        let mut longer = data.clone();
        longer.extend_from_slice(&[0xAA; 3]);
        assert_eq!(
            emu.convert_trace_chunk_to_text(&longer),
            "at 0100\nat 0102\n",
            "a trailing part of an entry makes no line"
        );
        emu.clear_trace_flushed();
        assert!(emu.get_trace_flush_data().is_empty());
        assert!(!emu.needs_trace_flush());
        emu.set_tracing_enabled(false);
        assert!(!emu.is_tracing_enabled());
    }

    #[wasm_bindgen_test]
    fn the_flush_flag_reads_every_cpu_s_ring() {
        let mut emu = emulator();
        assert!(!emu.needs_trace_flush());
        DebuggerCore::set_tracing_enabled(&mut emu.core.debugger, 1, true);
        let chunk = vec![0u8; 1024 * 1024];
        for _ in 0..9 {
            emu.core.debugger.record_trace_bytes(1, &chunk);
        }
        assert!(
            emu.needs_trace_flush(),
            "the sound CPU's ring alone asks to be flushed"
        );
        assert!(
            emu.get_trace_flush_data().is_empty(),
            "the main CPU's is empty"
        );
        DebuggerCore::clear_trace_flushed(&mut emu.core.debugger, 1);
        assert!(!emu.needs_trace_flush());
    }
}
