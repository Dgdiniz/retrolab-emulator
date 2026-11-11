//! Audio system traits and types for retro emulators.
//!
//! This module provides generic traits and structures for audio emulation
//! across different retro console systems (Genesis, SNES, NES, etc.).

use crate::cpu::Clockable;
use serde::{Deserialize, Serialize};

pub mod buffer;
pub mod limiter;
pub mod rate_converter;
pub mod resample;
pub mod ring_buffer;

pub use buffer::ChipSampleBuffer;
pub use limiter::{soft_clip, soft_clip_buffer};
pub use rate_converter::IntegerRateConverter;
pub use resample::LinearResampler;
pub use ring_buffer::AudioRingBuffer;

/// Audio channel state (expanded for comprehensive debugging).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelState {
    pub channel_id: u32,
    pub enabled: bool,
    pub muted: bool,

    // Frequency/Tone
    pub frequency: f32,     // Hz (normalized)
    pub frequency_raw: u16, // Raw register value (chip-specific)
    pub period: u32,        // Clock cycles per period

    // Volume/Attenuation
    pub volume: f32,     // 0.0 to 1.0 (normalized)
    pub attenuation: u8, // Raw attenuation value (chip-specific)

    // Stereo
    pub pan: f32, // -1.0 (left) to 1.0 (right)

    // Type and metadata
    pub channel_type: ChannelType,
    pub envelope: Option<EnvelopeState>,

    // Waveform for visualization
    pub current_output: f32, // Current output level (-1.0 to 1.0)

    // Noise-specific (optional)
    pub noise_params: Option<NoiseParams>,
}

/// Type of audio channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelType {
    Tone,
    Noise,
    FM,
    PCM,
    Sample,
}

/// Envelope state for channels with ADSR envelopes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvelopeState {
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    pub current_phase: EnvelopePhase,
}

/// Current envelope phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvelopePhase {
    Attack,
    Decay,
    Sustain,
    Release,
    Off,
}

/// Noise channel parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoiseParams {
    pub shift_rate: NoiseShiftRate,
    pub mode: NoiseMode,
    pub lfsr_state: u16,
}

/// Noise shift rate (for PSG-style noise).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoiseShiftRate {
    N512,
    N1024,
    N2048,
    Tone3, // Follow tone channel 3 (PSG-specific)
}

/// Noise mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoiseMode {
    Periodic, // Tapped bits create periodic noise
    White,    // Full LFSR creates white noise
}

/// Audio chip metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioChipInfo {
    pub name: &'static str,
    pub manufacturer: &'static str,
    pub channels: u32,
    pub sample_rate: u32,
    pub stereo: bool,
    pub channel_types: Vec<ChannelType>,
}

//
// ===== AUDIO CHIP TRAIT =====
//

/// Core audio chip functionality.
///
/// This trait defines the interface for audio chip emulation. All audio chips
/// (PSG, YM2612, APU, etc.) should implement this trait to provide a consistent
/// interface for the emulator core and WASM bindings.
///
/// # Example
///
/// ```ignore
/// use retro_core::audio::{AudioChip, AudioChipInfo};
/// use retro_core::cpu::Clockable;
///
/// struct MyAudioChip { /* ... */ }
///
/// impl AudioChip for MyAudioChip {
///     fn chip_name(&self) -> &'static str {
///         "MY-CHIP-01"
///     }
///     
///     fn get_info(&self) -> AudioChipInfo {
///         AudioChipInfo {
///             name: "MY-CHIP-01",
///             manufacturer: "MyCompany",
///             channels: 4,
///             sample_rate: 44100,
///             stereo: false,
///             channel_types: vec![/* ... */],
///         }
///     }
///     
///     // ... implement other methods
/// }
/// ```
pub trait AudioChip: Clockable {
    /// Get chip name (e.g., "SN76489", "YM2612").
    fn chip_name(&self) -> &'static str;

    /// Get audio chip metadata.
    fn get_info(&self) -> AudioChipInfo;

    /// Write to chip register (address/data depend on chip).
    ///
    /// # Arguments
    /// * `address` - Register address (chip-specific)
    /// * `data` - Data to write
    fn write_register(&mut self, address: u8, data: u8);

    /// Read from chip register (if applicable).
    ///
    /// # Arguments
    /// * `address` - Register address (chip-specific)
    ///
    /// # Returns
    /// Register value, or 0xFF if not readable
    fn read_register(&mut self, address: u8) -> u8;

    /// Generate audio samples (stereo interleaved: L, R, L, R, ...).
    ///
    /// # Arguments
    /// * `output` - Output buffer to fill with samples
    /// * `sample_rate` - Target sample rate (e.g., 44100)
    ///
    /// # Returns
    /// Number of samples generated (2 per frame for stereo: L+R)
    fn generate_samples(&mut self, output: &mut [f32], sample_rate: u32) -> usize;

    /// Reset chip to initial state.
    fn reset(&mut self);

