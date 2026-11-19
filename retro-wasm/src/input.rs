//! [`input_bindings!`], which writes the input methods every binding crate
//! exports the same way over `retro_core`'s `GenericInputManager` (keys,
//! pads, their maps, the ports and the split keyboard), and the readers of
//! the maps a host sends.
//!
//! [`input_bindings!`]: crate::input_bindings

use std::collections::HashMap;

use retro_core::input::gamepad_map_from_text_keys;
use wasm_bindgen::JsValue;

/// A keyboard map sent from JavaScript, `{ "KeyA": 6, ... }` (a key code to
/// a button's bit); `None` when it is not such an object, so a malformed map
/// never replaces a working one.
pub fn keyboard_map_from_js(value: JsValue) -> Option<HashMap<String, u32>> {
    serde_wasm_bindgen::from_value(value).ok()
}

/// A gamepad's button map sent from JavaScript: a `Map` with number keys, or
/// a plain object, whose keys JavaScript always holds as text (a pad's
/// button index to a button's bit). `None` when it is neither, so a
/// malformed map never replaces a working one.
pub fn gamepad_map_from_js(value: JsValue) -> Option<HashMap<u32, u32>> {
    if let Ok(map) = serde_wasm_bindgen::from_value(value.clone()) {
        return Some(map);
    }
    let map: HashMap<String, u32> = serde_wasm_bindgen::from_value(value).ok()?;
    gamepad_map_from_text_keys(map)
}

