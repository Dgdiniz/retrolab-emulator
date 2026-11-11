//! Console-agnostic input manager.
//!
//! Manages an arbitrary number of controller ports (set at construction
//! time) and routes input from registered providers (keyboard, gamepad,
//! future touch / mouse...) onto them. Each core decides how many ports
//! it needs:
//!
//! - SG1 (Mega Drive): 2 ports (DE-9 P1 + P2). Use [`GenericInputManager::for_2_ports`].
//! - SNES1: 2 ports by default; multitap adapter brings it up to 5.
//!   Use [`GenericInputManager::new(5)`](GenericInputManager::new).
//!
//! Design choices:
//!
//! - **`Vec` instead of `[T; N]` const generic**: const generics with
//!   `Option<String>` in array literals are ergonomically awkward (need
//!   `[const { None }; N]`, stable only since Rust 1.79) and the
//!   runtime overhead at N <= 5 is irrelevant. `Vec` also lets a single
//!   manager handle multitap without recompiling.
//! - **The wire format is a plain array**: `InputSnapshot::port_states`
//!   is a `Vec<u32>`, which serializes to JSON as `[a, b, ...]` whatever
//!   the port count, so the frontend reads it as `port_states: number[]`
//!   in TypeScript.
//! - **A port can be fed by several providers.** Its state is the OR of
//!   all of them, so a keyboard and a gamepad can play the same player
//!   at once. A host places each provider with
//!   [`GenericInputManager::set_provider_port`]; one that never does
//!   gets the automatic placement described on
//!   [`GenericInputManager::update_gamepad_button`].
//! - **Direct overrides are per-port `Vec<u32>`**, OR-merged with the
//!   providers' state (touch overlay can inject buttons without stealing
//!   the port from keyboard / gamepad).
//! - **Out-of-range port values are silently ignored** to keep call
//!   sites that don't track `n_ports` clean.

use crate::input::{GamepadInputProvider, InputProvider, InputProviderInfo, KeyboardInputProvider};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Wrapper enum that lets the manager store providers of different
/// concrete types in the same `HashMap` while still allowing
/// type-specific updates (keyboard key state, gamepad axis values).
enum ProviderWrapper {
    Keyboard(KeyboardInputProvider),
    Gamepad(GamepadInputProvider),
}

impl ProviderWrapper {
    fn as_provider(&self) -> &dyn InputProvider {
        match self {
            ProviderWrapper::Keyboard(k) => k,
            ProviderWrapper::Gamepad(g) => g,
        }
    }

    fn as_keyboard_mut(&mut self) -> Option<&mut KeyboardInputProvider> {
        match self {
            ProviderWrapper::Keyboard(k) => Some(k),
            _ => None,
        }
    }

    fn as_gamepad_mut(&mut self) -> Option<&mut GamepadInputProvider> {
        match self {
            ProviderWrapper::Gamepad(g) => Some(g),
            _ => None,
        }
    }
}

/// Snapshot of input state at a specific frame.
///
/// `port_states` is `Vec<u32>` so the same struct round-trips between
/// consoles with different port counts. For two ports its JSON is the
/// same `[a, b]` array a `[u32; 2]` gives, so a two-port frontend reads
/// it as is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputSnapshot {
    pub frame: u64,
    pub timestamp_ms: u64,
    pub port_states: Vec<u32>,
}

/// Manages input providers and routes their state to console-defined
/// controller ports.
///
/// See module-level docs for the design rationale. Concretely, this
/// type owns:
///
/// - A keyed `HashMap` of registered providers (one keyboard plus N
///   gamepads is typical).
/// - The providers feeding each port (`Vec<Vec<String>>`, length =
///   `n_ports`), in the order they joined it.
/// - Per-port direct override masks (`Vec<u32>`, length = `n_ports`)
///   that the touch overlay / virtual input frontend pokes via
///   `set_direct_override`.
/// - Bounded ring-style snapshot history for debugging.
pub struct GenericInputManager {
    providers: HashMap<String, ProviderWrapper>,
    port_providers: Vec<Vec<String>>,
    direct_overrides: Vec<u32>,
    input_history: Vec<InputSnapshot>,
    max_history: usize,
    n_ports: usize,
    /// Button map handed to gamepads the manager registers itself (see
    /// [`add_gamepad`](Self::add_gamepad)). `None` keeps the Genesis
    /// layout baked into [`GamepadInputProvider::new`], so a core that
    /// never sets it (SG1) gets that layout. A core with a different
    /// physical pad (SNES face buttons, NES two-button) sets this once
    /// at construction.
    default_gamepad_mapping: Option<HashMap<u32, u32>>,
    /// Providers the host has placed itself with
    /// [`set_provider_port`](Self::set_provider_port). The automatic
    /// placement of a newly seen gamepad never applies to one of them,
    /// so a pad the host turned off before its first input stays off.
    host_placed: HashSet<String>,
    /// The single keyboard's mapping while the split keyboard replaces
    /// it, so turning the split off brings back that mapping rather than
    /// the Genesis default of [`KeyboardInputProvider::new`].
    single_keyboard_mapping: Option<HashMap<String, u32>>,
}

