//! Breakpoint container shared across console cores.
//!
//! Each core (Genesis, SNES, ...) owns one [`BreakpointList`] inside its
//! debugger. The list stores execute / read / write / read+write
//! breakpoints and exposes the hot-loop check methods the CPU step path
//! calls per instruction:
//!
//! - [`BreakpointList::check_execute`]: at every fetch boundary.
//! - [`BreakpointList::check_memory_access`]: at every bus read/write
//!   (wire in via [`BusWatchpointHook`] from the bus impl).
//!
//! Both checks **filter on `cpu_id`** so a breakpoint set on the host
//! CPU (e.g. 68K, address space 0-0xFFFFFF) never spuriously fires for
//! the sub-CPU (e.g. Z80, address space 0-0xFFFF) at an overlapping
//! address. Breakpoints with `cpu_id == None` apply to all CPUs and are
//! intended for system-wide hooks.
//!
//! Watchpoint wiring on the bus is opt-in via the [`BusWatchpointHook`]
//! trait. SG1 and the SNES core both use execute breakpoints, read/write
//! watchpoints and the data-carrying [`BreakpointType::Event`] hooks via
//! [`BreakpointList::check_event`].

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
    /// execution-tier selector to pick the fast path without any debug
    /// overhead.
    pub fn is_empty(&self) -> bool {
        self.breakpoints.is_empty()
    }

    /// Number of breakpoints currently registered.
    pub fn len(&self) -> usize {
        self.breakpoints.len()
    }

    /// Hot-path: returns `true` (halt) when an enabled execute
    /// breakpoint matches `(cpu_id, pc)` and its [`MatchSpec`] conditions
    /// fire. A breakpoint with `cpu_id: None` matches every CPU; an
    /// `address_end` range matches any pc in `[address, address_end]`.
    /// Execute breakpoints carry no observed value, so a value predicate
    /// on one never fires.
    ///
    /// This is the value-/expression-agnostic entry. A breakpoint carrying
    /// an `expr` condition cannot fire here (no evaluation context) and
    /// stays silent; route conditional breakpoints through
    /// [`Self::check_execute_ctx`].
    pub fn check_execute(&mut self, cpu_id: usize, pc: u32) -> bool {
        self.check_execute_ctx(cpu_id, pc, None)
    }

    /// Hot-path: like [`Self::check_execute`] but supplies an optional
    /// [`ExprContext`] so a conditional execute breakpoint (`MatchSpec.expr`)
    /// can be evaluated against live registers / memory. With `ctx: None`
    /// this behaves exactly like [`Self::check_execute`] (expression
    /// breakpoints fail closed). The context is only consulted after the
    /// pc/cpu match holds, so the common (no-expression) case pays nothing
    /// beyond a never-taken branch.
    pub fn check_execute_ctx(
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

    /// Hot-path: value-agnostic memory watchpoint check. Matches when an
    /// enabled read/write/read+write breakpoint covers `(cpu_id, addr,
    /// write)`. A breakpoint with a value predicate set cannot fire here
    /// (no observed byte is available), use
    /// [`Self::check_memory_access_valued`] for those.
    ///
    /// Wire this in from the bus via [`BusWatchpointHook`] (or call it
    /// directly).
    pub fn check_memory_access(&mut self, cpu_id: usize, addr: u32, write: bool) -> bool {
        self.check_memory_access_inner(cpu_id, addr, write, None)
    }

    /// Hot-path: value-aware memory watchpoint check. Like
    /// [`Self::check_memory_access`] but supplies the observed byte so
    /// single-byte value-matched watchpoints (`(observed & mask) <op>
    /// target`) can fire. Multi-byte watchpoints (`value_size > 1`) can't
    /// assemble their full value here, drive those through
    /// [`Self::check_memory_access_batch`], which reads the consecutive
    /// bytes from memory. Cores that can cheaply recover the accessed byte
    /// call this for single-byte watches.
    pub fn check_memory_access_valued(
        &mut self,
        cpu_id: usize,
        addr: u32,
        write: bool,
        value: u32,
    ) -> bool {
        self.check_memory_access_inner(cpu_id, addr, write, Some(value))
    }

    /// Batch watchpoint check for every byte access recorded during one
    /// instruction. Each enabled read/write watchpoint fires **at most
    /// once** here (on the first covered access matching its direction),
    /// so a multi-byte or ranged watch counts a single hit per instruction
    /// even when several of its covered bytes were touched. A multi-byte
    /// watch (`value_size > 1`) assembles its observed value from
    /// `value_size` consecutive bytes starting at the watch's base address
    /// via `read_byte` (a side-effect-free peek) in `big_endian` order;
    /// single-byte watches use the recorded access byte. Returns whether
    /// any watchpoint halts.
    ///
    /// `accesses` is a slice of `(addr, is_write, byte)` tuples, the
    /// per-byte expansion of the instruction's bus traffic.
    pub fn check_memory_access_batch<F: FnMut(u32) -> u8>(
        &mut self,
        cpu_id: usize,
        accesses: &[(u32, bool, u8)],
        big_endian: bool,
        mut read_byte: F,
    ) -> bool {
        let mut should_break = false;
        for bp in &mut self.breakpoints {
            if !bp.enabled {
                continue;
            }
            let Some(flags) = bp.breakpoint.bp_type.access() else {
                continue;
            };
            if !matches_cpu(bp.breakpoint.cpu_id, cpu_id) {
                continue;
            }
            // The first covered access whose direction this watch cares
            // about; `None` means this watch saw no relevant traffic.
            let touched = accesses.iter().copied().find(|&(addr, write, _)| {
                addr_matches(bp, addr) && if write { flags.write } else { flags.read }
            });
            let Some((_, _, byte)) = touched else {
                continue;
            };
            let observed = if bp.match_spec.needs_value() {
                let width = bp.match_spec.value_width();
                if width > 1 {
                    Some(assemble_value(
                        bp.breakpoint.address,
                        width,
                        big_endian,
                        &mut read_byte,
                    ))
                } else {
                    Some(byte as u32)
                }
            } else {
                None
            };
            // No evaluation context on this path: an expression-bearing
            // watch fails closed (use `check_memory_access_batch_ctx`).
            should_break |= record_and_should_break(bp, observed, None);
        }
        should_break
    }

    /// Like [`Self::check_memory_access_batch`] but evaluates everything,
    /// the multi-byte value assembly *and* any `MatchSpec.expr` condition,
    /// through a single [`ExprContext`]. Folding both into the context
    /// sidesteps the borrow conflict a host would hit otherwise (the value
    /// peek and the expression evaluator both need the live memory bus).
    /// The context's [`ExprContext::read_mem`] supplies the observed value
    /// for sized watches (honouring the core's endianness), so this takes
    /// no separate `big_endian` flag or peek closure.
    pub fn check_memory_access_batch_ctx(
        &mut self,
        cpu_id: usize,
        accesses: &[(u32, bool, u8)],
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
            if !matches_cpu(bp.breakpoint.cpu_id, cpu_id) {
                continue;
            }
            let touched = accesses.iter().copied().find(|&(addr, write, _)| {
                addr_matches(bp, addr) && if write { flags.write } else { flags.read }
            });
            let Some((_, _, byte)) = touched else {
                continue;
            };
            let observed = if bp.match_spec.needs_value() {
                let width = bp.match_spec.value_width();
                if width > 1 {
                    // `read_mem` returns the value already assembled in the
                    // core's byte order, masked to `width` bytes.
                    Some(ctx.read_mem(bp.breakpoint.address, width) as u32)
                } else {
                    Some(byte as u32)
                }
            } else {
                None
            };
            should_break |= record_and_should_break(bp, observed, Some(&mut *ctx));
        }
        should_break
    }

    fn check_memory_access_inner(
        &mut self,
        cpu_id: usize,
        addr: u32,
        write: bool,
        value: Option<u32>,
    ) -> bool {
        let mut should_break = false;
        for bp in &mut self.breakpoints {
            if !bp.enabled {
                continue;
            }
            let Some(flags) = bp.breakpoint.bp_type.access() else {
                continue;
            };
            if !matches_cpu(bp.breakpoint.cpu_id, cpu_id) {
                continue;
            }
            if !addr_matches(bp, addr) {
                continue;
            }
            let matched = if write { flags.write } else { flags.read };
            if !matched {
                continue;
            }
            // The single-byte access path carries no evaluation context;
            // an expression-bearing watch fails closed here (it fires via
            // the batch path, which threads an `ExprContext`).
            should_break |= record_and_should_break(bp, value, None);
        }
        should_break
    }

    /// Hot-path (only when armed): returns `true` (halt) when an enabled
    /// [`BreakpointType::Event`] breakpoint matches `code` and either
    /// matches `param` exactly or was stored with [`BREAKPOINT_PARAM_ANY`],
    /// and its [`MatchSpec`] conditions (hit-skip / action / one-shot)
    /// fire.
    ///
    /// Events are system-wide, so this does **not** filter on `cpu_id`.
    /// The `code`/`param` taxonomy is owned by the calling core; this
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

