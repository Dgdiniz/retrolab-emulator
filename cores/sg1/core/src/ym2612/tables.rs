// Lookup tables for YM2612 FM synthesis
//
// The YM2612 uses a logarithmic pipeline for output:
// 1. 10-bit phase -> quarter-wave log-sine table (256 entries) -> attenuation in log domain
// 2. Add envelope level + total level in log domain
// 3. Exponential table (256 entries) -> convert log back to linear
// 4. Apply sign from phase quadrant
//
// Tables are from hardware analysis (matching Nuked-OPN2 / MAME)

/// Log-sine table: 256 entries (quarter wave)
/// Input: 8-bit phase (0-255, representing 0 to pi/2)
/// Output: 12-bit log-attenuation value (0 = max volume, larger = quieter)
/// This is the base-2 logarithm of sin(x), scaled to fixed point
/// Format: 4.8 fixed point (4 integer bits, 8 fractional bits)
pub static LOG_SINE_TABLE: [u16; 256] = [
    0x859, 0x6C3, 0x607, 0x58B, 0x52E, 0x4E4, 0x4A6, 0x471, 0x443, 0x41A, 0x3F5, 0x3D3, 0x3B5,
    0x398, 0x37E, 0x365, 0x34E, 0x339, 0x324, 0x311, 0x2FF, 0x2ED, 0x2DC, 0x2CD, 0x2BD, 0x2AF,
    0x2A0, 0x293, 0x286, 0x279, 0x26D, 0x261, 0x256, 0x24B, 0x240, 0x236, 0x22C, 0x222, 0x218,
    0x20F, 0x206, 0x1FD, 0x1F5, 0x1EC, 0x1E4, 0x1DC, 0x1D4, 0x1CD, 0x1C5, 0x1BE, 0x1B7, 0x1B0,
    0x1A9, 0x1A2, 0x19B, 0x195, 0x18F, 0x188, 0x182, 0x17C, 0x177, 0x171, 0x16B, 0x166, 0x160,
    0x15B, 0x155, 0x150, 0x14B, 0x146, 0x141, 0x13C, 0x137, 0x133, 0x12E, 0x129, 0x125, 0x121,
    0x11C, 0x118, 0x114, 0x10F, 0x10B, 0x107, 0x103, 0x0FF, 0x0FB, 0x0F8, 0x0F4, 0x0F0, 0x0EC,
    0x0E9, 0x0E5, 0x0E2, 0x0DE, 0x0DB, 0x0D7, 0x0D4, 0x0D1, 0x0CD, 0x0CA, 0x0C7, 0x0C4, 0x0C1,
    0x0BE, 0x0BB, 0x0B8, 0x0B5, 0x0B2, 0x0AF, 0x0AC, 0x0A9, 0x0A7, 0x0A4, 0x0A1, 0x09F, 0x09C,
    0x099, 0x097, 0x094, 0x092, 0x08F, 0x08D, 0x08A, 0x088, 0x086, 0x083, 0x081, 0x07F, 0x07D,
    0x07A, 0x078, 0x076, 0x074, 0x072, 0x070, 0x06E, 0x06C, 0x06A, 0x068, 0x066, 0x064, 0x062,
    0x060, 0x05E, 0x05C, 0x05B, 0x059, 0x057, 0x055, 0x053, 0x052, 0x050, 0x04E, 0x04D, 0x04B,
    0x04A, 0x048, 0x046, 0x045, 0x043, 0x042, 0x040, 0x03F, 0x03E, 0x03C, 0x03B, 0x039, 0x038,
    0x037, 0x035, 0x034, 0x033, 0x031, 0x030, 0x02F, 0x02E, 0x02D, 0x02B, 0x02A, 0x029, 0x028,
    0x027, 0x026, 0x025, 0x024, 0x023, 0x022, 0x021, 0x020, 0x01F, 0x01E, 0x01D, 0x01C, 0x01B,
    0x01A, 0x019, 0x018, 0x017, 0x017, 0x016, 0x015, 0x014, 0x014, 0x013, 0x012, 0x011, 0x011,
    0x010, 0x00F, 0x00F, 0x00E, 0x00D, 0x00D, 0x00C, 0x00C, 0x00B, 0x00A, 0x00A, 0x009, 0x009,
    0x008, 0x008, 0x007, 0x007, 0x007, 0x006, 0x006, 0x005, 0x005, 0x005, 0x004, 0x004, 0x004,
    0x003, 0x003, 0x003, 0x002, 0x002, 0x002, 0x002, 0x001, 0x001, 0x001, 0x001, 0x001, 0x001,
    0x001, 0x000, 0x000, 0x000, 0x000, 0x000, 0x000, 0x000, 0x000,
];