impl GenericInputManager {
    /// Build an empty manager with `n_ports` ports.
    ///
    /// No providers are registered. Cores that want the SG1-style
    /// "default keyboard on port 0" behaviour should call
    /// [`add_keyboard_provider`](Self::add_keyboard_provider) +
    /// [`assign_provider_to_port`](Self::assign_provider_to_port)
    /// after `new` (the SG1 shim does this).
    pub fn new(n_ports: usize) -> Self {
        Self {
            providers: HashMap::new(),
            port_providers: vec![Vec::new(); n_ports],
            direct_overrides: vec![0; n_ports],
            input_history: Vec::new(),
            max_history: 60,
            n_ports,
            default_gamepad_mapping: None,
            host_placed: HashSet::new(),
            single_keyboard_mapping: None,
        }
    }

    /// Convenience for the common SG1/SNES1 two-port layout.
    pub fn for_2_ports() -> Self {
        Self::new(2)
    }

    /// Number of ports this manager was configured for. Doesn't change
    /// over the lifetime of the manager.
    pub fn n_ports(&self) -> usize {
        self.n_ports
    }

    pub fn add_keyboard_provider(&mut self, provider: KeyboardInputProvider) {
        let id = provider.provider_info().id.clone();
        self.providers
            .insert(id, ProviderWrapper::Keyboard(provider));
    }

    pub fn add_gamepad_provider(&mut self, provider: GamepadInputProvider) {
        let id = provider.provider_info().id.clone();
        self.providers
            .insert(id, ProviderWrapper::Gamepad(provider));
    }

    /// Set the button map applied to gamepads the manager registers
    /// itself. Already-registered providers are untouched (rebind those
    /// via [`set_gamepad_mapping`](Self::set_gamepad_mapping)).
    ///
    /// Cores call this once at construction to pick a console-correct
    /// default, e.g. the SNES face-button order, instead of the
    /// Genesis layout that [`GamepadInputProvider::new`] bakes in. Left
    /// unset, the manager falls back to that Genesis layout.
    pub fn set_default_gamepad_mapping(&mut self, mapping: HashMap<u32, u32>) {
        self.default_gamepad_mapping = Some(mapping);
    }

    /// Build a gamepad provider for an id the manager hasn't seen
    /// before, honouring [`set_default_gamepad_mapping`](Self::set_default_gamepad_mapping)
    /// when present and falling back to the Genesis default otherwise.
    fn new_auto_gamepad(&self, gamepad_id: String) -> GamepadInputProvider {
        match &self.default_gamepad_mapping {
            Some(mapping) => GamepadInputProvider::with_mapping(gamepad_id, mapping.clone()),
            None => GamepadInputProvider::new(gamepad_id),
        }
    }

    /// Register a gamepad the manager has not seen yet, with the core's
    /// default button map, on no port. Returns whether it was new.
    ///
    /// A host that places pads itself calls this and then
    /// [`set_provider_port`](Self::set_provider_port), so the pad has
    /// its map (and can be given another) before its first input.
    pub fn add_gamepad(&mut self, gamepad_id: &str) -> bool {
        if self.providers.contains_key(gamepad_id) {
            return false;
        }
        let provider = self.new_auto_gamepad(gamepad_id.to_string());
        self.add_gamepad_provider(provider);
        true
    }

    /// Make `provider_id` the only provider on `port`; an empty id
    /// clears the port. Other ports are untouched, so this can put the
    /// same provider on two ports.
    ///
    /// This is the call for hosts that expect one provider per port;
    /// [`set_provider_port`](Self::set_provider_port) moves a
    /// provider instead and leaves the others on the port.
    pub fn assign_provider_to_port(&mut self, provider_id: String, port: usize) {
        if port < self.n_ports {
            self.port_providers[port].clear();
            if !provider_id.is_empty() {
                self.port_providers[port].push(provider_id);
            }
        }
    }

    /// Move `provider_id` onto `port`, beside whatever else feeds it,
    /// after taking it off every other port. `None`, or a port out of
    /// range, leaves it on no port: its input then reaches no player.
    ///
    /// The provider need not be registered yet; the placement holds once
    /// it is, and the manager never places it automatically afterwards.
    pub fn set_provider_port(&mut self, provider_id: &str, port: Option<usize>) {
        self.host_placed.insert(provider_id.to_string());
        self.move_provider(provider_id, port);
    }

    fn move_provider(&mut self, provider_id: &str, port: Option<usize>) {
        for providers in &mut self.port_providers {
            providers.retain(|id| id != provider_id);
        }
        if let Some(port) = port.filter(|&p| p < self.n_ports) {
            self.port_providers[port].push(provider_id.to_string());
        }
    }

    /// The port `provider_id` feeds, if any. For a provider
    /// [`assign_provider_to_port`](Self::assign_provider_to_port) put on
    /// several ports, the lowest of them.
    pub fn provider_port(&self, provider_id: &str) -> Option<usize> {
        self.port_providers
            .iter()
            .position(|providers| providers.iter().any(|id| id == provider_id))
    }