    /// Get current output level (for visualization, -1.0 to 1.0).
    fn get_output_level(&self) -> f32;
}

//
// ===== AUDIO MIXER TRAIT =====
//

/// Mixes multiple audio chips together.
///
/// This trait provides an interface for mixing audio from multiple chips
/// (e.g., PSG + YM2612 on Genesis, or multiple APUs on SNES).
///
/// # Example
///
/// ```ignore
/// use retro_core::audio::{AudioMixer, SimpleMixer};
///
/// let mut mixer = SimpleMixer::new();
/// mixer.add_chip(Box::new(psg));
/// mixer.add_chip(Box::new(ym2612));
/// mixer.set_master_volume(0.8);
///
/// let mut output = vec![0.0; 2048];
/// mixer.mix_samples(&mut output, 44100);
/// ```
pub trait AudioMixer {
    /// Add an audio chip to the mixer.
    fn add_chip(&mut self, chip: Box<dyn AudioChip>);

    /// Remove a chip by index.
    fn remove_chip(&mut self, index: usize);

    /// Generate mixed samples from all chips.
    ///
    /// # Arguments
    /// * `output` - Output buffer to fill with mixed samples
    /// * `sample_rate` - Target sample rate
    ///
    /// # Returns
    /// Number of samples generated
    fn mix_samples(&mut self, output: &mut [f32], sample_rate: u32) -> usize;

    /// Set master volume (0.0 to 1.0).
    fn set_master_volume(&mut self, volume: f32);

    /// Get master volume.
    fn get_master_volume(&self) -> f32;
}

/// Simple audio mixer implementation.
///
/// This is a basic implementation of the `AudioMixer` trait that simply
/// adds the output of all chips together and scales by master volume.
pub struct SimpleMixer {
    chips: Vec<Box<dyn AudioChip>>,
    master_volume: f32,
    temp_buffer: Vec<f32>,
}

impl SimpleMixer {
    /// Create a new simple mixer.
    pub fn new() -> Self {
        Self {
            chips: Vec::new(),
            master_volume: 1.0,
            temp_buffer: Vec::new(),
        }
    }
}

impl Default for SimpleMixer {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioMixer for SimpleMixer {
    fn add_chip(&mut self, chip: Box<dyn AudioChip>) {
        self.chips.push(chip);
    }

    fn remove_chip(&mut self, index: usize) {
        if index < self.chips.len() {
            self.chips.remove(index);
        }
    }

    fn mix_samples(&mut self, output: &mut [f32], sample_rate: u32) -> usize {
        if self.chips.is_empty() {
            // No chips, output silence
            output.fill(0.0);
            return output.len();
        }

        // Clear output
        output.fill(0.0);

        // Resize temp buffer if needed
        if self.temp_buffer.len() < output.len() {
            self.temp_buffer.resize(output.len(), 0.0);
        }

        // Mix each chip
        for chip in &mut self.chips {
            // Generate samples for this chip
            self.temp_buffer.fill(0.0);
            chip.generate_samples(&mut self.temp_buffer[..output.len()], sample_rate);

            // Add to output
            for (out, &chip_sample) in output.iter_mut().zip(self.temp_buffer.iter()) {
                *out += chip_sample;
            }
        }

        // Apply master volume
        for sample in output.iter_mut() {
            *sample *= self.master_volume;

            // Clamp to [-1.0, 1.0] to prevent clipping
            *sample = sample.clamp(-1.0, 1.0);
        }

        output.len()
    }

    fn set_master_volume(&mut self, volume: f32) {
        self.master_volume = volume.clamp(0.0, 1.0);
    }

    fn get_master_volume(&self) -> f32 {
        self.master_volume
    }
}

//
// ===== AUDIO DEBUGGER TRAIT =====
//

/// Audio debugging interface.
///
/// This trait provides methods for inspecting and debugging audio chip state.
/// It's implemented by audio chips to expose internal state for visualization
/// and debugging in the browser.
pub trait AudioDebugger {
    /// Get total number of audio channels.
    fn get_channel_count(&self) -> u32;

    /// Get state of a specific channel.
    fn get_channel_state(&self, channel: u32) -> Option<ChannelState>;

    /// Mute or unmute a channel.
    fn mute_channel(&mut self, channel: u32, muted: bool);

    /// Get waveform data for visualization.
    ///
    /// # Arguments
    /// * `channel` - Channel index
    /// * `samples` - Number of samples to return
    ///
    /// # Returns
    /// Vector of samples in range [-1.0, 1.0]
    fn get_waveform(&self, channel: u32, samples: u32) -> Vec<f32>;

    /// Get mixed audio output waveform.
    ///
    /// # Arguments
    /// * `samples` - Number of samples to return
    ///
    /// # Returns
    /// Vector of samples in range [-1.0, 1.0]
    fn get_output_waveform(&self, samples: u32) -> Vec<f32>;
}
