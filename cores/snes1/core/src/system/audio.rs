//! Audio: the S-DSP's samples resampled at a frame's end into the host's
//! ring, and the sound hooks a host calls (`SoundCore`).

use retro_core::audio::{
    SoundChipKind, SoundCore, SoundLog, SoundLogContents, StemLayout, StemSet, StemTake,
};
use retro_core::frame::run_frame_on;
use retro_core::ExecutionTier;

use super::SnesEmulator;
use crate::apu::{decode_sample, Apu, BrrSample, DirectoryEntry};
use crate::sound::{build_spc, Id666};

/// The stems are the S-DSP's, taken where its output is made, at its own
/// 32 kHz; the master is the sample it pushes, before the host's resampling.
impl SoundCore for SnesEmulator {
    fn render_frame(&mut self) {
        run_frame_on(self, ExecutionTier::Play);
    }

    fn stem_layout(&self) -> Option<StemLayout> {
        Some(Apu::stem_layout())
    }

    /// Armed between two frames, or at a frame's start before anything of
    /// it ran (the frame a reset arms), the stems start at once.
    fn arm_stems(&mut self, stems: StemSet) {
        let between_frames =
            !self.frame_in_progress() || self.master_cycles == self.frame_base_cycle;
        self.bus.apu_mut().arm_stems(stems, between_frames);
    }

    fn take_stems(&mut self) -> Option<StemTake> {
        self.bus.apu_mut().take_stems()
    }

    fn sound_log_chips(&self) -> Vec<SoundChipKind> {
        vec![SoundChipKind::SDsp]
    }

    fn start_sound_log(&mut self, contents: SoundLogContents) {
        self.bus.apu_mut().start_sound_log(contents);
    }

    fn stop_sound_log(&mut self) {
        self.bus.apu_mut().stop_sound_log();
    }

    fn take_sound_log(&mut self) -> Option<SoundLog> {
        self.bus.apu_mut().take_sound_log()
    }
}

impl SnesEmulator {
    /// The APU as an SPC file a player runs, tagged with `tag`: as it
    /// stands, which between two frames is the frame's end (within a frame
    /// the APU runs behind the CPU until it next catches up).
    pub fn export_spc(&self, tag: &Id666) -> Vec<u8> {
        build_spc(&self.bus.apu().to_savestate(), tag)
    }

    /// Keep the APU as it stands after the first SPC700 instruction that
    /// keys a voice on, from now on ([`Apu::arm_key_on_latch`]); a load
    /// keeps it waiting.
    pub fn arm_spc_at_key_on(&mut self) {
        self.bus.apu_mut().arm_key_on_latch();
    }

    /// The SPC file of what the key-on latch kept, tagged with `tag`, once;
    /// `None` while it waits or was never armed.
    pub fn take_key_on_spc(&mut self, tag: &Id666) -> Option<Vec<u8>> {
        let state = self.bus.apu_mut().take_key_on_state()?;
        Some(build_spc(&state, tag))
    }

    /// The sample directory the S-DSP's registers name ([`Apu::sample_directory`]).
    pub fn sample_directory(&self) -> Vec<DirectoryEntry> {
        self.bus.apu().sample_directory()
    }

    /// The BRR sample at `start` of ARAM decoded as a voice decodes it,
    /// `loop_start` its loop address ([`decode_sample`]).
    pub fn decode_brr(&self, start: u16, loop_start: u16) -> BrrSample {
        decode_sample(self.bus.apu().aram(), start, loop_start)
    }

    /// Pull every queued 32 kHz `i16` sample out of the APU, resample it
    /// to the 44.1 kHz host rate, and push the `f32` result into the host
    /// audio ring. Called once per frame after the APU catch-up.
    ///
    /// Skipped entirely when audio is disabled; the APU's own 32 kHz ring
    /// then fills and bounds-overflows its oldest samples (benign), and
    /// resampling resumes from the live ring once audio is re-enabled.
    pub(super) fn drain_apu_to_host_audio(&mut self) {
        if !self.audio_enabled {
            return;
        }
        let available = self.bus.apu_mut().audio_samples_available();
        if available == 0 {
            return;
        }
        self.apu_drain_buf.resize(available, 0);
        let drained = self.bus.apu_mut().drain_audio(&mut self.apu_drain_buf);
        self.audio_resample_buf.clear();
        self.resampler
            .resample_into(&self.apu_drain_buf[..drained], &mut self.audio_resample_buf);
        // The host ring is bounded; if the host is far behind, `push_slice`
        // drops the oldest samples and counts them, which
        // `host_audio().overflows()` reports.
        self.host_audio.push_slice(&self.audio_resample_buf);
    }
}
