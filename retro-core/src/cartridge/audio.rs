//! Cartridge Audio Expansion
//!
//! This module contains the MSU-1 stub implementation as an example
//! of the CartridgeAudio trait. It serves as a template for future
//! audio expansion chips (VRC6, VRC7, MMC5, Namco 163, FME7, etc).

use super::*;

/// MSU-1 Audio Stub (SNES)
///
/// MSU-1 is a theoretical SNES expansion chip that allows playing
/// CD-quality audio (16-bit PCM, 44.1kHz) from SD card.
///
/// This is a stub implementation that demonstrates the CartridgeAudio trait.
/// It does not actually generate audio yet, but provides the complete
/// structure for future implementation.
pub struct Msu1Audio {
    /// PCM audio data (16-bit stereo samples)
    pcm_data: Vec<i16>,

    /// Current playback position (in samples)
    position: usize,

    /// Loop point (in samples, None = no loop)
    loop_point: Option<usize>,

    /// Is audio playing?
    playing: bool,

    /// Current track number
    track_number: u32,

    /// Volume (0.0 to 1.0)
    volume: f32,

    /// Is audio enabled?
    enabled: bool,

    /// Audio info (cached)
    audio_info: CartridgeAudioInfo,
}

impl Default for Msu1Audio {
    fn default() -> Self {
        Self::new()
    }
}

impl Msu1Audio {
    /// Create new MSU-1 audio stub
    pub fn new() -> Self {
        Msu1Audio {
            pcm_data: Vec::new(),
            position: 0,
            loop_point: None,
            playing: false,
            track_number: 0,
            volume: 1.0,
            enabled: true,
            audio_info: CartridgeAudioInfo {
                chip_type: CartridgeAudioType::MSU1,
                channels: 2, // Stereo
                native_sample_rate: Some(44100),
                features: vec![
                    "16-bit PCM".to_string(),
                    "CD-quality audio".to_string(),
                    "Loop support".to_string(),
                    "44.1kHz stereo".to_string(),
                ],
            },
        }
    }

    /// Load PCM track data (stub)
    ///
    /// In a real implementation, this would load .pcm files from filesystem
    pub fn load_track(&mut self, track: u32, _pcm_data: &[u8]) {
        self.track_number = track;
        self.pcm_data.clear();
        self.position = 0;

        // TODO: Parse PCM data when implementing
        // For now, just clear the buffer
    }

    /// Set loop point (in samples)
    pub fn set_loop_point(&mut self, loop_point: Option<usize>) {
        self.loop_point = loop_point;
    }

    /// Start playback
    pub fn play(&mut self) {
        self.playing = true;
    }

    /// Stop playback
    pub fn stop(&mut self) {
        self.playing = false;
    }

    /// Get current playback state
    pub fn get_playback_state(&self) -> AudioPlaybackState {
        AudioPlaybackState {
            playing: self.playing,
            position: self.position as u64,
            loop_point: self.loop_point.map(|p| p as u64),
            volume: self.volume,
        }
    }

    /// Get current track info
    pub fn get_track_info(&self) -> AudioTrackInfo {
        AudioTrackInfo {
            track_number: self.track_number,
            title: None, // Could be loaded from metadata
            duration: Some(self.pcm_data.len() as u64),
            loop_enabled: self.loop_point.is_some(),
        }
    }
}

impl CartridgeAudio for Msu1Audio {
    fn audio_info(&self) -> &CartridgeAudioInfo {
        &self.audio_info
    }

    fn generate_audio(&mut self, output: &mut [f32], _sample_rate: u32) -> usize {
        if !self.enabled || !self.playing || self.pcm_data.is_empty() {
            // Silence
            for sample in output.iter_mut() {
                *sample = 0.0;
            }
            return 0;
        }

        // TODO: Implement actual PCM playback with sample rate conversion
        // For now, just return silence

        let mut samples_generated = 0;

        // Stub: Fill with silence
        for i in (0..output.len()).step_by(2) {
            if i + 1 >= output.len() {
                break;
            }

            // Check if we've reached the end
            if self.position >= self.pcm_data.len() {
                // Handle loop
                if let Some(loop_pos) = self.loop_point {
                    self.position = loop_pos;
                } else {
                    // End of track
                    self.playing = false;
                    break;
                }
            }

            // Convert 16-bit PCM to f32 (-1.0 to 1.0)
            // In a real implementation, we'd read from pcm_data here
            let left = 0.0f32; // Stub: silence
            let right = 0.0f32; // Stub: silence

            output[i] = left * self.volume;
            output[i + 1] = right * self.volume;

            self.position += 2;
            samples_generated += 2;
        }

        samples_generated
    }