/// Assemble a `width`-byte (1-4) observed value from memory starting at
/// `base`, reading each byte through `read_byte`. `big_endian` selects
/// the byte order: the first byte is the most-significant byte for
/// big-endian cores (68000) and the least-significant for little-endian
/// cores (65816). Out-of-range widths are clamped by the caller via
/// [`MatchSpec::value_width`].
#[inline]
fn assemble_value<F: FnMut(u32) -> u8>(
    base: u32,
    width: u8,
    big_endian: bool,
    read_byte: &mut F,
) -> u32 {
    let mut acc = 0u32;
    for i in 0..width as u32 {
        let byte = read_byte(base.wrapping_add(i)) as u32;
        if big_endian {
            acc = (acc << 8) | byte;
        } else {
            acc |= byte << (8 * i);
        }
    }
    acc
}

/// Apply a base-matched breakpoint's [`MatchSpec`] conditions and update
/// its runtime counters. Returns whether execution should **halt**.
///
/// The caller has already confirmed the address/access (or event) match.
/// `observed` is the byte seen at the access for value-matched
/// watchpoints, or `None` for execute / event checks (which carry no
/// value). A spec with a value predicate never fires without an observed
/// value, so a value-conditioned watchpoint stays inert on the
/// value-agnostic path.
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
    // Value predicate: only memory accesses provide an observed byte.
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

