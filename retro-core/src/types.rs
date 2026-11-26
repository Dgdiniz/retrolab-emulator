//! Common types shared across all emulator cores.

use serde::{Deserialize, Serialize};

/// System information and metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub name: String,
    pub short_name: String,
    pub manufacturer: String,
    pub year: u16,
    pub cpus: Vec<CpuInfo>,
    pub display: DisplayInfo,
    pub audio: AudioInfo,
    pub input: InputInfo,
}

/// CPU information and metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuInfo {
    pub id: usize,
    pub name: String,
    pub architecture: CpuArchitecture,
    pub clock_speed: u64,
    pub registers: Vec<RegisterInfo>,
    /// Where this CPU lives in the system
    pub location: CpuLocation,
    /// Address width in bits (e.g. 24 for 68K, 16 for Z80)
    pub address_width: u8,
}

/// Where a CPU is physically located in the system
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CpuLocation {
    /// CPU on the main board (e.g. 68K, Z80 on Genesis)
    Motherboard,
    /// CPU inside a cartridge (e.g. SVP, SA-1, SuperFX)
    Cartridge,
}

/// CPU architecture types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CpuArchitecture {
    M68000,
    Z80,
    Ricoh2A03, // NES
    Mos65816,  // SNES main CPU (5A22 core)
    Spc700,    // SNES audio CPU
    SuperFx,   // Super FX GSU (16-bit RISC coprocessor)
    Hg51b,     // Cx4 Hitachi HG51B169 (24-bit DSP coprocessor)
    R3000,     // PS1
    SH2,       // Saturn/32X
}

/// Register information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterInfo {
    pub name: String,
    pub size_bits: u8,
    pub display_format: DisplayFormat,
    pub category: RegisterCategory,
}

/// How to display a value
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisplayFormat {
    Hexadecimal,
    Decimal,
    Binary,
    Address,
}

/// Register category for grouping in UI
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegisterCategory {
    General,
    Address,
    Special,
    Flags,
}

/// Display information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub width: u32,
    pub height: u32,
    pub aspect_ratio: f32,
    pub refresh_rate: f32,
    pub pixel_format: PixelFormat,
    /// How the picture lies over the console's dots and lines.
    pub grid: PictureGrid,
}

/// How a frame's picture lies over the console's grid of dots and lines:
/// the picture columns a dot takes, the picture rows a line gives, whether
/// a line's rows are two fields woven together, and where the picture
/// starts.
///
/// Dots and lines are counted as a sprite's box counts them
/// ([`crate::video::SpriteData`]): a line from the first line the frame
/// draws, a dot from the first dot of a line. The picture's first column
/// shows dot [`Self::first_dot`] and its first row line
/// [`Self::first_line`], so the picture's column `c` and row `r` show dot
/// `first_dot + c / columns_per_dot` of line `first_line + r /
/// rows_per_line`, of field `r % rows_per_line` on a woven picture; the
/// other way, [`crate::video::PictureRect::on_grid`] lays a box of dots
/// and lines on the picture, and [`Self::row`] gives a field's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PictureGrid {
    /// Picture columns per dot: 2 on a picture drawn at two columns a dot
    /// (the SNES's hires frames), else 1.
    pub columns_per_dot: u8,
    /// Picture rows per line: 2 when the frame is drawn at twice its
    /// lines, else 1.
    pub rows_per_line: u8,
    /// The rows of a line are two fields woven: each was drawn from a
    /// field of its own. `false` when a line's rows repeat one drawing of
    /// it, and when a line gives one row.
    pub woven: bool,
    /// The dot the picture's first column shows: 0 unless the picture
    /// leaves dots at the start of each line out.
    pub first_dot: u32,
    /// The line the picture's first row shows: 0 unless the picture
    /// leaves lines the frame draws above it out (the SNES's overscan
    /// frame cut to the TV window).
    pub first_line: u32,
}

impl PictureGrid {
    /// One picture column a dot and one row a line, from the first dot of
    /// the first line drawn.
    pub const ONE: Self = Self {
        columns_per_dot: 1,
        rows_per_line: 1,
        woven: false,
        first_dot: 0,
        first_line: 0,
    };

