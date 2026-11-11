//! The DSP-1's commands: which byte asks for what, how many words each one
//! takes, and the registers they share.
//!
//! A command byte's low six bits pick one of 64 entries in the chip's
//! dispatch table; a byte with either of the top two bits set is no
//! command. Several entries lead to the same routine, so most commands
//! answer to more than one byte.

use super::attitude::{self, Matrix};
use super::camera::{self, Camera};
use super::number::{cosine, length, q15, sine, sum_of_squares, Scaled};

/// Which of the chip's three attitude matrices a command works on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Frame {
    A,
    B,
    C,
}

/// What a command byte asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Command {
    /// The 1.15 product of two words. The second form answers it with its
    /// lowest bit set, the product, or one more when the product is even.
    Multiply { odd: bool },
    /// The reciprocal of a mantissa and exponent.
    Inverse,
    /// A radius times the sine and the cosine of an angle.
    Triangle,
    /// Twice the sum of three squares, as two words.
    Radius,
    /// The sum of three squares less a fourth, cut to a word; `plus_one`
    /// is the variant that adds 1.
    Range { plus_one: bool },
    /// The length of a vector.
    Distance,
    /// A point turned in its plane.
    Rotate,
    /// A vector turned about three axes.
    Polar,
    /// Place the camera.
    Parameter,
    /// The Mode 7 matrix of a raster line, then of each line after it.
    Raster,
    /// A point in space to its place on the screen.
    Project,
    /// A place on the screen to the ground under it.
    Target,
    /// Build an attitude matrix.
    Attitude(Frame),
    /// A vector into a matrix's frame.
    Objective(Frame),
    /// A vector out of a matrix's frame.
    Subjective(Frame),
    /// The first component of a vector in a matrix's frame.
    Scalar(Frame),
    /// Three angles advanced by a turn.
    Gyrate,
    /// The RAM self-test, which always passes.
    MemoryTest,
    /// The RAM size.
    MemorySize,
    /// The data ROM, word by word.
    MemoryDump,
}

use Command::*;
use Frame::{A, B, C};

/// The dispatch table: a row for each value of the command byte's bits 5-4,
/// a column for each value of its low four bits.
#[rustfmt::skip]
const DISPATCH: [[Command; 16]; 4] = [
    [
        Multiply { odd: false }, Attitude(A), Parameter, Subjective(A),
        Triangle, Attitude(A), Project, MemoryTest,
        Radius, Objective(A), Raster, Scalar(A),
        Rotate, Objective(A), Target, MemoryTest,
    ],
    [
        Inverse, Attitude(B), Parameter, Subjective(B),
        Gyrate, Attitude(B), Project, MemoryDump,
        Range { plus_one: false }, Objective(B), Raster, Scalar(B),
        Polar, Objective(B), Target, MemoryDump,
    ],
    [
        Multiply { odd: true }, Attitude(C), Parameter, Subjective(C),
        Triangle, Attitude(C), Project, MemorySize,
        Distance, Objective(C), Raster, Scalar(C),
        Rotate, Objective(C), Target, MemorySize,
    ],
    [
        Inverse, Attitude(A), Parameter, Subjective(A),
        Gyrate, Attitude(A), Project, MemoryDump,
        Range { plus_one: true }, Objective(A), Raster, Scalar(A),
        Polar, Objective(A), Target, MemoryDump,
    ],
];

impl Command {
    /// The command a byte asks for, if it asks for one.
    pub(super) fn of(byte: u8) -> Option<Self> {
        (byte < 0x40).then(|| DISPATCH[usize::from(byte >> 4)][usize::from(byte & 0x0f)])
    }

    /// Words the command reads before it runs.
    pub(super) fn words_in(self) -> usize {
        match self {
            Raster | MemoryTest | MemorySize | MemoryDump => 1,
            Multiply { .. } | Inverse | Triangle | Target => 2,
            Radius | Distance | Rotate | Project | Objective(_) | Subjective(_) | Scalar(_) => 3,
            Range { .. } | Attitude(_) => 4,
            Polar | Gyrate => 6,
            Parameter => 7,
        }
    }
}

/// Words of state a savestate carries: the camera, the three matrices and
/// the next raster line.
pub(super) const STATE_WORDS: usize = camera::WORDS + 3 * 9 + 1;