    /// Every provider feeding `port`, in the order they joined it. Empty
    /// for a port out of range.
    pub fn port_providers(&self, port: usize) -> &[String] {
        self.port_providers
            .get(port)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Take every provider off `port`.
    pub fn unassign_port(&mut self, port: usize) {
        if let Some(providers) = self.port_providers.get_mut(port) {
            providers.clear();
        }
    }

    /// Exchange everything feeding port `a` with everything feeding
    /// port `b`. A port out of range makes it a no-op.
    pub fn swap_ports(&mut self, a: usize, b: usize) {
        if a < self.n_ports && b < self.n_ports {
            self.port_providers.swap(a, b);
        }
    }

    /// Current bitmask for `port`: the OR of every provider feeding it,
    /// OR-merged with any direct override pumped through
    /// [`set_direct_override`](Self::set_direct_override).
    ///
    /// Returns `0` for out-of-range ports (caller doesn't have to
    /// check `n_ports`).
    pub fn get_port_state(&self, port: usize) -> u32 {
        let Some(providers) = self.port_providers.get(port) else {
            return 0;
        };

        let provider_state = providers
            .iter()
            .filter_map(|id| self.providers.get(id))
            .fold(0, |state, wrapper| {
                state | wrapper.as_provider().get_state()
            });

        provider_state | self.direct_overrides[port]
    }

    /// Set the direct override bitmask for `port`. REPLACES the
    /// previous mask (tracks the currently held set, not a pulse).
    ///
    /// Used by the JS bridge so the touch overlay can inject button
    /// presses without taking the port from keyboard / gamepad. The
    /// two sources are OR-merged in
    /// [`get_port_state`](Self::get_port_state).
    pub fn set_direct_override(&mut self, port: usize, mask: u32) {
        if port < self.n_ports {
            self.direct_overrides[port] = mask;
        }
    }

    pub fn get_direct_override(&self, port: usize) -> u32 {
        self.direct_overrides.get(port).copied().unwrap_or(0)
    }

    pub fn clear_direct_override(&mut self, port: usize) {
        if port < self.n_ports {
            self.direct_overrides[port] = 0;
        }
    }

    /// Reset every port's direct override to 0. Convenience for
    /// shutdown / detach paths.
    pub fn clear_all_direct_overrides(&mut self) {
        self.direct_overrides.fill(0);
    }

    /// Broadcast a key press/release to every keyboard provider
    /// (single keyboard + optional split P1/P2 keyboards).
    pub fn update_keyboard(&mut self, keycode: String, pressed: bool) {
        for wrapper in self.providers.values_mut() {
            if let Some(kb) = wrapper.as_keyboard_mut() {
                kb.set_key(keycode.clone(), pressed);
            }
        }
    }

    /// Update a gamepad button. A gamepad the manager has not seen is
    /// registered first and, unless the host placed it, put on a port:
    /// the first one plays alongside the keyboard on port 0, each next
    /// one takes the lowest port no gamepad feeds yet. Once every port
    /// has one, a new pad is registered on no port for the host to
    /// place.
    pub fn update_gamepad_button(&mut self, gamepad_id: String, button: u32, pressed: bool) {
        self.register_unseen_gamepad(&gamepad_id);

        if let Some(wrapper) = self.providers.get_mut(&gamepad_id) {
            if let Some(gp) = wrapper.as_gamepad_mut() {
                gp.update_button(button, pressed);
            }
        }
    }

    /// Update a gamepad axis. Same registration and placement rules as
    /// [`update_gamepad_button`](Self::update_gamepad_button).
    pub fn update_gamepad_axis(&mut self, gamepad_id: String, axis: u32, value: f32) {
        self.register_unseen_gamepad(&gamepad_id);

        if let Some(wrapper) = self.providers.get_mut(&gamepad_id) {
            if let Some(gp) = wrapper.as_gamepad_mut() {
                gp.update_axis(axis, value);
            }
        }
    }

    fn register_unseen_gamepad(&mut self, gamepad_id: &str) {
        if self.add_gamepad(gamepad_id) && !self.host_placed.contains(gamepad_id) {
            self.place_new_gamepad(gamepad_id);
        }
    }

    fn place_new_gamepad(&mut self, gamepad_id: &str) {
        if self.provider_port(gamepad_id).is_some() {
            return;
        }
        let has_gamepad = |providers: &Vec<String>| {
            providers
                .iter()
                .any(|id| matches!(self.providers.get(id), Some(ProviderWrapper::Gamepad(_))))
        };
        if let Some(port) = self.port_providers.iter().position(|p| !has_gamepad(p)) {
            self.port_providers[port].push(gamepad_id.to_string());
        }
    }

    /// Snapshot every port's current state and append to history.
    /// History is bounded to `max_history` (default 60, ~1s @ 60 FPS).
    pub fn snapshot(&mut self, frame: u64) {
        let port_states: Vec<u32> = (0..self.n_ports).map(|p| self.get_port_state(p)).collect();
        let snapshot = InputSnapshot {
            frame,
            timestamp_ms: Self::current_time_ms(),
            port_states,
        };

        self.input_history.push(snapshot);

        if self.input_history.len() > self.max_history {
            self.input_history.remove(0);
        }
    }

    pub fn get_history(&self, count: usize) -> &[InputSnapshot] {
        let start = self
            .input_history
            .len()
            .saturating_sub(count.min(self.max_history));
        &self.input_history[start..]
    }

    pub fn get_providers(&self) -> Vec<InputProviderInfo> {
        self.providers
            .values()
            .map(|w| w.as_provider().provider_info())
            .collect()
    }

    /// The first provider on `port`, for hosts that expect one provider
    /// per port. [`port_providers`](Self::port_providers) lists them all.
    pub fn get_port_assignment(&self, port: usize) -> Option<String> {
        self.port_providers
            .get(port)
            .and_then(|providers| providers.first().cloned())
    }

    pub fn set_keyboard_mapping(&mut self, mapping: HashMap<String, u32>) {
        for wrapper in self.providers.values_mut() {
            if let Some(kb) = wrapper.as_keyboard_mut() {
                kb.set_button_mapping(mapping.clone());
            }
        }
    }

    pub fn get_keyboard_mapping(&self) -> HashMap<String, u32> {
        for wrapper in self.providers.values() {
            if let ProviderWrapper::Keyboard(kb) = wrapper {
                return kb.get_button_mapping().clone();
            }
        }
        HashMap::new()
    }

    pub fn clear_keyboard_state(&mut self) {
        for wrapper in self.providers.values_mut() {
            if let Some(kb) = wrapper.as_keyboard_mut() {
                kb.clear();
            }
        }
    }

    pub fn set_gamepad_mapping(&mut self, gamepad_id: &str, mapping: HashMap<u32, u32>) {
        if let Some(wrapper) = self.providers.get_mut(gamepad_id) {
            if let Some(gp) = wrapper.as_gamepad_mut() {
                gp.set_button_mapping(mapping);
            }
        }
    }

    pub fn get_gamepad_mapping(&self, gamepad_id: &str) -> HashMap<u32, u32> {
        if let Some(ProviderWrapper::Gamepad(gp)) = self.providers.get(gamepad_id) {
            return gp.get_button_mapping().clone();
        }
        HashMap::new()
    }

    pub fn set_gamepad_deadzone(&mut self, gamepad_id: &str, deadzone: f32) {
        if let Some(wrapper) = self.providers.get_mut(gamepad_id) {
            if let Some(gp) = wrapper.as_gamepad_mut() {
                gp.set_deadzone(deadzone);
            }
        }
    }

    pub fn get_gamepad_deadzone(&self, gamepad_id: &str) -> f32 {
        if let Some(ProviderWrapper::Gamepad(gp)) = self.providers.get(gamepad_id) {
            return gp.get_deadzone();
        }
        0.15
    }

    pub fn clear_gamepad_state(&mut self, gamepad_id: &str) {
        if let Some(wrapper) = self.providers.get_mut(gamepad_id) {
            if let Some(gp) = wrapper.as_gamepad_mut() {
                gp.clear_state();
            }
        }
    }

    /// Replace the default keyboard with two scoped keyboards
    /// (`keyboard-p1`, `keyboard-p2`) so P1 and P2 can use different
    /// halves of the same keyboard. No-op when `n_ports < 2`.
    ///
    /// Turning it on puts `keyboard-p1` on port 0 and `keyboard-p2` on
    /// port 1, beside whatever else feeds those ports; the single
    /// keyboard's mapping is kept for
    /// [`disable_split_keyboard`](Self::disable_split_keyboard). Called
    /// again while on, it only replaces the two halves' mappings and
    /// leaves them where the host put them.
    pub fn enable_split_keyboard(
        &mut self,
        mapping_p1: HashMap<String, u32>,
        mapping_p2: HashMap<String, u32>,
    ) {
        if self.n_ports < 2 {
            return;
        }

        if self.is_split_keyboard_enabled() {
            self.set_keyboard_half_mapping("keyboard-p1", mapping_p1);
            self.set_keyboard_half_mapping("keyboard-p2", mapping_p2);
            return;
        }

        if let Some(ProviderWrapper::Keyboard(kb)) = self.providers.remove("keyboard") {
            self.single_keyboard_mapping = Some(kb.get_button_mapping().clone());
        }
        self.move_provider("keyboard", None);

        let kb_p1 =
            KeyboardInputProvider::with_mapping(mapping_p1).with_id("keyboard-p1".to_string());
        let kb_p2 =
            KeyboardInputProvider::with_mapping(mapping_p2).with_id("keyboard-p2".to_string());

        self.providers
            .insert("keyboard-p1".to_string(), ProviderWrapper::Keyboard(kb_p1));
        self.providers
            .insert("keyboard-p2".to_string(), ProviderWrapper::Keyboard(kb_p2));

        self.move_provider("keyboard-p1", Some(0));
        self.move_provider("keyboard-p2", Some(1));
    }

    fn set_keyboard_half_mapping(&mut self, id: &str, mapping: HashMap<String, u32>) {
        if let Some(kb) = self
            .providers
            .get_mut(id)
            .and_then(ProviderWrapper::as_keyboard_mut)
        {
            kb.set_button_mapping(mapping);
        }
    }

    /// Revert the split keyboard back to a single default `keyboard`
    /// provider with the mapping it had before the split, on the port
    /// `keyboard-p1` fed (port 0 if it fed none). No-op while the split
    /// is off.
    pub fn disable_split_keyboard(&mut self) {
        if !self.is_split_keyboard_enabled() {
            return;
        }
        let port = self.provider_port("keyboard-p1").unwrap_or(0);

        self.providers.remove("keyboard-p1");
        self.providers.remove("keyboard-p2");
        self.move_provider("keyboard-p1", None);
        self.move_provider("keyboard-p2", None);

        let keyboard = match self.single_keyboard_mapping.take() {
            Some(mapping) => KeyboardInputProvider::with_mapping(mapping),
            None => KeyboardInputProvider::new(),
        };
        self.add_keyboard_provider(keyboard);
        self.move_provider("keyboard", Some(port));
    }

    pub fn is_split_keyboard_enabled(&self) -> bool {
        self.providers.contains_key("keyboard-p1")
    }

    fn current_time_ms() -> u64 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64
        }

