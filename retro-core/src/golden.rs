//! Golden runs: drive a ROM for a fixed window with scripted input and
//! record, frame by frame, a hash of the machine state, of the visible
//! picture and of the audio produced. A change to a core is then held to
//! "the same output, frame for frame" against a recorded run, the gate
//! a rewrite of a module passes before it lands.
//!
//! Deterministic by construction: scripted presses and resets at fixed
//! frames, the presenting `run_frame()` (so the picture is rendered),
//! audio capture armed so the host mix is produced and drained every
//! frame. What is here is the same for every console, the run loop, the
//! record file, the comparison, the manifest grammar and where the records
//! are looked for; each core adds its own layer (the state hash, the button
//! names, its manifest flags).
//!
//! The records themselves are not in this tree. They are made from games
//! the repository does not carry (hashes of their frames, savestates taken
//! in them), so they live outside it ([`GoldenDir`]).

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::input_schedule::{parse_press_schedule, parse_reset_frames, PressSchedule};
use crate::{fnv1a64, EmulatorCore, InputState};

/// What a golden run does: how long, when to take pictures, what to press
/// and when to reset.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GoldenSpec {
    /// Frames to run.
    pub frames: u64,
    /// Frame counts after which a picture is handed to the shot callback
    /// (`300` = after 300 frames, the same count `screenshot --frames 300`
    /// means).
    pub shots: BTreeSet<u64>,
    /// Scripted player-1 input, if any.
    pub presses: Option<PressSchedule>,
    /// Frames before which the console is reset.
    pub resets: BTreeSet<u64>,
}

/// What one frame produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameRecord {
    /// Frames completed, counting from 1.
    pub frame: u64,
    /// Hash of the serialized machine state.
    pub state: u64,
    /// Hash of the visible picture (the active display rectangle, RGB).
    pub video: u64,
    /// Hash of the audio samples the frame produced.
    pub audio: u64,
    /// How many samples (interleaved stereo values) the frame produced.
    pub samples: u32,
    /// The active display size the picture was hashed at.
    pub width: u32,
    pub height: u32,
}

/// Drive `emu` through `spec`, calling `on_shot(frame, width, height,
/// rgb)` at each listed shot and `on_audio(samples)` with every frame's
/// samples, and return one record per frame. `run_frame` advances the
/// machine one presented frame (the trait's `run_frame`, or the frame
/// loop a player's host actually calls when a core has more than one),
/// and `state_hash` is the core's hash of its whole serialized state.
/// `emu` must have its ROM loaded and be ready to run; audio capture is
/// armed here.
pub fn run_golden<E: EmulatorCore>(
    emu: &mut E,
    spec: &GoldenSpec,
    mut run_frame: impl FnMut(&mut E),
    state_hash: impl Fn(&E) -> u64,
    mut on_shot: impl FnMut(u64, u32, u32, &[u8]),
    mut on_audio: impl FnMut(&[f32]),
) -> Vec<FrameRecord> {
    emu.set_audio_enabled(true);
    let mut records = Vec::with_capacity(spec.frames as usize);
    for i in 0..spec.frames {
        if spec.resets.contains(&i) {
            emu.reset();
        }
        if let Some(sched) = &spec.presses {
            if let Some(buttons) = sched.mask_at(i) {
                emu.set_input(0, InputState { buttons });
            }
        }
        run_frame(emu);

        let (audio, samples) = {
            let s = emu.get_audio_samples();
            on_audio(s);
            (hash_samples(s), s.len() as u32)
        };
        let (width, height, rgb) = {
            let disp = emu.get_display_info();
            let (w, h) = (disp.width, disp.height);
            let fb = emu.get_framebuffer();
            crop_framebuffer_rgb(&fb.data, fb.width, fb.height, w, h)
        };
        let frame = i + 1;
        if spec.shots.contains(&frame) {
            on_shot(frame, width, height, &rgb);
        }
        records.push(FrameRecord {
            frame,
            state: state_hash(emu),
            video: fnv1a64(&rgb),
            audio,
            samples,
            width,
            height,
        });
    }
    records
}

/// Hash interleaved `f32` samples by their exact bit patterns, so a change
/// of one bit in one sample changes the hash.
pub fn hash_samples(samples: &[f32]) -> u64 {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        bytes.extend_from_slice(&s.to_bits().to_le_bytes());
    }
    fnv1a64(&bytes)
}

