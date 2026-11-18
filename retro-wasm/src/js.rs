//! The JavaScript values every binding crate builds the same way: plain
//! objects, typed-array copies owned by JavaScript, values serialized with
//! every map as a plain object, and a savestate's refusal.

use js_sys::{Object, Reflect};
use retro_core::savestate::LoadStateError;
use serde::Serialize;
use wasm_bindgen::JsValue;

/// A `loadState` refusal for JS. A savestate made on another ROM is an
/// `Error` named `SaveStateOtherRom`, what the platform reads to offer
/// loading it anyway (`loadStateAnyRom`); any other refusal is its reason
/// as text.
pub fn load_state_refusal(error: LoadStateError) -> JsValue {
    match &error {
        LoadStateError::OtherRom { .. } => {
            let err = js_sys::Error::new(&error.to_string());
            err.set_name("SaveStateOtherRom");
            err.into()
        }
        LoadStateError::Unreadable(reason) => JsValue::from_str(reason),
    }
}

/// A plain JS object with these properties, in this order.
pub fn plain_object(entries: &[(&str, JsValue)]) -> JsValue {
    let obj = Object::new();
    for (key, value) in entries {
        set_property(&obj, key, value);
    }
    obj.into()
}

/// `value` serialized with every map, and every `serde_json` object, as a
/// plain JS object instead of a `Map` (which a host reading `x[key]` sees as
/// empty); a struct is a plain object either way. The keys of a map must be
/// text. A value the serializer refuses gives `undefined` rather than a
/// panic, which would leave the whole module refusing every later call.
pub fn map_as_object<T: Serialize + ?Sized>(value: &T) -> JsValue {
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    value.serialize(&serializer).unwrap_or(JsValue::UNDEFINED)
}

/// A `Uint8Array` holding a copy of `values`, owned by JavaScript: not a
/// view into this module's memory, which a later call can move or reuse.
pub fn uint8_array(values: &[u8]) -> JsValue {
    js_sys::Uint8Array::from(values).into()
}

/// A `Uint16Array` holding a copy of `values` ([`uint8_array`]).
pub fn uint16_array(values: &[u16]) -> JsValue {
    js_sys::Uint16Array::from(values).into()
}

/// A `Uint32Array` holding a copy of `values` ([`uint8_array`]).
pub fn uint32_array(values: &[u32]) -> JsValue {
    js_sys::Uint32Array::from(values).into()
}

/// An `Int16Array` holding a copy of `values` ([`uint8_array`]).
pub fn int16_array(values: &[i16]) -> JsValue {
    js_sys::Int16Array::from(values).into()
}

/// Set property `key` of `target` to `value`. Setting a property of an
/// object cannot fail.
pub fn set_property(target: &JsValue, key: &str, value: &JsValue) {
    let _ = Reflect::set(target, &JsValue::from_str(key), value);
}