    /// The picture row that shows field `field` of line `line`, the line
    /// counted from the picture's first ([`Self::first_line`] less). This
    /// is the one rule for which field goes on which row: a woven picture
    /// draws field 0 of a line on the line's first row (an even row) and
    /// field 1 on its second (an odd row); on any other picture every
    /// field of a line is drawn on the line's first row, and a doubled line
    /// repeats it on the next.
    pub fn row(&self, line: u32, field: u8) -> u32 {
        let rows = u32::from(self.rows_per_line.max(1));
        let first = line.saturating_mul(rows);
        if self.woven {
            first.saturating_add(u32::from(field) % rows)
        } else {
            first
        }
    }
}

impl Default for PictureGrid {
    fn default() -> Self {
        Self::ONE
    }
}

/// Pixel format for framebuffer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    RGBA8888,
    RGB888,
    RGB565,
}

/// Framebuffer data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>, // RGBA format by default
}

impl Framebuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; (width * height * 4) as usize],
        }
    }
}

/// Audio information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
}

/// Input device information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputInfo {
    pub max_players: u8,
    pub button_layout: Vec<ButtonInfo>,
}

/// Button information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ButtonInfo {
    pub name: String,
    pub id: u32,
}

/// Input state for a player
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InputState {
    pub buttons: u32, // Bitmask of pressed buttons
}

/// ROM information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RomInfo {
    pub title: String,
    pub region: String,
    pub size: usize,
    pub checksum: u32,
    pub mapper: Option<String>,
    /// Confidence in the parsed title: "high" (clean ASCII), "low" (mostly printable), "none" (garbage/empty).
    #[serde(default = "default_title_confidence")]
    pub title_confidence: String,
}

fn default_title_confidence() -> String {
    "high".to_string()
}

/// What a memory region holds, the same words on every console.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryRegion {
    /// The main CPU's work RAM.
    MainRam,
    /// The video chip's memory: tiles, maps.
    VideoRam,
    /// The sound CPU's RAM.
    AudioRam,
    /// The cartridge's ROM.
    Rom,
    /// The cartridge's battery-backed RAM.
    SaveRam,
    /// The palette memory: the colours the video chip draws with.
    PaletteRam,
    /// The sprite table: each sprite's place, size and character.
    SpriteTable,
}

/// Memory region metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRegionInfo {
    pub region: MemoryRegion,
    pub name: String,
    pub start_address: u32,
    pub size: u32,
    pub writable: bool,
}

/// CPU state snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuState {
    pub cpu_id: usize,
    pub registers: Vec<RegisterValue>,
    pub pc: u32,
    pub flags: u32,
}

/// Register value
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterValue {
    pub name: String,
    pub value: u64,
}

/// Execution mode for debugger
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionMode {
    Running,
    Paused,
    StepInto,
    StepOver,
    StepOut,
}

/// The television standard a console's video and clocks follow.
///
/// Not the market a cartridge was sold for
/// ([`crate::cartridge::Region`]): a machine runs in one of these, and a
/// console says which it supports
/// ([`crate::EmulatorCore::supported_regions`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum VideoRegion {
    /// 525 lines at about 60 frames a second.
    #[default]
    Ntsc,
    /// 625 lines at about 50 frames a second.
    Pal,
}

impl VideoRegion {
    /// The region as the host names it: `"ntsc"` or `"pal"`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Ntsc => "ntsc",
            Self::Pal => "pal",
        }
    }

    /// The region `name` names (any case, spaces around ignored), if
    /// `supported` holds it. The refusal lists the names `supported`
    /// holds: "unknown region 'secam' (expected 'ntsc' or 'pal')".
    pub fn parse_among(name: &str, supported: &[VideoRegion]) -> Result<Self, String> {
        let wanted = name.trim().to_ascii_lowercase();
        if let Some(region) = supported.iter().find(|r| r.name() == wanted) {
            return Ok(*region);
        }
        let names: Vec<String> = supported
            .iter()
            .map(|r| format!("'{}'", r.name()))
            .collect();
        let expected = match names.split_last() {
            None => String::new(),
            Some((last, [])) => last.clone(),
            Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        };
        Err(format!("unknown region '{wanted}' (expected {expected})"))
    }
}

/// Access kinds a breakpoint watches, as independent flags.
///
/// A single breakpoint can match any combination of execute / read /
/// write, like the per-row R/W/X checkboxes of bsnes-plus's debugger. The
/// common combinations are the constants `EXEC`, `READ`, `WRITE` and
/// `READ_WRITE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessFlags {
    pub exec: bool,
    pub read: bool,
    pub write: bool,
}

