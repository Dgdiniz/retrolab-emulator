//! Breakpoint container shared across console cores.
//!
//! Each core (Genesis, SNES, ...) owns one [`BreakpointList`] inside its
//! debugger. The list stores execute / read / write / read+write
//! breakpoints and event breakpoints, and exposes the checks the CPU step
//! path makes per instruction:
//!
//! - [`BreakpointList::check_execute`]: before an instruction, at its
//!   address.
//! - [`BreakpointList::check_accesses`]: after an instruction, over the
//!   bytes it read and wrote ([`crate::debug::access`]).
//! - [`BreakpointList::check_event`]: when the hardware raises an event a
//!   breakpoint can stop on ([`crate::debug::events`]).
//!
//! The address checks **filter on `cpu_id`** so a breakpoint set on the
//! host CPU (e.g. 68K, address space 0-0xFFFFFF) never spuriously fires for
//! the sub-CPU (e.g. Z80, address space 0-0xFFFF) at an overlapping
//! address. Breakpoints with `cpu_id == None` apply to all CPUs and are
//! intended for system-wide hooks.
//!
//! A condition (`MatchSpec::expr`) and a watch wider than a byte read the
//! machine through an [`ExprContext`] the core builds over the CPU's
//! registers and a side-effect-free peek of its memory
//! ([`crate::debug::expr::CpuExprContext`]).

use crate::debug::access::Access;
use crate::debug::events::ParamSet;
use crate::debug::expr::{eval_str, ExprContext};
use crate::types::{
    AccessFlags, Breakpoint, BreakpointAction, BreakpointId, BreakpointInfo, BreakpointType,
    MatchSpec, BREAKPOINT_PARAM_ANY,
};

/// Append-only list of breakpoints with id allocation and per-CPU
/// filtering.
///
/// The container is cheap: each check is a linear scan over the list,
/// dominated by the fact that real debug sessions hold ~1-10 active
/// breakpoints. Storage is stable: IDs are assigned monotonically and
/// never reused, so frontends can keep references safely.
#[derive(Debug, Clone, Default)]
pub struct BreakpointList {
    breakpoints: Vec<BreakpointInfo>,
    next_id: BreakpointId,
}