/// Exponential table: 256 entries
/// Converts log-domain attenuation back to linear amplitude
/// Input: 8-bit fractional part of log-attenuation
/// Output: 11-bit linear value with bit 10 always set (1024-2042)
pub static EXP_TABLE: [u16; 256] = [
    0x7FA, 0x7F5, 0x7EF, 0x7EA, 0x7E4, 0x7DF, 0x7DA, 0x7D4, 0x7CF, 0x7C9, 0x7C4, 0x7BF, 0x7B9,
    0x7B4, 0x7AE, 0x7A9, 0x7A4, 0x79F, 0x799, 0x794, 0x78F, 0x78A, 0x784, 0x77F, 0x77A, 0x775,
    0x770, 0x76A, 0x765, 0x760, 0x75B, 0x756, 0x751, 0x74C, 0x747, 0x742, 0x73D, 0x738, 0x733,
    0x72E, 0x729, 0x724, 0x71F, 0x71A, 0x715, 0x710, 0x70B, 0x706, 0x702, 0x6FD, 0x6F8, 0x6F3,
    0x6EE, 0x6E9, 0x6E5, 0x6E0, 0x6DB, 0x6D6, 0x6D2, 0x6CD, 0x6C8, 0x6C4, 0x6BF, 0x6BA, 0x6B5,
    0x6B1, 0x6AC, 0x6A8, 0x6A3, 0x69E, 0x69A, 0x695, 0x691, 0x68C, 0x688, 0x683, 0x67F, 0x67A,
    0x676, 0x671, 0x66D, 0x668, 0x664, 0x65F, 0x65B, 0x657, 0x652, 0x64E, 0x649, 0x645, 0x641,
    0x63C, 0x638, 0x634, 0x630, 0x62B, 0x627, 0x623, 0x61E, 0x61A, 0x616, 0x612, 0x60E, 0x609,
    0x605, 0x601, 0x5FD, 0x5F9, 0x5F5, 0x5F0, 0x5EC, 0x5E8, 0x5E4, 0x5E0, 0x5DC, 0x5D8, 0x5D4,
    0x5D0, 0x5CC, 0x5C8, 0x5C4, 0x5C0, 0x5BC, 0x5B8, 0x5B4, 0x5B0, 0x5AC, 0x5A8, 0x5A4, 0x5A0,
    0x59C, 0x599, 0x595, 0x591, 0x58D, 0x589, 0x585, 0x582, 0x57E, 0x57A, 0x576, 0x572, 0x56F,
    0x56B, 0x567, 0x563, 0x560, 0x55C, 0x558, 0x554, 0x551, 0x54D, 0x549, 0x546, 0x542, 0x53E,
    0x53B, 0x537, 0x534, 0x530, 0x52C, 0x529, 0x525, 0x522, 0x51E, 0x51B, 0x517, 0x514, 0x510,
    0x50C, 0x509, 0x506, 0x502, 0x4FF, 0x4FB, 0x4F8, 0x4F4, 0x4F1, 0x4ED, 0x4EA, 0x4E7, 0x4E3,
    0x4E0, 0x4DC, 0x4D9, 0x4D6, 0x4D2, 0x4CF, 0x4CC, 0x4C8, 0x4C5, 0x4C2, 0x4BE, 0x4BB, 0x4B8,
    0x4B5, 0x4B1, 0x4AE, 0x4AB, 0x4A8, 0x4A4, 0x4A1, 0x49E, 0x49B, 0x498, 0x494, 0x491, 0x48E,
    0x48B, 0x488, 0x485, 0x482, 0x47E, 0x47B, 0x478, 0x475, 0x472, 0x46F, 0x46C, 0x469, 0x466,
    0x463, 0x460, 0x45D, 0x45A, 0x457, 0x454, 0x451, 0x44E, 0x44B, 0x448, 0x445, 0x442, 0x43F,
    0x43C, 0x439, 0x436, 0x433, 0x430, 0x42D, 0x42A, 0x428, 0x425, 0x422, 0x41F, 0x41C, 0x419,
    0x416, 0x414, 0x411, 0x40E, 0x40B, 0x408, 0x406, 0x403, 0x400,
];