/// The registers the commands share: what `Parameter` and the three
/// `Attitude` commands leave for the others, and the line `Raster` is at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Registers {
    camera: Camera,
    frames: [Matrix; 3],
    /// The line the next raster answer is for.
    pub(super) raster_line: i16,
}

/// A command's answer: up to four words.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Answer {
    words: [u16; 4],
    count: usize,
}

impl Answer {
    fn of<const N: usize>(words: [i16; N]) -> Self {
        let mut answer = Self::default();
        for (slot, word) in answer.words.iter_mut().zip(words) {
            *slot = word as u16;
        }
        answer.count = N.min(4);
        answer
    }

    pub(super) fn words(&self) -> &[u16] {
        &self.words[..self.count]
    }
}

impl Registers {
    fn frame(&mut self, frame: Frame) -> &mut Matrix {
        &mut self.frames[frame as usize]
    }

    /// The Mode 7 matrix of the current raster line; the line moves on.
    pub(super) fn next_raster(&mut self) -> Answer {
        let matrix = self.camera.raster(self.raster_line);
        self.raster_line = self.raster_line.wrapping_add(1);
        Answer::of(matrix)
    }

    /// Run a command over the words the host sent. A word the host did not
    /// send reads as 0.
    pub(super) fn run(&mut self, command: Command, words: &[u16]) -> Answer {
        let w = |at: usize| words.get(at).copied().unwrap_or(0) as i16;
        let vector = |at: usize| [w(at), w(at + 1), w(at + 2)];
        match command {
            Multiply { odd } => Answer::of([q15(w(0), w(1)) | i16::from(odd)]),
            Inverse => {
                let reciprocal = Scaled {
                    mantissa: w(0),
                    exponent: w(1),
                }
                .reciprocal();
                Answer::of([reciprocal.mantissa, reciprocal.exponent])
            }
            Triangle => Answer::of([q15(sine(w(0)), w(1)), q15(cosine(w(0)), w(1))]),
            Radius => {
                let doubled = sum_of_squares(vector(0)) << 1;
                Answer::of([doubled as i16, (doubled >> 16) as i16])
            }
            Range { plus_one } => {
                let short =
                    sum_of_squares(vector(0)).wrapping_sub(i32::from(w(3)) * i32::from(w(3))) >> 15;
                Answer::of([(short + i32::from(plus_one)) as i16])
            }
            Distance => Answer::of([length(vector(0))]),
            Rotate => Answer::of(attitude::rotate(w(0), w(1), w(2))),
            Polar => Answer::of(attitude::polar(vector(0), vector(3))),
            Parameter => Answer::of(self.camera.aim(vector(0), w(3), w(4), w(5), w(6))),
            Raster => {
                self.raster_line = w(0);
                self.next_raster()
            }
            Project => Answer::of(self.camera.project(vector(0))),
            Target => Answer::of(self.camera.target(w(0), w(1))),
            Attitude(frame) => {
                *self.frame(frame) = attitude::attitude(w(0), w(1), w(2), w(3));
                Answer::default()
            }
            Objective(frame) => Answer::of(attitude::objective(self.frame(frame), vector(0))),
            Subjective(frame) => Answer::of(attitude::subjective(self.frame(frame), vector(0))),
            Scalar(frame) => Answer::of([attitude::scalar(self.frame(frame), vector(0))]),
            Gyrate => Answer::of(attitude::gyrate(vector(0), vector(3))),
            MemoryTest => Answer::of([0]),
            MemorySize => Answer::of([0x0100]),
            // The dump is streamed from the ROM by the host port; nothing
            // is computed.
            MemoryDump => Answer::default(),
        }
    }

    /// The registers in the order a savestate keeps them.
    pub(super) fn words(&self) -> Vec<i16> {
        let mut words = Vec::with_capacity(STATE_WORDS);
        words.extend_from_slice(&self.camera.words());
        words.extend(self.frames.iter().flatten().flatten());
        words.push(self.raster_line);
        words
    }

