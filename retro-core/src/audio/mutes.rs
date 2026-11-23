//! [`ChannelMutes`], the channels a host muted on a machine's sound chips,
//! which a core carries over to the chips it builds again.

use crate::EmulatorCore;

/// The channels the host muted on each of a machine's sound chips. A mute
/// is the host's, as the sound switch is, and the mixer's alone: it lasts
/// until the host lifts it, across the reset button, a power cycle and a
/// ROM load. A core whose chips are built again on a load or a power cycle
/// takes them from the old chips with [`Self::of`] and gives them to the
/// new ones with [`Self::apply`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelMutes {
    /// Per chip, its muted channels as bits: channel `n` is bit `n`.
    chips: Vec<u64>,
}

impl ChannelMutes {
    /// The channels muted on `machine`'s sound chips, as each chip's
    /// debugger reports them (the first 64 of a chip).
    pub fn of<E: EmulatorCore>(machine: &mut E) -> Self {
        let chips = (0..machine.audio_chip_count())
            .map(|chip| {
                machine.audio_debugger(chip).map_or(0, |debugger| {
                    (0..debugger.get_channel_count().min(64))
                        .filter(|&channel| {
                            debugger
                                .get_channel_state(channel)
                                .is_some_and(|state| state.muted)
                        })
                        .fold(0, |mask, channel| mask | 1 << channel)
                })
            })
            .collect();
        Self { chips }
    }

    /// Whether no channel is muted.
    pub fn is_empty(&self) -> bool {
        self.chips.iter().all(|&mask| mask == 0)
    }

    /// Mute on `machine`'s sound chips every channel muted here; a channel
    /// not muted here is left as it is.
    pub fn apply<E: EmulatorCore>(&self, machine: &mut E) {
        for (chip, &mask) in self.chips.iter().enumerate() {
            if mask == 0 {
                continue;
            }
            if let Some(debugger) = machine.audio_debugger(chip) {
                for channel in (0..64).filter(|&channel| mask & 1 << channel != 0) {
                    debugger.mute_channel(channel, true);
                }
            }
        }
    }
}