/// Compute operator output using hardware-accurate log-sine + exp pipeline
///
/// This matches the real YM2612 hardware pipeline:
/// 1. Extract 10-bit phase, determine quadrant
/// 2. Look up quarter-wave log-sine attenuation (8-bit index)
/// 3. Add envelope and total level attenuation in log domain
/// 4. Convert back to linear using exp table
/// 5. Apply sign from phase quadrant
///
/// # Arguments
/// * `phase` - 20-bit phase accumulator value
/// * `modulation` - FM modulation input (added to phase before lookup)
/// * `envelope_level` - Envelope attenuation (0=max, 1023=silent)
/// * `total_level` - Total level attenuation (0=max, 127=silent)
///
/// # Returns
/// Signed output (max +/-8168 per operator: thirteen bits)
#[inline]
pub fn compute_operator_output(
    phase: u32,
    modulation: i16,
    envelope_level: u16,
    total_level: u8,
) -> i16 {
    // Extract 10-bit phase and add modulation
    // Phase accumulator is 20-bit, we use upper 10 bits
    let phase_10bit = ((phase >> 10) as i16).wrapping_add(modulation);
    let phase_10bit = phase_10bit as u16;

    // Determine quadrant from upper 2 bits
    let negate = (phase_10bit >> 9) & 1; // bit 9: 1 = negative half
    let mirror = (phase_10bit >> 8) & 1; // bit 8: 1 = mirror (descending)

    // Get 8-bit index into quarter-wave table
    let mut index = (phase_10bit & 0xFF) as usize;
    if mirror != 0 {
        index = 255 - index; // Mirror for descending quarter
    }

    // Look up log-sine attenuation
    let log_sin = LOG_SINE_TABLE[index] as u32;

    // Add envelope and TL attenuation in log domain
    //
    // The hardware combines three sources of attenuation in log domain:
    // 1. LOG_SINE_TABLE output: 12-bit (4.8 fixed point), ~0.023 dB per unit
    // 2. Envelope level: 10-bit (0-1023), ~0.094 dB per unit (96 dB range)
    // 3. Total Level: 7-bit -> 10-bit (register << 3), same scale as envelope
    //
    // To combine, envelope+TL (10-bit, 0.094 dB/unit) must be converted to
    // the same scale as the sine table (12-bit, 0.023 dB/unit).
    // Scale factor: 0.094 / 0.023 ~ 4, so << 2.
    //
    // Checked against Genesis Plus GX, whose tables hold these values at
    // twice the scale: TL=32 -> (256 << 2)=1024 -> output 127 here, 128 there.
    let eg_tl = (envelope_level as u32) + ((total_level as u32) * 8);
    let total_atten = log_sin + (eg_tl << 2);

    // If total attenuation exceeds max, output is silent
    if total_atten >= 0x1000 {
        return 0;
    }

    // Convert from log domain to linear using exp table
    // Split attenuation: integer part (shift) and fractional part (table index)
    let exp_index = (total_atten & 0xFF) as usize;
    let exp_shift = (total_atten >> 8) & 0x0F;

    // Look up exp table and apply the << 2 scaling factor to match real hardware.
    //
    // The operator's output is thirteen bits: eleven from the exponent
    // lookup, shifted left by two. That gives a max of ~8168, which is critical for correct
    // FM modulation depth -- the modulator output magnitude directly controls
    // how much phase modulation is applied to the carrier. Without this << 2,
    // our FM modulation is 4x too weak, producing thin sine-like timbres
    // instead of the rich harmonics that FM synthesis should create.
    let linear = (EXP_TABLE[exp_index] as u32) << 2 >> exp_shift;

    // Apply sign
    let output = linear as i16;
    if negate != 0 {
        -output
    } else {
        output
    }
}

/// LFO phase-modulation shifts, by PM sensitivity (0-7) and quarter-wave
/// step (0-7). Each entry packs two right-shift amounts, one per nibble,
/// to apply to the seven F-number bits the LFO modulates; a shift of 7
/// leaves nothing, so `0x77` contributes no modulation.
///
/// The table is ymfm's `s_lfo_pm_shifts` (`ymfm_fm.ipp`,
/// `opn_lfo_pm_phase_adjustment`), Copyright (c) 2021 Aaron Giles,
/// BSD 3-Clause; the notice is at the end of `LICENSE`.
const LFO_PM_SHIFTS: [[u8; 8]; 8] = [
    [0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77],
    [0x77, 0x77, 0x77, 0x77, 0x72, 0x72, 0x72, 0x72],
    [0x77, 0x77, 0x77, 0x72, 0x72, 0x72, 0x17, 0x17],
    [0x77, 0x77, 0x72, 0x72, 0x17, 0x17, 0x12, 0x12],
    [0x77, 0x77, 0x72, 0x17, 0x17, 0x17, 0x12, 0x07],
    [0x77, 0x77, 0x17, 0x12, 0x07, 0x07, 0x02, 0x01],
    [0x77, 0x77, 0x17, 0x12, 0x07, 0x07, 0x02, 0x01],
    [0x77, 0x77, 0x17, 0x12, 0x07, 0x07, 0x02, 0x01],
];