    /// The registers back from [`Registers::words`]. Too few words, a
    /// state written before the registers were kept, leaves them clear.
    pub(super) fn from_words(words: &[i16]) -> Self {
        let Some(words) = words.first_chunk::<STATE_WORDS>() else {
            return Self::default();
        };
        let (camera_words, rest) = words.split_at(camera::WORDS);
        let mut frames = [Matrix::default(); 3];
        for (cell, &word) in frames.iter_mut().flatten().flatten().zip(rest) {
            *cell = word;
        }
        Self {
            camera: Camera::from_words(camera_words.try_into().expect("the split's length")),
            frames,
            raster_line: words[STATE_WORDS - 1],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_byte_under_0x40_is_a_command_and_none_above() {
        for byte in 0..=0xffu8 {
            assert_eq!(Command::of(byte).is_some(), byte < 0x40, "{byte:#04x}");
        }
        // The idle byte the host pads with is not one.
        assert_eq!(Command::of(0x80), None);
    }

    #[test]
    fn a_command_answers_to_each_of_its_bytes() {
        let bytes = |command: Command| -> Vec<u8> {
            (0..0x40)
                .filter(|&b| Command::of(b) == Some(command))
                .collect()
        };
        assert_eq!(bytes(Parameter), [0x02, 0x12, 0x22, 0x32]);
        assert_eq!(bytes(Raster), [0x0a, 0x1a, 0x2a, 0x3a]);
        assert_eq!(bytes(Project), [0x06, 0x16, 0x26, 0x36]);
        assert_eq!(bytes(Target), [0x0e, 0x1e, 0x2e, 0x3e]);
        assert_eq!(bytes(Attitude(A)), [0x01, 0x05, 0x31, 0x35]);
        assert_eq!(bytes(Attitude(B)), [0x11, 0x15]);
        assert_eq!(bytes(Attitude(C)), [0x21, 0x25]);
        assert_eq!(bytes(Objective(A)), [0x09, 0x0d, 0x39, 0x3d]);
        assert_eq!(bytes(Subjective(A)), [0x03, 0x33]);
        assert_eq!(bytes(Scalar(C)), [0x2b]);
        assert_eq!(bytes(Multiply { odd: true }), [0x20]);
        assert_eq!(bytes(Range { plus_one: true }), [0x38]);
        assert_eq!(bytes(Distance), [0x28]);
        assert_eq!(bytes(Radius), [0x08]);
        assert_eq!(bytes(MemoryDump), [0x17, 0x1f, 0x37, 0x3f]);
    }

    #[test]
    fn the_simple_commands_compute() {
        let mut registers = Registers::default();
        let mut run = |byte: u8, words: &[u16]| {
            registers
                .run(Command::of(byte).expect("a command"), words)
                .words()
                .to_vec()
        };
        assert_eq!(run(0x00, &[0x4000, 0x4000]), [0x2000]);
        // The second form sets the lowest bit: one more than an even
        // product, an odd product as it is.
        assert_eq!(run(0x20, &[0x4000, 0x4000]), [0x2001]);
        assert_eq!(run(0x00, &[0x0002, 0x6000]), [0x0001]);
        assert_eq!(run(0x20, &[0x0002, 0x6000]), [0x0001]);
        // 2 x (3^2 + 4^2 + 0^2) = 50, in two words.
        assert_eq!(run(0x08, &[3, 4, 0]), [50, 0]);
        // (0x100^2 + 0 + 0 - 0x80^2) >> 15 = 1, and the variant one more.
        assert_eq!(run(0x18, &[0x100, 0, 0, 0x80]), [1]);
        assert_eq!(run(0x38, &[0x100, 0, 0, 0x80]), [2]);
        assert_eq!(run(0x0f, &[0]), [0]);
        assert_eq!(run(0x2f, &[0]), [0x0100]);
        // An attitude command answers nothing; a missing word reads as 0.
        assert_eq!(run(0x01, &[0x7fff]), [] as [u16; 0]);
    }

    #[test]
    fn the_registers_round_trip_through_their_words() {
        let mut registers = Registers::default();
        registers.run(Parameter, &[0, 0, 0x40, 0x20, 0x100, 0x1000, 0x2000]);
        registers.run(Attitude(B), &[0x7fff, 0x1000, 0x2000, 0x3000]);
        registers.run(Raster, &[5]);
        let words = registers.words();
        assert_eq!(words.len(), STATE_WORDS);
        assert_eq!(Registers::from_words(&words), registers);
        // A short list clears them.
        assert_eq!(Registers::from_words(&words[..10]), Registers::default());
    }
}
