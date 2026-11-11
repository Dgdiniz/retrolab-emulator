//! The interpolator: four decoded samples weighted by a Gaussian curve
//! the chip holds in ROM, picked by the fractional part of the voice's
//! position.
//!
//! A voice's position has twelve fraction bits; the top eight of them
//! index the 512-entry table from both ends, so the two outer samples
//! get the tails of the curve and the two inner ones its body. The four
//! weights sum to about 2048, and each product is scaled by 2048, so a
//! constant input comes out unchanged. The chip adds the first three
//! terms in sixteen bits (the sum wraps there), adds the fourth in full
//! width, clamps, and clears bit 0.
//!
//! The table is the chip's own ROM, as published in fullsnes (hardware
//! data); the arithmetic around it was checked against bsnes-plus.

use super::brr::SampleRing;
use super::clamp16;

/// The S-DSP's Gaussian weights.
const GAUSSIAN: [i16; 512] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2,
    2, 2, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 8, 8, 8, 9, 9, 9, 10, 10,
    10, 11, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15, 15, 16, 16, 17, 17, 18, 19, 19, 20, 20, 21, 21,
    22, 23, 23, 24, 24, 25, 26, 27, 27, 28, 29, 29, 30, 31, 32, 32, 33, 34, 35, 36, 36, 37, 38, 39,
    40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 58, 59, 60, 61, 62, 64, 65,
    66, 67, 69, 70, 71, 73, 74, 76, 77, 78, 80, 81, 83, 84, 86, 87, 89, 90, 92, 94, 95, 97, 99,
    100, 102, 104, 106, 107, 109, 111, 113, 115, 117, 118, 120, 122, 124, 126, 128, 130, 132, 134,
    137, 139, 141, 143, 145, 147, 150, 152, 154, 156, 159, 161, 163, 166, 168, 171, 173, 175, 178,
    180, 183, 186, 188, 191, 193, 196, 199, 201, 204, 207, 210, 212, 215, 218, 221, 224, 227, 230,
    233, 236, 239, 242, 245, 248, 251, 254, 257, 260, 263, 267, 270, 273, 276, 280, 283, 286, 290,
    293, 297, 300, 304, 307, 311, 314, 318, 321, 325, 328, 332, 336, 339, 343, 347, 351, 354, 358,
    362, 366, 370, 374, 378, 381, 385, 389, 393, 397, 401, 405, 410, 414, 418, 422, 426, 430, 434,
    439, 443, 447, 451, 456, 460, 464, 469, 473, 477, 482, 486, 491, 495, 499, 504, 508, 513, 517,
    522, 527, 531, 536, 540, 545, 550, 554, 559, 563, 568, 573, 577, 582, 587, 592, 596, 601, 606,
    611, 615, 620, 625, 630, 635, 640, 644, 649, 654, 659, 664, 669, 674, 678, 683, 688, 693, 698,
    703, 708, 713, 718, 723, 728, 732, 737, 742, 747, 752, 757, 762, 767, 772, 777, 782, 787, 792,
    797, 802, 806, 811, 816, 821, 826, 831, 836, 841, 846, 851, 855, 860, 865, 870, 875, 880, 884,
    889, 894, 899, 904, 908, 913, 918, 923, 927, 932, 937, 941, 946, 951, 955, 960, 965, 969, 974,
    978, 983, 988, 992, 997, 1001, 1005, 1010, 1014, 1019, 1023, 1027, 1032, 1036, 1040, 1045,
    1049, 1053, 1057, 1061, 1066, 1070, 1074, 1078, 1082, 1086, 1090, 1094, 1098, 1102, 1106, 1109,
    1113, 1117, 1121, 1125, 1128, 1132, 1136, 1139, 1143, 1146, 1150, 1153, 1157, 1160, 1164, 1167,
    1170, 1174, 1177, 1180, 1183, 1186, 1190, 1193, 1196, 1199, 1202, 1205, 1207, 1210, 1213, 1216,
    1219, 1221, 1224, 1227, 1229, 1232, 1234, 1237, 1239, 1241, 1244, 1246, 1248, 1251, 1253, 1255,
    1257, 1259, 1261, 1263, 1265, 1267, 1269, 1270, 1272, 1274, 1275, 1277, 1279, 1280, 1282, 1283,
    1284, 1286, 1287, 1288, 1290, 1291, 1292, 1293, 1294, 1295, 1296, 1297, 1297, 1298, 1299, 1300,
    1300, 1301, 1302, 1302, 1303, 1303, 1303, 1304, 1304, 1304, 1304, 1304, 1305, 1305,
];

/// The four weights for a fraction (the top eight of the twelve fraction
/// bits), in sample order: the outer two (first and last) get the curve's
/// tails, the inner two its body.
#[inline]
fn weights(fraction: usize) -> [i32; 4] {
    [
        i32::from(GAUSSIAN[255 - fraction]),
        i32::from(GAUSSIAN[511 - fraction]),
        i32::from(GAUSSIAN[256 + fraction]),
        i32::from(GAUSSIAN[fraction]),
    ]
}

/// The sample at `position`, whole samples ahead of the ring's head in
/// the bits above the twelve fraction bits, interpolated from the four
/// samples around it.
pub(super) fn interpolate(ring: &SampleRing, position: i32) -> i32 {
    let fraction = ((position >> 4) & 0xff) as usize;
    let whole = position >> 12;
    let [w0, w1, w2, w3] = weights(fraction);
    let term = |weight: i32, ahead: i32| (weight * ring.at(whole + ahead)) >> 11;
    // The first three terms add up in a 16-bit adder.
    let mut out = term(w0, 0) + term(w1, 1) + term(w2, 2);
    out = i32::from(out as i16);
    out += term(w3, 3);
    clamp16(out) & !1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_weights_of_any_fraction_sum_to_unity() {
        for fraction in 0..256 {
            let sum: i32 = weights(fraction).iter().sum();
            assert!((sum - 2048).abs() <= 2, "fraction {fraction}: {sum}");
        }
    }

    #[test]
    fn a_constant_input_comes_out_unchanged_and_even() {
        let mut ring = SampleRing::EMPTY;
        for _ in 0..SampleRing::LEN {
            ring.push(1000);
        }
        for position in [0, 0x800, 0xfff, 0x3000, 0x3fff] {
            let out = interpolate(&ring, position);
            assert!((980..=1010).contains(&out), "position {position:#x}: {out}");
            assert_eq!(out & 1, 0);
        }
    }

    #[test]
    fn the_fraction_moves_the_weight_between_neighbours() {
        // A single non-zero sample three slots ahead of the head: at
        // fraction 0 it is the outermost tap (weight 0); as the position
        // advances it gains weight.
        let mut ring = SampleRing::EMPTY;
        ring.push(0);
        ring.push(0);
        ring.push(0);
        ring.push(10000);
        ring.rewind();
        assert_eq!(interpolate(&ring, 0), 0);
        let near = interpolate(&ring, 0xf00);
        assert!(near > 0, "{near}");
        assert!(interpolate(&ring, 0x1800) > near);
    }
}