/// What one F-number bit contributes to the pitch modulation at a PM
/// sensitivity and quarter-wave step: ymfm's
/// `opn_lfo_pm_phase_adjustment` applied to a single set bit.
const fn lfo_pm_bit_adjustment(bit: usize, pms: usize, step: usize) -> u8 {
    let shifts = LFO_PM_SHIFTS[pms][step];
    let fnum_bits = 1u32 << bit;
    let mut adjust = (fnum_bits >> (shifts & 0x0F)) + (fnum_bits >> (shifts >> 4));
    if pms > 5 {
        adjust <<= pms - 5;
    }
    (adjust >> 2) as u8
}

/// LFO PM output: 7 F-number bits x 8 PM depths, each with 8 quarter-wave
/// steps; row `bit * 8 + depth`. Only the first quarter (positive,
/// ascending) is stored; [`compute_lfo_pm_offset`] reconstructs the full
/// 32-step waveform.
///
/// [`compute_lfo_pm_offset`] sums one row per set F-number bit. Applying
/// the shifts to all seven bits at once, as ymfm does, rounds differently
/// by one unit on 384 of the 8192 combinations of bits, depth and step.
static LFO_PM_OUTPUT: [[u8; 8]; 56] = build_lfo_pm_output();

const fn build_lfo_pm_output() -> [[u8; 8]; 56] {
    let mut table = [[0u8; 8]; 56];
    let mut bit = 0;
    while bit < 7 {
        let mut depth = 0;
        while depth < 8 {
            let mut step = 0;
            while step < 8 {
                table[bit * 8 + depth][step] = lfo_pm_bit_adjustment(bit, depth, step);
                step += 1;
            }
            depth += 1;
        }
        bit += 1;
    }
    table
}

/// Compute the LFO PM F-number offset.
///
/// # Arguments
/// * `fnum` - F-Number (11-bit, 0-2047). Bits 4-10 are used.
/// * `pms` - PM Sensitivity (0-7)
/// * `lfo_step` - LFO PM step (0-31, quarter wave is 8 steps)
///
/// # Returns
/// F-number offset (can be negative) to apply to the channel's F-number
pub fn compute_lfo_pm_offset(fnum: u16, pms: u8, lfo_step: u8) -> i32 {
    if pms == 0 {
        return 0;
    }

    // Extract the 7 meaningful bits of F-number (bits 4-10)
    let fnum_bits = ((fnum >> 4) & 0x7F) as usize;

    // Determine which quarter of the waveform we're in
    // step 0-7: positive ascending
    // step 8-15: positive descending (step ^ 7)
    // step 16-23: negative ascending
    // step 24-31: negative descending (step ^ 7)
    let step = lfo_step & 31;
    let quarter_step = if (step & 8) != 0 {
        (step & 7) ^ 7 // descending quarter
    } else {
        step & 7 // ascending quarter
    };
    let negate = (step & 16) != 0;

    // Sum contributions from each set F-number bit
    let depth = pms as usize;
    let mut value: i32 = 0;
    for bit in 0..7 {
        if (fnum_bits & (1 << bit)) != 0 {
            let row = bit * 8 + depth;
            value += LFO_PM_OUTPUT[row][quarter_step as usize] as i32;
        }
    }

    if negate {
        -value
    } else {
        value
    }
}

