//! What the binding crates' tests compare JavaScript values with: a full,
//! deterministic description of a value's shape and contents, so that a
//! test can record what a reader returns today and hold every later build
//! to it; and the property reader every such test needs.

use std::fmt::Write;

use js_sys::{Array, ArrayBuffer, Function, Map, Object, Reflect, Set, Uint8Array};
use retro_core::hash::fnv1a64;
use wasm_bindgen::{JsCast, JsValue};

/// A description of `value`, the same for every two values a host cannot
/// tell apart by their properties, types and bytes:
///
/// - `null`, `undefined`, `true`, `false`;
/// - a number as Rust prints an `f64` (`5`, `1.5`, `-0`, `NaN`, `inf`); a
///   bigint with an `n` after it (`5n`);
/// - a string quoted, as Rust's `Debug` quotes it (`"text"`);
/// - an array, element by element: `[1, "a", null]`;
/// - a typed array, its constructor's name, length and the FNV-1a hash of
///   its bytes, and `view` when it is a view into this module's memory
///   rather than a copy: `Uint16Array(len 3, fnv1a64 0x...)`; an
///   `ArrayBuffer`, its length in bytes and their hash;
/// - an object, its own enumerable properties sorted by name, each with its
///   value's description: `{a: 1, b: [2]}` for a plain object, the
///   constructor's name before the braces for any other (`Date{}`), and
///   `null-prototype{...}` for one with no prototype; a `Map`, its entries
///   sorted (`Map{"a" => 1}`); a `Set`, its values sorted (`Set{1, 2}`);
/// - a function, `function` and its name; a symbol, `symbol`.
pub fn shape(value: &JsValue) -> String {
    let mut out = String::new();
    describe(value, &mut out);
    out
}

/// `value[key]`, as a host reads it: `undefined` for a key the value lacks.
///
/// # Panics
///
/// When `value` is not an object a property can be read from.
pub fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

/// Write the description of `value` to `out`.
fn describe(value: &JsValue, out: &mut String) {
    if value.is_null() {
        out.push_str("null");
    } else if value.is_undefined() {
        out.push_str("undefined");
    } else if let Some(flag) = value.as_bool() {
        out.push_str(if flag { "true" } else { "false" });
    } else if let Some(number) = value.as_f64() {
        let _ = write!(out, "{number}");
    } else if value.is_bigint() {
        let digits = value
            .unchecked_ref::<js_sys::BigInt>()
            .to_string(10)
            .map(String::from)
            .unwrap_or_default();
        let _ = write!(out, "{digits}n");
    } else if let Some(text) = value.as_string() {
        let _ = write!(out, "{text:?}");
    } else if value.is_symbol() {
        out.push_str("symbol");
    } else if value.is_function() {
        let name = String::from(value.unchecked_ref::<Function>().name());
        let _ = write!(out, "function {name}");
    } else if Array::is_array(value) {
        let items: Vec<String> = value
            .unchecked_ref::<Array>()
            .iter()
            .map(|item| shape(&item))
            .collect();
        let _ = write!(out, "[{}]", items.join(", "));
    } else if ArrayBuffer::is_view(value) {
        describe_view(value, out);
    } else if value.is_instance_of::<ArrayBuffer>() {
        let bytes = Uint8Array::new(value).to_vec();
        let _ = write!(
            out,
            "ArrayBuffer(len {}, fnv1a64 {:#018x})",
            bytes.len(),
            fnv1a64(&bytes)
        );
    } else if value.is_instance_of::<Map>() {
        let mut entries = Vec::new();
        value.unchecked_ref::<Map>().for_each(&mut |item, key| {
            entries.push(format!("{} => {}", shape(&key), shape(&item)))
        });
        entries.sort();
        let _ = write!(out, "Map{{{}}}", entries.join(", "));
    } else if value.is_instance_of::<Set>() {
        let mut items = Vec::new();
        value
            .unchecked_ref::<Set>()
            .for_each(&mut |item, _, _| items.push(shape(&item)));
        items.sort();
        let _ = write!(out, "Set{{{}}}", items.join(", "));
    } else {
        describe_object(value, out);
    }
}