        #[cfg(target_arch = "wasm32")]
        {
            js_sys::Date::now() as u64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the canonical SG1-style manager (2 ports, default keyboard
    /// on port 0) used by most of the tests. Lets us keep test
    /// bodies focused on what they actually assert.
    fn sg1_style_manager() -> GenericInputManager {
        let mut mgr = GenericInputManager::for_2_ports();
        let keyboard = KeyboardInputProvider::new();
        mgr.add_keyboard_provider(keyboard);
        mgr.assign_provider_to_port("keyboard".to_string(), 0);
        mgr
    }

    // --------------------------------------------------------------------
    // Core provider / assignment / history tests
    // --------------------------------------------------------------------

    #[test]
    fn test_default_keyboard() {
        let manager = sg1_style_manager();
        assert_eq!(manager.providers.len(), 1);
        assert_eq!(manager.port_providers(0), ["keyboard".to_string()]);
    }

    #[test]
    fn test_keyboard_input() {
        let mut manager = sg1_style_manager();

        manager.update_keyboard("ArrowUp".to_string(), true);

        let state = manager.get_port_state(0);
        assert!(state != 0);
    }

    #[test]
    fn test_gamepad_assignment() {
        let mut manager = sg1_style_manager();

        let gamepad = GamepadInputProvider::new("gamepad-0".to_string());
        manager.add_gamepad_provider(gamepad);

        manager.assign_provider_to_port("gamepad-0".to_string(), 1);

        manager.update_gamepad_button("gamepad-0".to_string(), 0, true);

        let state = manager.get_port_state(1);
        assert!(state != 0);
    }

    #[test]
    fn test_input_history() {
        let mut manager = sg1_style_manager();

        manager.snapshot(0);
        manager.snapshot(1);
        manager.snapshot(2);

        let history = manager.get_history(3);
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].frame, 0);
        assert_eq!(history[1].frame, 1);
        assert_eq!(history[2].frame, 2);
    }

