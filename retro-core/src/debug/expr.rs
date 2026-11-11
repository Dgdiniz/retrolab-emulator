//! A tiny expression DSL for conditional breakpoints and watchpoints.
//!
//! A breakpoint/watchpoint can carry an optional boolean expression over
//! the live machine state. When present, the breakpoint only fires if the
//! expression evaluates to a non-zero value *in addition* to its address /
//! access / value / hit-skip conditions. This lets a user express things a
//! flat `MatchSpec` cannot, e.g.
//!
//! ```text
//! d0 == 5 && [0xFF0000].w > 0x100
//! a7 < 0xFFF000 || !(d1 & 1)
//! ```
//!
//! The engine is deliberately self-contained and `core`-only (no host
//! types): the host supplies machine state through the [`ExprContext`]
//! trait, so the same parser/evaluator serves every console core. The
//! grammar is a conventional C-like precedence ladder:
//!
//! ```text
//! or      := and  ( "||" and )*
//! and     := bor  ( "&&" bor )*
//! bor     := bxor ( "|"  bxor )*
//! bxor    := band ( "^"  band )*
//! band    := eq   ( "&"  eq )*
//! eq      := rel  ( ("==" | "!=") rel )*
//! rel     := shift( ("<" | "<=" | ">" | ">=") shift )*
//! shift   := add  ( ("<<" | ">>") add )*
//! add     := mul  ( ("+" | "-") mul )*
//! mul     := unary( ("*" | "/" | "%") unary )*
//! unary   := ("!" | "~" | "-") unary | primary
//! primary := number | register | mem | "(" or ")"
//! mem     := "[" or "]" ( "." ("b" | "w" | "l") )?      // default byte
//! number  := dec | "0x" hex | "$" hex | "%" bin
//! register:= [A-Za-z_][A-Za-z0-9_]*
//! ```
//!
//! Truthiness follows C: any non-zero value is "true"; comparisons and the
//! logical operators (`&& || !`) yield `1` or `0`. Memory reads are
//! unsigned and zero-extended; the `.b/.w/.l` suffix selects 1/2/4 bytes
//! (endianness is the context's concern). Division / modulo by zero is an
//! error so a malformed condition fails closed rather than panicking.

use core::fmt;

/// State the evaluator queries. The host (a console core) implements this
/// over its live CPU registers and memory bus.
pub trait ExprContext {
    /// Resolve an identifier (already lowercased) to a register value, or
    /// `None` when the name is not a register on this CPU. Returning
    /// `None` makes the whole expression fail closed (the breakpoint does
    /// not fire) rather than guessing a value.
    fn register(&mut self, name: &str) -> Option<i64>;

    /// Read `width` (1, 2 or 4) bytes from memory at `addr`, zero-extended
    /// to `i64`, honouring the core's endianness. Must be side-effect free
    /// (a debugger peek), since it runs on the breakpoint check path.
    fn read_mem(&mut self, addr: u32, width: u8) -> i64;
}

/// Parse / evaluation failure. Callers treat any error as "do not fire".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExprError {
    /// The source was empty or whitespace-only.
    Empty,
    /// An unrecognised character in the input.
    BadChar(char),
    /// A malformed or out-of-place token while parsing.
    Syntax(String),
    /// Trailing tokens after a complete expression.
    Trailing,
    /// A register name the context does not recognise.
    UnknownRegister(String),
    /// Division or modulo by zero.
    DivByZero,
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty expression"),
            Self::BadChar(c) => write!(f, "unexpected character '{c}'"),
            Self::Syntax(s) => write!(f, "syntax error: {s}"),
            Self::Trailing => write!(f, "unexpected trailing input"),
            Self::UnknownRegister(r) => write!(f, "unknown register '{r}'"),
            Self::DivByZero => write!(f, "division by zero"),
        }
    }
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// Logical NOT (`!x` -> `1` when `x == 0`, else `0`).
    Not,
    /// Bitwise NOT (`~x`).
    BitNot,
    /// Arithmetic negation (`-x`).
    Neg,
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    BitOr,
    BitXor,
    BitAnd,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Shl,
    Shr,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// A parsed expression tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// Integer literal.
    Num(i64),
    /// Register reference (name stored lowercased).
    Reg(String),
    /// Memory read of `width` bytes at the address sub-expression.
    Mem(Box<Expr>, u8),
    /// Prefix unary operator.
    Unary(UnOp, Box<Expr>),
    /// Infix binary operator.
    Binary(BinOp, Box<Expr>, Box<Expr>),
}

