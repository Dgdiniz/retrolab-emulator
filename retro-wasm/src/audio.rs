//! [`audio_bindings!`], which writes the methods every binding crate exports
//! the same way for the host's sound: the ring of samples a machine made
//! and the host has not taken yet, the switch that makes them, and the
//! sound chips as the debugger sees them.
//!
//! [`audio_bindings!`]: crate::audio_bindings

use retro_core::audio::{
    encode_wav_samples, wav_with_loop, AudioChipInfo, ChannelState, SampleKind, SoundChipKind,
    SoundLog, SoundSamples, StemTake, WavMarks,
};
use retro_core::EmulatorCore;
use wasm_bindgen::{JsCast, JsValue};

use crate::js::{int16_array, plain_object, to_js, uint32_array, uint8_array};

/// Every sound chip's description, chip 0 the console's main chip.
pub fn chips<E: EmulatorCore>(core: &mut E) -> Vec<AudioChipInfo> {
    (0..core.audio_chip_count())
        .filter_map(|chip| {
            core.audio_debugger(chip)
                .map(|debugger| debugger.chip_info())
        })
        .collect()
}

/// Every channel of sound chip `chip`, in channel order (`None` where a
/// channel reports no state); `None` for a chip the console lacks.
pub fn chip_channels<E: EmulatorCore>(
    core: &mut E,
    chip: usize,
) -> Option<Vec<Option<ChannelState>>> {
    let debugger = core.audio_debugger(chip)?;
    Some(
        (0..debugger.get_channel_count())
            .map(|channel| debugger.get_channel_state(channel))
            .collect(),
    )
}

/// A buffer of samples for the host: an `Int16Array` or a `Float32Array`,
/// as the console makes them, copied.
pub fn samples_array(samples: &SoundSamples) -> JsValue {
    match samples {
        SoundSamples::I16(values) => int16_array(values),
        SoundSamples::F32(values) => js_sys::Float32Array::from(&values[..]).into(),
    }
}

/// A take of the stems for the host: `{ frames, armed, stems, master,
/// departures, dropped }`, each stem and the master a buffer of the
/// console's samples ([`samples_array`]), stereo ones interleaved, and the
/// departures a `Uint32Array` of `[start, frames]` pairs.
pub fn stem_take(take: &StemTake) -> JsValue {
    let stems = js_sys::Array::new();
    for stem in &take.stems {
        stems.push(&samples_array(stem));
    }
    let departures: Vec<u32> = take
        .departures
        .iter()
        .flat_map(|span| [span.start, span.frames])
        .collect();
    plain_object(&[
        ("frames", JsValue::from(take.frames)),
        ("armed", JsValue::from(take.armed.bits() as f64)),
        ("stems", stems.into()),
        ("master", samples_array(&take.master)),
        ("departures", uint32_array(&departures)),
        ("dropped", JsValue::from(take.dropped as f64)),
    ])
}

/// A take of the sound log for the host: `{ clockHz, imageLen, interrupted,
/// chips, records }`, `chips` the chips its entries name and `records` its
/// entries in the record form, a `Uint8Array` of 24 bytes an entry.
pub fn sound_log(log: &SoundLog, chips: &[SoundChipKind]) -> JsValue {
    plain_object(&[
        ("clockHz", JsValue::from(log.clock_hz as f64)),
        ("imageLen", JsValue::from(log.image_len as f64)),
        ("interrupted", JsValue::from(log.interrupted)),
        ("chips", to_js(chips)),
        ("records", uint8_array(&log.records())),
    ])
}

/// The log a host hands back: its entries in the record form, on a clock of
/// `clock_hz`, as `takeSoundLog` gave them.
pub fn sound_log_of(records: &[u8], clock_hz: f64) -> Result<SoundLog, JsValue> {
    Ok(SoundLog {
        clock_hz: clock_hz as u64,
        entries: SoundLog::entries_of(records).map_err(|e| JsValue::from_str(&e))?,
        image_len: 0,
        interrupted: false,
    })
}

