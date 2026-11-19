//! [`audio_bindings!`], which writes the methods every binding crate exports
//! the same way for the host's sound: the ring of samples a machine made
//! and the host has not taken yet, and the switch that makes them.
//!
//! [`audio_bindings!`]: crate::audio_bindings

/// Writes the host's sound methods on `$emulator`, over the `EmulatorCore`
/// in its field `$core`: `getAudioSamples()`, `getAudioSamplesN(n)`,
/// `getAudioAvailable()`, `getAudioRingOverflows()`, `clearAudioBuffer()`
/// (a no-op kept for older hosts), `discardPendingAudio()`,
/// `getAudioInfo()`, `setAudioEnabled(enabled)` and `isAudioEnabled()`.
///
/// Each method's Rust name is its JavaScript name in snake case, which no
/// other method of the struct may have. The crate expanding the macro
/// depends on `wasm-bindgen` itself, as every crate declaring a
/// `#[wasm_bindgen]` struct does.
#[macro_export]
macro_rules! audio_bindings {
    ($emulator:ident, $core:ident $(,)?) => {
        #[$crate::__private::wasm_bindgen::prelude::wasm_bindgen]
        impl $emulator {
            /// Take every queued sample from the host's ring into a new
            /// `Float32Array` (interleaved stereo, -1 to 1). Each call
            /// empties the ring, so a host never needs `clearAudioBuffer`
            /// between drains. A host that pulls a fixed lookahead prefers
            /// `getAudioSamplesN(n)`.
            #[wasm_bindgen(js_name = getAudioSamples)]
            pub fn get_audio_samples(&mut self) -> ::std::vec::Vec<f32> {
                $crate::__private::retro_core::EmulatorCore::host_audio_mut(&mut self.$core)
                    .take_all()
            }

            /// Take at most `n` queued samples from the host's ring: a
            /// `Float32Array` of `min(n, getAudioAvailable())` samples.
            /// For an `AudioWorklet` that pulls a fixed lookahead each
            /// tick: it neither drains more than it wants (silence on the
            /// next tick) nor gets a buffer larger than it asked for.
            #[wasm_bindgen(js_name = getAudioSamplesN)]
            pub fn get_audio_samples_n(&mut self, n: usize) -> ::std::vec::Vec<f32> {
                let available =
                    $crate::__private::retro_core::EmulatorCore::host_audio(&self.$core)
                        .available();
                let mut out = ::std::vec![0.0_f32; n.min(available)];
                let written =
                    $crate::__private::retro_core::EmulatorCore::host_audio_mut(&mut self.$core)
                        .drain_into(&mut out);
                out.truncate(written);
                out
            }

            /// The samples queued in the host's ring, both channels counted
            /// (`1470` is 735 stereo pairs), to size a `getAudioSamplesN`
            /// pull.
            #[wasm_bindgen(js_name = getAudioAvailable)]
            pub fn get_audio_available(&self) -> usize {
                $crate::__private::retro_core::EmulatorCore::host_audio(&self.$core).available()
            }

            /// The samples the ring has dropped from its front so far
            /// because new ones did not fit: above 0, the host is behind.
            ///
            /// There is no underrun counterpart, on purpose. The ring can
            /// only record an underrun when a drain asks for more than it
            /// holds, and every path out of here sizes its request from the
            /// queued count first, so such a counter could never move, and
            /// the host would read a constant zero as though it were a
            /// measurement.
            #[wasm_bindgen(js_name = getAudioRingOverflows)]
            pub fn get_audio_ring_overflows(&self) -> u64 {
                $crate::__private::retro_core::EmulatorCore::host_audio(&self.$core).overflows()
            }

            /// **Deprecated, and on purpose a no-op.** The `getAudioSamples`
            /// family drains the ring, so a host never needs to clear it;
            /// clearing it between drains would race the producer and click.
            /// Kept so an older host that still calls it keeps working. Do
            /// not call it from new code.
            #[wasm_bindgen(js_name = clearAudioBuffer)]
            pub fn clear_audio_buffer(&mut self) {}

            /// Drop sound made but not yet handed to the host.
            ///
            /// Not `clearAudioBuffer`, which would drop what is already
            /// queued *for* the host, sound the player is about to hear:
            /// this drops only what a headless frame left waiting, which
            /// after a rollback's frames run again is sound already heard in
            /// its mispredicted form. Safe in the middle of a session: it
            /// touches no serialized state, so it cannot move the netplay
            /// state hash.
            #[wasm_bindgen(js_name = discardPendingAudio)]
            pub fn discard_pending_audio(&mut self) {
                $crate::__private::retro_core::EmulatorCore::discard_pending_audio(
                    &mut self.$core,
                );
            }

            /// The host sound's description: sample rate, channels and bits.
            #[wasm_bindgen(js_name = getAudioInfo)]
            pub fn get_audio_info(&self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js($crate::__private::retro_core::EmulatorCore::get_audio_info(
                    &self.$core,
                ))
            }

            /// Make sound (`true`) or not (`false`, the default). Switched
            /// off, the sound chips still answer their registers but no
            /// sample is made.
            #[wasm_bindgen(js_name = setAudioEnabled)]
            pub fn set_audio_enabled(&mut self, enabled: bool) {
                $crate::__private::retro_core::EmulatorCore::set_audio_enabled(
                    &mut self.$core,
                    enabled,
                );
            }

            /// Whether sound is made.
            #[wasm_bindgen(js_name = isAudioEnabled)]
            pub fn is_audio_enabled(&self) -> bool {
                $crate::__private::retro_core::EmulatorCore::is_audio_enabled(&self.$core)
            }
        }
    };
}