impl Expr {
    /// Parse `src` into an expression tree, or return a descriptive error.
    pub fn parse(src: &str) -> Result<Expr, ExprError> {
        let tokens = tokenize(src)?;
        if tokens.is_empty() {
            return Err(ExprError::Empty);
        }
        let mut p = Parser {
            tokens: &tokens,
            pos: 0,
        };
        let expr = p.parse_or()?;
        if p.pos != p.tokens.len() {
            return Err(ExprError::Trailing);
        }
        Ok(expr)
    }

    /// Evaluate against `ctx`. Comparisons and logical operators yield
    /// `1`/`0`; the breakpoint gate treats any non-zero result as "fire".
    pub fn eval(&self, ctx: &mut dyn ExprContext) -> Result<i64, ExprError> {
        match self {
            Expr::Num(n) => Ok(*n),
            Expr::Reg(name) => ctx
                .register(name)
                .ok_or_else(|| ExprError::UnknownRegister(name.clone())),
            Expr::Mem(addr, width) => {
                let a = addr.eval(ctx)? as u32;
                Ok(ctx.read_mem(a, *width))
            }
            Expr::Unary(op, e) => {
                let v = e.eval(ctx)?;
                Ok(match op {
                    UnOp::Not => i64::from(v == 0),
                    UnOp::BitNot => !v,
                    UnOp::Neg => v.wrapping_neg(),
                })
            }
            Expr::Binary(op, l, r) => {
                // Short-circuit the logical operators so the right side
                // isn't evaluated (and can't error) when the left decides
                // the result, matching C semantics.
                match op {
                    BinOp::And => {
                        return Ok(i64::from(l.eval(ctx)? != 0 && r.eval(ctx)? != 0));
                    }
                    BinOp::Or => {
                        return Ok(i64::from(l.eval(ctx)? != 0 || r.eval(ctx)? != 0));
                    }
                    _ => {}
                }
                let a = l.eval(ctx)?;
                let b = r.eval(ctx)?;
                Ok(match op {
                    BinOp::BitOr => a | b,
                    BinOp::BitXor => a ^ b,
                    BinOp::BitAnd => a & b,
                    BinOp::Eq => i64::from(a == b),
                    BinOp::Ne => i64::from(a != b),
                    BinOp::Lt => i64::from(a < b),
                    BinOp::Le => i64::from(a <= b),
                    BinOp::Gt => i64::from(a > b),
                    BinOp::Ge => i64::from(a >= b),
                    BinOp::Shl => a.wrapping_shl((b & 63) as u32),
                    BinOp::Shr => a.wrapping_shr((b & 63) as u32),
                    BinOp::Add => a.wrapping_add(b),
                    BinOp::Sub => a.wrapping_sub(b),
                    BinOp::Mul => a.wrapping_mul(b),
                    BinOp::Div => {
                        if b == 0 {
                            return Err(ExprError::DivByZero);
                        }
                        a.wrapping_div(b)
                    }
                    BinOp::Rem => {
                        if b == 0 {
                            return Err(ExprError::DivByZero);
                        }
                        a.wrapping_rem(b)
                    }
                    // Handled above by short-circuit.
                    BinOp::And | BinOp::Or => unreachable!(),
                })
            }
        }
    }
}

/// Convenience: parse + evaluate in one go, returning the boolean gate
/// (`Ok(true)` => the condition holds). Any parse or eval failure maps to
/// `Err`, which the breakpoint path treats as "do not fire".
pub fn eval_str(src: &str, ctx: &mut dyn ExprContext) -> Result<bool, ExprError> {
    Ok(Expr::parse(src)?.eval(ctx)? != 0)
}

// -- Tokenizer --------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Num(i64),
    Ident(String),
    LParen,
    RParen,
    LBrack,
    RBrack,
    Dot,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Shl,
    Shr,
    Lt,
    Le,
    Gt,
    Ge,
    EqEq,
    Ne,
    Amp,
    AndAnd,
    Caret,
    Pipe,
    OrOr,
    Bang,
    Tilde,
}

