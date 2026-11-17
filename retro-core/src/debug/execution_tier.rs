//! Execution-tier selector shared across console cores.
//!
//! The frame driver runs a core's frames on one of three tiers, each
//! trading runtime cost against debug instrumentation:
//!
//! - **Play** ([`crate::frame::Play`]): no breakpoint checks, no tracing,
//!   no per-instruction bookkeeping but the main CPU's coverage. The hot
//!   path when the user is just playing the game with the debugger panel
//!   idle.
//! - **LightDebug** ([`crate::frame::LightDebug`]): breakpoint checks,
//!   watchpoints, events, traces and the call stack, while the user has a
//!   breakpoint or tracing on but is still in `Running` mode.
//! - **FullDebug** ([`crate::frame::run_full`]): the debugger paused or
//!   stepping; a step also records the instruction history. This is the
//!   only tier appropriate for `Paused` / `StepInto` / `StepOver` /
//!   `StepOut`.
//!
//! Picking the right tier for a given snapshot of execution state is
//! pure logic, independent of which console is being emulated, so
//! we centralize the rule here.

use crate::types::ExecutionMode;

/// Tier of the per-frame execution loop that a core should run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionTier {
    /// What a player runs ([`crate::frame::Play`]): no debug
    /// instrumentation but the main CPU's coverage. Fastest path.
    Play,
    /// Breakpoint checks + trace recording, but no disassembly /
    /// register history / call-stack work per instruction.
    LightDebug,
    /// Everything wired: state, disassembly, history, call stack.
    /// Required for any non-`Running` `ExecutionMode`.
    FullDebug,
}

/// Snapshot of debugger state required to pick the right
/// [`ExecutionTier`].
///
/// `tracing_enabled_per_cpu` is a slice so cores with multiple CPUs
/// (Genesis: 68K + Z80; SNES: 65816 + SPC700; NES: 6502) can pass
/// their per-CPU tracing flags without the helper needing to know
/// the topology in advance. The selector treats tracing as "active"
/// if *any* CPU has it enabled.
pub struct TierSelectorInputs<'a> {
    pub execution_mode: ExecutionMode,
    pub has_breakpoints: bool,
    pub tracing_enabled_per_cpu: &'a [bool],
}

/// Decide which execution tier the core should run next.
///
/// Rules:
/// - Any non-`Running` mode -> [`ExecutionTier::FullDebug`] (the user
///   is actively stepping or paused; debugger state must be live).
/// - `Running` + no breakpoints + no tracing on any CPU ->
///   [`ExecutionTier::Play`].
/// - `Running` with breakpoints or tracing -> [`ExecutionTier::LightDebug`].
pub fn select_tier(inputs: TierSelectorInputs<'_>) -> ExecutionTier {
    if inputs.execution_mode != ExecutionMode::Running {
        return ExecutionTier::FullDebug;
    }
    let any_trace = inputs.tracing_enabled_per_cpu.iter().any(|&t| t);
    if !inputs.has_breakpoints && !any_trace {
        ExecutionTier::Play
    } else {
        ExecutionTier::LightDebug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs<'a>(mode: ExecutionMode, bps: bool, traces: &'a [bool]) -> TierSelectorInputs<'a> {
        TierSelectorInputs {
            execution_mode: mode,
            has_breakpoints: bps,
            tracing_enabled_per_cpu: traces,
        }
    }

    #[test]
    fn paused_is_always_full_debug() {
        assert_eq!(
            select_tier(inputs(ExecutionMode::Paused, false, &[])),
            ExecutionTier::FullDebug
        );
    }

    #[test]
    fn step_modes_are_full_debug() {
        for mode in [
            ExecutionMode::StepInto,
            ExecutionMode::StepOver,
            ExecutionMode::StepOut,
        ] {
            assert_eq!(
                select_tier(inputs(mode, false, &[])),
                ExecutionTier::FullDebug
            );
        }
    }

    #[test]
    fn running_clean_is_play() {
        assert_eq!(
            select_tier(inputs(ExecutionMode::Running, false, &[false, false])),
            ExecutionTier::Play
        );
    }

    #[test]
    fn running_with_breakpoints_is_light_debug() {
        assert_eq!(
            select_tier(inputs(ExecutionMode::Running, true, &[false, false])),
            ExecutionTier::LightDebug
        );
    }

    #[test]
    fn running_with_any_cpu_tracing_is_light_debug() {
        assert_eq!(
            select_tier(inputs(ExecutionMode::Running, false, &[false, true])),
            ExecutionTier::LightDebug
        );
        assert_eq!(
            select_tier(inputs(ExecutionMode::Running, false, &[true, false])),
            ExecutionTier::LightDebug
        );
    }

    #[test]
    fn empty_cpu_list_is_treated_as_no_tracing() {
        assert_eq!(
            select_tier(inputs(ExecutionMode::Running, false, &[])),
            ExecutionTier::Play
        );
    }

    #[test]
    fn breakpoints_override_tracing_check_does_not_matter_when_both_set() {
        assert_eq!(
            select_tier(inputs(ExecutionMode::Running, true, &[true, true])),
            ExecutionTier::LightDebug
        );
    }
}