    #[test]
    fn test_history_limit() {
        let mut manager = sg1_style_manager();
        manager.max_history = 5;

        for i in 0..10 {
            manager.snapshot(i);
        }

        let history = manager.get_history(100);
        assert_eq!(history.len(), 5);
        assert_eq!(history[0].frame, 5);
    }

    // --------------------------------------------------------------------
    // Direct overrides, touch overlay / virtual-input compatibility
    // --------------------------------------------------------------------

    #[test]
    fn test_direct_override_default_zero() {
        let manager = sg1_style_manager();
        assert_eq!(manager.get_direct_override(0), 0);
        assert_eq!(manager.get_direct_override(1), 0);
        assert_eq!(manager.get_port_state(0), 0);
        assert_eq!(manager.get_port_state(1), 0);
    }

    #[test]
    fn test_direct_override_drives_port_state() {
        let mut manager = sg1_style_manager();

        manager.set_direct_override(0, 1 << 6);
        assert_eq!(manager.get_port_state(0), 1 << 6);

        assert_eq!(manager.get_port_state(1), 0);

        manager.set_direct_override(0, 0);
        assert_eq!(manager.get_port_state(0), 0);
    }

    #[test]
    fn test_direct_override_combines_with_keyboard_provider() {
        let mut manager = sg1_style_manager();

        manager.update_keyboard("ArrowUp".to_string(), true);
        let kb_state = manager.get_port_state(0);
        assert_ne!(kb_state, 0, "keyboard alone should produce non-zero state");

        manager.set_direct_override(0, 1 << 6);
        let combined = manager.get_port_state(0);
        assert_eq!(
            combined,
            kb_state | (1 << 6),
            "direct override must OR-merge with the assigned provider"
        );

        manager.set_direct_override(0, 0);
        assert_eq!(manager.get_port_state(0), kb_state);
    }

    #[test]
    fn test_direct_override_independent_per_port() {
        let mut manager = sg1_style_manager();
        manager.set_direct_override(0, 0xAAAA);
        manager.set_direct_override(1, 0x5555);
        assert_eq!(manager.get_direct_override(0), 0xAAAA);
        assert_eq!(manager.get_direct_override(1), 0x5555);
        assert_eq!(manager.get_port_state(0), 0xAAAA);
        assert_eq!(manager.get_port_state(1), 0x5555);
    }

    #[test]
    fn test_direct_override_replaces_previous_mask() {
        let mut manager = sg1_style_manager();
        manager.set_direct_override(0, 1 << 6);
        assert_eq!(manager.get_port_state(0), 1 << 6);
        manager.set_direct_override(0, 1 << 4);
        assert_eq!(
            manager.get_port_state(0),
            1 << 4,
            "set_direct_override REPLACES the previous mask, not OR-accumulates"
        );
    }