fn tokenize(src: &str) -> Result<Vec<Tok>, ExprError> {
    let bytes = src.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            ' ' | '\t' | '\r' | '\n' => i += 1,
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '[' => {
                out.push(Tok::LBrack);
                i += 1;
            }
            ']' => {
                out.push(Tok::RBrack);
                i += 1;
            }
            '.' => {
                out.push(Tok::Dot);
                i += 1;
            }
            '+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            '-' => {
                out.push(Tok::Minus);
                i += 1;
            }
            '*' => {
                out.push(Tok::Star);
                i += 1;
            }
            '/' => {
                out.push(Tok::Slash);
                i += 1;
            }
            '%' => {
                // `%` introduces a binary literal only when followed by a
                // binary digit; otherwise it is the modulo operator.
                if matches!(bytes.get(i + 1), Some(b'0' | b'1')) {
                    let (val, next) = read_radix(bytes, i + 1, 2);
                    out.push(Tok::Num(val));
                    i = next;
                } else {
                    out.push(Tok::Percent);
                    i += 1;
                }
            }
            '~' => {
                out.push(Tok::Tilde);
                i += 1;
            }
            '^' => {
                out.push(Tok::Caret);
                i += 1;
            }
            '<' => {
                if bytes.get(i + 1) == Some(&b'<') {
                    out.push(Tok::Shl);
                    i += 2;
                } else if bytes.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Le);
                    i += 2;
                } else {
                    out.push(Tok::Lt);
                    i += 1;
                }
            }
            '>' => {
                if bytes.get(i + 1) == Some(&b'>') {
                    out.push(Tok::Shr);
                    i += 2;
                } else if bytes.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Ge);
                    i += 2;
                } else {
                    out.push(Tok::Gt);
                    i += 1;
                }
            }
            '=' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    out.push(Tok::EqEq);
                    i += 2;
                } else {
                    return Err(ExprError::Syntax("'=' must be '=='".to_string()));
                }
            }
            '!' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Ne);
                    i += 2;
                } else {
                    out.push(Tok::Bang);
                    i += 1;
                }
            }
            '&' => {
                if bytes.get(i + 1) == Some(&b'&') {
                    out.push(Tok::AndAnd);
                    i += 2;
                } else {
                    out.push(Tok::Amp);
                    i += 1;
                }
            }
            '|' => {
                if bytes.get(i + 1) == Some(&b'|') {
                    out.push(Tok::OrOr);
                    i += 2;
                } else {
                    out.push(Tok::Pipe);
                    i += 1;
                }
            }
            '$' => {
                if !matches!(bytes.get(i + 1), Some(b) if (*b as char).is_ascii_hexdigit()) {
                    return Err(ExprError::Syntax("'$' needs hex digits".to_string()));
                }
                let (val, next) = read_radix(bytes, i + 1, 16);
                out.push(Tok::Num(val));
                i = next;
            }
            '0'..='9' => {
                if c == '0' && matches!(bytes.get(i + 1), Some(b'x' | b'X')) {
                    if !matches!(bytes.get(i + 2), Some(b) if (*b as char).is_ascii_hexdigit()) {
                        return Err(ExprError::Syntax("'0x' needs hex digits".to_string()));
                    }
                    let (val, next) = read_radix(bytes, i + 2, 16);
                    out.push(Tok::Num(val));
                    i = next;
                } else {
                    let (val, next) = read_radix(bytes, i, 10);
                    out.push(Tok::Num(val));
                    i = next;
                }
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < bytes.len() {
                    let ch = bytes[i] as char;
                    if ch.is_ascii_alphanumeric() || ch == '_' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                out.push(Tok::Ident(src[start..i].to_ascii_lowercase()));
            }
            other => return Err(ExprError::BadChar(other)),
        }
    }
    Ok(out)
}

/// Read a run of digits in `radix` starting at `start`, returning the
/// accumulated value (wrapping) and the index just past the last digit.
fn read_radix(bytes: &[u8], start: usize, radix: u32) -> (i64, usize) {
    let mut i = start;
    let mut acc: i64 = 0;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        match ch.to_digit(radix) {
            Some(d) => {
                acc = acc.wrapping_mul(radix as i64).wrapping_add(d as i64);
                i += 1;
            }
            None => break,
        }
    }
    (acc, i)
}