/// Hook a bus can implement to forward read/write events to a
/// [`BreakpointList`] (or any other watcher).
///
/// Intentionally minimal: just the two events we need. Cores that
/// support N CPUs use `cpu_id` to disambiguate; uni-CPU cores pass `0`.
///
/// **Wiring status**: defined here as the public contract for bus
/// implementations that want a push-style watch hook. Neither core wires
/// it: SG1 and the SNES core record their bus accesses in their own debug
/// buffers and run the [`BreakpointList`] checks from there.
pub trait BusWatchpointHook {
    /// Fired before/after a bus read at `addr`. Implementations decide
    /// the exact semantics; the convention is "after the read resolved
    /// but before the caller observes the returned byte".
    fn on_read(&mut self, cpu_id: usize, addr: u32);

    /// Fired before/after a bus write at `addr`. Implementations decide
    /// the exact semantics; the convention is "after the value has been
    /// committed to the target device".
    fn on_write(&mut self, cpu_id: usize, addr: u32);
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn check_execute_filters_by_cpu_id() {
        let mut list = BreakpointList::new();
        list.add(exec_bp(0x1000, Some(0))); // 68K-only
        list.add(exec_bp(0x1000, Some(1))); // Z80-only
                                            // 68K fires only on cpu 0
        assert!(list.check_execute(0, 0x1000));
        assert!(list.check_execute(1, 0x1000));
        // Wrong address
        assert!(!list.check_execute(0, 0x2000));
    }

    #[test]
    fn check_execute_wildcard_cpu_fires_for_all() {
        let mut list = BreakpointList::new();
        list.add(exec_bp(0x1000, None));
        assert!(list.check_execute(0, 0x1000));
        assert!(list.check_execute(1, 0x1000));
        assert!(list.check_execute(42, 0x1000));
    }

    #[test]
    fn check_execute_skips_disabled() {
        let mut list = BreakpointList::new();
        let id = list.add(exec_bp(0x1000, Some(0)));
        list.enable(id, false);
        assert!(!list.check_execute(0, 0x1000));
    }

