//! Integration tests for the YM2612 calls the WASM bindings make
//!
//! Checks that the `GenesisEmulator` methods behind the bindings work correctly

use retro_core::audio::AudioDebugger;
use sg1_core::GenesisEmulator;

#[test]
fn test_ym2612_channel_count() {
    let genesis = GenesisEmulator::new();
    assert_eq!(genesis.ym2612().get_channel_count(), 6);
}

#[test]
fn test_ym2612_get_channel_state() {
    let genesis = GenesisEmulator::new();

    // All 6 channels must return a state
    for channel in 0..6 {
        let state = genesis.get_ym2612_channel_state(channel);
        assert!(state.is_some(), "Channel {} should return state", channel);

        let state = state.unwrap();
        assert_eq!(state.channel_id, channel);
        // The channel is not enabled at first (no key on)
        assert!(
            !state.enabled,
            "Channel {} should not be enabled initially",
            channel
        );
        assert!(
            !state.muted,
            "Channel {} should not be muted initially",
            channel
        );
    }

    // Channel 6 (out of range) must return None
    assert!(genesis.get_ym2612_channel_state(6).is_none());
}

#[test]
fn test_ym2612_mute_channel() {
    let mut genesis = GenesisEmulator::new();

    // Mute channel 0
    genesis.mute_ym2612_channel(0, true);

    let state = genesis.get_ym2612_channel_state(0).unwrap();
    assert!(state.muted, "Channel 0 should be muted");

    // Unmute
    genesis.mute_ym2612_channel(0, false);

    let state = genesis.get_ym2612_channel_state(0).unwrap();
    assert!(!state.muted, "Channel 0 should not be muted");
}

#[test]
fn test_ym2612_lfo_state() {
    let genesis = GenesisEmulator::new();

    let lfo_state = genesis.get_ym2612_lfo_state();

    // The JSON must have the fields enabled and frequency
    assert!(lfo_state.get("enabled").is_some());
    assert!(lfo_state.get("frequency").is_some());

    // The LFO starts disabled
    assert!(!lfo_state.get("enabled").unwrap().as_bool().unwrap());
}

#[test]
fn test_ym2612_dac_state() {
    let genesis = GenesisEmulator::new();

    let dac_state = genesis.get_ym2612_dac_state();

    // The JSON must have the field enabled
    assert!(dac_state.get("enabled").is_some());

    // The DAC starts disabled
    assert!(!dac_state.get("enabled").unwrap().as_bool().unwrap());
}

#[test]
fn test_ym2612_timer_a_state() {
    let genesis = GenesisEmulator::new();

    let timer_state = genesis.get_ym2612_timer_a_state();

    // JSON has fields `running` (not `enabled`), `period`, `overflowed`.
    assert!(timer_state.get("running").is_some());
    assert!(timer_state.get("period").is_some());
    assert!(timer_state.get("overflowed").is_some());

    // Initially the timer is stopped.
    assert!(!timer_state.get("running").unwrap().as_bool().unwrap());
}

#[test]
fn test_ym2612_timer_b_state() {
    let genesis = GenesisEmulator::new();

    let timer_state = genesis.get_ym2612_timer_b_state();

    // Same shape as Timer A.
    assert!(timer_state.get("running").is_some());
    assert!(timer_state.get("period").is_some());
    assert!(timer_state.get("overflowed").is_some());

    // Initially the timer is stopped.
    assert!(!timer_state.get("running").unwrap().as_bool().unwrap());
}

#[test]
fn test_ym2612_channel_state_fields() {
    let genesis = GenesisEmulator::new();

    let state = genesis.get_ym2612_channel_state(0).unwrap();

    // Check that all the expected fields exist
    assert_eq!(state.channel_id, 0);

    // channel_type must be FM (except channel 5 while the DAC is on)
    use retro_core::audio::ChannelType;
    assert_eq!(state.channel_type, ChannelType::FM);

    // It must have an envelope
    assert!(state.envelope.is_some());

    // It must have no noise_params (FM is not noise)
    assert!(state.noise_params.is_none());
}

#[test]
fn test_ym2612_all_channels_independent() {
    let mut genesis = GenesisEmulator::new();

    // Mute channel 1
    genesis.mute_ym2612_channel(1, true);

    // Check that only channel 1 is muted
    for channel in 0..6 {
        let state = genesis.get_ym2612_channel_state(channel).unwrap();
        if channel == 1 {
            assert!(state.muted, "Channel 1 should be muted");
        } else {
            assert!(!state.muted, "Channel {} should not be muted", channel);
        }
    }
}