impl AccessFlags {
    /// Execute-only.
    pub const EXEC: Self = Self {
        exec: true,
        read: false,
        write: false,
    };
    /// Read-only watchpoint.
    pub const READ: Self = Self {
        exec: false,
        read: true,
        write: false,
    };
    /// Write-only watchpoint.
    pub const WRITE: Self = Self {
        exec: false,
        read: false,
        write: true,
    };
    /// Read + write watchpoint.
    pub const READ_WRITE: Self = Self {
        exec: false,
        read: true,
        write: true,
    };
    /// No access selected (the breakpoint can never fire).
    pub const NONE: Self = Self {
        exec: false,
        read: false,
        write: false,
    };

    /// Build from the three independent flags.
    pub const fn new(exec: bool, read: bool, write: bool) -> Self {
        Self { exec, read, write }
    }

    /// `true` when no access kind is selected.
    pub const fn is_empty(self) -> bool {
        !self.exec && !self.read && !self.write
    }

    /// `true` when this watches any memory access (read or write).
    pub const fn watches_memory(self) -> bool {
        self.read || self.write
    }
}

/// Breakpoint type.
///
/// `Access` is address-keyed and checked on the CPU fetch / bus-access
/// hot paths; its [`AccessFlags`] select execute / read / write
/// independently. `Event` is a **core-defined** hook: the `code`
/// selects an event class (e.g. a SNES VBlank / NMI / DMA-start /
/// register-write) and `param` an optional discriminator (channel,
/// register, scanline). `param == BREAKPOINT_PARAM_ANY` is a wildcard.
/// retro-core stays agnostic about what the codes mean, each core owns
/// its own event taxonomy and the hook sites that call
/// [`crate::debug::BreakpointList::check_event`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BreakpointType {
    Access(AccessFlags),
    Event { code: u32, param: u32 },
}

impl BreakpointType {
    /// Execute-only access breakpoint.
    pub const fn execute() -> Self {
        Self::Access(AccessFlags::EXEC)
    }
    /// Read-only watchpoint.
    pub const fn read() -> Self {
        Self::Access(AccessFlags::READ)
    }
    /// Write-only watchpoint.
    pub const fn write() -> Self {
        Self::Access(AccessFlags::WRITE)
    }
    /// Read + write watchpoint.
    pub const fn read_write() -> Self {
        Self::Access(AccessFlags::READ_WRITE)
    }

    /// The access flags when this is an access breakpoint, else `None`.
    pub const fn access(self) -> Option<AccessFlags> {
        match self {
            Self::Access(flags) => Some(flags),
            Self::Event { .. } => None,
        }
    }
}

/// Wildcard `param` for [`BreakpointType::Event`]: an event breakpoint
/// stored with this param fires for any `param` the core reports for
/// the matching `code`.
pub const BREAKPOINT_PARAM_ANY: u32 = u32::MAX;

/// Breakpoint definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Breakpoint {
    pub address: u32,
    pub bp_type: BreakpointType,
    pub cpu_id: Option<usize>,
    pub condition: Option<String>,
}

/// Unique breakpoint identifier
pub type BreakpointId = u32;

/// Comparison operator for a value-matched watchpoint. Applied as
/// `(observed & mask) <op> target`, where `observed` is the byte the
/// CPU read or wrote at the watched address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ValueCompare {
    /// `observed == target` (the default).
    #[default]
    Eq,
    /// `observed != target`.
    Ne,
    /// `observed < target`.
    Lt,
    /// `observed <= target`.
    Le,
    /// `observed > target`.
    Gt,
    /// `observed >= target`.
    Ge,
}

impl ValueCompare {
    /// Evaluate the comparison against an already-masked observed value
    /// and target.
    #[inline]
    pub fn matches(self, observed: u32, target: u32) -> bool {
        match self {
            Self::Eq => observed == target,
            Self::Ne => observed != target,
            Self::Lt => observed < target,
            Self::Le => observed <= target,
            Self::Gt => observed > target,
            Self::Ge => observed >= target,
        }
    }
}

/// What the debugger does when a breakpoint's conditions are satisfied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BreakpointAction {
    /// Halt execution, the classic breakpoint behaviour.
    #[default]
    Break,
    /// Count the hit and keep running. A passive logpoint: the hit
    /// counter still advances so the UI can show how often the site was
    /// reached without ever stopping the emulator.
    Log,
}