    #[test]
    fn check_execute_increments_hit_count() {
        let mut list = BreakpointList::new();
        let id = list.add(exec_bp(0x1000, Some(0)));
        list.check_execute(0, 0x1000);
        list.check_execute(0, 0x1000);
        list.check_execute(0, 0x2000); // miss
        let item = list.list().into_iter().find(|b| b.id == id).unwrap();
        assert_eq!(item.hit_count, 2);
    }

    #[test]
    fn check_memory_access_respects_type() {
        let mut list = BreakpointList::new();
        list.add(mem_bp(0x1000, BreakpointType::write(), Some(0)));
        list.add(mem_bp(0x2000, BreakpointType::read(), Some(0)));
        list.add(mem_bp(0x3000, BreakpointType::read_write(), Some(0)));

        assert!(list.check_memory_access(0, 0x1000, true));
        assert!(!list.check_memory_access(0, 0x1000, false));

        assert!(list.check_memory_access(0, 0x2000, false));
        assert!(!list.check_memory_access(0, 0x2000, true));

        assert!(list.check_memory_access(0, 0x3000, false));
        assert!(list.check_memory_access(0, 0x3000, true));
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
        assert!(list.check_execute(0, 0x1000));
        assert!(list.check_memory_access(0, 0x1000, true));
        assert!(!list.check_memory_access(0, 0x1000, false));
    }

    #[test]
    fn set_access_mutates_flags_in_place_keeping_hit_count() {
        let mut list = BreakpointList::new();
        let id = list.add(exec_bp(0x1000, Some(0)));
        // Accrue a hit so we can prove it survives the type change.
        assert!(list.check_execute(0, 0x1000));
        // Flip it to a write watchpoint.
        list.set_access(id, AccessFlags::WRITE);
        assert!(!list.check_execute(0, 0x1000));
        assert!(list.check_memory_access(0, 0x1000, true));
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
    fn check_memory_access_ignores_execute_breakpoints() {
        let mut list = BreakpointList::new();
        list.add(exec_bp(0x1000, Some(0)));
        assert!(!list.check_memory_access(0, 0x1000, true));
        assert!(!list.check_memory_access(0, 0x1000, false));
    }

    fn event_bp(code: u32, param: u32) -> Breakpoint {
        Breakpoint {
            address: 0,
            bp_type: BreakpointType::Event { code, param },
            cpu_id: None,
            condition: None,
        }
    }

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
        let item = list.list().into_iter().find(|b| b.id == id).unwrap();
        assert_eq!(item.hit_count, 2);
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

    // -- Advanced match conditions (MatchSpec) ---------------------------

    fn hit_count(list: &BreakpointList, id: BreakpointId) -> u32 {
        list.list()
            .into_iter()
            .find(|b| b.id == id)
            .unwrap()
            .hit_count
    }

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
        assert!(list.check_execute(0, 0x1000)); // low edge
        assert!(list.check_execute(0, 0x1004)); // inside
        assert!(list.check_execute(0, 0x1008)); // high edge
        assert!(!list.check_execute(0, 0x1009)); // just past
        assert!(!list.check_execute(0, 0x0FFF)); // just before
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
        assert!(list.check_memory_access(0, 0xFF0000, true));
        assert!(list.check_memory_access(0, 0xFF000F, true));
        assert!(!list.check_memory_access(0, 0xFF0010, true));
        // Read on a write-only range still doesn't fire.
        assert!(!list.check_memory_access(0, 0xFF0008, false));
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
        assert!(list.check_execute(0, 0x2000));
        assert!(!list.check_execute(0, 0x1800));
        assert!(!list.check_execute(0, 0x1000));
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
        // Value-agnostic path can never fire a value-conditioned watch.
        assert!(!list.check_memory_access(0, 0xFF0000, true));
        // Wrong value: no fire.
        assert!(!list.check_memory_access_valued(0, 0xFF0000, true, 0x41));
        // Right value: fire.
        assert!(list.check_memory_access_valued(0, 0xFF0000, true, 0x42));
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
        assert!(!list.check_memory_access_valued(0, 0x10, false, 0x7F));
        assert!(list.check_memory_access_valued(0, 0x10, false, 0x80));
        assert!(list.check_memory_access_valued(0, 0x10, false, 0xFF));
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
        // 0x8F & 0xF0 == 0x80 -> match; 0x70 & 0xF0 == 0x70 -> miss.
        assert!(list.check_memory_access_valued(0, 0x10, true, 0x8F));
        assert!(!list.check_memory_access_valued(0, 0x10, true, 0x70));
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
        assert!(!list.check_execute(0, 0x1000)); // skip 1
        assert!(!list.check_execute(0, 0x1000)); // skip 2
        assert!(list.check_execute(0, 0x1000)); // fires
        assert!(list.check_execute(0, 0x1000)); // keeps firing
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
        // Non-matching values must not consume the skip budget.
        assert!(!list.check_memory_access_valued(0, 0x10, true, 0x00));
        assert!(!list.check_memory_access_valued(0, 0x10, true, 0x00));
        // First value match -> consumed by skip; second -> fires.
        assert!(!list.check_memory_access_valued(0, 0x10, true, 0x99));
        assert!(list.check_memory_access_valued(0, 0x10, true, 0x99));
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
        assert!(list.check_execute(0, 0x1000));
        // Auto-disabled: no further fires, and the list reflects it.
        assert!(!list.check_execute(0, 0x1000));
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
        assert!(!list.check_execute(0, 0x1000));
        assert!(!list.check_execute(0, 0x1000));
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
        assert!(list.check_execute(0, 0x1000));
        assert_eq!(hit_count(&list, brk), 1);
    }