/// A value of the host's as `T`, or an error naming what was wrong.
pub fn from_js<T: serde::de::DeserializeOwned>(value: JsValue) -> Result<T, JsValue> {
    serde_wasm_bindgen::from_value(value).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// The samples a host hands a WAV writer, copied: an `Int16Array` as 16-bit
/// integers, a `Float32Array` as 32-bit floats, the two kinds a take of the
/// stems gives; an error for any other value.
pub fn samples_of(value: &JsValue) -> Result<SoundSamples, JsValue> {
    if let Some(array) = value.dyn_ref::<js_sys::Int16Array>() {
        Ok(SoundSamples::I16(array.to_vec()))
    } else if let Some(array) = value.dyn_ref::<js_sys::Float32Array>() {
        Ok(SoundSamples::F32(array.to_vec()))
    } else {
        Err(JsValue::from_str(
            "the samples are not an Int16Array or a Float32Array",
        ))
    }
}

/// The sample kind a host names as `getStemLayout` names it, `"I16"` or
/// `"F32"`; an error for any other name.
pub fn sample_kind(name: &str) -> Result<SampleKind, JsValue> {
    match name {
        "I16" => Ok(SampleKind::I16),
        "F32" => Ok(SampleKind::F32),
        _ => Err(JsValue::from_str(&format!(
            "{name:?} is not a sample kind: \"I16\" or \"F32\""
        ))),
    }
}

/// The header of a WAV of `frames` sample frames of `channels` samples of
/// `kind` at `sample_rate`, followed by `trailer_bytes` bytes of other
/// chunks ([`retro_core::audio::wav_header`]), or an error naming why a
/// WAV's header cannot hold such a file.
fn header_of(
    kind: SampleKind,
    sample_rate: u32,
    channels: u16,
    frames: u32,
    trailer_bytes: u32,
) -> Result<Vec<u8>, JsValue> {
    if channels == 0 {
        return Err(JsValue::from_str("a WAV has at least one channel"));
    }
    retro_core::audio::wav_header(kind, sample_rate, channels, frames, trailer_bytes).ok_or_else(
        || {
            let after = match trailer_bytes {
                0 => String::new(),
                bytes => format!(" and {bytes} bytes after them"),
            };
            JsValue::from_str(&format!(
                "a WAV's header cannot hold {frames} frames of {channels} channels of {kind:?} \
                 samples at {sample_rate} Hz{after}"
            ))
        },
    )
}

/// What `wavHeader` answers: the header of a WAV of `frames` sample frames
/// of `channels` samples of the kind named `kind` at `sample_rate`, whose
/// samples are followed by `trailer_bytes` bytes of other chunks, or an
/// error naming why there is none.
pub fn wav_header(
    kind: &str,
    sample_rate: u32,
    channels: u16,
    frames: u32,
    trailer_bytes: u32,
) -> Result<Vec<u8>, JsValue> {
    header_of(
        sample_kind(kind)?,
        sample_rate,
        channels,
        frames,
        trailer_bytes,
    )
}

/// What `wavTrailer` answers: the chunks a WAV carries after its samples
/// for the marks a host hands, `{ loop: [start, end], cues: [{ frame,
/// label }] }` ([`retro_core::audio::wav_trailer`]), or an error naming
/// what is wrong: marks of another shape, or a loop that ends before it
/// starts.
pub fn wav_trailer(sample_rate: u32, marks: JsValue) -> Result<Vec<u8>, JsValue> {
    let marks: WavMarks = from_js(marks)?;
    if let Some((start, end)) = marks.r#loop {
        if start > end {
            return Err(JsValue::from_str(&format!(
                "the loop from frame {start} to frame {end} ends before it starts"
            )));
        }
    }
    Ok(retro_core::audio::wav_trailer(sample_rate, &marks))
}