    #[test]
    fn test_clear_direct_override_single_port() {
        let mut manager = sg1_style_manager();
        manager.set_direct_override(0, 0xFF);
        manager.set_direct_override(1, 0xFF);
        manager.clear_direct_override(0);
        assert_eq!(manager.get_direct_override(0), 0);
        assert_eq!(
            manager.get_direct_override(1),
            0xFF,
            "clearing one port must not touch the other"
        );
    }

    #[test]
    fn test_clear_all_direct_overrides() {
        let mut manager = sg1_style_manager();
        manager.set_direct_override(0, 0xFF);
        manager.set_direct_override(1, 0xFF);
        manager.clear_all_direct_overrides();
        assert_eq!(manager.get_direct_override(0), 0);
        assert_eq!(manager.get_direct_override(1), 0);
    }

    #[test]
    fn test_direct_override_out_of_range_port_is_noop() {
        let mut manager = sg1_style_manager();
        manager.set_direct_override(2, 0xFFFFFFFF);
        manager.set_direct_override(99, 0xFFFFFFFF);
        manager.clear_direct_override(2);
        assert_eq!(manager.get_direct_override(0), 0);
        assert_eq!(manager.get_direct_override(1), 0);
        assert_eq!(manager.get_direct_override(2), 0);
        assert_eq!(manager.get_port_state(2), 0);
    }

    #[test]
    fn test_direct_override_works_without_any_provider() {
        let mut manager = sg1_style_manager();
        manager.unassign_port(0);
        manager.set_direct_override(0, 1 << 7);
        assert_eq!(manager.get_port_state(0), 1 << 7);
    }

    // --------------------------------------------------------------------
    // Generic manager / multi-port behaviour
    // --------------------------------------------------------------------

    #[test]
    fn test_n_ports_zero_is_legal_but_inert() {
        // Edge case: a console with no controllers (?). Manager
        // should not panic and every port query returns 0.
        let manager = GenericInputManager::new(0);
        assert_eq!(manager.n_ports(), 0);
        assert_eq!(manager.get_port_state(0), 0);
        assert_eq!(manager.get_port_state(7), 0);
    }

    #[test]
    fn test_n_ports_five_for_multitap() {
        // SNES multitap = 5 ports. Each must be independently
        // addressable.
        let mut manager = GenericInputManager::new(5);
        for p in 0..5 {
            manager.set_direct_override(p, 1 << p);
        }
        for p in 0..5 {
            assert_eq!(manager.get_port_state(p), 1 << p);
        }
        // Port 5 doesn't exist.
        assert_eq!(manager.get_port_state(5), 0);
    }

    #[test]
    fn test_assign_out_of_range_is_silent() {
        let mut manager = GenericInputManager::for_2_ports();
        manager.assign_provider_to_port("missing".to_string(), 99);
        assert_eq!(manager.get_port_assignment(99), None);
        assert_eq!(manager.get_port_assignment(0), None);
        assert_eq!(manager.get_port_assignment(1), None);
    }

    #[test]
    fn test_first_gamepad_plays_beside_the_keyboard_on_port_zero() {
        // A single player with a keyboard and a gamepad expects both to
        // drive player 1; the pad must join port 0 without displacing
        // the keyboard.
        let mut manager = sg1_style_manager();
        manager.update_gamepad_button("pad-A".to_string(), 0, true);
        assert_eq!(
            manager.port_providers(0),
            ["keyboard".to_string(), "pad-A".to_string()],
            "the pad must join the keyboard on port 0"
        );
        assert!(
            manager.port_providers(1).is_empty(),
            "port 1 must stay empty"
        );
        assert_eq!(
            manager.get_port_assignment(0),
            Some("keyboard".to_string()),
            "the first provider on the port is still the keyboard"
        );
    }

    #[test]
    fn test_each_next_gamepad_takes_the_next_port_without_one() {
        let mut manager = sg1_style_manager();
        manager.update_gamepad_button("pad-A".to_string(), 0, true);
        manager.update_gamepad_axis("pad-B".to_string(), 0, 0.0);
        manager.update_gamepad_button("pad-C".to_string(), 0, true);
        assert_eq!(manager.provider_port("pad-A"), Some(0));
        assert_eq!(manager.provider_port("pad-B"), Some(1));
        assert_eq!(
            manager.provider_port("pad-C"),
            None,
            "with every port holding a pad, a third one waits for the host"
        );
        assert!(
            manager.get_providers().iter().any(|p| p.id == "pad-C"),
            "the third pad is still registered"
        );
    }

    #[test]
    fn test_keyboard_and_gamepad_on_one_port_or_merge() {
        let mut manager = sg1_style_manager();
        manager.update_gamepad_button("pad-A".to_string(), 0, true);
        let pad_state = manager.get_port_state(0);
        assert_ne!(pad_state, 0, "the pad alone must reach port 0");

        manager.update_keyboard("ArrowUp".to_string(), true);
        let both = manager.get_port_state(0);
        assert_ne!(both, pad_state, "the keyboard must add its button");
        assert_eq!(
            both & pad_state,
            pad_state,
            "the pad's button must stay held"
        );

        manager.update_gamepad_button("pad-A".to_string(), 0, false);
        assert_ne!(
            manager.get_port_state(0),
            0,
            "the keyboard alone still holds"
        );
    }