/// Write the description of a typed array or a `DataView`.
fn describe_view(value: &JsValue, out: &mut String) {
    let property = |name: &str| Reflect::get(value, &JsValue::from_str(name)).unwrap_or_default();
    let number = |name: &str| property(name).as_f64().unwrap_or(0.0) as u32;
    let buffer = property("buffer");
    // Asked before anything is allocated: memory growing would give the
    // module a new buffer and leave a view on the old one.
    let memory =
        Reflect::get(&wasm_bindgen::memory(), &JsValue::from_str("buffer")).unwrap_or_default();
    let is_view = buffer == memory;
    let length = if property("length").is_undefined() {
        number("byteLength")
    } else {
        number("length")
    };
    let bytes = Uint8Array::new_with_byte_offset_and_length(
        &buffer,
        number("byteOffset"),
        number("byteLength"),
    )
    .to_vec();
    let _ = write!(
        out,
        "{}(len {length}, fnv1a64 {:#018x}",
        constructor_name(value).unwrap_or_default(),
        fnv1a64(&bytes)
    );
    if is_view {
        out.push_str(", view");
    }
    out.push(')');
}

/// Write the description of an object: its kind, then its own enumerable
/// properties sorted by name.
fn describe_object(value: &JsValue, out: &mut String) {
    let prototype = Object::get_prototype_of(value);
    if prototype.is_null() {
        out.push_str("null-prototype");
    } else if JsValue::from(prototype) != JsValue::from(Object::get_prototype_of(&Object::new())) {
        out.push_str(&constructor_name(value).unwrap_or_default());
    }
    let mut keys: Vec<String> = Object::keys(value.unchecked_ref::<Object>())
        .iter()
        .filter_map(|key| key.as_string())
        .collect();
    keys.sort();
    let members: Vec<String> = keys
        .iter()
        .map(|key| {
            let member = Reflect::get(value, &JsValue::from_str(key)).unwrap_or_default();
            format!("{key}: {}", shape(&member))
        })
        .collect();
    let _ = write!(out, "{{{}}}", members.join(", "));
}