/// What `encodeWavSamples` answers: the samples a host hands
/// ([`samples_of`]) as a WAV of their kind
/// ([`retro_core::audio::encode_wav_samples`]), with a loop from sample
/// frame `loop_start` to `loop_end` in a `smpl` chunk
/// ([`retro_core::audio::wav_with_loop`]) when both are given; or an error
/// naming what is wrong: samples of another type, samples that are not
/// whole frames of `channels`, a loop with one end or not running forward
/// inside the frames, or a file whose sizes a WAV's header cannot hold.
pub fn wav_of(
    samples: &JsValue,
    sample_rate: u32,
    channels: u16,
    loop_start: Option<u32>,
    loop_end: Option<u32>,
) -> Result<Vec<u8>, JsValue> {
    let samples = samples_of(samples)?;
    let whole = usize::from(channels.max(1));
    if samples.len() % whole != 0 {
        return Err(JsValue::from_str(&format!(
            "{} samples are not whole frames of {channels} channels",
            samples.len()
        )));
    }
    let frames = u32::try_from(samples.len() / whole)
        .map_err(|_| JsValue::from_str("a WAV's header cannot hold so many frames"))?;
    header_of(samples.kind(), sample_rate, channels, frames, 0)?;
    let wav = encode_wav_samples(&samples, sample_rate, channels);
    match (loop_start, loop_end) {
        (None, None) => Ok(wav),
        (Some(start), Some(end)) if start <= end && end < frames => {
            Ok(wav_with_loop(wav, sample_rate, start, end))
        }
        (Some(start), Some(end)) => Err(JsValue::from_str(&format!(
            "the loop from frame {start} to frame {end} does not run forward inside the {frames} \
             frames"
        ))),
        _ => Err(JsValue::from_str(
            "a loop needs its first and its last frame",
        )),
    }
}

