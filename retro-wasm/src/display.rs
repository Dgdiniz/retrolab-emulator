//! [`display_bindings!`], which writes the picture's methods every binding
//! crate exports the same way, and [`PresentBuffer`], the packed picture a
//! console hands the host when its framebuffer is wider than the picture.
//!
//! [`display_bindings!`]: crate::display_bindings

use retro_core::{DisplayInfo, Framebuffer};

/// Pack the active display rectangle out of a framebuffer into `out`,
/// tightly (`width * height * 4` bytes, row by row), and return its
/// `(width, height)`: the display's size cut to the framebuffer's, and to
/// the rows the framebuffer holds.
pub fn pack_active_rgba(fb: &Framebuffer, disp: &DisplayInfo, out: &mut Vec<u8>) -> (u32, u32) {
    let stride = fb.width as usize * 4;
    let backed_rows = fb.data.len().checked_div(stride).unwrap_or(0);
    let w = disp.width.min(fb.width);
    let h = disp.height.min(fb.height).min(backed_rows as u32);
    let row_bytes = w as usize * 4;
    out.clear();
    out.reserve(row_bytes * h as usize);
    for y in 0..h as usize {
        let base = y * stride;
        out.extend_from_slice(&fb.data[base..base + row_bytes]);
    }
    (w, h)
}

/// The picture a host is handed, packed out of a framebuffer a console
/// sizes for its largest picture (the SNES's 512 by 480), so the host never
/// reads the framebuffer's stride. A binding crate refreshes it after every
/// call that can change the picture.
#[derive(Debug, Default)]
pub struct PresentBuffer {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

impl PresentBuffer {
    /// An empty picture, 0 by 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pack the display's rectangle of `framebuffer` ([`pack_active_rgba`]).
    pub fn refresh(&mut self, framebuffer: &Framebuffer, display: &DisplayInfo) {
        let (width, height) = pack_active_rgba(framebuffer, display, &mut self.data);
        self.width = width;
        self.height = height;
    }

    /// The packed RGBA bytes, `width * height * 4` of them.
    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// The picture's width in pixels.
    #[inline]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The picture's height in pixels.
    #[inline]
    pub fn height(&self) -> u32 {
        self.height
    }
}

/// The picture's RGBA bytes: the core's framebuffer, or the present buffer
/// in the field named.
#[doc(hidden)]
#[macro_export]
macro_rules! __picture_data {
    ($self:ident, $core:ident) => {
        &$crate::__private::retro_core::EmulatorCore::get_framebuffer(&$self.$core).data[..]
    };
    ($self:ident, $core:ident, $present:ident) => {
        $self.$present.data()
    };
}

/// The picture's width in pixels: the core's framebuffer's, or the present
/// buffer's.
#[doc(hidden)]
#[macro_export]
macro_rules! __picture_width {
    ($self:ident, $core:ident) => {
        $crate::__private::retro_core::EmulatorCore::get_framebuffer(&$self.$core).width
    };
    ($self:ident, $core:ident, $present:ident) => {
        $self.$present.width()
    };
}

/// The picture's height in pixels: the core's framebuffer's, or the present
/// buffer's.
#[doc(hidden)]
#[macro_export]
macro_rules! __picture_height {
    ($self:ident, $core:ident) => {
        $crate::__private::retro_core::EmulatorCore::get_framebuffer(&$self.$core).height
    };
    ($self:ident, $core:ident, $present:ident) => {
        $self.$present.height()
    };
}

/// Writes the picture's methods on `$emulator`, over the `EmulatorCore` in
/// its field `$core`: `getFramebuffer()` and `getDisplayInfo()` (the
/// core's own), and the fast readers `getFramebufferData()`,
/// `getFramebufferWidth()`, `getFramebufferHeight()`, `getFramebufferPtr()`
/// and `getFramebufferLen()`, which read the core's framebuffer, or with
/// `present = field` the [`PresentBuffer`] in that field.
///
/// Each method's Rust name is its JavaScript name in snake case, which no
/// other method of the struct may have. The crate expanding the macro
/// depends on `wasm-bindgen` itself, as every crate declaring a
/// `#[wasm_bindgen]` struct does.
#[macro_export]
macro_rules! display_bindings {
    ($emulator:ident, $core:ident $(, present = $present:ident)? $(,)?) => {
        #[$crate::__private::wasm_bindgen::prelude::wasm_bindgen]
        impl $emulator {
            /// The core's framebuffer as a JavaScript object, at its full
            /// size (slow: it is serialized). Prefer the fast readers
            /// `getFramebufferData`, `getFramebufferPtr` and their sizes.
            #[wasm_bindgen(js_name = getFramebuffer)]
            pub fn get_framebuffer(&self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js($crate::__private::retro_core::EmulatorCore::get_framebuffer(
                    &self.$core,
                ))
            }

            /// The picture's RGBA pixels, copied into a `Uint8ClampedArray`,
            /// ready for an `ImageData`.
            #[wasm_bindgen(js_name = getFramebufferData)]
            pub fn get_framebuffer_data(&self) -> $crate::__private::js_sys::Uint8ClampedArray {
                let data: &[u8] = $crate::__picture_data!(self, $core $(, $present)?);
                let arr = $crate::__private::js_sys::Uint8ClampedArray::new_with_length(
                    data.len() as u32,
                );
                arr.copy_from(data);
                arr
            }

            /// The picture's width in pixels.
            #[wasm_bindgen(js_name = getFramebufferWidth)]
            pub fn get_framebuffer_width(&self) -> u32 {
                $crate::__picture_width!(self, $core $(, $present)?)
            }

            /// The picture's height in pixels.
            #[wasm_bindgen(js_name = getFramebufferHeight)]
            pub fn get_framebuffer_height(&self) -> u32 {
                $crate::__picture_height!(self, $core $(, $present)?)
            }

            /// Where the picture's RGBA pixels start in the module's memory
            /// (no copy). With `getFramebufferLen` it makes a live view,
            /// `new Uint8Array(memory.buffer, ptr, len)`, which must be made
            /// again when the module's memory grows.
            #[wasm_bindgen(js_name = getFramebufferPtr)]
            pub fn get_framebuffer_ptr(&self) -> *const u8 {
                let data: &[u8] = $crate::__picture_data!(self, $core $(, $present)?);
                data.as_ptr()
            }

            /// The length in bytes of the picture's RGBA pixels
            /// (`width * height * 4`).
            #[wasm_bindgen(js_name = getFramebufferLen)]
            pub fn get_framebuffer_len(&self) -> u32 {
                let data: &[u8] = $crate::__picture_data!(self, $core $(, $present)?);
                data.len() as u32
            }

            /// The display's description: the size of the picture the core's
            /// framebuffer holds, its aspect, refresh rate and pixel format,
            /// and `grid` (`{ columns_per_dot, rows_per_line, woven,
            /// first_dot, first_line }`), how the picture lies over the
            /// console's dots and lines: the picture columns a dot takes,
            /// the rows a line gives, whether a line's two rows are two
            /// fields woven, and the dot and the line the picture's first
            /// column and row show, counted as a sprite's `x` and `y` count
            /// them (from the first dot of a line and the first line the
            /// frame draws).
            #[wasm_bindgen(js_name = getDisplayInfo)]
            pub fn get_display_info(&self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js($crate::__private::retro_core::EmulatorCore::get_display_info(
                    &self.$core,
                ))
            }
        }
    };
}
