import { describe, expect, it } from 'vitest'

import { ym2612StatusHtml } from '../src/audio-debugger-multi-chip.js'

// The objects the Mega Drive core's readers give (`getYm2612LfoState`,
// `getYm2612DacState`, `getYm2612TimerAState`, `getYm2612TimerBState`),
// with the keys and nothing else.
const lfo = { enabled: true, frequency: 0 }
const dac = { enabled: true, value: -12 }
const timerA = { running: true, period: 1023, overflowed: false }
const timerB = { running: false, period: 200, overflowed: true }

describe('the YM2612 status lines of the audio panel', () => {
  const html = ym2612StatusHtml(lfo, dac, timerA, timerB)

  it('shows the LFO rate setting, 0 included', () => {
    expect(html.lfo).toContain('Enabled: ✅')
    expect(html.lfo).toContain('Freq: 0')
  })

  it("shows the DAC's sample", () => {
    expect(html.dac).toContain('Enabled: ✅')
    expect(html.dac).toContain('Value: -12')
  })

  it("shows each timer's run, period and overflow flag", () => {
    expect(html.timers).toContain('A: ✅ (1023),')
    expect(html.timers).toContain('B: ❌ (200), overflowed')
    expect(html.timers).not.toContain('A: ✅ (1023), overflowed')
  })
})
