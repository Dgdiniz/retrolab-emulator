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

/// Memory region information
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryRegion {
    MainRam,
    VideoRam,
    AudioRam,
    Rom,
    SaveRam,
    Custom(u8),
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
/// 2. **value**: for memory watchpoints only, the observed byte must
///    satisfy `(observed & mask) <value_compare> value`.
/// 3. **expression**: an optional [`crate::debug::expr`] condition over
///    live registers / memory (`d0 == 5 && [0xFF0000].w > 0x100`). When
///    set, it must evaluate non-zero for the breakpoint to fire. A spec
///    that carries an expression but is checked on a path without an
///    evaluation context (or whose expression fails to parse / evaluate)
///    fails **closed**, the breakpoint stays silent rather than firing on
///    an unverifiable condition.
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
    /// whole little/big-endian word assembled from the watched bytes.
    /// Ignored by execute and event breakpoints (no observed value).
    #[serde(default)]
    pub value: Option<u32>,
    /// Width in bytes of the value predicate (1-4). A width > 1 makes this
    /// a multi-byte watchpoint: the observed value is assembled from
    /// `value_size` consecutive bytes starting at the watched address
    /// (honoring the core's endianness), and the coverage span widens to
    /// `[address, address + value_size - 1]` so a write to *any* of those
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
    /// `true` when the spec carries a value predicate that needs the
    /// observed byte to evaluate. Cores use this to decide whether the
    /// value-aware [`crate::debug::BreakpointList::check_memory_access_valued`]
    /// path is worth the extra debug read.
    #[inline]
    pub fn needs_value(&self) -> bool {
        self.value.is_some()
    }

    /// The value predicate's byte width, clamped to the supported 1-4
    /// range (`0` -> `1`). Drives both the coverage span and how many
    /// consecutive bytes a core assembles into the observed value.
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

    /// Evaluate the value predicate against an observed byte. Returns
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

/// One event class a core can break on, used to populate the UI's
/// event-breakpoint picker. A core advertises its taxonomy through
/// [`crate::DebuggerCore::available_events`]; retro-core stays agnostic
/// about what the codes mean.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventClass {
    /// The `code` to store on a [`BreakpointType::Event`].
    pub code: u32,
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