    #[test]
    fn set_match_spec_updates_in_place_and_resets_skip_window() {
        let mut list = BreakpointList::new();
        let id = list.add(exec_bp(0x1000, Some(0)));
        // Accrue a hit, then attach a skip-2 spec.
        assert!(list.check_execute(0, 0x1000));
        list.set_match_spec(
            id,
            MatchSpec {
                hit_skip: 1,
                ..Default::default()
            },
        );
        // The skip window restarts: first post-spec match is skipped.
        assert!(!list.check_execute(0, 0x1000));
        assert!(list.check_execute(0, 0x1000));
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
        // breakpoint can never fire.
        assert!(!list.check_execute(0, 0x1000));
    }

    // -- Multi-byte (sized) value watchpoints -------------------------

    /// A little 16-entry fake memory at base 0x1000, big-endian-ordered
    /// bytes 0x12 0x34 0x56 0x78 ... so a 32-bit big-endian read at
    /// 0x1000 yields 0x12345678.
    fn fake_mem(addr: u32) -> u8 {
        const BYTES: [u8; 4] = [0x12, 0x34, 0x56, 0x78];
        BYTES
            .get((addr.wrapping_sub(0x1000)) as usize)
            .copied()
            .unwrap_or(0)
    }

    #[test]
    fn sized_watch_covers_consecutive_bytes() {
        let mut list = BreakpointList::new();
        list.add_with_spec(
            mem_bp(0x1000, BreakpointType::write(), Some(0)),
            MatchSpec {
                value: Some(0x12345678),
                value_size: 4,
                ..Default::default()
            },
        );
        // A write to any byte of 0x1000..=0x1003 is a candidate and the
        // assembled 32-bit value matches.
        assert!(list.check_memory_access_batch(0, &[(0x1003, true, 0x78)], true, fake_mem));
        // The byte just past the span is not covered.
        assert!(!list.check_memory_access_batch(0, &[(0x1004, true, 0x00)], true, fake_mem));
    }

    #[test]
    fn sized_watch_assembles_big_endian_value() {
        let mut list = BreakpointList::new();
        list.add_with_spec(
            mem_bp(0x1000, BreakpointType::write(), Some(0)),
            MatchSpec {
                value: Some(0x12345678),
                value_size: 4,
                ..Default::default()
            },
        );
        assert!(list.check_memory_access_batch(0, &[(0x1000, true, 0x12)], true, fake_mem));

        // Wrong target: no fire even though a covered byte was written.
        let mut list2 = BreakpointList::new();
        list2.add_with_spec(
            mem_bp(0x1000, BreakpointType::write(), Some(0)),
            MatchSpec {
                value: Some(0xDEADBEEF),
                value_size: 4,
                ..Default::default()
            },
        );
        assert!(!list2.check_memory_access_batch(0, &[(0x1000, true, 0x12)], true, fake_mem));
    }