/// Detune adjustment by key code (0-31) and detune amount (0-3).
///
/// The table is ymfm's `s_detune_adjustment` (`ymfm_fm.ipp`,
/// `detune_adjustment`), Copyright (c) 2021 Aaron Giles, BSD 3-Clause; the
/// notice is at the end of `LICENSE`.
const DETUNE_BY_KEYCODE: [[u8; 4]; 32] = [
    [0, 0, 1, 2],
    [0, 0, 1, 2],
    [0, 0, 1, 2],
    [0, 0, 1, 2],
    [0, 1, 2, 2],
    [0, 1, 2, 3],
    [0, 1, 2, 3],
    [0, 1, 2, 3],
    [0, 1, 2, 4],
    [0, 1, 3, 4],
    [0, 1, 3, 4],
    [0, 1, 3, 5],
    [0, 2, 4, 5],
    [0, 2, 4, 6],
    [0, 2, 4, 6],
    [0, 2, 5, 7],
    [0, 2, 5, 8],
    [0, 3, 6, 8],
    [0, 3, 6, 9],
    [0, 3, 7, 10],
    [0, 4, 8, 11],
    [0, 4, 8, 12],
    [0, 4, 9, 13],
    [0, 5, 10, 14],
    [0, 5, 11, 16],
    [0, 6, 12, 17],
    [0, 6, 13, 19],
    [0, 7, 14, 20],
    [0, 8, 16, 22],
    [0, 8, 16, 22],
    [0, 8, 16, 22],
    [0, 8, 16, 22],
];

/// DT1 (detune) table: 4 detune settings x 32 key codes, as phase
/// increment adjustments.
/// DT1 = 0: no detune
/// DT1 = 1-3: positive detune (increasing amounts)
/// DT1 = 5-7: negative versions of 1-3
/// (DT1 bit 2 is the sign bit in the register)
pub static DETUNE_TABLE: [[i32; 32]; 4] = build_detune_table();

const fn build_detune_table() -> [[i32; 32]; 4] {
    let mut table = [[0i32; 32]; 4];
    let mut detune = 0;
    while detune < 4 {
        let mut key_code = 0;
        while key_code < 32 {
            table[detune][key_code] = DETUNE_BY_KEYCODE[key_code][detune] as i32;
            key_code += 1;
        }
        detune += 1;
    }
    table
}

// A plain sine table. The FM path reads LOG_SINE_TABLE and EXP_TABLE instead; this one
// stays in the public interface, and the tests below use it.
use std::f64::consts::PI;

pub const SINE_TABLE_SIZE: usize = 4096;
pub type SineTable = [i16; SINE_TABLE_SIZE];

pub fn generate_sine_table_runtime() -> SineTable {
    let mut table = [0i16; SINE_TABLE_SIZE];
    for (i, entry) in table.iter_mut().enumerate() {
        let angle = (i as f64 / SINE_TABLE_SIZE as f64) * 2.0 * PI;
        *entry = (angle.sin() * 4095.0) as i16;
    }
    table
}

use std::sync::OnceLock;
static SINE_TABLE_CACHE: OnceLock<SineTable> = OnceLock::new();

pub fn get_sine_table() -> &'static SineTable {
    SINE_TABLE_CACHE.get_or_init(generate_sine_table_runtime)
}

pub static SINE_TABLE: OnceLock<SineTable> = OnceLock::new();

pub fn init_sine_table() {
    let _ = SINE_TABLE.get_or_init(generate_sine_table_runtime);
}

pub fn sine_lookup(phase: u16) -> i16 {
    let index = (phase & 0xFFF) as usize;
    get_sine_table()[index]
}