/// The name of the function that constructed `value`, through its
/// prototype's `constructor`.
fn constructor_name(value: &JsValue) -> Option<String> {
    let constructor = Reflect::get(value, &JsValue::from_str("constructor")).ok()?;
    let constructor = constructor.dyn_ref::<Function>()?;
    Some(String::from(constructor.name()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    fn object(entries: &[(&str, JsValue)]) -> JsValue {
        let obj = Object::new();
        for (key, value) in entries {
            Reflect::set(&obj, &JsValue::from_str(key), value).expect("an object takes a key");
        }
        obj.into()
    }

    #[wasm_bindgen_test]
    fn the_simple_values_describe_themselves() {
        assert_eq!(shape(&JsValue::NULL), "null");
        assert_eq!(shape(&JsValue::UNDEFINED), "undefined");
        assert_eq!(shape(&JsValue::TRUE), "true");
        assert_eq!(shape(&JsValue::FALSE), "false");
        assert_eq!(shape(&JsValue::from_f64(5.0)), "5");
        assert_eq!(shape(&JsValue::from_f64(1.5)), "1.5");
        assert_eq!(shape(&JsValue::from_f64(-0.0)), "-0");
        assert_eq!(shape(&JsValue::from_f64(f64::NAN)), "NaN");
        assert_eq!(shape(&JsValue::from_str("a \"b\"")), "\"a \\\"b\\\"\"");
        assert_eq!(shape(&js_sys::BigInt::from(5u64).into()), "5n");
        assert_eq!(shape(&js_sys::Symbol::for_("s").into()), "symbol");
    }

    #[wasm_bindgen_test]
    fn an_array_is_described_element_by_element() {
        let array = Array::of3(
            &JsValue::from_f64(1.0),
            &JsValue::from_str("a"),
            &Array::of1(&JsValue::NULL),
        );
        assert_eq!(shape(&array.into()), "[1, \"a\", [null]]");
        assert_eq!(shape(&Array::new().into()), "[]");
    }

    #[wasm_bindgen_test]
    fn a_typed_array_is_its_constructor_length_and_bytes() {
        let bytes = [1u8, 2, 3, 4];
        let copy: JsValue = Uint8Array::from(&bytes[..]).into();
        assert_eq!(
            shape(&copy),
            format!("Uint8Array(len 4, fnv1a64 {:#018x})", fnv1a64(&bytes))
        );
        let words: JsValue = js_sys::Uint16Array::from(&[0x0201u16, 0x0403][..]).into();
        assert_eq!(
            shape(&words),
            format!("Uint16Array(len 2, fnv1a64 {:#018x})", fnv1a64(&bytes)),
            "the bytes as the array holds them"
        );
        let signed: JsValue = js_sys::Int16Array::from(&[0x0201i16, 0x0403][..]).into();
        assert!(shape(&signed).starts_with("Int16Array(len 2, "));
        assert_ne!(
            shape(&copy),
            shape(&Uint8Array::from(&[1u8, 2, 3, 5][..]).into())
        );
    }

    #[wasm_bindgen_test]
    fn a_view_into_the_module_s_memory_is_told_from_a_copy() {
        let bytes = vec![9u8; 8];
        // A view must not outlive the bytes, and nothing may allocate while
        // it is read.
        let view: JsValue = unsafe { Uint8Array::view(&bytes) }.into();
        let described = shape(&view);
        assert_eq!(
            described,
            format!("Uint8Array(len 8, fnv1a64 {:#018x}, view)", fnv1a64(&bytes))
        );
        drop(bytes);
    }

    #[wasm_bindgen_test]
    fn an_object_lists_its_properties_sorted_and_says_what_it_is() {
        let plain = object(&[
            ("width", JsValue::from_f64(2.0)),
            ("data", Array::new().into()),
            ("fromLineStates", JsValue::TRUE),
        ]);
        assert_eq!(shape(&plain), "{data: [], fromLineStates: true, width: 2}");

        let nested = object(&[(
            "inner",
            object(&[("b", JsValue::NULL), ("a", JsValue::UNDEFINED)]),
        )]);
        assert_eq!(shape(&nested), "{inner: {a: undefined, b: null}}");

        let bare: JsValue = Object::create(JsValue::NULL.unchecked_ref()).into();
        Reflect::set(&bare, &JsValue::from_str("x"), &JsValue::from_f64(1.0)).unwrap();
        assert_eq!(shape(&bare), "null-prototype{x: 1}");

        let date: JsValue = js_sys::Date::new_0().into();
        assert_eq!(shape(&date), "Date{}");
    }

    #[wasm_bindgen_test]
    fn a_map_is_never_taken_for_a_plain_object() {
        let map = Map::new();
        map.set(&JsValue::from_str("b"), &JsValue::from_f64(2.0));
        map.set(&JsValue::from_str("a"), &JsValue::from_f64(1.0));
        assert_eq!(shape(&map.into()), "Map{\"a\" => 1, \"b\" => 2}");
        let plain = object(&[("a", JsValue::from_f64(1.0)), ("b", JsValue::from_f64(2.0))]);
        assert_eq!(shape(&plain), "{a: 1, b: 2}");

        let set = Set::new(&JsValue::UNDEFINED);
        set.add(&JsValue::from_f64(2.0));
        set.add(&JsValue::from_f64(1.0));
        assert_eq!(shape(&set.into()), "Set{1, 2}");
    }

    #[wasm_bindgen_test]
    fn a_function_is_named() {
        let function = Function::new_no_args("return 1");
        assert_eq!(shape(&function.into()), "function anonymous");
    }
}