/// Structured match conditions layered on top of an `Access` or `Event`
/// breakpoint, turning a plain breakpoint into an "advanced watchpoint".
///
/// Every field defaults to "unset" so a `MatchSpec::default()` reproduces
/// exactly the bare address / access / event semantics. The conditions
/// compose in this order when a base match (address + access kind, or
/// event code + param) already holds:
///
/// 1. **range**: `address_end` widens an access match from the single
///    `address` to the inclusive span `[address, address_end]`.
/// 2. **value**: for memory watchpoints only, the observed value must
///    satisfy `(observed & mask) <value_compare> value`. A one-byte watch
///    observes the byte the access moved; a wider one the whole value at
///    its address after the instruction, read through the check's
///    [`crate::debug::ExprContext`].
/// 3. **expression**: an optional [`crate::debug::expr`] condition over
///    live registers / memory (`d0 == 5 && [0xFF0000].w > 0x100`). When
///    set, it must evaluate non-zero for the breakpoint to fire. A spec
///    that carries an expression but is checked with no evaluation context
///    (or whose expression fails to parse / evaluate) fails **closed**:
///    the breakpoint stays silent rather than firing on an unverifiable
///    condition.
/// 4. **hit-skip**: the first `hit_skip` otherwise-qualifying matches
///    are ignored before the breakpoint starts firing.
/// 5. **action**: `Break` halts; `Log` counts and continues.
/// 6. **one-shot**: after a real fire, the breakpoint auto-disables.
///
/// Not `Copy`: the optional `expr` source string makes it move/clone-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MatchSpec {
    /// Inclusive end of an address range. When `Some(end)` with
    /// `end >= address`, an access breakpoint matches any address in
    /// `[address, end]`. Ignored by event breakpoints.
    #[serde(default)]
    pub address_end: Option<u32>,
    /// Target value for a memory watchpoint. When `Some`, the access
    /// only matches if the observed value satisfies the comparison. For a
    /// multi-byte watchpoint (see `value_size`) the observed value is the
    /// whole value at the watched address, in the CPU's byte order.
    /// Ignored by execute and event breakpoints (no observed value).
    #[serde(default)]
    pub value: Option<u32>,
    /// Width in bytes of the value predicate (1-4). A width > 1 makes this
    /// a multi-byte watchpoint: the observed value is read from
    /// `value_size` consecutive bytes starting at the watched address, in
    /// the CPU's byte order, and the address span widens to
    /// `[address, address + value_size - 1]` so an access to *any* of those
    /// bytes is a candidate. `0` is treated as `1` (single byte) so older
    /// serialized specs and plain byte watchpoints round-trip unchanged.
    #[serde(default)]
    pub value_size: u8,
    /// Mask AND-ed with both observed value and target before comparing.
    /// `0` is treated as "no mask" (compare the full value) so a default
    /// spec carrying only a `value` still compares every bit.
    #[serde(default)]
    pub value_mask: u32,
    /// Comparison operator for the value predicate.
    #[serde(default)]
    pub value_compare: ValueCompare,
    /// Optional boolean expression over live registers / memory (see
    /// [`crate::debug::expr`]). `None` (the default) imposes no extra
    /// condition. When `Some`, the breakpoint only fires if the expression
    /// evaluates non-zero. Stored as source text and parsed on the (rare)
    /// check where the base match already holds; an empty / whitespace-only
    /// string is treated as no constraint.
    #[serde(default)]
    pub expr: Option<String>,
    /// Number of qualifying matches to ignore before the breakpoint
    /// starts firing. `0` fires from the first match.
    #[serde(default)]
    pub hit_skip: u32,
    /// Break vs. passively log-and-continue.
    #[serde(default)]
    pub action: BreakpointAction,
    /// Auto-disable the breakpoint after its first real fire.
    #[serde(default)]
    pub one_shot: bool,
}

impl MatchSpec {
    /// `true` when the spec carries a value predicate, which only an
    /// access's observed value can satisfy.
    #[inline]
    pub fn needs_value(&self) -> bool {
        self.value.is_some()
    }

    /// The value predicate's byte width, clamped to the supported 1-4
    /// range (`0` -> `1`). Drives both the address span and how many
    /// consecutive bytes are read into the observed value.
    #[inline]
    pub fn value_width(&self) -> u8 {
        self.value_size.clamp(1, 4)
    }