/// Writes the input methods on `$emulator`, over the `EmulatorCore` in its
/// field `$core`: `setInput(player, buttons)`, the key and pad events
/// (`keyDown`, `keyUp`, `gamepadButton`, `gamepadAxis`), the providers and
/// ports (`getInputProviders`, `getPortAssignment`, `getInputHistory`,
/// `getPortState`, `assignProviderToPort`, `setProviderPort`,
/// `getProviderPort`, `getPortProviders`, `addGamepad`,
/// `swapPortAssignments`), the maps (`setKeyboardMapping`,
/// `getKeyboardMapping`, `clearKeyboardState`, `setGamepadMapping`,
/// `getGamepadMapping`, `setGamepadDeadzone`, `getGamepadDeadzone`,
/// `clearGamepadState`) and the split keyboard (`enableSplitKeyboard`,
/// `disableSplitKeyboard`, `isSplitKeyboardEnabled`).
///
/// Each method's Rust name is its JavaScript name in snake case, which no
/// other method of the struct may have. The crate expanding the macro
/// depends on `wasm-bindgen` itself, as every crate declaring a
/// `#[wasm_bindgen]` struct does.
#[macro_export]
macro_rules! input_bindings {
    ($emulator:ident, $core:ident $(,)?) => {
        #[$crate::__private::wasm_bindgen::prelude::wasm_bindgen]
        impl $emulator {
            /// Hold `buttons` (a bitmask in the console's button layout) on
            /// player `player`'s port, beside whatever else feeds it: the
            /// virtual input of a touch overlay, added to the keyboard's and
            /// the pads'.
            #[wasm_bindgen(js_name = setInput)]
            pub fn set_input(&mut self, player: u8, buttons: u32) {
                $crate::__private::retro_core::EmulatorCore::set_input(
                    &mut self.$core,
                    player,
                    $crate::__private::retro_core::InputState { buttons },
                );
            }

            /// A key pressed (its `KeyboardEvent.code`).
            #[wasm_bindgen(js_name = keyDown)]
            pub fn key_down(&mut self, keycode: ::std::string::String) {
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .update_keyboard(keycode, true);
            }

            /// A key released.
            #[wasm_bindgen(js_name = keyUp)]
            pub fn key_up(&mut self, keycode: ::std::string::String) {
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .update_keyboard(keycode, false);
            }

            /// A pad's button pressed or released.
            #[wasm_bindgen(js_name = gamepadButton)]
            pub fn gamepad_button(
                &mut self,
                gamepad_id: ::std::string::String,
                button: u32,
                pressed: bool,
            ) {
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .update_gamepad_button(gamepad_id, button, pressed);
            }

            /// A pad's axis moved.
            #[wasm_bindgen(js_name = gamepadAxis)]
            pub fn gamepad_axis(
                &mut self,
                gamepad_id: ::std::string::String,
                axis: u32,
                value: f32,
            ) {
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .update_gamepad_axis(gamepad_id, axis, value);
            }

            /// Every input provider registered: the keyboard, or its two
            /// halves, and each pad.
            #[wasm_bindgen(js_name = getInputProviders)]
            pub fn get_input_providers(&self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js(
                    &$crate::__private::retro_core::EmulatorCore::input_manager(&self.$core)
                        .get_providers(),
                )
            }

            /// The provider assigned to a controller port.
            #[wasm_bindgen(js_name = getPortAssignment)]
            pub fn get_port_assignment(
                &self,
                port: u32,
            ) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js(
                    &$crate::__private::retro_core::EmulatorCore::input_manager(&self.$core)
                        .get_port_assignment(port as usize),
                )
            }

            /// The last `count` input snapshots.
            #[wasm_bindgen(js_name = getInputHistory)]
            pub fn get_input_history(
                &self,
                count: u32,
            ) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js(
                    &$crate::__private::retro_core::EmulatorCore::input_manager(&self.$core)
                        .get_history(count as usize),
                )
            }

            /// The buttons a controller port holds now, a bitmask.
            #[wasm_bindgen(js_name = getPortState)]
            pub fn get_port_state(&self, port: u32) -> u32 {
                $crate::__private::retro_core::EmulatorCore::input_manager(&self.$core)
                    .get_port_state(port as usize)
            }

            /// Make an input provider the only one on a controller port (an
            /// empty id clears the port). `setProviderPort` moves a provider
            /// and keeps the others.
            #[wasm_bindgen(js_name = assignProviderToPort)]
            pub fn assign_provider_to_port(
                &mut self,
                provider_id: ::std::string::String,
                port: u32,
            ) {
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .assign_provider_to_port(provider_id, port as usize);
            }

            /// Put an input provider on a controller port, beside whatever
            /// else feeds it, after taking it off every other port. A
            /// negative `port` takes it off every port, so its input reaches
            /// no player.
            #[wasm_bindgen(js_name = setProviderPort)]
            pub fn set_provider_port(&mut self, provider_id: ::std::string::String, port: i32) {
                let port = usize::try_from(port).ok();
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .set_provider_port(&provider_id, port);
            }

            /// The controller port an input provider feeds, or -1 when none.
            #[wasm_bindgen(js_name = getProviderPort)]
            pub fn get_provider_port(&self, provider_id: ::std::string::String) -> i32 {
                $crate::__private::retro_core::EmulatorCore::input_manager(&self.$core)
                    .provider_port(&provider_id)
                    .map_or(-1, |port| port as i32)
            }

            /// Every input provider feeding a controller port (`string[]`).
            #[wasm_bindgen(js_name = getPortProviders)]
            pub fn get_port_providers(
                &self,
                port: u32,
            ) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js(
                    $crate::__private::retro_core::EmulatorCore::input_manager(&self.$core)
                        .port_providers(port as usize),
                )
            }

            /// Register a gamepad the core has not seen yet, with the core's
            /// default button map, feeding no player. Returns whether it was
            /// new.
            #[wasm_bindgen(js_name = addGamepad)]
            pub fn add_gamepad(&mut self, gamepad_id: ::std::string::String) -> bool {
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .add_gamepad(&gamepad_id)
            }

            /// Exchange everything feeding port 0 with everything feeding
            /// port 1.
            #[wasm_bindgen(js_name = swapPortAssignments)]
            pub fn swap_port_assignments(&mut self) {
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .swap_ports(0, 1);
            }

            /// Set the keyboard's map, `{ "KeyA": 6, "ArrowUp": 0, ... }` (a
            /// key code to a button's bit). Returns `false`, keeping the
            /// current map, when `mapping` is not such an object.
            #[wasm_bindgen(js_name = setKeyboardMapping)]
            pub fn set_keyboard_mapping(
                &mut self,
                mapping: $crate::__private::wasm_bindgen::JsValue,
            ) -> bool {
                let ::core::option::Option::Some(map) =
                    $crate::input::keyboard_map_from_js(mapping)
                else {
                    return false;
                };
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .set_keyboard_mapping(map);
                true
            }

            /// The keyboard's map, a plain object of key codes.
            #[wasm_bindgen(js_name = getKeyboardMapping)]
            pub fn get_keyboard_mapping(&self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js(
                    &$crate::__private::retro_core::EmulatorCore::input_manager(&self.$core)
                        .get_keyboard_mapping(),
                )
            }

            /// Release every key the keyboard holds.
            #[wasm_bindgen(js_name = clearKeyboardState)]
            pub fn clear_keyboard_state(&mut self) {
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .clear_keyboard_state();
            }

            /// Set a pad's button map, `{ 0: 6, 1: 4, ... }` (the pad's
            /// button index to a button's bit), whose keys JavaScript holds
            /// as text, or a `Map` with number keys. Returns `false`,
            /// keeping the pad's current map, when `mapping` is neither.
            #[wasm_bindgen(js_name = setGamepadMapping)]
            pub fn set_gamepad_mapping(
                &mut self,
                gamepad_id: ::std::string::String,
                mapping: $crate::__private::wasm_bindgen::JsValue,
            ) -> bool {
                let ::core::option::Option::Some(map) = $crate::input::gamepad_map_from_js(mapping)
                else {
                    return false;
                };
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .set_gamepad_mapping(&gamepad_id, map);
                true
            }

            /// A pad's button map, as a plain object keyed by the button
            /// index written as text (`{ "0": 6, ... }`).
            #[wasm_bindgen(js_name = getGamepadMapping)]
            pub fn get_gamepad_mapping(
                &self,
                gamepad_id: ::std::string::String,
            ) -> $crate::__private::wasm_bindgen::JsValue {
                let map = $crate::__private::retro_core::EmulatorCore::input_manager(&self.$core)
                    .get_gamepad_mapping(&gamepad_id);
                $crate::js::to_js(
                    &$crate::__private::retro_core::input::gamepad_map_with_text_keys(&map),
                )
            }

            /// Set a pad's dead zone (0 to 1).
            #[wasm_bindgen(js_name = setGamepadDeadzone)]
            pub fn set_gamepad_deadzone(
                &mut self,
                gamepad_id: ::std::string::String,
                deadzone: f32,
            ) {
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .set_gamepad_deadzone(&gamepad_id, deadzone);
            }

            /// A pad's dead zone.
            #[wasm_bindgen(js_name = getGamepadDeadzone)]
            pub fn get_gamepad_deadzone(&self, gamepad_id: ::std::string::String) -> f32 {
                $crate::__private::retro_core::EmulatorCore::input_manager(&self.$core)
                    .get_gamepad_deadzone(&gamepad_id)
            }

            /// Release everything a pad holds, its buttons and its axes.
            #[wasm_bindgen(js_name = clearGamepadState)]
            pub fn clear_gamepad_state(&mut self, gamepad_id: ::std::string::String) {
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .clear_gamepad_state(&gamepad_id);
            }

            /// Split the keyboard between two players, each half with its map
            /// (`{ "KeyA": 6, ... }`). Returns `false`, changing nothing,
            /// when either map is not such an object.
            #[wasm_bindgen(js_name = enableSplitKeyboard)]
            pub fn enable_split_keyboard(
                &mut self,
                mapping_p1: $crate::__private::wasm_bindgen::JsValue,
                mapping_p2: $crate::__private::wasm_bindgen::JsValue,
            ) -> bool {
                let (::core::option::Option::Some(map_p1), ::core::option::Option::Some(map_p2)) = (
                    $crate::input::keyboard_map_from_js(mapping_p1),
                    $crate::input::keyboard_map_from_js(mapping_p2),
                ) else {
                    return false;
                };
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .enable_split_keyboard(map_p1, map_p2);
                true
            }

            /// Join the keyboard's halves again, one keyboard for player 1.
            #[wasm_bindgen(js_name = disableSplitKeyboard)]
            pub fn disable_split_keyboard(&mut self) {
                $crate::__private::retro_core::EmulatorCore::input_manager_mut(&mut self.$core)
                    .disable_split_keyboard();
            }

            /// Whether the keyboard is split between two players.
            #[wasm_bindgen(js_name = isSplitKeyboardEnabled)]
            pub fn is_split_keyboard_enabled(&self) -> bool {
                $crate::__private::retro_core::EmulatorCore::input_manager(&self.$core)
                    .is_split_keyboard_enabled()
            }
        }
    };
}