    #[test]
    fn sized_watch_assembles_little_endian_value() {
        let mut list = BreakpointList::new();
        list.add_with_spec(
            mem_bp(0x1000, BreakpointType::write(), Some(0)),
            MatchSpec {
                // Little-endian read of 12 34 56 78 -> 0x78563412.
                value: Some(0x78563412),
                value_size: 4,
                ..Default::default()
            },
        );
        assert!(list.check_memory_access_batch(0, &[(0x1002, true, 0x56)], false, fake_mem));
    }

    #[test]
    fn sized_watch_supports_24_bit_width() {
        let mut list = BreakpointList::new();
        list.add_with_spec(
            mem_bp(0x1000, BreakpointType::write(), Some(0)),
            MatchSpec {
                // 24-bit big-endian read of 12 34 56 -> 0x123456.
                value: Some(0x123456),
                value_size: 3,
                ..Default::default()
            },
        );
        assert!(list.check_memory_access_batch(0, &[(0x1002, true, 0x56)], true, fake_mem));
        // 0x1003 is outside the 3-byte span.
        assert!(!list.check_memory_access_batch(0, &[(0x1003, true, 0x78)], true, fake_mem));
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
        // read_byte must never be consulted for a single-byte watch:
        // panic if it is, proving the accessed byte is used directly.
        let panicking = |_: u32| -> u8 { panic!("single-byte watch must not peek memory") };
        assert!(list.check_memory_access_batch(0, &[(0x2000, true, 0x42)], true, panicking));
    }