    /// The trimmed expression source, or `None` when absent / blank. The
    /// breakpoint path uses this to decide whether to engage the
    /// [`crate::debug::expr`] evaluator at all.
    #[inline]
    pub fn expr_source(&self) -> Option<&str> {
        match self.expr.as_deref().map(str::trim) {
            Some(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }

    /// `true` when an expression condition is attached. Cores use this to
    /// decide whether a check needs an [`crate::debug::ExprContext`].
    #[inline]
    pub fn needs_expr(&self) -> bool {
        self.expr_source().is_some()
    }

    /// Evaluate the value predicate against an observed value. Returns
    /// `true` (no constraint) when no value target is set.
    #[inline]
    pub fn value_matches(&self, observed: u32) -> bool {
        match self.value {
            None => true,
            Some(target) => {
                let mask = if self.value_mask == 0 {
                    u32::MAX
                } else {
                    self.value_mask
                };
                self.value_compare.matches(observed & mask, target & mask)
            }
        }
    }
}

/// Breakpoint information: a stored [`Breakpoint`] plus its runtime
/// state (id, enabled flag, hit count) and the structured
/// [`MatchSpec`] advanced conditions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreakpointInfo {
    pub id: BreakpointId,
    pub breakpoint: Breakpoint,
    pub enabled: bool,
    pub hit_count: u32,
    /// Advanced match conditions (range / value / hit-skip / action /
    /// one-shot). Defaults to the bare-breakpoint semantics so older
    /// serialized lists and plain breakpoints round-trip unchanged.
    #[serde(default)]
    pub match_spec: MatchSpec,
    /// Runtime count of base matches that have qualified (passed the
    /// value predicate), used to honour `match_spec.hit_skip`. Never
    /// serialized: it is rebuilt from zero each session.
    #[serde(default, skip)]
    pub(crate) raw_hits: u32,
}

/// One selectable value for an event breakpoint's `param`, e.g. a DMA
/// channel number or a PPU register index. Drives a dropdown in the
/// event-breakpoint picker; free-form / param-less events ship none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventParamOption {
    /// The concrete `param` value stored on the [`BreakpointType::Event`].
    pub value: u32,
    /// Human label for the option (e.g. "Channel 3", "BG1 scroll").
    pub label: String,
}

/// What an event a debugger can stop on is, in words every console shares,
/// beside the code each console numbered it with. A host looks an event up
/// by its kind; the code stays what a breakpoint stores. Each kind says what
/// its `param` holds; a kind with no param reports 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// The beam entered vertical blank. No param.
    VerticalBlank,
    /// The vertical blank's interrupt reached the main CPU. No param.
    VerticalBlankInterrupt,
    /// An interrupt the beam's position raises (at a line or a dot) reached
    /// the main CPU. No param.
    RasterInterrupt,
    /// The beam reached a line; the param is the line, as the console counts
    /// its lines.
    Line,
    /// A transfer from memory started. No param.
    TransferStart,
    /// A transfer finished; the param is its target, coded as a frame-write
    /// record carries it ([`crate::capture::FrameWrite::target`]).
    TransferDone,
    /// A transfer made line by line was set up for the frame. No param.
    LineTransferSetUp,
    /// A colour of the palette was written; the param is the colour's index.
    PaletteWrite,
    /// A write changed a tile; the param is the tile, as the 32-byte block
    /// of video memory it is (its byte address divided by 32).
    TileWrite,
    /// A transfer wrote into the sprite table. No param.
    SpriteTableWrite,
    /// A video register was written; the param is the register's index.
    VideoRegisterWrite,
    /// The main CPU wrote a port of the sound system; the param is the port.
    AudioPortWrite,
    /// A sound chip's register was written; the param is the register's
    /// index.
    AudioRegisterWrite,
}

impl EventKind {
    /// Whether an event of this kind carries a param.
    pub const fn takes_param(self) -> bool {
        match self {
            Self::VerticalBlank
            | Self::VerticalBlankInterrupt
            | Self::RasterInterrupt
            | Self::TransferStart
            | Self::LineTransferSetUp
            | Self::SpriteTableWrite => false,
            Self::Line
            | Self::TransferDone
            | Self::PaletteWrite
            | Self::TileWrite
            | Self::VideoRegisterWrite
            | Self::AudioPortWrite
            | Self::AudioRegisterWrite => true,
        }
    }
}