/// FNV-1a over a table's entries. Lets a test pin every entry of a
/// generated table without restating the table.
#[cfg(test)]
pub(crate) fn table_checksum(entries: impl Iterator<Item = u8>) -> u64 {
    entries.fold(0xcbf2_9ce4_8422_2325, |hash, entry| {
        (hash ^ u64::from(entry)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfo_pm_output_is_generated_entry_for_entry() {
        assert_eq!(
            table_checksum(LFO_PM_OUTPUT.iter().flatten().copied()),
            0x5ccc_84f6_d948_b76c
        );
        // PM sensitivity 0 never modulates, whatever the F-number bit.
        for bit in 0..7 {
            assert_eq!(LFO_PM_OUTPUT[bit * 8], [0; 8]);
        }
        // The top F-number bit at the deepest sensitivity.
        assert_eq!(LFO_PM_OUTPUT[6 * 8 + 7], [0, 0, 32, 48, 64, 64, 80, 96]);
    }

    #[test]
    fn detune_table_is_the_key_code_table_turned() {
        assert_eq!(
            table_checksum(DETUNE_TABLE.iter().flatten().map(|&v| v as u8)),
            0xa941_7216_12dd_493b
        );
        assert_eq!(DETUNE_TABLE[0], [0; 32]);
        assert_eq!(DETUNE_TABLE[1][4], 1);
        assert_eq!(DETUNE_TABLE[3][31], 22);
    }

    #[test]
    fn test_compute_output_zero_phase() {
        // At phase 0, sin(0) ~ 0, so output should be near zero
        // log_sine_table[0] = 0x859 = large attenuation (near zero amplitude)
        let out = compute_operator_output(0, 0, 0, 0);
        assert!(out.abs() < 400, "sin(0) should be near zero, got {}", out);
    }

    #[test]
    fn test_compute_output_quarter() {
        // At phase = 1/4 of full cycle (pi/2), sin = 1 (maximum)
        // Phase 10-bit: 256 = quarter cycle
        // With the << 2, max output is ~8168 (thirteen bits)
        let phase = 256u32 << 10; // quarter cycle in 20-bit
        let out = compute_operator_output(phase, 0, 0, 0);
        // Should be near maximum positive output (~8168)
        assert!(
            out > 7000,
            "sin(pi/2) should be large positive (~8168), got {}",
            out
        );
    }

    #[test]
    fn test_compute_output_negative_half() {
        // At phase = 3/4 of full cycle (3pi/2), sin = -1 (minimum)
        let phase = 768u32 << 10; // 3/4 cycle in 20-bit
        let out = compute_operator_output(phase, 0, 0, 0);
        // Should be near maximum negative output (~-8168)
        assert!(
            out < -7000,
            "sin(3pi/2) should be large negative (~-8168), got {}",
            out
        );
    }

    #[test]
    fn test_compute_output_envelope_attenuates() {
        // With the << 2 output scaling + << 2 attenuation scaling:
        // env=1023: (1023 << 2) = 4092 -> output 0 (silence)
        // env=256: (256 << 2) = 1024 -> ~24 dB attenuation
        let phase = 256u32 << 10;
        let out_full = compute_operator_output(phase, 0, 0, 0);
        let out_max_env = compute_operator_output(phase, 0, 1023, 0);
        assert_eq!(
            out_max_env, 0,
            "Max envelope (1023) should produce silence, got {}",
            out_max_env
        );
        // Mid-range attenuation: env=256 ~ 1/16 of full output
        let out_mid_env = compute_operator_output(phase, 0, 256, 0);
        assert!(
            out_mid_env.abs() < out_full.abs() / 8,
            "env=256 should be ~1/16 of full: full={}, mid_env={}",
            out_full,
            out_mid_env
        );
        assert!(
            out_mid_env.abs() > 0,
            "env=256 should still produce audible output, got 0"
        );
    }

    #[test]
    fn test_compute_output_combined_mutes() {
        // With both max TL and max envelope, output is zero (total silence)
        // (1023 + 127*8) << 2 = 2039 << 2 = 8156, well above 4096 threshold
        let phase = 256u32 << 10;
        let out = compute_operator_output(phase, 0, 1023, 127);
        assert_eq!(
            out, 0,
            "Max TL + max envelope should be completely silent, got {}",
            out
        );
    }

    #[test]
    fn test_compute_output_modulation() {
        // Same phase but different modulation should give different output
        let phase = 256u32 << 10;
        let out1 = compute_operator_output(phase, 0, 0, 0);
        let out2 = compute_operator_output(phase, 200, 0, 0);
        assert_ne!(out1, out2, "Modulation should change output");
    }

    #[test]
    fn test_compute_output_symmetry() {
        // Output at phase and phase+half_cycle should be negatives of each other
        let phase1 = 128u32 << 10; // 1/8 cycle
        let phase2 = (128 + 512) << 10; // 1/8 + 1/2 cycle
        let out1 = compute_operator_output(phase1, 0, 0, 0);
        let out2 = compute_operator_output(phase2, 0, 0, 0);
        // They should be approximately negatives (might differ by 1 due to rounding)
        assert!(
            (out1 + out2).abs() <= 1,
            "Should be symmetric: {} vs {}",
            out1,
            out2
        );
    }

    // Tests of the plain sine table
    #[test]
    fn test_sine_table_generation() {
        let table = generate_sine_table_runtime();
        assert_eq!(table.len(), SINE_TABLE_SIZE);
        let non_zero = table.iter().filter(|&&x| x != 0).count();
        assert!(non_zero > 100);
    }

    #[test]
    fn test_sine_lookup_wrapping() {
        init_sine_table();
        let val1 = sine_lookup(0);
        let val2 = sine_lookup(4096);
        assert_eq!(val1, val2);
    }
}