    fn get_volume(&self) -> f32 {
        self.volume
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.playing = false;
        }
    }

    fn audio_debugger(&self) -> Option<&dyn CartridgeAudioDebugger> {
        Some(self)
    }

    fn audio_debugger_mut(&mut self) -> Option<&mut dyn CartridgeAudioDebugger> {
        Some(self)
    }
}

impl CartridgeAudioDebugger for Msu1Audio {
    fn get_playback_state(&self) -> AudioPlaybackState {
        self.get_playback_state()
    }

    fn get_waveform(&self, samples: usize) -> Vec<f32> {
        // Return recent waveform data for visualization
        let start = self.position.saturating_sub(samples);
        let end = (start + samples).min(self.pcm_data.len());

        let mut waveform = Vec::new();
        for i in (start..end).step_by(2) {
            if i + 1 < self.pcm_data.len() {
                // Convert to f32 for visualization
                let left = (self.pcm_data[i] as f32) / 32768.0;
                waveform.push(left);
            }
        }

        waveform
    }

    fn get_track_info(&self) -> Option<AudioTrackInfo> {
        Some(self.get_track_info())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_msu1_creation() {
        let msu1 = Msu1Audio::new();
        assert_eq!(msu1.audio_info().chip_type, CartridgeAudioType::MSU1);
        assert_eq!(msu1.audio_info().channels, 2);
        assert_eq!(msu1.audio_info().native_sample_rate, Some(44100));
    }

    #[test]
    fn test_msu1_playback_control() {
        let mut msu1 = Msu1Audio::new();

        assert!(!msu1.playing);

        msu1.play();
        assert!(msu1.playing);

        msu1.stop();
        assert!(!msu1.playing);
    }

    #[test]
    fn test_msu1_volume_control() {
        let mut msu1 = Msu1Audio::new();

        assert_eq!(msu1.get_volume(), 1.0);

        msu1.set_volume(0.5);
        assert_eq!(msu1.get_volume(), 0.5);

        // Test clamping
        msu1.set_volume(2.0);
        assert_eq!(msu1.get_volume(), 1.0);

        msu1.set_volume(-0.5);
        assert_eq!(msu1.get_volume(), 0.0);
    }

    #[test]
    fn test_msu1_enabled() {
        let mut msu1 = Msu1Audio::new();

        assert!(msu1.is_enabled());

        msu1.play();
        assert!(msu1.playing);

        msu1.set_enabled(false);
        assert!(!msu1.is_enabled());
        assert!(!msu1.playing); // Should stop when disabled
    }

    #[test]
    fn test_msu1_loop_point() {
        let mut msu1 = Msu1Audio::new();

        assert!(msu1.loop_point.is_none());

        msu1.set_loop_point(Some(1000));
        assert_eq!(msu1.loop_point, Some(1000));

        msu1.set_loop_point(None);
        assert!(msu1.loop_point.is_none());
    }

    #[test]
    fn test_msu1_audio_generation_silence() {
        let mut msu1 = Msu1Audio::new();
        let mut output = vec![0.5f32; 256]; // Pre-fill with non-zero

        // Should generate silence when not playing
        let generated = msu1.generate_audio(&mut output, 44100);
        assert_eq!(generated, 0);
        assert_eq!(output[0], 0.0); // Should be cleared to silence
    }

    #[test]
    fn test_msu1_playback_state() {
        let mut msu1 = Msu1Audio::new();
        msu1.play();
        msu1.set_volume(0.75);
        msu1.set_loop_point(Some(500));

        let state = msu1.get_playback_state();
        assert!(state.playing);
        assert_eq!(state.volume, 0.75);
        assert_eq!(state.loop_point, Some(500));
    }

    #[test]
    fn test_msu1_track_info() {
        let mut msu1 = Msu1Audio::new();
        msu1.load_track(5, &[]);
        msu1.set_loop_point(Some(1000));

        let info = msu1.get_track_info();
        assert_eq!(info.track_number, 5);
        assert!(info.loop_enabled);
    }

    #[test]
    fn test_msu1_debugger() {
        let msu1 = Msu1Audio::new();

        // Test debugger access
        let debugger = msu1.audio_debugger();
        assert!(debugger.is_some());

        if let Some(dbg) = debugger {
            let state = dbg.get_playback_state();
            assert!(!state.playing);
        }
    }

    #[test]
    fn test_msu1_waveform() {
        let msu1 = Msu1Audio::new();

        if let Some(dbg) = msu1.audio_debugger() {
            let waveform = dbg.get_waveform(100);
            // Should return empty waveform (no data loaded)
            assert!(waveform.is_empty());
        }
    }
}