/// One event class a core can break on, used to populate the UI's
/// event-breakpoint picker. A core advertises its catalogue through
/// [`crate::DebuggerCore::available_events`], built from its
/// [`crate::debug::events::EventSpec`] list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventClass {
    /// The `code` to store on a [`BreakpointType::Event`].
    pub code: u32,
    /// What the event is, in words every console shares.
    pub kind: EventKind,
    /// Human label for the event (e.g. "VBlank", "DMA start").
    pub label: String,
    /// Longer description of what the event fires on, if useful.
    #[serde(default)]
    pub description: Option<String>,
    /// Human label for what `param` selects (e.g. "Channel", "Register"),
    /// or `None` when the event takes no parameter (the UI then stores
    /// [`BREAKPOINT_PARAM_ANY`]).
    #[serde(default)]
    pub param_label: Option<String>,
    /// Enumerable param choices for a dropdown. Empty means the param is
    /// free-form numeric (with `param_label` set) or absent.
    #[serde(default)]
    pub param_options: Vec<EventParamOption>,
}

/// Disassembled instruction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instruction {
    pub address: u32,
    pub bytes: Vec<u8>,
    pub mnemonic: String,
    pub operands: String,
    pub comment: Option<String>,
}

/// Whether a row in a disassembly window is a real decoded instruction
/// or a placeholder for an address the CPU has never executed.
/// Drives the UI's `....` rendering for never-executed regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisasmRowKind {
    /// Decoded instruction at a covered address.
    Code,
    /// Address(es) the CPU has not executed; the UI renders `....`.
    Unknown,
}

/// One row in a `disassemble_window` result. Extends `Instruction`
/// with a `kind` discriminator so the UI can render real instructions
/// and unknown (uncovered) regions distinctly without having to guess
/// from empty mnemonics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisasmRow {
    pub address: u32,
    pub kind: DisasmRowKind,
    pub bytes: Vec<u8>,
    pub mnemonic: String,
    pub operands: String,
    pub comment: Option<String>,
}

/// Stack frame for call stack
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackFrame {
    pub return_address: u32,
    pub call_address: u32,
    pub function_name: Option<String>,
}

/// Instruction trace for history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstructionTrace {
    pub address: u32,
    pub instruction: String,
    pub registers_before: Vec<RegisterValue>,
    pub cycle_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_of_one_row_a_line_puts_every_field_on_the_line_s_row() {
        let grid = PictureGrid::ONE;
        assert_eq!(PictureGrid::default(), grid);
        for line in [0, 1, 223] {
            assert_eq!(grid.row(line, 0), line);
            assert_eq!(grid.row(line, 1), line);
        }
    }

    #[test]
    fn a_woven_picture_puts_field_0_on_even_rows_and_field_1_on_odd_ones() {
        let grid = PictureGrid {
            rows_per_line: 2,
            woven: true,
            ..PictureGrid::ONE
        };
        assert_eq!([grid.row(0, 0), grid.row(0, 1)], [0, 1]);
        assert_eq!([grid.row(5, 0), grid.row(5, 1)], [10, 11]);
        assert_eq!([grid.row(223, 0), grid.row(223, 1)], [446, 447]);
    }

    #[test]
    fn a_doubled_line_starts_on_its_first_row_whatever_the_field() {
        let grid = PictureGrid {
            columns_per_dot: 2,
            rows_per_line: 2,
            ..PictureGrid::ONE
        };
        assert_eq!([grid.row(7, 0), grid.row(7, 1)], [14, 14]);
    }

    #[test]
    fn the_grid_reads_as_a_host_receives_it() {
        let grid = PictureGrid {
            columns_per_dot: 2,
            rows_per_line: 2,
            woven: true,
            first_dot: 0,
            first_line: 7,
        };
        assert_eq!(
            serde_json::to_string(&grid).unwrap(),
            r#"{"columns_per_dot":2,"rows_per_line":2,"woven":true,"first_dot":0,"first_line":7}"#
        );
    }

    #[test]
    fn the_rows_of_a_line_do_not_depend_on_where_the_picture_starts() {
        let grid = PictureGrid {
            rows_per_line: 2,
            woven: true,
            first_line: 7,
            ..PictureGrid::ONE
        };
        assert_eq!([grid.row(0, 0), grid.row(0, 1)], [0, 1]);
    }
}