/// Crop the active `disp_width x disp_height` rectangle out of an
/// RGBA8888 framebuffer that is physically `fb_width` pixels wide, and
/// return it as packed RGB (3 bytes/px).
///
/// The SNES renderer writes every row at the full `fb_width` stride
/// (512 px) regardless of the active mode, so cropping a 256-wide
/// display means skipping the cold right half of each row. The returned
/// `(w, h)` are clamped to both the requested display size and what the
/// buffer can actually supply, so a short or mis-sized buffer can never
/// panic or read past the end.
pub fn crop_framebuffer_rgb(
    data: &[u8],
    fb_width: u32,
    fb_height: u32,
    disp_width: u32,
    disp_height: u32,
) -> (u32, u32, Vec<u8>) {
    let w = disp_width.min(fb_width);
    let stride = fb_width as usize * 4;
    // How many whole rows the buffer can actually back.
    let backed_rows = data.len().checked_div(stride).unwrap_or(0) as u32;
    let h = disp_height.min(fb_height).min(backed_rows);

    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        let row_base = y as usize * stride;
        for x in 0..w {
            let idx = row_base + x as usize * 4;
            rgb.push(data[idx]);
            rgb.push(data[idx + 1]);
            rgb.push(data[idx + 2]);
        }
    }
    (w, h, rgb)
}

// -- The record file -----------------------------------------------------

/// Render records as the text file a CLI writes and a test reads: the
/// format line (`format` is the console's version tag, e.g.
/// `snes1-golden 1`), the caller's header lines (each prefixed `# `), a
/// column line, then one line per frame.
pub fn format_records(format: &str, header: &[String], records: &[FrameRecord]) -> String {
    let mut out = String::new();
    out.push_str("# ");
    out.push_str(format);
    out.push('\n');
    for line in header {
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("# frame state video audio samples width height\n");
    for r in records {
        out.push_str(&format!(
            "{} {:016x} {:016x} {:016x} {} {} {}\n",
            r.frame, r.state, r.video, r.audio, r.samples, r.width, r.height
        ));
    }
    out
}

/// Parse a record file back. Header lines are returned without their
/// `# ` prefix; the format line must be `format`.
pub fn parse_records(format: &str, text: &str) -> Result<(Vec<String>, Vec<FrameRecord>), String> {
    let mut lines = text.lines();
    match lines.next() {
        Some(first) if first.trim_start_matches("# ").trim() == format => {}
        other => return Err(format!("not a golden record file (first line {other:?})")),
    }
    let mut header = Vec::new();
    let mut records = Vec::new();
    for (n, line) in lines.enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(h) = line.strip_prefix('#') {
            let h = h.trim();
            if !h.starts_with("frame state") {
                header.push(h.to_string());
            }
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() != 7 {
            return Err(format!(
                "line {}: expected 7 fields, found {}",
                n + 2,
                f.len()
            ));
        }
        let hex = |s: &str, what: &str| {
            u64::from_str_radix(s, 16).map_err(|_| format!("line {}: bad {what} '{s}'", n + 2))
        };
        let num = |s: &str, what: &str| {
            s.parse::<u64>()
                .map_err(|_| format!("line {}: bad {what} '{s}'", n + 2))
        };
        records.push(FrameRecord {
            frame: num(f[0], "frame")?,
            state: hex(f[1], "state hash")?,
            video: hex(f[2], "video hash")?,
            audio: hex(f[3], "audio hash")?,
            samples: num(f[4], "sample count")? as u32,
            width: num(f[5], "width")? as u32,
            height: num(f[6], "height")? as u32,
        });
    }
    Ok((header, records))
}

/// Where two runs first part: which frame, and which of the three
/// outputs differ there. `None` when every frame of both matches and both
/// have the same length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    pub frame: u64,
    pub state: bool,
    pub video: bool,
    pub audio: bool,
    /// One run ended before the other reached this frame.
    pub length: bool,
}

impl Divergence {
    /// The differing outputs as a short word list, for a report line.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.length {
            parts.push("length");
        }
        if self.state {
            parts.push("state");
        }
        if self.video {
            parts.push("video");
        }
        if self.audio {
            parts.push("audio");
        }
        parts.join("+")
    }
}

