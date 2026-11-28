import { describe, expect, it } from 'vitest'

import { channelFrequencyText } from '../src/audio-debugger-multi-chip.js'
import { describeShape, recordedIn } from './reader-shapes'

// Three channels as the Mega Drive binding's own test records
// `getAudioChipChannels()` (`what_the_sound_chips_readers_answer`): a PSG tone
// channel playing, the PSG's noise channel, and an FM channel at rest.
const tone = {
  attenuation: 0,
  channel_id: 0,
  channel_type: 'Tone',
  current_output: -1,
  enabled: true,
  envelope: undefined,
  frequency: 440.39678955078125,
  frequency_raw: 254,
  muted: false,
  noise_params: undefined,
  pan: 0,
  period: 8128,
  volume: 1,
}
const noise = {
  attenuation: 15,
  channel_id: 3,
  channel_type: 'Noise',
  current_output: -0,
  enabled: false,
  envelope: undefined,
  frequency: 0,
  frequency_raw: 0,
  muted: false,
  noise_params: { lfsr_state: 2, mode: 'Periodic', shift_rate: 'N512' },
  pan: 0,
  period: 512,
  volume: 0,
}
const fmAtRest = {
  attenuation: 127,
  channel_id: 5,
  channel_type: 'FM',
  current_output: 0.015625,
  enabled: false,
  envelope: { attack: 1, current_phase: 'Off', decay: 0, release: 0.46666666865348816, sustain: 1 },
  frequency: 0,
  frequency_raw: 0,
  muted: false,
  noise_params: undefined,
  pan: 0,
  period: 0,
  volume: 0,
}

describe("the audio panel's channel cards", () => {
  it('read the channels as the binding records them', () => {
    const records = recordedIn('cores/sg1/wasm/src/tests/binding_baseline.rs')
    expect(records).toContain(`getAudioChipChannels(1)[0] => ${describeShape(tone)}`)
    expect(records).toContain(`getAudioChipChannels(1)[3] => ${describeShape(noise)}`)
    expect(records).toContain(`getAudioChipChannels(0)[5] => ${describeShape(fmAtRest)}`)
  })

  it('show a frequency in hertz, 0 included', () => {
    expect(channelFrequencyText(tone)).toBe('440.40 Hz')
    expect(channelFrequencyText(fmAtRest)).toBe('0.00 Hz')
  })

  it('show none for the noise channel, whose 0 is no frequency', () => {
    expect(channelFrequencyText(noise)).toBe('N/A')
  })
})