impl BreakpointList {
    /// Empty list, ready to receive breakpoints.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a breakpoint and return its newly-allocated stable id.
    pub fn add(&mut self, bp: Breakpoint) -> BreakpointId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.breakpoints.push(BreakpointInfo {
            id,
            breakpoint: bp,
            enabled: true,
            hit_count: 0,
            match_spec: MatchSpec::default(),
            raw_hits: 0,
        });
        id
    }

    /// Append a breakpoint together with its advanced [`MatchSpec`] and
    /// return its newly-allocated stable id. Equivalent to [`Self::add`]
    /// followed by [`Self::set_match_spec`].
    pub fn add_with_spec(&mut self, bp: Breakpoint, spec: MatchSpec) -> BreakpointId {
        let id = self.add(bp);
        self.set_match_spec(id, spec);
        id
    }

    /// Drop the breakpoint with the given id. No-op if the id is
    /// unknown.
    pub fn remove(&mut self, id: BreakpointId) {
        self.breakpoints.retain(|bp| bp.id != id);
    }

    /// Toggle the enabled flag on the given breakpoint. No-op if the
    /// id is unknown.
    pub fn enable(&mut self, id: BreakpointId, enabled: bool) {
        if let Some(bp) = self.breakpoints.iter_mut().find(|bp| bp.id == id) {
            bp.enabled = enabled;
        }
    }

    /// Replace the access flags (exec/read/write) of an access
    /// breakpoint in place, preserving its id, enabled flag, and hit
    /// count. No-op if the id is unknown or the breakpoint is an event
    /// hook (events carry no access flags). This backs the per-row
    /// R/W/X checkboxes in the UI without losing the running hit count
    /// that a remove + re-add would reset.
    pub fn set_access(&mut self, id: BreakpointId, access: AccessFlags) {
        if let Some(bp) = self.breakpoints.iter_mut().find(|bp| bp.id == id) {
            if let BreakpointType::Access(ref mut flags) = bp.breakpoint.bp_type {
                *flags = access;
            }
        }
    }

    /// Replace the advanced [`MatchSpec`] (range / value / hit-skip /
    /// action / one-shot) of a breakpoint in place, preserving its id,
    /// type, enabled flag, and accrued hit count. The runtime skip
    /// counter is reset so the new `hit_skip` window starts fresh. No-op
    /// if the id is unknown. This backs the watchpoints panel's
    /// per-condition editors without losing the running hit count a
    /// remove + re-add would discard.
    pub fn set_match_spec(&mut self, id: BreakpointId, spec: MatchSpec) {
        if let Some(bp) = self.breakpoints.iter_mut().find(|bp| bp.id == id) {
            bp.match_spec = spec;
            bp.raw_hits = 0;
        }
    }

    /// Snapshot of every breakpoint (enabled + disabled). Suitable for
    /// rendering the UI list.
    pub fn list(&self) -> Vec<BreakpointInfo> {
        self.breakpoints.clone()
    }

    /// `true` when no breakpoints are registered. Useful for the
    /// execution-tier selector to pick the play tier without any debug
    /// overhead.
    pub fn is_empty(&self) -> bool {
        self.breakpoints.is_empty()
    }

    /// Number of breakpoints currently registered.
    pub fn len(&self) -> usize {
        self.breakpoints.len()
    }

    /// Returns `true` (halt) when an enabled execute breakpoint matches
    /// `(cpu_id, pc)` and its [`MatchSpec`] conditions fire. A breakpoint
    /// with `cpu_id: None` matches every CPU; an `address_end` range
    /// matches any pc in `[address, address_end]`. Execute breakpoints
    /// carry no observed value, so a value predicate on one never fires.
    ///
    /// `ctx` evaluates a breakpoint's condition (`MatchSpec::expr`) against
    /// the CPU's registers and memory before the instruction runs. With
    /// `None` (a CPU the core builds no context for) a breakpoint carrying
    /// a condition fails closed and stays silent. The context is only
    /// consulted after the pc/cpu match holds, so the common
    /// (no-condition) case pays nothing beyond a never-taken branch.
    pub fn check_execute(
        &mut self,
        cpu_id: usize,
        pc: u32,
        mut ctx: Option<&mut dyn ExprContext>,
    ) -> bool {
        let mut should_break = false;
        for bp in &mut self.breakpoints {
            if !bp.enabled {
                continue;
            }
            let Some(flags) = bp.breakpoint.bp_type.access() else {
                continue;
            };
            if !flags.exec {
                continue;
            }
            if !matches_cpu(bp.breakpoint.cpu_id, cpu_id) {
                continue;
            }
            if !addr_matches(bp, pc) {
                continue;
            }
            // Don't early-return: a passive `Log` breakpoint must still
            // count, and a later `Break` breakpoint at the same pc must
            // still be able to halt.
            should_break |= record_and_should_break(bp, None, ctx.as_deref_mut());
        }
        should_break
    }

    /// Returns `true` (halt) when a read or write watchpoint on `cpu_id`
    /// fires for the bytes one instruction accessed. Each enabled
    /// watchpoint fires **at most once** per call, on the first access it
    /// covers in a direction it watches, so a multi-byte or ranged watch
    /// counts one hit per instruction however many of its bytes were
    /// touched.
    ///
    /// A one-byte value watch compares the byte that access moved. A wider
    /// one (`value_size` 2-4) compares the whole value at its address,
    /// `ctx.read_mem(address, width)`, which the context reads in the CPU's
    /// byte order after the instruction. A condition (`MatchSpec::expr`) is
    /// evaluated through the same context.
    pub fn check_accesses(
        &mut self,
        cpu_id: usize,
        accesses: &[Access],
        ctx: &mut dyn ExprContext,
    ) -> bool {
        let mut should_break = false;
        for bp in &mut self.breakpoints {
            if !bp.enabled {
                continue;
            }
            let Some(flags) = bp.breakpoint.bp_type.access() else {
                continue;
            };
            if !flags.watches_memory() || !matches_cpu(bp.breakpoint.cpu_id, cpu_id) {
                continue;
            }
            // The first covered access whose direction this watch cares
            // about; `None` means this watch saw no relevant traffic.
            let touched = accesses.iter().find(|access| {
                addr_matches(bp, access.address)
                    && if access.write {
                        flags.write
                    } else {
                        flags.read
                    }
            });
            let Some(touched) = touched else {
                continue;
            };
            let observed = if bp.match_spec.needs_value() {
                let width = bp.match_spec.value_width();
                if width > 1 {
                    Some(ctx.read_mem(bp.breakpoint.address, width) as u32)
                } else {
                    Some(u32::from(touched.value))
                }
            } else {
                None
            };
            should_break |= record_and_should_break(bp, observed, Some(&mut *ctx));
        }
        should_break
    }

    /// Returns `true` (halt) when an enabled [`BreakpointType::Event`]
    /// breakpoint matches `code` and either matches `param` exactly or was
    /// stored with [`BREAKPOINT_PARAM_ANY`], and its [`MatchSpec`]
    /// conditions (hit-skip / action / one-shot) fire.
    ///
    /// Events are system-wide, so this does **not** filter on `cpu_id`.
    /// The `code`/`param` catalogue is owned by the calling core; this
    /// container only does the equality match + condition bookkeeping.
    pub fn check_event(&mut self, code: u32, param: u32) -> bool {
        let mut should_break = false;
        for bp in &mut self.breakpoints {
            if !bp.enabled {
                continue;
            }
            if let BreakpointType::Event {
                code: c,
                param: stored,
            } = bp.breakpoint.bp_type
            {
                if c == code
                    && (stored == param || stored == BREAKPOINT_PARAM_ANY)
                    && record_and_should_break(bp, None, None)
                {
                    should_break = true;
                }
            }
        }
        should_break
    }

    /// `true` when any enabled breakpoint is **not** a plain `Execute`
    /// breakpoint (i.e. a read/write watchpoint or an event hook).
    ///
    /// Cores use this to keep the hot path branch-free: bus-level
    /// watchpoint recording and event-hook checks are only armed when
    /// this returns `true`.
    pub fn has_watchpoints_or_events(&self) -> bool {
        self.breakpoints.iter().any(|bp| {
            bp.enabled
                && match bp.breakpoint.bp_type {
                    BreakpointType::Access(flags) => flags.watches_memory(),
                    BreakpointType::Event { .. } => true,
                }
        })
    }

    /// The `(code, param)` of every enabled event breakpoint, in list
    /// order.
    pub fn enabled_events(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.breakpoints
            .iter()
            .filter_map(|bp| match bp.breakpoint.bp_type {
                BreakpointType::Event { code, param } if bp.enabled => Some((code, param)),
                _ => None,
            })
    }

    /// The params the enabled event breakpoints on `code` watch
    /// ([`ParamSet::insert`]: [`BREAKPOINT_PARAM_ANY`] watches them all, a
    /// param past the set's capacity none), or `None` when they watch
    /// nothing. A chip raises an event only for a param in the set.
    pub fn watched_params<const W: usize>(&self, code: u32) -> Option<ParamSet<W>> {
        let mut watched = ParamSet::<W>::new();
        for (event, param) in self.enabled_events() {
            if event == code {
                watched.insert(param);
            }
        }
        (!watched.is_empty()).then_some(watched)
    }
}

/// `cpu_id: None` is a wildcard breakpoint (any CPU); otherwise we
/// require an exact match against the caller-supplied id.
#[inline]
fn matches_cpu(bp_cpu: Option<usize>, current: usize) -> bool {
    match bp_cpu {
        None => true,
        Some(id) => id == current,
    }
}