/// Compare two runs frame by frame.
pub fn first_divergence(a: &[FrameRecord], b: &[FrameRecord]) -> Option<Divergence> {
    for (ra, rb) in a.iter().zip(b) {
        let state = ra.state != rb.state;
        let video = ra.video != rb.video || ra.width != rb.width || ra.height != rb.height;
        let audio = ra.audio != rb.audio || ra.samples != rb.samples;
        if state || video || audio {
            return Some(Divergence {
                frame: ra.frame,
                state,
                video,
                audio,
                length: false,
            });
        }
    }
    if a.len() != b.len() {
        let frame = a.len().min(b.len()) as u64 + 1;
        return Some(Divergence {
            frame,
            state: false,
            video: false,
            audio: false,
            length: true,
        });
    }
    None
}

// -- Where the records live ----------------------------------------------

/// The environment variable that names the folder holding the golden
/// records, in place of the one [`GoldenDir::from_env`] takes by default.
pub const GOLDEN_DIR_VAR: &str = "RETROLAB_GOLDEN_DIR";

/// The folder holding a set of golden records: the manifests (`snes.txt`,
/// `genesis.txt`), the records beside them, and the savestates taken in
/// the games they run.
///
/// It is kept outside the emulator's tree, by default `emulator-golden`,
/// beside the emulator's own folder. A test that finds no records there
/// skips. A folder named through [`GOLDEN_DIR_VAR`] that holds none is an
/// error, so a mistyped name cannot pass for a clean run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoldenDir {
    /// The folder.
    pub path: PathBuf,
    /// Whether [`GOLDEN_DIR_VAR`] named it, rather than it being taken by
    /// default.
    pub named: bool,
}

impl GoldenDir {
    /// The folder [`GOLDEN_DIR_VAR`] names, else `emulator-golden` beside
    /// `emulator_root`.
    pub fn from_env(emulator_root: &Path) -> Self {
        Self::resolve(std::env::var_os(GOLDEN_DIR_VAR), emulator_root)
    }

    /// [`GoldenDir::from_env`] with the variable's value given; an empty
    /// value counts as unset.
    pub fn resolve(named: Option<OsString>, emulator_root: &Path) -> Self {
        match named {
            Some(path) if !path.is_empty() => Self {
                path: PathBuf::from(path),
                named: true,
            },
            _ => Self {
                path: beside(emulator_root),
                named: false,
            },
        }
    }

    /// `file`, a path inside the folder, when it is there. When it is not:
    /// `Ok(None)` for the default folder (there are no records here, and
    /// the caller skips), and an error for a named one.
    pub fn find(&self, file: &str) -> Result<Option<PathBuf>, String> {
        let path = self.path.join(file);
        if path.is_file() {
            Ok(Some(path))
        } else if self.named {
            Err(format!(
                "{GOLDEN_DIR_VAR} names {}, which holds no {file}",
                self.path.display()
            ))
        } else {
            Ok(None)
        }
    }
}

/// `emulator-golden` in the folder that holds `emulator_root`. A test
/// gives its manifest directory with `..` steps after it, so the root is
/// resolved first when it exists, and the path a message shows is plain.
fn beside(emulator_root: &Path) -> PathBuf {
    let root = emulator_root
        .canonicalize()
        .unwrap_or_else(|_| emulator_root.to_path_buf());
    match (root.file_name(), root.parent()) {
        (Some(_), Some(parent)) => parent.join("emulator-golden"),
        _ => root.join("..").join("emulator-golden"),
    }
}

// -- The manifest --------------------------------------------------------

/// One line of a golden manifest: a ROM and how to run it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoldenEntry {
    /// Short name; the record is `<name>.hashes` and the output folder
    /// `<out>/<name>/`.
    pub name: String,
    /// ROM path, relative to the emulator root.
    pub rom: PathBuf,
    pub frames: u64,
    /// Shot frames; empty means "the last frame".
    pub shots: BTreeSet<u64>,
    pub press_at: Option<String>,
    pub reset_at: Option<String>,
    /// The bare words on the line (`force-detect`, `six-button`...): the
    /// switches of the console the manifest is for.
    pub flags: BTreeSet<String>,
}

impl GoldenEntry {
    /// Whether the line carries the bare word `flag`.
    pub fn has_flag(&self, flag: &str) -> bool {
        self.flags.contains(flag)
    }

    /// The run spec this entry describes, its presses read with the
    /// console's `buttons`.
    pub fn spec(&self, buttons: &[(&str, u32)]) -> Result<GoldenSpec, String> {
        let shots = if self.shots.is_empty() {
            BTreeSet::from([self.frames])
        } else {
            self.shots.clone()
        };
        Ok(GoldenSpec {
            frames: self.frames,
            shots,
            presses: parse_press_schedule(self.press_at.as_deref(), buttons)?,
            resets: parse_reset_frames(self.reset_at.as_deref())?,
        })
    }
}