/// Writes the host's sound methods on `$emulator`, over the `EmulatorCore`
/// in its field `$core`: `getAudioSamples()`, `getAudioSamplesN(n)`,
/// `getAudioAvailable()`, `getAudioRingOverflows()`, `clearAudioBuffer()`
/// (a no-op kept for older hosts), `discardPendingAudio()`,
/// `getAudioInfo()`, `setAudioEnabled(enabled)` and `isAudioEnabled()`;
/// and the sound chips the debugger opens, chip 0 the console's main chip:
/// `getAudioChips()`, `getAudioChipChannels(chip)`,
/// `muteAudioChipChannel(chip, channel, muted)`,
/// `getAudioChipWaveform(chip, channel, samples)` and
/// `getAudioOutputWaveform(samples)`, the mixed sound; and the sound hooks
/// over `SoundCore`: `renderFrames(frames)`, the stems' `getStemLayout()`,
/// `armStems(mask)` and `takeStems()`, the log's `startSoundLog(writes,
/// keys, mailbox)`, `stopSoundLog()` and `takeSoundLog()`, and the
/// associated WAV writers `encodeWav(samples, sampleRate, channels)`,
/// `encodeWavSamples(samples, sampleRate, channels, loopStart, loopEnd)`,
/// `wavHeader(kind, sampleRate, channels, frames, trailerBytes)` and
/// `wavTrailer(sampleRate, marks)`.
///
/// `after_frame = m` names a method of `$emulator` run after
/// `renderFrames`, the one method here that runs frames, for a crate that
/// keeps a copy of the picture (`machine_bindings!` and
/// `capture_bindings!` take the same hook).
///
/// Each method's Rust name is its JavaScript name in snake case, which no
/// other method of the struct may have. The crate expanding the macro
/// depends on `wasm-bindgen` itself, as every crate declaring a
/// `#[wasm_bindgen]` struct does.
#[macro_export]
macro_rules! audio_bindings {
    (
        $emulator:ident, $core:ident
        $(, after_frame = $after_frame:ident)?
        $(,)?
    ) => {
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

            /// The sound chips, `AudioChipInfo[]` (`{ name, manufacturer,
            /// channels, sample_rate, stereo, channel_types }`), chip 0 the
            /// console's main chip: the index each other chip method takes.
            #[wasm_bindgen(js_name = getAudioChips)]
            pub fn get_audio_chips(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                $crate::js::to_js(&$crate::audio::chips(&mut self.$core))
            }

            /// Every channel of sound chip `chip`, `ChannelState[]` in
            /// channel order (`undefined` for a channel that reports no
            /// state), or `null` for a chip the console lacks.
            #[wasm_bindgen(js_name = getAudioChipChannels)]
            pub fn get_audio_chip_channels(
                &mut self,
                chip: usize,
            ) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::audio::chip_channels(&mut self.$core, chip) {
                    ::core::option::Option::Some(channels) => $crate::js::to_js(&channels),
                    ::core::option::Option::None => {
                        $crate::__private::wasm_bindgen::JsValue::NULL
                    }
                }
            }

            /// Mute or unmute a channel of sound chip `chip` for the
            /// debugger, without touching the game's registers; nothing for
            /// a chip or a channel the console lacks.
            #[wasm_bindgen(js_name = muteAudioChipChannel)]
            pub fn mute_audio_chip_channel(&mut self, chip: usize, channel: u32, muted: bool) {
                if let ::core::option::Option::Some(debugger) =
                    $crate::__private::retro_core::EmulatorCore::audio_debugger(
                        &mut self.$core,
                        chip,
                    )
                {
                    debugger.mute_channel(channel, muted);
                }
            }

            /// `samples` of one channel's waveform on sound chip `chip`, in
            /// [-1, 1], for a view (as the chip approximates a channel);
            /// empty for a chip the console lacks.
            #[wasm_bindgen(js_name = getAudioChipWaveform)]
            pub fn get_audio_chip_waveform(
                &mut self,
                chip: usize,
                channel: u32,
                samples: u32,
            ) -> ::std::vec::Vec<f32> {
                $crate::__private::retro_core::EmulatorCore::audio_debugger(&mut self.$core, chip)
                    .map(|debugger| debugger.get_waveform(channel, samples))
                    .unwrap_or_default()
            }

            /// The last `samples` of the console's mixed sound, mono, in
            /// [-1, 1], oldest first, read without taking anything from the
            /// host's ring; silence where none was made yet.
            #[wasm_bindgen(js_name = getAudioOutputWaveform)]
            pub fn get_audio_output_waveform(&self, samples: u32) -> ::std::vec::Vec<f32> {
                $crate::__private::retro_core::EmulatorCore::output_waveform(&self.$core, samples)
            }

            /// Run `frames` frames of the loop a player's host runs, whatever
            /// the debugger has armed, with the sound on (turned on first if
            /// it is off, and left on): `{ frames, sound_turned_on }`. The
            /// samples go to the host's ring as a frame call's do, and the
            /// picture the framebuffer readers give is the last frame's, as
            /// after `runFrame`.
            #[wasm_bindgen(js_name = renderFrames)]
            pub fn render_frames(&mut self, frames: u32) -> $crate::__private::wasm_bindgen::JsValue {
                let report = $crate::__private::retro_core::audio::render_frames(
                    &mut self.$core,
                    frames,
                );
                $(self.$after_frame();)?
                $crate::js::to_js(&report)
            }

            /// The stems the console takes, `{ rate, kind, channels, master,
            /// sum }` (each channel `{ name, chip, channel, stereo,
            /// in_master }`, stem `n` the layout's channel `n`), or `null`
            /// for a console that takes none.
            #[wasm_bindgen(js_name = getStemLayout)]
            pub fn get_stem_layout(&self) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::__private::retro_core::audio::SoundCore::stem_layout(&self.$core) {
                    ::core::option::Option::Some(layout) => $crate::js::to_js(&layout),
                    ::core::option::Option::None => {
                        $crate::__private::wasm_bindgen::JsValue::NULL
                    }
                }
            }

            /// Record the stems whose bits `mask` sets, stem `n` bit `n`,
            /// each one's samples and the master's, from the next frame's
            /// start, or at once while the machine stands between two
            /// frames; `0` disarms. Arming another set drops what was held
            /// and not taken.
            #[wasm_bindgen(js_name = armStems)]
            pub fn arm_stems(&mut self, mask: u32) {
                $crate::__private::retro_core::audio::SoundCore::arm_stems(
                    &mut self.$core,
                    $crate::__private::retro_core::audio::StemSet::from_bits(
                        ::core::primitive::u64::from(mask),
                    ),
                );
            }

            /// What the stems recorded since the last take, by whole
            /// frames: `{ frames, armed, stems, master, departures,
            /// dropped }`, each stem and the master an `Int16Array` or a
            /// `Float32Array` (the layout's kind), stereo ones
            /// interleaved, and the departures a `Uint32Array` of
            /// `[start, frames]` pairs; `null` while nothing is armed or
            /// held.
            #[wasm_bindgen(js_name = takeStems)]
            pub fn take_stems(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::__private::retro_core::audio::SoundCore::take_stems(&mut self.$core) {
                    ::core::option::Option::Some(take) => $crate::audio::stem_take(&take),
                    ::core::option::Option::None => {
                        $crate::__private::wasm_bindgen::JsValue::NULL
                    }
                }
            }

            /// Start a sound log of the chips' register writes (`writes`),
            /// their channels' keys (`keys`) and the values the main
            /// processor writes into its sound processor's mailbox
            /// (`mailbox`, none when left out), its first entries, at time
            /// 0, the chips as they stand (their registers with `writes`, a
            /// key-on for each channel sounding with `keys`); what an
            /// earlier log left is dropped. None of them stops the log.
            #[wasm_bindgen(js_name = startSoundLog)]
            pub fn start_sound_log(
                &mut self,
                writes: bool,
                keys: bool,
                mailbox: ::core::option::Option<bool>,
            ) {
                $crate::__private::retro_core::audio::SoundCore::start_sound_log(
                    &mut self.$core,
                    $crate::__private::retro_core::audio::SoundLogContents {
                        writes,
                        keys,
                        mailbox: mailbox.unwrap_or(false),
                    },
                );
            }

            /// Stop the sound log; what it logged waits for the next take.
            #[wasm_bindgen(js_name = stopSoundLog)]
            pub fn stop_sound_log(&mut self) {
                $crate::__private::retro_core::audio::SoundCore::stop_sound_log(&mut self.$core);
            }

            /// What the sound log took down since the last take, by whole
            /// frames: `{ clockHz, imageLen, interrupted, chips, records }`,
            /// `records` a `Uint8Array` of 24 bytes an entry (the record
            /// form, which grows only by new kinds: a mailbox's write is
            /// kind 6); `null` while no log runs and nothing waits.
            #[wasm_bindgen(js_name = takeSoundLog)]
            pub fn take_sound_log(&mut self) -> $crate::__private::wasm_bindgen::JsValue {
                match $crate::__private::retro_core::audio::SoundCore::take_sound_log(
                    &mut self.$core,
                ) {
                    ::core::option::Option::Some(log) => $crate::audio::sound_log(
                        &log,
                        &$crate::__private::retro_core::audio::SoundCore::sound_log_chips(
                            &self.$core,
                        ),
                    ),
                    ::core::option::Option::None => {
                        $crate::__private::wasm_bindgen::JsValue::NULL
                    }
                }
            }

            /// Interleaved samples in [-1, 1] as a WAV file's bytes: 16-bit
            /// PCM, the 44-byte canonical header, each sample clamped.
            #[wasm_bindgen(js_name = encodeWav)]
            pub fn encode_wav(
                samples: &[f32],
                sample_rate: u32,
                channels: u16,
            ) -> ::std::vec::Vec<u8> {
                $crate::__private::retro_core::audio::encode_wav(samples, sample_rate, channels)
            }

            /// Interleaved samples as a WAV file's bytes, of the kind they
            /// come in, neither converted: an `Int16Array` as 16-bit PCM, a
            /// `Float32Array` as 32-bit floats, as `takeStems` gives each
            /// console's; with a loop in a `smpl` chunk after the samples
            /// when `loopStart` and `loopEnd` are given, the first and the
            /// last sample frame it plays, for ever. An error for samples of
            /// another type, samples that are not whole frames of
            /// `channels`, a loop with one end or not running forward inside
            /// the frames, or a file whose sizes a WAV's header cannot hold.
            #[wasm_bindgen(js_name = encodeWavSamples)]
            pub fn encode_wav_samples(
                samples: $crate::__private::wasm_bindgen::JsValue,
                sample_rate: u32,
                channels: u16,
                loop_start: ::core::option::Option<u32>,
                loop_end: ::core::option::Option<u32>,
            ) -> ::core::result::Result<
                ::std::vec::Vec<u8>,
                $crate::__private::wasm_bindgen::JsValue,
            > {
                $crate::audio::wav_of(&samples, sample_rate, channels, loop_start, loop_end)
            }

            /// The header alone of a WAV of `frames` sample frames of
            /// `channels` interleaved samples of `kind`, `"I16"` or `"F32"`
            /// as `getStemLayout` names it, at `sampleRate`, whose samples
            /// are followed by `trailerBytes` bytes of other chunks (the
            /// length of what `wavTrailer` gives; none when left out): the
            /// bytes `encodeWavSamples` writes before such samples, its
            /// RIFF size counting the chunks after them too. A file
            /// streamed to disk is this header, then the samples' bytes,
            /// little-endian, then the trailer; one written before its
            /// frames are counted starts with the header of none, written
            /// again over it once they are. An error for another kind, no
            /// channel, or a file whose sizes a WAV's header cannot hold.
            #[wasm_bindgen(js_name = wavHeader)]
            pub fn wav_header(
                kind: &str,
                sample_rate: u32,
                channels: u16,
                frames: u32,
                trailer_bytes: ::core::option::Option<u32>,
            ) -> ::core::result::Result<
                ::std::vec::Vec<u8>,
                $crate::__private::wasm_bindgen::JsValue,
            > {
                $crate::audio::wav_header(
                    kind,
                    sample_rate,
                    channels,
                    frames,
                    trailer_bytes.unwrap_or(0),
                )
            }

            /// The chunks a WAV carries after its samples for `marks`,
            /// `{ loop: [start, end], cues: [{ frame, label }] }`, either
            /// left out for none: with a loop, a `smpl` chunk naming it,
            /// played forward for ever from its first sample frame to its
            /// last (the chunk `encodeWavSamples` writes for its loop);
            /// with cues, a `cue ` chunk of a point at each one's sample
            /// frame, numbered from 1 in their order, and a `LIST` chunk
            /// of type `adtl` of a `labl` with each one's label; no byte
            /// for no mark. A file streamed to disk ends with these bytes,
            /// its header written by `wavHeader` with their length. An
            /// error for marks of another shape or a loop that ends before
            /// it starts.
            #[wasm_bindgen(js_name = wavTrailer)]
            pub fn wav_trailer(
                sample_rate: u32,
                marks: $crate::__private::wasm_bindgen::JsValue,
            ) -> ::core::result::Result<
                ::std::vec::Vec<u8>,
                $crate::__private::wasm_bindgen::JsValue,
            > {
                $crate::audio::wav_trailer(sample_rate, marks)
            }
        }
    };
}