/// Address match honouring an optional inclusive range end and the
/// implicit span of a multi-byte value watchpoint. A plain breakpoint
/// matches its single `address`; a ranged one matches any address in
/// `[address, address_end]`; a multi-byte value watch (`value_size > 1`)
/// matches any address in `[address, address + value_size - 1]` so a
/// write to *any* byte of the watched word is a candidate. When both a
/// range and a width are present the wider of the two ends wins. An
/// `address_end < address` is treated as no range (single-address match)
/// so a malformed spec can't silently swallow the whole space.
#[inline]
fn addr_matches(info: &BreakpointInfo, addr: u32) -> bool {
    let base = info.breakpoint.address;
    let mut end = match info.match_spec.address_end {
        Some(e) if e >= base => e,
        _ => base,
    };
    if info.match_spec.needs_value() {
        let width = info.match_spec.value_width() as u32;
        if width > 1 {
            end = end.max(base.saturating_add(width - 1));
        }
    }
    addr >= base && addr <= end
}

/// Apply a base-matched breakpoint's [`MatchSpec`] conditions and update
/// its runtime counters. Returns whether execution should **halt**.
///
/// The caller has already confirmed the address/access (or event) match.
/// `observed` is the value seen at the access for value-matched
/// watchpoints, or `None` for execute / event checks (which carry no
/// value). A spec with a value predicate never fires without an observed
/// value, so a value-conditioned execute breakpoint stays inert.
///
/// `ctx` is the optional expression-evaluation context. A breakpoint that
/// carries an `expr` only fires when the expression evaluates non-zero;
/// with no context (or a parse / eval failure) it fails **closed** so an
/// unverifiable condition never produces a spurious halt.
#[inline]
fn record_and_should_break<'c>(
    info: &mut BreakpointInfo,
    observed: Option<u32>,
    ctx: Option<&mut (dyn ExprContext + 'c)>,
) -> bool {
    // Value predicate: only memory accesses provide an observed value.
    if info.match_spec.needs_value() {
        match observed {
            Some(v) if info.match_spec.value_matches(v) => {}
            _ => return false,
        }
    }
    // Expression predicate: evaluate against live machine state. Borrow of
    // `info.match_spec` ends before the counter mutations below.
    if let Some(src) = info.match_spec.expr_source() {
        let fired = match ctx {
            Some(ctx) => eval_str(src, ctx).unwrap_or(false),
            None => false,
        };
        if !fired {
            return false;
        }
    }
    // Hit-skip: ignore the first N qualifying matches.
    info.raw_hits = info.raw_hits.wrapping_add(1);
    if info.raw_hits <= info.match_spec.hit_skip {
        return false;
    }
    // A real fire: count it, honour one-shot, then let `action` decide
    // whether to actually halt (a `Log` point counts but keeps running).
    info.hit_count = info.hit_count.wrapping_add(1);
    if info.match_spec.one_shot {
        info.enabled = false;
    }
    matches!(info.match_spec.action, BreakpointAction::Break)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug::expr::{ByteOrder, CpuExprContext};
    use std::collections::BTreeMap;

    fn exec_bp(addr: u32, cpu: Option<usize>) -> Breakpoint {
        Breakpoint {
            address: addr,
            bp_type: BreakpointType::execute(),
            cpu_id: cpu,
            condition: None,
        }
    }

    fn mem_bp(addr: u32, bp_type: BreakpointType, cpu: Option<usize>) -> Breakpoint {
        Breakpoint {
            address: addr,
            bp_type,
            cpu_id: cpu,
            condition: None,
        }
    }

    fn event_bp(code: u32, param: u32) -> Breakpoint {
        Breakpoint {
            address: 0,
            bp_type: BreakpointType::Event { code, param },
            cpu_id: None,
            condition: None,
        }
    }

    fn hit_count(list: &BreakpointList, id: BreakpointId) -> u32 {
        list.list()
            .into_iter()
            .find(|b| b.id == id)
            .unwrap()
            .hit_count
    }

    fn read(address: u32, value: u8) -> Access {
        Access {
            address,
            write: false,
            value,
        }
    }

    fn write(address: u32, value: u8) -> Access {
        Access {
            address,
            write: true,
            value,
        }
    }

    /// A machine for the checks: a register map and a sparse memory (absent
    /// bytes read 0), in a byte order.
    struct Machine {
        registers: BTreeMap<&'static str, i64>,
        memory: BTreeMap<u32, u8>,
        order: ByteOrder,
    }

    impl Machine {
        fn new(order: ByteOrder) -> Self {
            Self {
                registers: BTreeMap::new(),
                memory: BTreeMap::new(),
                order,
            }
        }

        fn with_registers(mut self, pairs: &[(&'static str, i64)]) -> Self {
            self.registers.extend(pairs.iter().copied());
            self
        }

        fn with_bytes(mut self, base: u32, bytes: &[u8]) -> Self {
            for (offset, &byte) in bytes.iter().enumerate() {
                self.memory.insert(base + offset as u32, byte);
            }
            self
        }

        /// Run `check` with a context over this machine.
        fn check<T>(&self, check: impl FnOnce(&mut dyn ExprContext) -> T) -> T {
            let mut ctx = CpuExprContext::new(
                |name| self.registers.get(name).copied(),
                |address| *self.memory.get(&address).unwrap_or(&0),
                self.order,
            );
            check(&mut ctx)
        }
    }

    /// No registers, no memory, big-endian.
    fn empty() -> Machine {
        Machine::new(ByteOrder::BigEndian)
    }

    /// Check one instruction's accesses against `list` over `machine`.
    fn accesses(
        list: &mut BreakpointList,
        cpu: usize,
        found: &[Access],
        machine: &Machine,
    ) -> bool {
        machine.check(|ctx| list.check_accesses(cpu, found, ctx))
    }

    // -- The list -------------------------------------------------------

    #[test]
    fn add_assigns_monotonic_ids() {
        let mut list = BreakpointList::new();
        let a = list.add(exec_bp(0x100, Some(0)));
        let b = list.add(exec_bp(0x200, Some(0)));
        let c = list.add(exec_bp(0x300, Some(0)));
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        assert_eq!(c, 2);
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn ids_stay_stable_after_remove() {
        let mut list = BreakpointList::new();
        let a = list.add(exec_bp(0x100, Some(0)));
        let _b = list.add(exec_bp(0x200, Some(0)));
        list.remove(a);
        let c = list.add(exec_bp(0x300, Some(0)));
        // `c` is a brand-new id, not a reused 0.
        assert_eq!(c, 2);
        let items = list.list();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, 1);
        assert_eq!(items[1].id, 2);
    }

    #[test]
    fn enable_toggles_flag() {
        let mut list = BreakpointList::new();
        let id = list.add(exec_bp(0x100, Some(0)));
        assert!(list.list()[0].enabled);
        list.enable(id, false);
        assert!(!list.list()[0].enabled);
        list.enable(id, true);
        assert!(list.list()[0].enabled);
    }

    // -- Execute breakpoints -------------------------------------------

    #[test]
    fn check_execute_filters_by_cpu_id() {
        let mut list = BreakpointList::new();
        list.add(exec_bp(0x1000, Some(0))); // the main CPU's
        list.add(exec_bp(0x1000, Some(1))); // the sound CPU's
        assert!(list.check_execute(0, 0x1000, None));
        assert!(list.check_execute(1, 0x1000, None));
        assert!(!list.check_execute(0, 0x2000, None), "wrong address");
        assert!(!list.check_execute(2, 0x1000, None), "a CPU with none");
    }

    #[test]
    fn check_execute_wildcard_cpu_fires_for_all() {
        let mut list = BreakpointList::new();
        list.add(exec_bp(0x1000, None));
        assert!(list.check_execute(0, 0x1000, None));
        assert!(list.check_execute(1, 0x1000, None));
        assert!(list.check_execute(42, 0x1000, None));
    }

    #[test]
    fn check_execute_skips_disabled() {
        let mut list = BreakpointList::new();
        let id = list.add(exec_bp(0x1000, Some(0)));
        list.enable(id, false);
        assert!(!list.check_execute(0, 0x1000, None));
    }

    #[test]
    fn check_execute_increments_hit_count() {
        let mut list = BreakpointList::new();
        let id = list.add(exec_bp(0x1000, Some(0)));
        list.check_execute(0, 0x1000, None);
        list.check_execute(0, 0x1000, None);
        list.check_execute(0, 0x2000, None); // miss
        assert_eq!(hit_count(&list, id), 2);
    }

    // -- Watchpoints ----------------------------------------------------

    #[test]
    fn check_accesses_respects_the_direction_watched() {
        let mut list = BreakpointList::new();
        list.add(mem_bp(0x1000, BreakpointType::write(), Some(0)));
        list.add(mem_bp(0x2000, BreakpointType::read(), Some(0)));
        list.add(mem_bp(0x3000, BreakpointType::read_write(), Some(0)));
        let machine = empty();

        assert!(accesses(&mut list, 0, &[write(0x1000, 0)], &machine));
        assert!(!accesses(&mut list, 0, &[read(0x1000, 0)], &machine));

        assert!(accesses(&mut list, 0, &[read(0x2000, 0)], &machine));
        assert!(!accesses(&mut list, 0, &[write(0x2000, 0)], &machine));

        assert!(accesses(&mut list, 0, &[read(0x3000, 0)], &machine));
        assert!(accesses(&mut list, 0, &[write(0x3000, 0)], &machine));
    }

    #[test]
    fn combined_access_flags_fire_for_exec_and_memory() {
        // A single breakpoint that watches execute AND write, the kind
        // the per-row R/W/X checkboxes produce.
        let mut list = BreakpointList::new();
        list.add(mem_bp(
            0x1000,
            BreakpointType::Access(AccessFlags::new(true, false, true)),
            Some(0),
        ));
        let machine = empty();
        assert!(list.check_execute(0, 0x1000, None));
        assert!(accesses(&mut list, 0, &[write(0x1000, 0)], &machine));
        assert!(!accesses(&mut list, 0, &[read(0x1000, 0)], &machine));
    }

    #[test]
    fn set_access_mutates_flags_in_place_keeping_hit_count() {
        let mut list = BreakpointList::new();
        let id = list.add(exec_bp(0x1000, Some(0)));
        // Accrue a hit so we can prove it survives the type change.
        assert!(list.check_execute(0, 0x1000, None));
        // Flip it to a write watchpoint.
        list.set_access(id, AccessFlags::WRITE);
        assert!(!list.check_execute(0, 0x1000, None));
        assert!(accesses(&mut list, 0, &[write(0x1000, 0)], &empty()));
        let item = list.list().into_iter().find(|b| b.id == id).unwrap();
        assert_eq!(item.breakpoint.bp_type, BreakpointType::write());
        // Two matches total: one execute (before), one write (after).
        assert_eq!(item.hit_count, 2);
    }

    #[test]
    fn set_access_ignores_event_breakpoints() {
        let mut list = BreakpointList::new();
        let id = list.add(event_bp(7, 3));
        list.set_access(id, AccessFlags::WRITE);
        let item = list.list().into_iter().find(|b| b.id == id).unwrap();
        assert_eq!(
            item.breakpoint.bp_type,
            BreakpointType::Event { code: 7, param: 3 }
        );
    }

    #[test]
    fn check_accesses_ignores_execute_breakpoints() {
        let mut list = BreakpointList::new();
        list.add(exec_bp(0x1000, Some(0)));
        let machine = empty();
        assert!(!accesses(&mut list, 0, &[write(0x1000, 0)], &machine));
        assert!(!accesses(&mut list, 0, &[read(0x1000, 0)], &machine));
    }

    #[test]
    fn check_accesses_ignores_the_wrong_direction_and_cpu() {
        let mut list = BreakpointList::new();
        list.add(mem_bp(0x1000, BreakpointType::write(), Some(0)));
        let machine = empty();
        // A read access never trips a write-only watch.
        assert!(!accesses(&mut list, 0, &[read(0x1000, 0)], &machine));
        // The right access on the wrong CPU is filtered out.
        assert!(!accesses(&mut list, 1, &[write(0x1000, 0)], &machine));
        // No access at all fires nothing.
        assert!(!accesses(&mut list, 0, &[], &machine));
    }

    #[test]
    fn a_watch_counts_one_hit_per_instruction() {
        // A ranged watch whose bytes are all written in one instruction,
        // and a byte watch read and written in it: one hit each.
        let mut list = BreakpointList::new();
        let ranged = list.add_with_spec(
            mem_bp(0x1000, BreakpointType::write(), Some(0)),
            MatchSpec {
                address_end: Some(0x1003),
                ..Default::default()
            },
        );
        let byte = list.add(mem_bp(0x2000, BreakpointType::read_write(), Some(0)));
        let instruction = [
            write(0x1000, 1),
            write(0x1001, 2),
            write(0x1002, 3),
            write(0x1003, 4),
            read(0x2000, 5),
            write(0x2000, 6),
        ];
        assert!(accesses(&mut list, 0, &instruction, &empty()));
        assert_eq!(hit_count(&list, ranged), 1);
        assert_eq!(hit_count(&list, byte), 1);
        assert!(accesses(&mut list, 0, &instruction, &empty()));
        assert_eq!(
            hit_count(&list, ranged),
            2,
            "the next instruction counts again"
        );
    }

    #[test]
    fn a_watch_with_no_value_fires_without_reading_memory() {
        let mut list = BreakpointList::new();
        let id = list.add(mem_bp(0x1000, BreakpointType::write(), Some(0)));
        // No value predicate: memory is never read.
        let mut ctx = CpuExprContext::new(
            |_| None,
            |_| panic!("a watch with no value must not read memory"),
            ByteOrder::BigEndian,
        );
        assert!(list.check_accesses(0, &[write(0x1000, 0xAB)], &mut ctx));
        assert_eq!(hit_count(&list, id), 1);
    }

    // -- Events ---------------------------------------------------------

    #[test]
    fn check_event_matches_code_and_param() {
        let mut list = BreakpointList::new();
        list.add(event_bp(7, 3));
        assert!(list.check_event(7, 3));
        // Right code, wrong param.
        assert!(!list.check_event(7, 4));
        // Wrong code.
        assert!(!list.check_event(8, 3));
    }

    #[test]
    fn check_event_param_any_is_wildcard() {
        let mut list = BreakpointList::new();
        list.add(event_bp(7, BREAKPOINT_PARAM_ANY));
        assert!(list.check_event(7, 0));
        assert!(list.check_event(7, 999));
        assert!(!list.check_event(8, 0));
    }

    #[test]
    fn check_event_skips_disabled_and_bumps_hit_count() {
        let mut list = BreakpointList::new();
        let id = list.add(event_bp(2, 1));
        list.check_event(2, 1);
        list.check_event(2, 1);
        list.check_event(2, 9); // miss
        assert_eq!(hit_count(&list, id), 2);
        list.enable(id, false);
        assert!(!list.check_event(2, 1));
    }

    #[test]
    fn check_event_ignores_address_breakpoints() {
        let mut list = BreakpointList::new();
        list.add(exec_bp(0x1000, Some(0)));
        list.add(mem_bp(0x2000, BreakpointType::write(), Some(0)));
        assert!(!list.check_event(0, 0));
    }

    #[test]
    fn has_watchpoints_or_events_tracks_non_execute() {
        let mut list = BreakpointList::new();
        assert!(!list.has_watchpoints_or_events());
        let exec = list.add(exec_bp(0x1000, Some(0)));
        assert!(!list.has_watchpoints_or_events());
        let watch = list.add(mem_bp(0x2000, BreakpointType::write(), Some(0)));
        assert!(list.has_watchpoints_or_events());
        list.enable(watch, false);
        assert!(!list.has_watchpoints_or_events());
        let _evt = list.add(event_bp(1, 1));
        assert!(list.has_watchpoints_or_events());
        // Removing the execute one doesn't change the answer.
        list.remove(exec);
        assert!(list.has_watchpoints_or_events());
    }

    #[test]
    fn the_enabled_events_are_listed_in_order() {
        let mut list = BreakpointList::new();
        list.add(event_bp(6, 1));
        list.add(exec_bp(0x1000, Some(0)));
        let off = list.add(event_bp(8, 5));
        list.add(event_bp(6, BREAKPOINT_PARAM_ANY));
        list.enable(off, false);
        assert_eq!(
            list.enabled_events().collect::<Vec<_>>(),
            [(6, 1), (6, BREAKPOINT_PARAM_ANY)]
        );
    }

    #[test]
    fn the_watched_params_are_those_of_the_enabled_breakpoints_on_a_code() {
        let mut list = BreakpointList::new();
        assert_eq!(list.watched_params::<1>(6), None);
        list.add(event_bp(6, 1));
        list.add(event_bp(6, 63));
        list.add(event_bp(6, 64)); // past a one-word set
        list.add(event_bp(7, 2)); // another code
        let off = list.add(event_bp(6, 9));
        list.enable(off, false);
        let colours = list.watched_params::<1>(6).expect("two colours watched");
        assert!(colours.contains(1) && colours.contains(63));
        assert!(!colours.contains(2) && !colours.contains(9));
        let wider = list.watched_params::<2>(6).unwrap();
        assert!(wider.contains(64), "a wider set holds it");
        assert_eq!(list.watched_params::<1>(8), None);

        let mut only_past = BreakpointList::new();
        only_past.add(event_bp(6, 200));
        assert_eq!(
            only_past.watched_params::<1>(6),
            None,
            "a set holding nothing is None"
        );

        let mut any = BreakpointList::new();
        any.add(event_bp(8, BREAKPOINT_PARAM_ANY));
        let tiles = any.watched_params::<32>(8).unwrap();
        assert!(tiles.contains(0) && tiles.contains(2047));
    }

    // -- Advanced match conditions (MatchSpec) ---------------------------

    #[test]
    fn address_range_matches_inclusive_span_for_execute() {
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            exec_bp(0x1000, Some(0)),
            MatchSpec {
                address_end: Some(0x1008),
                ..Default::default()
            },
        );
        assert!(list.check_execute(0, 0x1000, None)); // low edge
        assert!(list.check_execute(0, 0x1004, None)); // inside
        assert!(list.check_execute(0, 0x1008, None)); // high edge
        assert!(!list.check_execute(0, 0x1009, None)); // just past
        assert!(!list.check_execute(0, 0x0FFF, None)); // just before
        assert_eq!(hit_count(&list, id), 3);
    }

    #[test]
    fn address_range_matches_inclusive_span_for_memory() {
        let mut list = BreakpointList::new();
        list.add_with_spec(
            mem_bp(0xFF0000, BreakpointType::write(), Some(0)),
            MatchSpec {
                address_end: Some(0xFF000F),
                ..Default::default()
            },
        );
        let machine = empty();
        assert!(accesses(&mut list, 0, &[write(0xFF0000, 0)], &machine));
        assert!(accesses(&mut list, 0, &[write(0xFF000F, 0)], &machine));
        assert!(!accesses(&mut list, 0, &[write(0xFF0010, 0)], &machine));
        // Read on a write-only range still doesn't fire.
        assert!(!accesses(&mut list, 0, &[read(0xFF0008, 0)], &machine));
    }

    #[test]
    fn inverted_range_degrades_to_single_address() {
        let mut list = BreakpointList::new();
        list.add_with_spec(
            exec_bp(0x2000, Some(0)),
            MatchSpec {
                address_end: Some(0x1000), // end < base: ignored
                ..Default::default()
            },
        );
        assert!(list.check_execute(0, 0x2000, None));
        assert!(!list.check_execute(0, 0x1800, None));
        assert!(!list.check_execute(0, 0x1000, None));
    }

    #[test]
    fn value_predicate_eq_only_fires_on_match() {
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            mem_bp(0xFF0000, BreakpointType::write(), Some(0)),
            MatchSpec {
                value: Some(0x42),
                ..Default::default()
            },
        );
        let machine = empty();
        // Wrong value: no fire.
        assert!(!accesses(&mut list, 0, &[write(0xFF0000, 0x41)], &machine));
        // The right value read, on a write watch: no fire.
        assert!(!accesses(&mut list, 0, &[read(0xFF0000, 0x42)], &machine));
        // Right value: fire.
        assert!(accesses(&mut list, 0, &[write(0xFF0000, 0x42)], &machine));
        assert_eq!(hit_count(&list, id), 1);
    }

    #[test]
    fn value_predicate_honours_compare_operators() {
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            mem_bp(0x10, BreakpointType::read(), Some(0)),
            MatchSpec {
                value: Some(0x80),
                value_compare: crate::types::ValueCompare::Ge,
                ..Default::default()
            },
        );
        let machine = empty();
        assert!(!accesses(&mut list, 0, &[read(0x10, 0x7F)], &machine));
        assert!(accesses(&mut list, 0, &[read(0x10, 0x80)], &machine));
        assert!(accesses(&mut list, 0, &[read(0x10, 0xFF)], &machine));
        assert_eq!(hit_count(&list, id), 2);
    }

    #[test]
    fn value_predicate_applies_mask_before_compare() {
        let mut list = BreakpointList::new();
        list.add_with_spec(
            mem_bp(0x10, BreakpointType::write(), Some(0)),
            MatchSpec {
                value: Some(0x80),
                value_mask: 0xF0,
                ..Default::default()
            },
        );
        let machine = empty();
        // 0x8F & 0xF0 == 0x80 -> match; 0x70 & 0xF0 == 0x70 -> miss.
        assert!(accesses(&mut list, 0, &[write(0x10, 0x8F)], &machine));
        assert!(!accesses(&mut list, 0, &[write(0x10, 0x70)], &machine));
    }

    #[test]
    fn hit_skip_ignores_first_n_qualifying_matches() {
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            exec_bp(0x1000, Some(0)),
            MatchSpec {
                hit_skip: 2,
                ..Default::default()
            },
        );
        assert!(!list.check_execute(0, 0x1000, None)); // skip 1
        assert!(!list.check_execute(0, 0x1000, None)); // skip 2
        assert!(list.check_execute(0, 0x1000, None)); // fires
        assert!(list.check_execute(0, 0x1000, None)); // keeps firing
        assert_eq!(hit_count(&list, id), 2);
    }

    #[test]
    fn hit_skip_only_counts_value_qualifying_matches() {
        let mut list = BreakpointList::new();
        list.add_with_spec(
            mem_bp(0x10, BreakpointType::write(), Some(0)),
            MatchSpec {
                value: Some(0x99),
                hit_skip: 1,
                ..Default::default()
            },
        );
        let machine = empty();
        // Non-matching values must not consume the skip budget.
        assert!(!accesses(&mut list, 0, &[write(0x10, 0x00)], &machine));
        assert!(!accesses(&mut list, 0, &[write(0x10, 0x00)], &machine));
        // First value match -> consumed by skip; second -> fires.
        assert!(!accesses(&mut list, 0, &[write(0x10, 0x99)], &machine));
        assert!(accesses(&mut list, 0, &[write(0x10, 0x99)], &machine));
    }

    #[test]
    fn one_shot_disables_after_first_fire() {
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            exec_bp(0x1000, Some(0)),
            MatchSpec {
                one_shot: true,
                ..Default::default()
            },
        );
        assert!(list.check_execute(0, 0x1000, None));
        // Auto-disabled: no further fires, and the list reflects it.
        assert!(!list.check_execute(0, 0x1000, None));
        assert!(
            !list
                .list()
                .into_iter()
                .find(|b| b.id == id)
                .unwrap()
                .enabled
        );
        assert_eq!(hit_count(&list, id), 1);
    }

    #[test]
    fn log_action_counts_without_halting() {
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            exec_bp(0x1000, Some(0)),
            MatchSpec {
                action: BreakpointAction::Log,
                ..Default::default()
            },
        );
        // Never halts...
        assert!(!list.check_execute(0, 0x1000, None));
        assert!(!list.check_execute(0, 0x1000, None));
        // ...but still counts each hit.
        assert_eq!(hit_count(&list, id), 2);
    }

    #[test]
    fn log_point_does_not_mask_a_real_breakpoint_at_same_address() {
        let mut list = BreakpointList::new();
        list.add_with_spec(
            exec_bp(0x1000, Some(0)),
            MatchSpec {
                action: BreakpointAction::Log,
                ..Default::default()
            },
        );
        let brk = list.add(exec_bp(0x1000, Some(0)));
        // The log point counts, the break point halts.
        assert!(list.check_execute(0, 0x1000, None));
        assert_eq!(hit_count(&list, brk), 1);
    }

    #[test]
    fn set_match_spec_updates_in_place_and_resets_skip_window() {
        let mut list = BreakpointList::new();
        let id = list.add(exec_bp(0x1000, Some(0)));
        // Accrue a hit, then attach a skip-1 spec.
        assert!(list.check_execute(0, 0x1000, None));
        list.set_match_spec(
            id,
            MatchSpec {
                hit_skip: 1,
                ..Default::default()
            },
        );
        // The skip window restarts: first post-spec match is skipped.
        assert!(!list.check_execute(0, 0x1000, None));
        assert!(list.check_execute(0, 0x1000, None));
        // hit_count survived the spec change (1 before + 1 after).
        assert_eq!(hit_count(&list, id), 2);
        let item = list.list().into_iter().find(|b| b.id == id).unwrap();
        assert_eq!(item.match_spec.hit_skip, 1);
    }

    #[test]
    fn set_match_spec_ignores_unknown_id() {
        let mut list = BreakpointList::new();
        list.set_match_spec(999, MatchSpec::default());
        assert!(list.is_empty());
    }

    #[test]
    fn event_breakpoints_honour_hit_skip_and_one_shot() {
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            event_bp(5, BREAKPOINT_PARAM_ANY),
            MatchSpec {
                hit_skip: 1,
                one_shot: true,
                ..Default::default()
            },
        );
        assert!(!list.check_event(5, 0)); // skipped
        assert!(list.check_event(5, 7)); // fires once, then one-shot
        assert!(!list.check_event(5, 9)); // disabled
        assert_eq!(hit_count(&list, id), 1);
    }

    #[test]
    fn value_predicate_inert_on_execute_breakpoints() {
        let mut list = BreakpointList::new();
        list.add_with_spec(
            exec_bp(0x1000, Some(0)),
            MatchSpec {
                value: Some(0x42),
                ..Default::default()
            },
        );
        // Execute carries no observed value, so the value-conditioned
        // breakpoint can never fire, context or not.
        assert!(!list.check_execute(0, 0x1000, None));
        assert!(!empty().check(|ctx| list.check_execute(0, 0x1000, Some(ctx))));
    }

    // -- Multi-byte (sized) value watchpoints -------------------------

    /// 12 34 56 78 at 0x1000.
    fn bytes_at_1000(order: ByteOrder) -> Machine {
        Machine::new(order).with_bytes(0x1000, &[0x12, 0x34, 0x56, 0x78])
    }

    fn sized_watch(list: &mut BreakpointList, value: u32, value_size: u8) -> BreakpointId {
        list.add_with_spec(
            mem_bp(0x1000, BreakpointType::write(), Some(0)),
            MatchSpec {
                value: Some(value),
                value_size,
                ..Default::default()
            },
        )
    }

    #[test]
    fn sized_watch_covers_consecutive_bytes() {
        let mut list = BreakpointList::new();
        sized_watch(&mut list, 0x1234_5678, 4);
        let machine = bytes_at_1000(ByteOrder::BigEndian);
        // A write to any byte of 0x1000..=0x1003 is a candidate and the
        // whole 32-bit value matches.
        assert!(accesses(&mut list, 0, &[write(0x1003, 0x78)], &machine));
        // The byte just past the span is not covered.
        assert!(!accesses(&mut list, 0, &[write(0x1004, 0x00)], &machine));
    }

    #[test]
    fn sized_watch_reads_a_big_endian_value() {
        let mut list = BreakpointList::new();
        sized_watch(&mut list, 0x1234_5678, 4);
        let machine = bytes_at_1000(ByteOrder::BigEndian);
        assert!(accesses(&mut list, 0, &[write(0x1000, 0x12)], &machine));

        // Wrong target: no fire even though a covered byte was written.
        let mut other = BreakpointList::new();
        sized_watch(&mut other, 0xDEAD_BEEF, 4);
        assert!(!accesses(&mut other, 0, &[write(0x1000, 0x12)], &machine));
    }

    #[test]
    fn sized_watch_reads_a_little_endian_value() {
        let mut list = BreakpointList::new();
        // Little-endian read of 12 34 56 78 -> 0x78563412.
        sized_watch(&mut list, 0x7856_3412, 4);
        let machine = bytes_at_1000(ByteOrder::LittleEndian);
        assert!(accesses(&mut list, 0, &[write(0x1002, 0x56)], &machine));
    }

    #[test]
    fn a_16_bit_watch_fires_on_the_whole_value() {
        // 0x3412 little-endian at 0x1000 (a value above 255).
        let mut list = BreakpointList::new();
        let id = sized_watch(&mut list, 0x3412, 2);
        let machine = bytes_at_1000(ByteOrder::LittleEndian);
        let instruction = [write(0x1000, 0x12), write(0x1001, 0x34)];
        assert!(accesses(&mut list, 0, &instruction, &machine));
        assert_eq!(hit_count(&list, id), 1, "one hit for the two bytes");
        // Its low byte alone does not fire a watch on the whole value.
        let mut low_byte = BreakpointList::new();
        sized_watch(&mut low_byte, 0x12, 2);
        assert!(!accesses(&mut low_byte, 0, &instruction, &machine));
    }

    #[test]
    fn a_24_bit_watch_fires_on_the_whole_value() {
        // 24-bit big-endian read of 12 34 56 -> 0x123456.
        let mut list = BreakpointList::new();
        sized_watch(&mut list, 0x12_3456, 3);
        let big = bytes_at_1000(ByteOrder::BigEndian);
        assert!(accesses(&mut list, 0, &[write(0x1002, 0x56)], &big));
        // 0x1003 is outside the 3-byte span.
        assert!(!accesses(&mut list, 0, &[write(0x1003, 0x78)], &big));

        // The same bytes little-endian are 0x563412.
        let mut little_list = BreakpointList::new();
        sized_watch(&mut little_list, 0x56_3412, 3);
        let little = bytes_at_1000(ByteOrder::LittleEndian);
        assert!(accesses(
            &mut little_list,
            0,
            &[write(0x1001, 0x34)],
            &little
        ));
        assert!(!accesses(&mut list, 0, &[write(0x1001, 0x34)], &little));
    }

    #[test]
    fn sized_watch_width_one_uses_accessed_byte_not_memory() {
        let mut list = BreakpointList::new();
        list.add_with_spec(
            mem_bp(0x2000, BreakpointType::write(), Some(0)),
            MatchSpec {
                value: Some(0x42),
                value_size: 1,
                ..Default::default()
            },
        );
        // Memory must never be read for a one-byte watch: the read panics,
        // proving the accessed byte is used directly.
        let mut ctx = CpuExprContext::new(
            |_| None,
            |_| panic!("a one-byte watch must not read memory"),
            ByteOrder::BigEndian,
        );
        assert!(list.check_accesses(0, &[write(0x2000, 0x42)], &mut ctx));
    }

    #[test]
    fn batch_fires_each_watch_once_per_instruction() {
        // A 32-bit watch whose four covered bytes are all written in one
        // instruction must count a single hit, not four.
        let mut list = BreakpointList::new();
        let id = sized_watch(&mut list, 0x1234_5678, 4);
        let instruction = [
            write(0x1000, 0x12),
            write(0x1001, 0x34),
            write(0x1002, 0x56),
            write(0x1003, 0x78),
        ];
        assert!(accesses(
            &mut list,
            0,
            &instruction,
            &bytes_at_1000(ByteOrder::BigEndian)
        ));
        assert_eq!(hit_count(&list, id), 1);
    }

    #[test]
    fn value_width_clamps_to_one_through_four() {
        let width = |value_size| {
            MatchSpec {
                value_size,
                ..Default::default()
            }
            .value_width()
        };
        assert_eq!(width(0), 1);
        assert_eq!(width(1), 1);
        assert_eq!(width(3), 3);
        assert_eq!(width(9), 4);
    }

    // -- Conditions -----------------------------------------------------

    #[test]
    fn expr_execute_fires_only_when_condition_holds() {
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            exec_bp(0x1000, Some(0)),
            MatchSpec {
                expr: Some("d0 == 5".into()),
                ..Default::default()
            },
        );
        let yes = empty().with_registers(&[("d0", 5)]);
        let no = empty().with_registers(&[("d0", 4)]);
        // Condition false -> no fire, no hit counted.
        assert!(!no.check(|ctx| list.check_execute(0, 0x1000, Some(ctx))));
        assert_eq!(hit_count(&list, id), 0);
        // Condition true -> fire.
        assert!(yes.check(|ctx| list.check_execute(0, 0x1000, Some(ctx))));
        assert_eq!(hit_count(&list, id), 1);
    }

    #[test]
    fn expr_execute_fails_closed_without_context() {
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            exec_bp(0x1000, Some(0)),
            MatchSpec {
                expr: Some("d0 == 5".into()),
                ..Default::default()
            },
        );
        // With no context the condition can't be evaluated, so the
        // breakpoint stays silent.
        assert!(!list.check_execute(0, 0x1000, None));
        assert_eq!(hit_count(&list, id), 0);
    }

    #[test]
    fn expr_unparseable_fails_closed() {
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            exec_bp(0x1000, Some(0)),
            MatchSpec {
                expr: Some("d0 ==".into()), // syntax error
                ..Default::default()
            },
        );
        let machine = empty().with_registers(&[("d0", 5)]);
        assert!(!machine.check(|ctx| list.check_execute(0, 0x1000, Some(ctx))));
        assert_eq!(hit_count(&list, id), 0);
    }

    #[test]
    fn expr_blank_string_imposes_no_condition() {
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            exec_bp(0x1000, Some(0)),
            MatchSpec {
                expr: Some("   ".into()), // whitespace-only -> no constraint
                ..Default::default()
            },
        );
        // Fires even with no context because the blank expression is
        // treated as absent.
        assert!(list.check_execute(0, 0x1000, None));
        assert_eq!(hit_count(&list, id), 1);
    }

    #[test]
    fn expr_watch_combines_with_value_predicate() {
        // A write watch that only fires when the stored word is 0x1234 AND
        // a1 points above 0xFF0000. The context supplies the whole value
        // (0x1234) and the register `a1`.
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            mem_bp(0x1000, BreakpointType::write(), Some(0)),
            MatchSpec {
                value: Some(0x1234),
                value_size: 2,
                expr: Some("a1 > 0xFF0000".into()),
                ..Default::default()
            },
        );
        let instruction = [write(0x1000, 0x12), write(0x1001, 0x34)];
        // Value matches but expression is false -> no fire.
        let low = empty()
            .with_registers(&[("a1", 0x100)])
            .with_bytes(0x1000, &[0x12, 0x34]);
        assert!(!accesses(&mut list, 0, &instruction, &low));
        assert_eq!(hit_count(&list, id), 0);
        // Value matches and expression holds -> fire once.
        let high = empty()
            .with_registers(&[("a1", 0xFF_8000)])
            .with_bytes(0x1000, &[0x12, 0x34]);
        assert!(accesses(&mut list, 0, &instruction, &high));
        assert_eq!(hit_count(&list, id), 1);
    }

    #[test]
    fn a_watch_condition_reads_memory_through_the_context() {
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            mem_bp(0x10, BreakpointType::write(), Some(0)),
            MatchSpec {
                expr: Some("[0x20].w == 0x0102".into()),
                ..Default::default()
            },
        );
        let wrong = Machine::new(ByteOrder::LittleEndian).with_bytes(0x20, &[0x01, 0x02]);
        assert!(!accesses(&mut list, 0, &[write(0x10, 0)], &wrong));
        let right = Machine::new(ByteOrder::LittleEndian).with_bytes(0x20, &[0x02, 0x01]);
        assert!(accesses(&mut list, 0, &[write(0x10, 0)], &right));
        assert_eq!(hit_count(&list, id), 1);
    }

    #[test]
    fn expr_watch_fails_closed_on_a_register_the_cpu_has_not() {
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            mem_bp(0x1000, BreakpointType::write(), Some(0)),
            MatchSpec {
                expr: Some("d0 != 0".into()),
                ..Default::default()
            },
        );
        // The context names no `d0`: the condition can't be verified, so
        // the watch stays silent.
        assert!(!accesses(&mut list, 0, &[write(0x1000, 0xAB)], &empty()));
        assert_eq!(hit_count(&list, id), 0);
    }
}
