//! The JavaScript values every binding crate builds the same way: plain
//! objects, typed-array copies owned by JavaScript, values serialized with
//! every map as a plain object, pictures, and the refusals a host tells
//! apart by name.

use js_sys::{Object, Reflect};
use retro_core::savestate::LoadStateError;
use retro_core::video::{IndexedImage, PixelImage};
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

/// A `loadRom` refusal for a cartridge whose chip this build has no module
/// for: an `Error` named `UnsupportedChip` with the chip's name in its
/// `chip` property, which a host reads to run the game on another core.
pub fn unsupported_chip_error(chip: &str, message: &str) -> JsValue {
    let err = js_sys::Error::new(message);
    err.set_name("UnsupportedChip");
    set_property(&err, "chip", &JsValue::from_str(chip));
    err.into()
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
pub fn to_js<T: Serialize + ?Sized>(value: &T) -> JsValue {
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

/// `{ width, height, data: Uint8Array }` for an RGBA picture, its bytes
/// copied.
pub fn picture_object(picture: &PixelImage) -> JsValue {
    plain_object(&[
        ("width", JsValue::from_f64(f64::from(picture.width))),
        ("height", JsValue::from_f64(f64::from(picture.height))),
        ("data", uint8_array(&picture.rgba)),
    ])
}

/// `{ width, height, pixels: Uint8Array, bpp, transparentIndex }` for an
/// indexed sheet, one palette index a pixel copied; `transparentIndex` is
/// `null` for a sheet with no transparent index.
pub fn indexed_image_object(image: &IndexedImage) -> JsValue {
    let transparent = match image.transparent_index {
        Some(index) => JsValue::from_f64(f64::from(index)),
        None => JsValue::NULL,
    };
    plain_object(&[
        ("width", JsValue::from_f64(f64::from(image.width))),
        ("height", JsValue::from_f64(f64::from(image.height))),
        ("pixels", uint8_array(&image.pixels)),
        ("bpp", JsValue::from_f64(f64::from(image.bpp))),
        ("transparentIndex", transparent),
    ])
}
