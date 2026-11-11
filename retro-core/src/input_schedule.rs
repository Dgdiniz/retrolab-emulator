//! Scripted controller input for a ROM run: a player-1 button mask
//! installed at a given frame and held until the next set-point, plus
//! console resets at given frames. The text grammar is the one the CLIs
//! take (`--press-at "180:start;186:;240:a,b"`, `--reset-at "360,720"`),
//! and the golden-run manifests use it too.
//!
//! The grammar is the same for every console; what differs is the names
//! of the buttons, so each core passes its own table of `(name, bit)`
//! pairs, the bit being the button's position in
//! [`InputState::buttons`](crate::InputState).

use std::collections::BTreeSet;

/// Parse a button list (names separated by `,` or `+`) into the generic
/// input bitmask, looking each name up in `buttons`. Returns `Ok(None)`
/// when no spec was given; an unknown name is an error so a typo fails
/// loudly instead of silently pressing nothing.
pub fn parse_button_mask(
    spec: Option<&str>,
    buttons: &[(&str, u32)],
) -> Result<Option<u32>, String> {
    let Some(spec) = spec else {
        return Ok(None);
    };
    let mut mask = 0u32;
    for name in spec
        .split([',', '+'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let lower = name.to_ascii_lowercase();
        let bit = buttons
            .iter()
            .find(|(n, _)| *n == lower)
            .map(|(_, bit)| *bit)
            .ok_or_else(|| {
                let known: Vec<&str> = buttons.iter().map(|(n, _)| *n).collect();
                format!("unknown button '{name}' (expected {})", known.join(","))
            })?;
        mask |= 1 << bit;
    }
    Ok(Some(mask))
}

/// A frame-scheduled controller-input program. Each `(frame, mask)`
/// set-point installs that player-1 mask starting on `frame` and holds it
/// until the next set-point; a mask of `0` releases every button.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PressSchedule {
    /// Sorted `(frame, mask)` set-points.
    points: Vec<(u64, u32)>,
}

impl PressSchedule {
    /// The mask in effect on `frame` (the latest set-point with
    /// `set_frame <= frame`), or `None` before the first set-point, so
    /// the caller leaves input untouched until the schedule begins.
    pub fn mask_at(&self, frame: u64) -> Option<u32> {
        let mut current = None;
        for &(f, m) in &self.points {
            if f <= frame {
                current = Some(m);
            } else {
                break;
            }
        }
        current
    }

    /// The sorted set-points.
    pub fn points(&self) -> &[(u64, u32)] {
        &self.points
    }
}

/// Parse a `"F:buttons;F:buttons"` spec into a [`PressSchedule`]. Returns
/// `Ok(None)` when no spec (or only separators) was given. A malformed
/// segment, a non-numeric frame or an unknown button name is an error.
pub fn parse_press_schedule(
    spec: Option<&str>,
    buttons: &[(&str, u32)],
) -> Result<Option<PressSchedule>, String> {
    let Some(spec) = spec else {
        return Ok(None);
    };
    let mut points: Vec<(u64, u32)> = Vec::new();
    for seg in spec.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (frame_s, buttons_s) = seg
            .split_once(':')
            .ok_or_else(|| format!("invalid press segment '{seg}' (expected 'frame:buttons')"))?;
        let frame: u64 = frame_s.trim().parse().map_err(|_| {
            format!(
                "invalid frame number '{}' in press schedule",
                frame_s.trim()
            )
        })?;
        // An empty button list releases all buttons on that frame.
        let mask = parse_button_mask(Some(buttons_s), buttons)?.unwrap_or(0);
        points.push((frame, mask));
    }
    if points.is_empty() {
        return Ok(None);
    }
    points.sort_by_key(|&(f, _)| f);
    Ok(Some(PressSchedule { points }))
}

/// Parse a `"360,720"` list of frames at which the console is reset into
/// a sorted, de-duplicated set. Empty / `None` -> no resets. A non-numeric
/// entry is an error so a typo cannot silently skip a reset.
pub fn parse_reset_frames(spec: Option<&str>) -> Result<BTreeSet<u64>, String> {
    let Some(spec) = spec else {
        return Ok(BTreeSet::new());
    };
    spec.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| {
            t.parse::<u64>()
                .map_err(|_| format!("invalid reset frame {t:?} (want an integer)"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAD: [(&str, u32); 4] = [("up", 0), ("a", 4), ("b", 5), ("start", 10)];

    #[test]
    fn button_mask_none_for_missing_spec() {
        assert_eq!(parse_button_mask(None, &PAD).unwrap(), None);
    }

    #[test]
    fn button_mask_combines_trims_ignores_case_and_takes_either_separator() {
        let mask = parse_button_mask(Some(" Start , A ,b"), &PAD)
            .unwrap()
            .unwrap();
        assert_eq!(mask, (1 << 10) | (1 << 4) | (1 << 5));
        assert_eq!(
            parse_button_mask(Some("a+b"), &PAD).unwrap(),
            Some((1 << 4) | (1 << 5))
        );
    }

    #[test]
    fn button_mask_rejects_a_name_the_table_lacks_and_lists_the_known_ones() {
        let err = parse_button_mask(Some("turbo"), &PAD).unwrap_err();
        assert!(
            err.contains("turbo") && err.contains("up,a,b,start"),
            "{err}"
        );
    }

    #[test]
    fn button_mask_of_separators_only_is_zero() {
        assert_eq!(parse_button_mask(Some(" , "), &PAD).unwrap(), Some(0));
    }

    #[test]
    fn press_schedule_none_for_missing_or_empty_spec() {
        assert_eq!(parse_press_schedule(None, &PAD).unwrap(), None);
        assert_eq!(parse_press_schedule(Some(" ; "), &PAD).unwrap(), None);
    }

    #[test]
    fn press_schedule_sorts_setpoints_and_reads_a_release() {
        let sched = parse_press_schedule(Some("186:;180:start"), &PAD)
            .unwrap()
            .unwrap();
        assert_eq!(sched.points(), &[(180, 1 << 10), (186, 0)]);
    }

    #[test]
    fn press_schedule_holds_a_mask_until_the_next_setpoint() {
        let sched = parse_press_schedule(Some("180:start;186:"), &PAD)
            .unwrap()
            .unwrap();
        assert_eq!(sched.mask_at(179), None);
        assert_eq!(sched.mask_at(180), Some(1 << 10));
        assert_eq!(sched.mask_at(185), Some(1 << 10));
        assert_eq!(sched.mask_at(186), Some(0));
        assert_eq!(sched.mask_at(10_000), Some(0));
    }

    #[test]
    fn press_schedule_rejects_bad_frame_and_button_and_shape() {
        assert!(parse_press_schedule(Some("notaframe:start"), &PAD).is_err());
        assert!(parse_press_schedule(Some("12:bogus"), &PAD).is_err());
        assert!(parse_press_schedule(Some("12start"), &PAD).is_err());
    }

    #[test]
    fn reset_frames_sorted_and_deduplicated() {
        let got = parse_reset_frames(Some(" 720, 360 ,360")).unwrap();
        assert_eq!(got.into_iter().collect::<Vec<_>>(), vec![360, 720]);
    }

    #[test]
    fn reset_frames_none_or_empty_is_no_resets() {
        assert!(parse_reset_frames(None).unwrap().is_empty());
        assert!(parse_reset_frames(Some("")).unwrap().is_empty());
        assert!(parse_reset_frames(Some(" , ")).unwrap().is_empty());
    }

    #[test]
    fn reset_frames_rejects_non_numeric() {
        assert!(parse_reset_frames(Some("360,oops")).is_err());
    }
}