    #[test]
    fn batch_fires_each_watch_once_per_instruction() {
        // A 32-bit watch whose four covered bytes are all written in one
        // instruction must count a single hit, not four.
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            mem_bp(0x1000, BreakpointType::write(), Some(0)),
            MatchSpec {
                value: Some(0x12345678),
                value_size: 4,
                ..Default::default()
            },
        );
        let accesses = [
            (0x1000, true, 0x12),
            (0x1001, true, 0x34),
            (0x1002, true, 0x56),
            (0x1003, true, 0x78),
        ];
        assert!(list.check_memory_access_batch(0, &accesses, true, fake_mem));
        assert_eq!(hit_count(&list, id), 1);
    }

    #[test]
    fn batch_value_agnostic_watch_fires_without_peek() {
        let mut list = BreakpointList::new();
        let id = list.add(mem_bp(0x1000, BreakpointType::write(), Some(0)));
        // No value predicate -> read_byte is never needed.
        let panicking = |_: u32| -> u8 { panic!("value-agnostic watch must not peek") };
        assert!(list.check_memory_access_batch(0, &[(0x1000, true, 0xAB)], true, panicking));
        assert_eq!(hit_count(&list, id), 1);
    }

    #[test]
    fn batch_ignores_wrong_direction_and_cpu() {
        let mut list = BreakpointList::new();
        list.add(mem_bp(0x1000, BreakpointType::write(), Some(0)));
        // A read access never trips a write-only watch.
        assert!(!list.check_memory_access_batch(0, &[(0x1000, false, 0)], true, fake_mem));
        // The right access on the wrong CPU is filtered out.
        assert!(!list.check_memory_access_batch(1, &[(0x1000, true, 0)], true, fake_mem));
    }

    #[test]
    fn value_width_clamps_to_one_through_four() {
        assert_eq!(
            MatchSpec {
                value_size: 0,
                ..Default::default()
            }
            .value_width(),
            1
        );
        assert_eq!(
            MatchSpec {
                value_size: 1,
                ..Default::default()
            }
            .value_width(),
            1
        );
        assert_eq!(
            MatchSpec {
                value_size: 3,
                ..Default::default()
            }
            .value_width(),
            3
        );
        assert_eq!(
            MatchSpec {
                value_size: 9,
                ..Default::default()
            }
            .value_width(),
            4
        );
    }

    /// Minimal [`ExprContext`] over a fixed register map plus a sparse
    /// big-endian memory map (absent bytes read as `0`).
    struct TestCtx {
        regs: std::collections::BTreeMap<&'static str, i64>,
        mem: std::collections::BTreeMap<u32, u8>,
    }

    impl ExprContext for TestCtx {
        fn register(&mut self, name: &str) -> Option<i64> {
            self.regs.get(name).copied()
        }
        fn read_mem(&mut self, addr: u32, width: u8) -> i64 {
            let mut v: i64 = 0;
            for i in 0..width as u32 {
                let byte = *self.mem.get(&addr.wrapping_add(i)).unwrap_or(&0);
                v = (v << 8) | byte as i64;
            }
            v
        }
    }

    fn ctx_with(pairs: &[(&'static str, i64)]) -> TestCtx {
        TestCtx {
            regs: pairs.iter().copied().collect(),
            mem: std::collections::BTreeMap::new(),
        }
    }

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
        let mut yes = ctx_with(&[("d0", 5)]);
        let mut no = ctx_with(&[("d0", 4)]);
        // Condition false -> no fire, no hit counted.
        assert!(!list.check_execute_ctx(0, 0x1000, Some(&mut no)));
        assert_eq!(hit_count(&list, id), 0);
        // Condition true -> fire.
        assert!(list.check_execute_ctx(0, 0x1000, Some(&mut yes)));
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
        // The value-/expression-agnostic entry can't evaluate the
        // condition, so an expression breakpoint stays silent there.
        assert!(!list.check_execute(0, 0x1000));
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
        let mut ctx = ctx_with(&[("d0", 5)]);
        assert!(!list.check_execute_ctx(0, 0x1000, Some(&mut ctx)));
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
        // Fires even on the context-less path because the blank expression
        // is treated as absent.
        assert!(list.check_execute(0, 0x1000));
        assert_eq!(hit_count(&list, id), 1);
    }

    #[test]
    fn expr_watch_combines_with_value_predicate() {
        // A write watch that only fires when the stored word is 0x1234 AND
        // a1 points above 0xFF0000. The context supplies the assembled
        // value (0x1234) via `read_mem` and the register `a1`.
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
        let accesses = [(0x1000, true, 0x12), (0x1001, true, 0x34)];
        let mem: std::collections::BTreeMap<u32, u8> = [(0x1000, 0x12), (0x1001, 0x34)].into();
        // Value matches but expression is false -> no fire.
        let mut low = ctx_with(&[("a1", 0x100)]);
        low.mem = mem.clone();
        assert!(!list.check_memory_access_batch_ctx(0, &accesses, &mut low));
        assert_eq!(hit_count(&list, id), 0);
        // Value matches and expression holds -> fire once.
        let mut high = ctx_with(&[("a1", 0xFF_8000)]);
        high.mem = mem;
        assert!(list.check_memory_access_batch_ctx(0, &accesses, &mut high));
        assert_eq!(hit_count(&list, id), 1);
    }

    #[test]
    fn ctx_batch_sized_watch_reads_value_through_context() {
        // Without an explicit expression, the ctx batch still assembles a
        // sized watch's observed value from `read_mem`.
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            mem_bp(0x1000, BreakpointType::write(), Some(0)),
            MatchSpec {
                value: Some(0x12345678),
                value_size: 4,
                ..Default::default()
            },
        );
        let mut ctx = ctx_with(&[]);
        ctx.mem = [
            (0x1000, 0x12),
            (0x1001, 0x34),
            (0x1002, 0x56),
            (0x1003, 0x78),
        ]
        .into();
        assert!(list.check_memory_access_batch_ctx(0, &[(0x1003, true, 0x78)], &mut ctx));
        assert_eq!(hit_count(&list, id), 1);
    }

    #[test]
    fn expr_watch_fails_closed_without_context() {
        let mut list = BreakpointList::new();
        let id = list.add_with_spec(
            mem_bp(0x1000, BreakpointType::write(), Some(0)),
            MatchSpec {
                expr: Some("d0 != 0".into()),
                ..Default::default()
            },
        );
        // No ctx -> the conditional watch can't be verified, stays silent.
        assert!(!list.check_memory_access_batch(0, &[(0x1000, true, 0xAB)], true, fake_mem));
        assert_eq!(hit_count(&list, id), 0);
    }
}