/// Split a manifest line into fields on whitespace, keeping a
/// double-quoted value whole (`rom="roms/Some Game (U).sfc"`) and
/// dropping its quotes. An unclosed quote is an error.
fn split_fields(line: &str) -> Result<Vec<String>, String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut pending = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                pending = true;
            }
            c if c.is_whitespace() && !quoted => {
                if pending {
                    fields.push(std::mem::take(&mut cur));
                    pending = false;
                }
            }
            c => {
                cur.push(c);
                pending = true;
            }
        }
    }
    if quoted {
        return Err("unclosed quote".to_string());
    }
    if pending {
        fields.push(cur);
    }
    Ok(fields)
}

/// Strip a trailing comment: a `#` that starts a field (at the start of
/// the line or after whitespace) and is not inside double quotes.
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut prev_space = true;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted && prev_space => return &line[..i],
            _ => {}
        }
        prev_space = c.is_whitespace();
    }
    line
}

/// Parse a manifest: one entry per line as `key=value` fields separated by
/// whitespace (`name=game rom=roms/game.sfc frames=900
/// shots=60,300,900 press-at=150:start;162: reset-at=480 force-detect`),
/// a value with spaces in double quotes; `#` starts a comment. `name`,
/// `rom` and `frames` are required; a repeated name is an error. The
/// presses are read with the console's `buttons`, and a bare word must be
/// one of the console's `flags`.
pub fn parse_manifest(
    text: &str,
    buttons: &[(&str, u32)],
    flags: &[&str],
) -> Result<Vec<GoldenEntry>, String> {
    let mut entries: Vec<GoldenEntry> = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let at = |msg: String| format!("manifest line {}: {msg}", n + 1);
        let mut name = None;
        let mut rom = None;
        let mut frames = None;
        let mut shots = BTreeSet::new();
        let mut press_at = None;
        let mut reset_at = None;
        let mut set = BTreeSet::new();
        for field in split_fields(line).map_err(at)? {
            let field = field.as_str();
            let (key, value) = match field.split_once('=') {
                Some(kv) => kv,
                None if flags.contains(&field) => {
                    set.insert(field.to_string());
                    continue;
                }
                None => return Err(at(format!("unexpected field '{field}'"))),
            };
            match key {
                "name" => name = Some(value.to_string()),
                "rom" => rom = Some(PathBuf::from(value)),
                "frames" => {
                    frames = Some(
                        value
                            .parse::<u64>()
                            .map_err(|_| at(format!("bad frames '{value}'")))?,
                    )
                }
                "shots" => {
                    for s in value.split(',').filter(|s| !s.is_empty()) {
                        shots.insert(
                            s.parse::<u64>()
                                .map_err(|_| at(format!("bad shot frame '{s}'")))?,
                        );
                    }
                }
                "press-at" => press_at = Some(value.to_string()),
                "reset-at" => reset_at = Some(value.to_string()),
                other => return Err(at(format!("unknown key '{other}'"))),
            }
        }
        let name = name.ok_or_else(|| at("missing name=".into()))?;
        let rom = rom.ok_or_else(|| at("missing rom=".into()))?;
        let frames = frames.ok_or_else(|| at("missing frames=".into()))?;
        if frames == 0 {
            return Err(at("frames must be > 0".into()));
        }
        if entries.iter().any(|e| e.name == name) {
            return Err(at(format!("duplicate name '{name}'")));
        }
        let entry = GoldenEntry {
            name,
            rom,
            frames,
            shots,
            press_at,
            reset_at,
            flags: set,
        };
        // Validate the schedules now, so a typo fails before any ROM runs.
        entry.spec(buttons).map_err(at)?;
        entries.push(entry);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORMAT: &str = "test-golden 1";
    const PAD: [(&str, u32); 2] = [("a", 4), ("start", 10)];
    const FLAGS: [&str; 1] = ["force-detect"];

    fn manifest(text: &str) -> Result<Vec<GoldenEntry>, String> {
        parse_manifest(text, &PAD, &FLAGS)
    }

    #[test]
    fn the_records_are_looked_for_beside_the_emulator_unless_a_folder_is_named() {
        let root = Path::new("/work/retrolab-emulator");
        let default = GoldenDir::resolve(None, root);
        assert_eq!(default.path, Path::new("/work/emulator-golden"));
        assert!(!default.named);
        assert_eq!(GoldenDir::resolve(Some(OsString::new()), root), default);

        // A root given with `..` steps after it, as a test gives its
        // manifest directory, is resolved when it exists.
        let tree = std::env::temp_dir().join(format!("retro-core-root-{}", std::process::id()));
        std::fs::create_dir_all(tree.join("emulator/cores/a/core")).unwrap();
        assert_eq!(
            GoldenDir::resolve(None, &tree.join("emulator/cores/a/core/../../..")).path,
            tree.canonicalize().unwrap().join("emulator-golden")
        );
        std::fs::remove_dir_all(&tree).unwrap();
        // One that does not exist and ends in a step is taken as given.
        let stepped = Path::new("/no/such/tree/..");
        assert_eq!(
            GoldenDir::resolve(None, stepped).path,
            stepped.join("..").join("emulator-golden")
        );

        let named = GoldenDir::resolve(Some(OsString::from("/elsewhere/records")), root);
        assert_eq!(named.path, PathBuf::from("/elsewhere/records"));
        assert!(named.named);
    }

    #[test]
    fn a_missing_record_skips_by_default_and_fails_in_a_named_folder() {
        let dir = std::env::temp_dir().join(format!("retro-core-golden-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("snes.txt"), "").unwrap();
        let named = GoldenDir {
            path: dir.clone(),
            named: true,
        };
        let default = GoldenDir {
            path: dir.clone(),
            named: false,
        };

        assert_eq!(named.find("snes.txt").unwrap(), Some(dir.join("snes.txt")));
        assert_eq!(default.find("genesis.txt").unwrap(), None);
        let error = named.find("genesis.txt").unwrap_err();
        assert!(
            error.contains(GOLDEN_DIR_VAR) && error.contains("genesis.txt"),
            "{error}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn rec(frame: u64, state: u64, video: u64, audio: u64) -> FrameRecord {
        FrameRecord {
            frame,
            state,
            video,
            audio,
            samples: 1470,
            width: 256,
            height: 224,
        }
    }

    #[test]
    fn records_round_trip_through_the_text_format() {
        let header = vec![
            "rom=game.sfc size=524288".to_string(),
            "frames=2".to_string(),
        ];
        let records = vec![rec(1, 1, 2, 3), rec(2, u64::MAX, 0, 0x1234_5678_9abc_def0)];
        let text = format_records(FORMAT, &header, &records);
        assert!(text.starts_with("# test-golden 1\n# rom=game.sfc size=524288\n# frames=2\n"));
        let (h, r) = parse_records(FORMAT, &text).unwrap();
        assert_eq!(h, header);
        assert_eq!(r, records);
    }

    #[test]
    fn parse_records_rejects_another_format_and_a_short_line() {
        assert!(parse_records(FORMAT, "hello\n1 2 3").is_err());
        assert!(parse_records(FORMAT, "# test-golden 1\n1 2 3\n").is_err());
        assert!(parse_records(FORMAT, "# test-golden 1\n1 zz 0 0 0 0 0\n").is_err());
        // Another console's record is another format.
        assert!(parse_records("other-golden 1", "# test-golden 1\n").is_err());
    }

    #[test]
    fn first_divergence_names_the_frame_and_the_outputs() {
        let a = vec![rec(1, 1, 1, 1), rec(2, 2, 2, 2), rec(3, 3, 3, 3)];
        let mut b = a.clone();
        assert_eq!(first_divergence(&a, &b), None);
        b[1].video = 9;
        b[1].audio = 9;
        let d = first_divergence(&a, &b).unwrap();
        assert_eq!(d.frame, 2);
        assert!(!d.state && d.video && d.audio && !d.length);
        assert_eq!(d.describe(), "video+audio");
    }

    #[test]
    fn a_changed_display_size_or_sample_count_is_a_divergence() {
        let a = vec![rec(1, 1, 1, 1)];
        let mut b = a.clone();
        b[0].height = 239;
        assert!(first_divergence(&a, &b).unwrap().video);
        let mut c = a.clone();
        c[0].samples += 1;
        assert!(first_divergence(&a, &c).unwrap().audio);
    }

    #[test]
    fn a_shorter_run_diverges_by_length_after_the_common_frames() {
        let a = vec![rec(1, 1, 1, 1), rec(2, 2, 2, 2)];
        let b = vec![rec(1, 1, 1, 1)];
        let d = first_divergence(&a, &b).unwrap();
        assert_eq!(d.frame, 2);
        assert!(d.length);
        assert_eq!(d.describe(), "length");
    }

    #[test]
    fn hash_samples_sees_one_bit() {
        let a = [0.5f32, -0.25, 0.0];
        let mut b = a;
        b[2] = -0.0;
        assert_ne!(hash_samples(&a), hash_samples(&b));
        assert_eq!(hash_samples(&a), hash_samples(&[0.5f32, -0.25, 0.0]));
    }

    #[test]
    fn crop_skips_the_cold_right_half_and_drops_alpha() {
        // 4-wide buffer whose R channel is the pixel's linear index.
        let mut data = Vec::new();
        for i in 0..8u8 {
            data.extend_from_slice(&[i, i + 1, i + 2, 0xFF]);
        }
        let (w, h, rgb) = crop_framebuffer_rgb(&data, 4, 2, 2, 2);
        assert_eq!((w, h), (2, 2));
        assert_eq!(rgb, vec![0, 1, 2, 1, 2, 3, 4, 5, 6, 5, 6, 7]);
        // A display larger than the buffer is clamped to what is backed.
        let (w, h, _) = crop_framebuffer_rgb(&data, 4, 2, 10, 10);
        assert_eq!((w, h), (4, 2));
        let (w, h, _) = crop_framebuffer_rgb(&data[..16], 4, 4, 4, 4);
        assert_eq!((w, h), (4, 1));
    }

    #[test]
    fn manifest_parses_fields_comments_and_defaults() {
        let text = "\
# the set
name=game rom=roms/game.sfc frames=900 shots=60,300 press-at=150:start;162: # title
name=test rom=roms/test.sfc frames=300 force-detect reset-at=100,200
";
        let e = manifest(text).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].name, "game");
        assert_eq!(e[0].rom, PathBuf::from("roms/game.sfc"));
        assert_eq!(e[0].frames, 900);
        assert_eq!(e[0].shots, BTreeSet::from([60, 300]));
        assert_eq!(e[0].press_at.as_deref(), Some("150:start;162:"));
        assert!(!e[0].has_flag("force-detect"));
        let spec = e[0].spec(&PAD).unwrap();
        assert_eq!(spec.presses.unwrap().mask_at(155), Some(1 << 10));
        assert_eq!(e[1].reset_at.as_deref(), Some("100,200"));
        assert!(e[1].has_flag("force-detect"));
        // No shots -> the last frame.
        assert_eq!(e[1].spec(&PAD).unwrap().shots, BTreeSet::from([300]));
        assert_eq!(e[1].spec(&PAD).unwrap().resets, BTreeSet::from([100, 200]));
    }

    #[test]
    fn manifest_keeps_a_quoted_path_whole() {
        let text = "name=quoted rom=\"roms/Some Game (U) [f1].sfc\" frames=10 # (U) # [f1]\n";
        let e = manifest(text).unwrap();
        assert_eq!(e[0].rom, PathBuf::from("roms/Some Game (U) [f1].sfc"));
        assert_eq!(e[0].frames, 10);
        assert!(manifest("name=a rom=\"x y.sfc frames=10").is_err());
        // A '#' inside quotes is part of the value, not a comment.
        let e = manifest("name=a rom=\"dir#1/x.sfc\" frames=10").unwrap();
        assert_eq!(e[0].rom, PathBuf::from("dir#1/x.sfc"));
    }

    #[test]
    fn manifest_rejects_missing_required_fields_unknown_keys_and_duplicates() {
        assert!(manifest("name=a frames=10").is_err());
        assert!(manifest("name=a rom=x.sfc").is_err());
        assert!(manifest("rom=x.sfc frames=10").is_err());
        assert!(manifest("name=a rom=x.sfc frames=0").is_err());
        assert!(manifest("name=a rom=x.sfc frames=10 colour=red").is_err());
        assert!(manifest("name=a rom=x.sfc frames=10 bogus").is_err());
        assert!(manifest("name=a rom=x.sfc frames=10\nname=a rom=y.sfc frames=10").is_err());
        // A bad schedule fails at parse time, not when the ROM runs.
        assert!(manifest("name=a rom=x.sfc frames=10 press-at=12:bogus").is_err());
        // A bare word is a flag only for a console that has it.
        assert!(parse_manifest("name=a rom=x.sfc frames=10 force-detect", &PAD, &[]).is_err());
        assert!(parse_manifest(
            "name=a rom=x.sfc frames=10 six-button",
            &PAD,
            &["six-button"]
        )
        .is_ok());
    }
}