    #[test]
    fn test_set_provider_port_moves_a_provider_and_none_turns_it_off() {
        let mut manager = sg1_style_manager();
        manager.update_gamepad_button("pad-A".to_string(), 0, true);

        manager.set_provider_port("pad-A", Some(1));
        assert_eq!(manager.port_providers(0), ["keyboard".to_string()]);
        assert_eq!(manager.port_providers(1), ["pad-A".to_string()]);
        assert_ne!(
            manager.get_port_state(1),
            0,
            "the held button moves with the pad"
        );
        assert_eq!(manager.get_port_state(0), 0);

        manager.set_provider_port("pad-A", None);
        assert_eq!(manager.provider_port("pad-A"), None);
        assert_eq!(
            manager.get_port_state(1),
            0,
            "an unplaced pad reaches no player"
        );

        manager.set_provider_port("pad-A", Some(9));
        assert_eq!(
            manager.provider_port("pad-A"),
            None,
            "a port out of range places it nowhere"
        );
    }

    #[test]
    fn test_a_pad_the_host_turned_off_stays_off_on_its_first_input() {
        let mut manager = sg1_style_manager();
        manager.set_provider_port("pad-A", None);
        manager.update_gamepad_button("pad-A".to_string(), 0, true);
        assert_eq!(manager.provider_port("pad-A"), None);
        assert_eq!(manager.get_port_state(0), 0);
        assert_eq!(manager.get_port_state(1), 0);
    }

    #[test]
    fn test_a_pad_the_host_placed_before_its_first_input_stays_there() {
        let mut manager = sg1_style_manager();
        assert!(manager.add_gamepad("pad-A"), "a new pad is registered");
        assert!(
            !manager.add_gamepad("pad-A"),
            "registering it twice is a no-op"
        );
        manager.set_provider_port("pad-A", Some(1));
        manager.update_gamepad_button("pad-A".to_string(), 0, true);
        assert_eq!(manager.provider_port("pad-A"), Some(1));
        assert_ne!(manager.get_port_state(1), 0);
    }

    #[test]
    fn test_add_gamepad_uses_the_default_mapping_and_no_port() {
        let mut manager = GenericInputManager::new(2);
        let mut map = HashMap::new();
        map.insert(0u32, 5u32);
        manager.set_default_gamepad_mapping(map.clone());
        manager.add_gamepad("pad");
        assert_eq!(manager.get_gamepad_mapping("pad"), map);
        assert_eq!(manager.provider_port("pad"), None);
    }

    #[test]
    fn test_legacy_assign_replaces_the_port_and_leaves_other_ports() {
        let mut manager = sg1_style_manager();
        manager.update_gamepad_button("pad-A".to_string(), 0, true);
        assert_eq!(manager.port_providers(0).len(), 2);

        manager.assign_provider_to_port("pad-A".to_string(), 1);
        assert_eq!(
            manager.port_providers(0),
            ["keyboard".to_string(), "pad-A".to_string()],
            "the one-provider call touches only the port it names"
        );
        assert_eq!(manager.port_providers(1), ["pad-A".to_string()]);

        manager.assign_provider_to_port(String::new(), 0);
        assert!(
            manager.port_providers(0).is_empty(),
            "an empty id clears the port"
        );
        assert_eq!(manager.provider_port("pad-A"), Some(1));
    }

    #[test]
    fn test_swap_ports_exchanges_everything_on_them() {
        let mut manager = sg1_style_manager();
        manager.update_gamepad_button("pad-A".to_string(), 0, true);
        manager.update_gamepad_button("pad-B".to_string(), 0, true);

        manager.swap_ports(0, 1);
        assert_eq!(manager.port_providers(0), ["pad-B".to_string()]);
        assert_eq!(
            manager.port_providers(1),
            ["keyboard".to_string(), "pad-A".to_string()]
        );

        manager.swap_ports(0, 7);
        assert_eq!(
            manager.port_providers(0),
            ["pad-B".to_string()],
            "a port out of range makes the swap a no-op"
        );
    }

    #[test]
    fn test_unassign_port_clears_every_provider_on_it() {
        let mut manager = sg1_style_manager();
        manager.update_gamepad_button("pad-A".to_string(), 0, true);
        manager.unassign_port(0);
        assert!(manager.port_providers(0).is_empty());
        manager.unassign_port(9);
        assert!(
            manager.port_providers(9).is_empty(),
            "out of range reads as empty"
        );
    }