// -- Parser (recursive descent over the token slice) ------------------

struct Parser<'a> {
    tokens: &'a [Tok],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<&Tok> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, want: &Tok) -> bool {
        if self.peek() == Some(want) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse_or(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_and()?;
        while self.eat(&Tok::OrOr) {
            let right = self.parse_and()?;
            left = Expr::Binary(BinOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_bor()?;
        while self.eat(&Tok::AndAnd) {
            let right = self.parse_bor()?;
            left = Expr::Binary(BinOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_bor(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_bxor()?;
        while self.eat(&Tok::Pipe) {
            let right = self.parse_bxor()?;
            left = Expr::Binary(BinOp::BitOr, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_bxor(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_band()?;
        while self.eat(&Tok::Caret) {
            let right = self.parse_band()?;
            left = Expr::Binary(BinOp::BitXor, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_band(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_eq()?;
        while self.eat(&Tok::Amp) {
            let right = self.parse_eq()?;
            left = Expr::Binary(BinOp::BitAnd, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_eq(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_rel()?;
        loop {
            let op = match self.peek() {
                Some(Tok::EqEq) => BinOp::Eq,
                Some(Tok::Ne) => BinOp::Ne,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_rel()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_rel(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_shift()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Lt) => BinOp::Lt,
                Some(Tok::Le) => BinOp::Le,
                Some(Tok::Gt) => BinOp::Gt,
                Some(Tok::Ge) => BinOp::Ge,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_shift()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_shift(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_add()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Shl) => BinOp::Shl,
                Some(Tok::Shr) => BinOp::Shr,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_add()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => BinOp::Add,
                Some(Tok::Minus) => BinOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_mul()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => BinOp::Mul,
                Some(Tok::Slash) => BinOp::Div,
                Some(Tok::Percent) => BinOp::Rem,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_unary()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, ExprError> {
        let op = match self.peek() {
            Some(Tok::Bang) => Some(UnOp::Not),
            Some(Tok::Tilde) => Some(UnOp::BitNot),
            Some(Tok::Minus) => Some(UnOp::Neg),
            _ => None,
        };
        if let Some(op) = op {
            self.pos += 1;
            let inner = self.parse_unary()?;
            return Ok(Expr::Unary(op, Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, ExprError> {
        match self.bump() {
            Some(Tok::Num(n)) => Ok(Expr::Num(*n)),
            Some(Tok::Ident(name)) => Ok(Expr::Reg(name.clone())),
            Some(Tok::LParen) => {
                let inner = self.parse_or()?;
                if !self.eat(&Tok::RParen) {
                    return Err(ExprError::Syntax("missing ')'".to_string()));
                }
                Ok(inner)
            }
            Some(Tok::LBrack) => {
                let addr = self.parse_or()?;
                if !self.eat(&Tok::RBrack) {
                    return Err(ExprError::Syntax("missing ']'".to_string()));
                }
                let width = self.parse_mem_width()?;
                Ok(Expr::Mem(Box::new(addr), width))
            }
            Some(other) => Err(ExprError::Syntax(format!("unexpected token {other:?}"))),
            None => Err(ExprError::Syntax("unexpected end of input".to_string())),
        }
    }

    /// An optional `.b` / `.w` / `.l` suffix after a `[...]` memory read.
    /// Absent => byte (width 1).
    fn parse_mem_width(&mut self) -> Result<u8, ExprError> {
        if !self.eat(&Tok::Dot) {
            return Ok(1);
        }
        match self.bump() {
            Some(Tok::Ident(s)) if s == "b" => Ok(1),
            Some(Tok::Ident(s)) if s == "w" => Ok(2),
            Some(Tok::Ident(s)) if s == "l" => Ok(4),
            _ => Err(ExprError::Syntax("expected .b/.w/.l after ']'".to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A simple test context: a register map plus a byte-addressed memory
    /// map, assembled big-endian (matching the 68000).
    struct FakeCtx {
        regs: BTreeMap<String, i64>,
        mem: BTreeMap<u32, u8>,
    }

    impl FakeCtx {
        fn new() -> Self {
            Self {
                regs: BTreeMap::new(),
                mem: BTreeMap::new(),
            }
        }
        fn reg(mut self, name: &str, v: i64) -> Self {
            self.regs.insert(name.to_string(), v);
            self
        }
        fn byte(mut self, addr: u32, v: u8) -> Self {
            self.mem.insert(addr, v);
            self
        }
    }

    impl ExprContext for FakeCtx {
        fn register(&mut self, name: &str) -> Option<i64> {
            self.regs.get(name).copied()
        }
        fn read_mem(&mut self, addr: u32, width: u8) -> i64 {
            let mut acc: i64 = 0;
            for i in 0..width as u32 {
                let b = self.mem.get(&(addr + i)).copied().unwrap_or(0) as i64;
                acc = (acc << 8) | b; // big-endian
            }
            acc
        }
    }

    fn eval(src: &str, ctx: &mut FakeCtx) -> Result<i64, ExprError> {
        Expr::parse(src)?.eval(ctx)
    }

    #[test]
    fn numbers_in_every_radix() {
        let mut c = FakeCtx::new();
        assert_eq!(eval("42", &mut c), Ok(42));
        assert_eq!(eval("0x2A", &mut c), Ok(42));
        assert_eq!(eval("$2a", &mut c), Ok(42));
        assert_eq!(eval("%101010", &mut c), Ok(42));
    }

    #[test]
    fn arithmetic_precedence() {
        let mut c = FakeCtx::new();
        assert_eq!(eval("2 + 3 * 4", &mut c), Ok(14));
        assert_eq!(eval("(2 + 3) * 4", &mut c), Ok(20));
        assert_eq!(eval("10 - 2 - 3", &mut c), Ok(5)); // left-assoc
        assert_eq!(eval("17 % 5", &mut c), Ok(2));
        assert_eq!(eval("1 << 4", &mut c), Ok(16));
        assert_eq!(eval("0xFF >> 4", &mut c), Ok(0x0F));
    }

    #[test]
    fn bitwise_and_unary() {
        let mut c = FakeCtx::new();
        assert_eq!(eval("0xF0 | 0x0F", &mut c), Ok(0xFF));
        assert_eq!(eval("0xFF & 0x0F", &mut c), Ok(0x0F));
        assert_eq!(eval("0xFF ^ 0x0F", &mut c), Ok(0xF0));
        assert_eq!(eval("~0", &mut c), Ok(-1));
        assert_eq!(eval("-5", &mut c), Ok(-5));
        assert_eq!(eval("!0", &mut c), Ok(1));
        assert_eq!(eval("!5", &mut c), Ok(0));
    }

    #[test]
    fn comparisons_yield_one_or_zero() {
        let mut c = FakeCtx::new();
        assert_eq!(eval("5 == 5", &mut c), Ok(1));
        assert_eq!(eval("5 != 5", &mut c), Ok(0));
        assert_eq!(eval("3 < 5", &mut c), Ok(1));
        assert_eq!(eval("5 <= 5", &mut c), Ok(1));
        assert_eq!(eval("6 > 5", &mut c), Ok(1));
        assert_eq!(eval("4 >= 5", &mut c), Ok(0));
    }

    #[test]
    fn logical_and_or_not_with_precedence() {
        let mut c = FakeCtx::new();
        // && binds tighter than ||
        assert_eq!(eval("1 || 0 && 0", &mut c), Ok(1));
        assert_eq!(eval("(1 || 0) && 0", &mut c), Ok(0));
        assert_eq!(eval("!(3 > 4)", &mut c), Ok(1));
    }

    #[test]
    fn registers_resolve_through_context() {
        let mut c = FakeCtx::new().reg("d0", 5).reg("a7", 0xFFF000);
        assert_eq!(eval("d0 == 5", &mut c), Ok(1));
        assert_eq!(eval("d0 + 1", &mut c), Ok(6));
        assert_eq!(eval("a7 < 0xFFF008", &mut c), Ok(1));
        // Case-insensitive: identifiers are lowercased on tokenize.
        assert_eq!(eval("D0", &mut c), Ok(5));
    }

    #[test]
    fn memory_reads_with_width_suffix() {
        let mut c = FakeCtx::new()
            .byte(0x1000, 0x12)
            .byte(0x1001, 0x34)
            .byte(0x1002, 0x56)
            .byte(0x1003, 0x78);
        assert_eq!(eval("[0x1000]", &mut c), Ok(0x12)); // default byte
        assert_eq!(eval("[0x1000].b", &mut c), Ok(0x12));
        assert_eq!(eval("[0x1000].w", &mut c), Ok(0x1234));
        assert_eq!(eval("[0x1000].l", &mut c), Ok(0x1234_5678));
        // Address can itself be an expression.
        let mut c2 = c;
        assert_eq!(eval("[0x0FFF + 1].w", &mut c2), Ok(0x1234));
    }

    #[test]
    fn combined_register_and_memory_condition() {
        let mut c = FakeCtx::new()
            .reg("d0", 5)
            .byte(0xFF0000, 0x01)
            .byte(0xFF0001, 0x00);
        assert_eq!(eval("d0 == 5 && [0xFF0000].w > 0x100", &mut c), Ok(0));
        assert_eq!(eval("d0 == 5 && [0xFF0000].w >= 0x100", &mut c), Ok(1));
        assert_eq!(eval("d0 != 5 || [0xFF0000].w == 0x100", &mut c), Ok(1));
    }

    #[test]
    fn eval_str_gate() {
        let mut c = FakeCtx::new().reg("d0", 7);
        assert_eq!(eval_str("d0 > 3", &mut c), Ok(true));
        assert_eq!(eval_str("d0 > 30", &mut c), Ok(false));
    }

    #[test]
    fn short_circuit_skips_erroring_side() {
        // The right side references an unknown register, but && short
        // circuits on the false left so the whole thing is still a clean
        // `false` rather than an error.
        let mut c = FakeCtx::new().reg("d0", 0);
        assert_eq!(eval_str("d0 != 0 && unknownreg == 1", &mut c), Ok(false));
        // Likewise || short circuits on a true left.
        let mut c2 = FakeCtx::new().reg("d0", 1);
        assert_eq!(eval_str("d0 != 0 || unknownreg == 1", &mut c2), Ok(true));
    }

    #[test]
    fn unknown_register_is_an_error() {
        let mut c = FakeCtx::new();
        assert_eq!(
            eval("nope", &mut c),
            Err(ExprError::UnknownRegister("nope".to_string()))
        );
    }

    #[test]
    fn division_by_zero_is_an_error() {
        let mut c = FakeCtx::new();
        assert_eq!(eval("1 / 0", &mut c), Err(ExprError::DivByZero));
        assert_eq!(eval("1 % 0", &mut c), Err(ExprError::DivByZero));
    }

    #[test]
    fn syntax_errors_are_reported() {
        assert_eq!(Expr::parse(""), Err(ExprError::Empty));
        assert_eq!(Expr::parse("   "), Err(ExprError::Empty));
        assert!(matches!(Expr::parse("1 +"), Err(ExprError::Syntax(_))));
        assert!(matches!(Expr::parse("(1 + 2"), Err(ExprError::Syntax(_))));
        assert!(matches!(Expr::parse("[1"), Err(ExprError::Syntax(_))));
        assert_eq!(Expr::parse("1 2"), Err(ExprError::Trailing));
        assert!(matches!(Expr::parse("1 = 2"), Err(ExprError::Syntax(_))));
        assert_eq!(Expr::parse("#"), Err(ExprError::BadChar('#')));
        assert!(matches!(Expr::parse("0x"), Err(ExprError::Syntax(_))));
        assert!(matches!(Expr::parse("$"), Err(ExprError::Syntax(_))));
        assert!(matches!(Expr::parse("[0].q"), Err(ExprError::Syntax(_))));
    }

    #[test]
    fn display_covers_error_variants() {
        // Exercise the Display impl so the messages don't rot.
        let msgs = [
            ExprError::Empty.to_string(),
            ExprError::BadChar('#').to_string(),
            ExprError::Syntax("x".to_string()).to_string(),
            ExprError::Trailing.to_string(),
            ExprError::UnknownRegister("d9".to_string()).to_string(),
            ExprError::DivByZero.to_string(),
        ];
        assert!(msgs.iter().all(|m| !m.is_empty()));
    }
}