    #[test]
    fn test_split_keyboard_keeps_the_pads_and_restores_the_single_mapping() {
        let mut manager = GenericInputManager::new(2);
        let mut single = HashMap::new();
        single.insert("KeyZ".to_string(), 3u32);
        manager.add_keyboard_provider(KeyboardInputProvider::with_mapping(single.clone()));
        manager.set_provider_port("keyboard", Some(0));
        manager.update_gamepad_button("pad-A".to_string(), 0, true);

        let mut p1 = HashMap::new();
        p1.insert("KeyW".to_string(), 0u32);
        let mut p2 = HashMap::new();
        p2.insert("ArrowUp".to_string(), 0u32);
        manager.enable_split_keyboard(p1.clone(), p2.clone());
        assert!(manager.is_split_keyboard_enabled());
        assert_eq!(
            manager.port_providers(0),
            ["pad-A".to_string(), "keyboard-p1".to_string()],
            "the pad stays on port 0 beside the first half"
        );
        assert_eq!(manager.port_providers(1), ["keyboard-p2".to_string()]);

        // Enabling again only replaces the halves' mappings, wherever
        // the host put them.
        manager.set_provider_port("keyboard-p2", None);
        let mut p2b = HashMap::new();
        p2b.insert("ArrowDown".to_string(), 1u32);
        manager.enable_split_keyboard(p1, p2b);
        assert_eq!(manager.provider_port("keyboard-p2"), None);
        manager.update_keyboard("ArrowDown".to_string(), true);
        manager.set_provider_port("keyboard-p2", Some(1));
        assert_eq!(manager.get_port_state(1), 1 << 1, "the new mapping applies");
        manager.update_keyboard("ArrowDown".to_string(), false);

        manager.disable_split_keyboard();
        assert!(!manager.is_split_keyboard_enabled());
        assert_eq!(
            manager.get_keyboard_mapping(),
            single,
            "the single mapping comes back"
        );
        assert_eq!(
            manager.port_providers(0),
            ["pad-A".to_string(), "keyboard".to_string()]
        );
        assert!(manager.port_providers(1).is_empty());

        manager.disable_split_keyboard();
        assert_eq!(
            manager.port_providers(0),
            ["pad-A".to_string(), "keyboard".to_string()],
            "disabling while off is a no-op"
        );
    }

    #[test]
    fn test_disable_split_keyboard_without_a_remembered_mapping_uses_the_default() {
        let mut manager = GenericInputManager::new(2);
        manager.enable_split_keyboard(HashMap::new(), HashMap::new());
        manager.disable_split_keyboard();
        assert_eq!(
            manager.get_keyboard_mapping(),
            KeyboardInputProvider::new().get_button_mapping().clone()
        );
        assert_eq!(manager.port_providers(0), ["keyboard".to_string()]);
    }

    #[test]
    fn test_auto_gamepad_uses_default_mapping_when_set() {
        // A core-supplied default must be applied to pads the manager
        // auto-registers. Use a distinctive map (Web button 0 -> console
        // bit 5) so we can tell it apart from the Genesis fallback,
        // which sends button 0 -> bit 6.
        let mut manager = GenericInputManager::new(2);
        let mut map = HashMap::new();
        map.insert(0u32, 5u32);
        manager.set_default_gamepad_mapping(map);

        manager.update_gamepad_button("pad".to_string(), 0, true);

        // The first pad lands on port 0.
        let state = manager.get_port_state(0);
        assert_eq!(state & (1 << 5), 1 << 5, "default mapping must apply");
        assert_eq!(
            state & (1 << 6),
            0,
            "the Genesis fallback must not leak when a default is set"
        );
    }

    #[test]
    fn test_auto_gamepad_falls_back_to_genesis_when_unset() {
        // With no default set (the SG1 case), an auto-registered pad
        // keeps the Genesis layout: Web button 0 -> bit 6.
        let mut manager = GenericInputManager::new(2);
        manager.update_gamepad_button("pad".to_string(), 0, true);
        let state = manager.get_port_state(0);
        assert_eq!(
            state & (1 << 6),
            1 << 6,
            "unset default keeps the Genesis mapping sg1 relies on"
        );
    }

    #[test]
    fn test_auto_assign_gamepad_takes_the_lowest_port_without_a_pad() {
        // 3-port manager with pad-A already on port 1: a new pad-B
        // takes port 0, the lowest port no pad feeds, and pad-C port 2.
        let mut manager = GenericInputManager::new(3);
        let pad_a = GamepadInputProvider::new("pad-A".to_string());
        manager.add_gamepad_provider(pad_a);
        manager.assign_provider_to_port("pad-A".to_string(), 1);

        manager.update_gamepad_button("pad-B".to_string(), 0, true);
        manager.update_gamepad_button("pad-C".to_string(), 0, true);
        assert_eq!(manager.get_port_assignment(0), Some("pad-B".to_string()));
        assert_eq!(manager.get_port_assignment(2), Some("pad-C".to_string()));
    }

    #[test]
    fn test_a_pad_put_on_a_port_before_its_first_input_is_not_moved() {
        let mut manager = GenericInputManager::new(2);
        manager.assign_provider_to_port("pad-A".to_string(), 1);
        manager.update_gamepad_button("pad-A".to_string(), 0, true);
        assert_eq!(manager.port_providers(0), [] as [String; 0]);
        assert_eq!(manager.port_providers(1), ["pad-A".to_string()]);
    }

    #[test]
    fn test_snapshot_port_states_length_matches_n_ports() {
        let mut mgr5 = GenericInputManager::new(5);
        mgr5.snapshot(7);
        let history = mgr5.get_history(1);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].port_states.len(), 5);
        assert_eq!(history[0].frame, 7);
    }

    #[test]
    fn test_split_keyboard_noop_when_single_port() {
        // n_ports = 1 -> split keyboard makes no sense, must be a
        // silent no-op (no panic, no provider mutation).
        let mut manager = GenericInputManager::new(1);
        let m_p1 = HashMap::new();
        let m_p2 = HashMap::new();
        manager.enable_split_keyboard(m_p1, m_p2);
        assert!(!manager.is_split_keyboard_enabled());
        assert_eq!(manager.get_port_assignment(0), None);
    }
}
