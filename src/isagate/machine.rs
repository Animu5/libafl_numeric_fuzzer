//! A Rust port of the `RISCV64GMachine` ISA model from the asltgen-afl-proxy
//! project (`atnwalk/grammars/emulator/RISCV64GMachine.go` + `FPClass.go`).
//!
//! This is a deliberately high-level, *gate-oriented* machine: it executes one
//! instruction at a time in program order and tracks just enough state (integer
//! and float register files, program counter, sparse byte memory) to validate
//! and fix snippets. Unlike the Go original it takes immediates as values
//! instead of drawing them from an atnwalk decoder; the field-width clamping
//! the decoder used to apply (`Decode(boundary)` returns `raw % boundary`)
//! is applied explicitly here, so the behavior matches the Go gate exactly.
//!
//! Porting notes / intentional deviations:
//! - Go panics on wacky operator sizes become `Err` returns here.
//! - `li` immediates in the Go pipeline are drawn as two 32-bit halves with
//!   `Decode(2^32-1)`, whose result is `raw % (2^32-1)` — the high half
//!   `0xFFFFFFFF` wraps to 0. `li_decode` reproduces that wrapping so emitted
//!   values match the Go gate bit-for-bit.
//! - Float comparisons in Go go through `big.Float`; for non-NaN operands that
//!   is exactly `==`/`<` on the float64 values (including -0.0 == 0.0), which is
//!   what we use.

use std::collections::HashMap;

/// Initial values for integer and float register sets.
pub const INITIAL_REGISTER_VALUE: i64 = 0x0;
/// Starting point for the program counter.
pub const START_ADDRESS: i64 = 0x8000_0000;
/// Lowest valid address for memory operations (0x2000 above the start).
pub const LOWER_VALID_MEM: i64 = START_ADDRESS + 0x2000;
/// Highest valid address for memory operations.
pub const UPPER_VALID_MEM: i64 = START_ADDRESS + 0x07FF_FFFF;
/// Middle of the valid memory range, used for the `li` fix-up.
pub const MIDDLE_VALID_MEM: i64 = START_ADDRESS + 0x400_0000;

/// Bits in U-type immediates (lui/auipc).
const U_TYPE_BITS: u32 = 20;
/// Bits in I-type immediates and load/store offsets.
const I_TYPE_BITS: u32 = 12;
/// Bits allowed for branch immediates.
const BRANCH_BITS: u32 = 4;
/// Highest valid shift amount (64-bit shifts).
const SHIFT_UPPER_BOUND: i64 = 63;
/// Highest valid shift amount for word-size shifts.
const WORD_SHIFT_UPPER_BOUND: i64 = 31;

/// Alignment for half-word memory operations.
const HALF_ALIGN: i64 = 2;
/// Alignment for word memory operations.
const WORD_ALIGN: i64 = 4;
/// Alignment for double-word memory operations.
const DOUBLE_ALIGN: i64 = 8;

/// fclass masks, ported from FPClass.go.
const SUBNORMAL_FLOAT64_MASK: u64 = 0x7FF0_0000_0000_0000;
const SUBNORMAL_FLOAT32_MASK: u32 = 0x7F80_0000;

/// fclass result bits, ported from FPClass.go.
pub mod fclass {
    pub const UNKNOWN: i64 = 0;
    pub const NEG_INFTY: i64 = 1 << 0;
    pub const NEG_NORMAL: i64 = 1 << 1;
    pub const NEG_SUBNORMAL: i64 = 1 << 2;
    pub const NEG_ZERO: i64 = 1 << 3;
    pub const POS_ZERO: i64 = 1 << 4;
    pub const POS_SUBNORMAL: i64 = 1 << 5;
    pub const POS_NORMAL: i64 = 1 << 6;
    pub const POS_INFTY: i64 = 1 << 7;
    pub const SIG_NAN: i64 = 1 << 8;
    pub const QUIET_NAN: i64 = 1 << 9;
}

/// What went wrong while executing a statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineError(pub String);

impl std::fmt::Display for MachineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Wrap an immediate into the signed field of `bits` bits, mirroring the Go
/// pipeline: the gate packs `(imm + 2^(bits-1)) & (2^bits - 1)` into the
/// decoder stream and `returnArbitrarySigned` reads it back as
/// `Decode(2^bits) - 2^(bits-1)`, i.e. the representative of `imm mod 2^bits`
/// in `[-2^(bits-1), 2^(bits-1) - 1]`.
fn clamp_signed(imm: i64, bits: u32) -> i64 {
    let modulus = 1i64 << bits;
    let r = imm.rem_euclid(modulus);
    if r >= modulus >> 1 {
        r - modulus
    } else {
        r
    }
}

/// Clamp an immediate to `raw % boundary`, mirroring `Decoder.Decode(boundary)`.
fn clamp_mod(imm: i64, boundary: i64) -> i64 {
    imm.rem_euclid(boundary)
}

/// The shift amount a shift-immediate instruction uses, mirroring the Go
/// pipeline: the gate packs the raw immediate into 6 (or 5) bits and the
/// machine draws `Decode(63)` (or `Decode(31)`), i.e.
/// `(imm mod 2^shift_bits) mod upper_bound`.
fn clamp_shift(imm: i64, shift_bits: u32, upper_bound: i64) -> i64 {
    clamp_mod(clamp_mod(imm, 1 << shift_bits), upper_bound)
}

/// Reproduce the Go `doLi` immediate: two 32-bit halves drawn with
/// `Decode(2^32-1)` (result = `raw % (2^32-1)`, so 0xFFFFFFFF wraps to 0),
/// recombined as `(high << 32) | low` with wrapping int64 semantics.
fn li_decode(imm: i64) -> i64 {
    let u = imm as u64;
    let low = (u & 0xFFFF_FFFF) % 0xFFFF_FFFF;
    let high = (u >> 32) % 0xFFFF_FFFF;
    ((high << 32) | low) as i64
}

/// The high-level RV64G machine state used by the ISA gate.
pub struct RiscV64Machine {
    int_regset: [i64; 32],
    float_regset: [f64; 32],
    program_counter: i64,
    /// Sparse byte memory (missing bytes read as 0, like a Go map).
    memory: HashMap<i64, u8>,
}

impl Default for RiscV64Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl RiscV64Machine {
    /// Creates a machine with zeroed registers, `pc = START_ADDRESS`, and empty memory.
    pub fn new() -> Self {
        Self {
            int_regset: [INITIAL_REGISTER_VALUE; 32],
            float_regset: [f64::from_bits(INITIAL_REGISTER_VALUE as u64); 32],
            program_counter: START_ADDRESS,
            memory: HashMap::new(),
        }
    }

    /// x0 is hardwired to zero; the Go code resets it after every write.
    fn reset_x0(&mut self, rd: usize) {
        if rd == 0 {
            self.int_regset[0] = 0;
        }
    }

    /// Reads a byte of memory (missing bytes are 0).
    fn mem(&self, addr: i64) -> u8 {
        *self.memory.get(&addr).unwrap_or(&0)
    }

    /// Writes a byte of memory.
    fn set_mem(&mut self, addr: i64, value: u8) {
        self.memory.insert(addr, value);
    }

    /// Reads `n` big-endian bytes starting at `addr` (address arithmetic wraps,
    /// like Go's int64 arithmetic).
    fn read_mem(&self, addr: i64, n: usize) -> u64 {
        let mut value = 0u64;
        for i in 0..n {
            value = (value << 8) | u64::from(self.mem(addr.wrapping_add(i as i64)));
        }
        value
    }

    /// Writes the low `n` bytes of `value` big-endian starting at `addr`.
    fn write_mem(&mut self, addr: i64, value: u64, n: usize) {
        for i in 0..n {
            let shift = 8 * (n - 1 - i);
            self.set_mem(addr.wrapping_add(i as i64), (value >> shift) as u8);
        }
    }

    // ------------------------------------------------------------------
    // Integer instructions
    // ------------------------------------------------------------------

    /// `mv xD,xS` (the only int2 instruction).
    pub fn parse_int2(&mut self, mnemonic: &str, rd: usize, rs1: usize) -> Result<(), MachineError> {
        self.program_counter += 4;
        match mnemonic {
            "mv" => self.int_regset[rd] = self.int_regset[rs1],
            other => return Err(MachineError(format!("unknown int2 mnemonic {other:?}"))),
        }
        self.reset_x0(rd);
        Ok(())
    }

    /// `li`/`lui`/`auipc xD,imm`; returns the immediate to emit.
    pub fn parse_int_imm2(&mut self, mnemonic: &str, rd: usize, imm: i64) -> Result<i64, MachineError> {
        self.program_counter += 4;
        let emitted = match mnemonic {
            "li" => {
                let value = li_decode(imm);
                self.int_regset[rd] = value;
                value
            }
            "lui" => {
                let value = clamp_mod(imm, 1 << U_TYPE_BITS);
                self.int_regset[rd] = value << 12;
                value
            }
            "auipc" => {
                let value = clamp_mod(imm, 1 << U_TYPE_BITS);
                self.int_regset[rd] = self.program_counter + (value << 12);
                value
            }
            other => return Err(MachineError(format!("unknown int-imm2 mnemonic {other:?}"))),
        };
        self.reset_x0(rd);
        Ok(emitted)
    }

    /// Three-register integer instructions (`add xD,xS1,xS2`, …).
    pub fn parse_int3(
        &mut self,
        mnemonic: &str,
        rd: usize,
        rs1: usize,
        rs2: usize,
    ) -> Result<(), MachineError> {
        self.program_counter += 4;
        let (a, b) = (self.int_regset[rs1], self.int_regset[rs2]);
        let result = match mnemonic {
            "sll" => a << (b & SHIFT_UPPER_BOUND),
            "srl" => (a as u64 >> (b & SHIFT_UPPER_BOUND)) as i64,
            "sra" => a >> (b & SHIFT_UPPER_BOUND),
            "sllw" => (a as i32).wrapping_shl((b & WORD_SHIFT_UPPER_BOUND) as u32) as i64,
            "srlw" => ((a as u32).wrapping_shr((b & WORD_SHIFT_UPPER_BOUND) as u32)) as i64,
            "sraw" => ((a as i32).wrapping_shr((b & WORD_SHIFT_UPPER_BOUND) as u32)) as i64,
            "add" => a.wrapping_add(b),
            "addw" => (a as i32).wrapping_add(b as i32) as i64,
            "sub" => a.wrapping_sub(b),
            "subw" => (a as i32).wrapping_sub(b as i32) as i64,
            "xor" => a ^ b,
            "or" => a | b,
            "and" => a & b,
            "slt" => i64::from(a < b),
            "sltu" => i64::from((a as u64) < (b as u64)),
            "mul" => a.wrapping_mul(b),
            "mulw" => (a as i32).wrapping_mul(b as i32) as i64,
            "mulh" => mulh(a, b),
            "mulhsu" => mulhsu(a, b),
            // High word of the 64x64 -> 128-bit product.
            "mulhu" => ((a as u64 as u128 * b as u64 as u128) >> 64) as i64,
            "div" => {
                if b == 0 {
                    -1
                } else if a == i64::MIN && b == -1 {
                    i64::MIN
                } else {
                    a / b
                }
            }
            "divw" => {
                if b as i32 == 0 {
                    -1
                } else if a as i32 == i32::MIN && b == -1 {
                    i64::from(i32::MIN)
                } else {
                    i64::from((a as i32) / (b as i32))
                }
            }
            "divu" => {
                if b == 0 {
                    -1 // Same as math.MaxUint64
                } else {
                    ((a as u64) / (b as u64)) as i64
                }
            }
            "divuw" => match (a as u32).checked_div(b as u32) {
                None => u32::MAX as i64, // division by zero
                Some(q) => q as i64,
            },
            "rem" => {
                if b == 0 {
                    a
                } else if a == i64::MIN && b == -1 {
                    0
                } else {
                    a % b
                }
            }
            "remw" => {
                if b as i32 == 0 {
                    i64::from(a as i32)
                } else if a as i32 == i32::MIN && b == -1 {
                    0
                } else {
                    i64::from((a as i32) % (b as i32))
                }
            }
            "remu" => {
                if b == 0 {
                    a
                } else {
                    ((a as u64) % (b as u64)) as i64
                }
            }
            "remuw" => {
                if b as u32 == 0 {
                    a
                } else {
                    ((a as u32) % (b as u32)) as i64
                }
            }
            other => return Err(MachineError(format!("unknown int3 mnemonic {other:?}"))),
        };
        self.int_regset[rd] = result;
        self.reset_x0(rd);
        Ok(())
    }

    /// Immediate/shift/branch instructions (`addi xD,xS,imm`, `slli …`, `beq …`);
    /// returns the value to emit (the clamped immediate, or the absolute branch
    /// target PC for branches — gas accepts an absolute branch operand).
    pub fn parse_int_imm3(
        &mut self,
        mnemonic: &str,
        rd: usize,
        rs1: usize,
        imm: i64,
    ) -> Result<i64, MachineError> {
        self.program_counter += 4;
        let a = self.int_regset[rs1];
        let emitted = match mnemonic {
            "slli" => {
                let sh = clamp_shift(imm, 6, SHIFT_UPPER_BOUND);
                self.int_regset[rd] = a << sh;
                sh
            }
            "srli" => {
                let sh = clamp_shift(imm, 6, SHIFT_UPPER_BOUND);
                self.int_regset[rd] = (a as u64 >> sh) as i64;
                sh
            }
            "srai" => {
                let sh = clamp_shift(imm, 6, SHIFT_UPPER_BOUND);
                self.int_regset[rd] = a >> sh;
                sh
            }
            "slliw" => {
                let sh = clamp_shift(imm, 5, WORD_SHIFT_UPPER_BOUND);
                self.int_regset[rd] = ((a as i32) << sh) as i64;
                sh
            }
            "srliw" => {
                let sh = clamp_shift(imm, 5, WORD_SHIFT_UPPER_BOUND);
                self.int_regset[rd] = ((a as u32) >> sh) as i64;
                sh
            }
            "sraiw" => {
                let sh = clamp_shift(imm, 5, WORD_SHIFT_UPPER_BOUND);
                self.int_regset[rd] = ((a as i32) >> sh) as i64;
                sh
            }
            "addi" => {
                let v = clamp_signed(imm, I_TYPE_BITS);
                self.int_regset[rd] = a.wrapping_add(v);
                v
            }
            "addiw" => {
                let v = clamp_signed(imm, I_TYPE_BITS);
                self.int_regset[rd] = ((a as i32).wrapping_add(v as i32)) as i64;
                v
            }
            "xori" => {
                let v = clamp_signed(imm, I_TYPE_BITS);
                self.int_regset[rd] = a ^ v;
                v
            }
            "ori" => {
                let v = clamp_signed(imm, I_TYPE_BITS);
                self.int_regset[rd] = a | v;
                v
            }
            "andi" => {
                let v = clamp_signed(imm, I_TYPE_BITS);
                self.int_regset[rd] = a & v;
                v
            }
            "slti" => {
                let v = clamp_signed(imm, I_TYPE_BITS);
                self.int_regset[rd] = i64::from(a < v);
                v
            }
            "sltiu" => {
                let v = clamp_signed(imm, I_TYPE_BITS);
                self.int_regset[rd] = i64::from((a as u64) < (v as u64));
                v
            }
            "beq" | "bne" | "blt" | "bltu" | "bge" | "bgeu" => return self.branch(imm),
            other => return Err(MachineError(format!("unknown int-imm3 mnemonic {other:?}"))),
        };
        self.reset_x0(rd);
        Ok(emitted)
    }

    /// Branch fix-up, ported from `doBranch`: clamp to the 4-bit signed field,
    /// never land below START_ADDRESS, clamp to ±16, then align the target to 4.
    /// Returns the absolute target PC (emitted as the branch operand).
    fn branch(&mut self, imm: i64) -> Result<i64, MachineError> {
        let mut imm = clamp_signed(imm, BRANCH_BITS);
        let mut target_pc = self.program_counter + imm;
        if target_pc < START_ADDRESS {
            imm += target_pc - START_ADDRESS;
            target_pc = self.program_counter + imm;
        }
        imm = imm.clamp(-16, 16);
        if target_pc % 4 != 0 {
            imm -= target_pc % 4;
            target_pc = self.program_counter + imm;
        }
        Ok(target_pc)
    }

    // ------------------------------------------------------------------
    // Float instructions
    // ------------------------------------------------------------------

    /// Splits `fadd.s` into (`fadd`, `s`), like Go's `strings.Cut(mnemonic, ".")`.
    fn split_mnemonic(mnemonic: &str) -> (&str, &str) {
        match mnemonic.split_once('.') {
            Some((op, suffixes)) => (op, suffixes),
            None => (mnemonic, ""),
        }
    }

    /// `fmv.x.w xD,fS` / `fcvt.*` / `fclass.*` — float to int.
    pub fn parse_float2int(&mut self, mnemonic: &str, rd: usize, rs1: usize) -> Result<(), MachineError> {
        self.program_counter += 4;
        let (op, suffixes) = Self::split_mnemonic(mnemonic);
        let f = self.float_regset[rs1];
        match op {
            "fmv" => {
                // suffixes look like "x.w" / "x.d": the float size is the second part.
                let floatsize = suffixes.split_once('.').map_or("", |(_, s)| s);
                match floatsize {
                    "w" => self.int_regset[rd] = (f as f32).to_bits() as i64,
                    "d" => self.int_regset[rd] = f.to_bits() as i64,
                    other => return Err(MachineError(format!("wacky float operator size {other:?}"))),
                }
            }
            "fcvt" => {
                // suffixes look like "w.s" / "l.d" / "wu.s" …: int size first, float size second.
                let (intsize, floatsize) = match suffixes.split_once('.') {
                    Some((i, f)) => (i, f),
                    None => return Err(MachineError(format!("wacky fcvt suffix {suffixes:?}"))),
                };
                let value = match intsize {
                    "w" => match floatsize {
                        "s" => (f as f32) as i32 as i64,
                        "d" => f as i32 as i64,
                        other => return Err(MachineError(format!("wacky float size {other:?}"))),
                    },
                    "l" => match floatsize {
                        "s" => (f as f32) as i64,
                        "d" => f as i64,
                        other => return Err(MachineError(format!("wacky float size {other:?}"))),
                    },
                    "wu" => match floatsize {
                        "s" => (f as f32) as u32 as i64,
                        "d" => f as u32 as i64,
                        other => return Err(MachineError(format!("wacky float size {other:?}"))),
                    },
                    "lu" => match floatsize {
                        "s" => (f as f32) as u64 as i64,
                        "d" => f as u64 as i64,
                        other => return Err(MachineError(format!("wacky float size {other:?}"))),
                    },
                    other => return Err(MachineError(format!("wacky integer operator size {other:?}"))),
                };
                self.int_regset[rd] = value;
            }
            "fclass" => {
                self.int_regset[rd] = match suffixes {
                    "s" => classify_float32(f as f32),
                    "d" => classify_float64(f),
                    other => return Err(MachineError(format!("wacky float operator size {other:?}"))),
                };
            }
            other => return Err(MachineError(format!("unknown float2int mnemonic {other:?}"))),
        }
        self.reset_x0(rd);
        Ok(())
    }

    /// `fmv.w.x fD,xS` / `fcvt.*` — int to float.
    pub fn parse_int2float(&mut self, mnemonic: &str, rd: usize, rs1: usize) -> Result<(), MachineError> {
        self.program_counter += 4;
        let (op, suffixes) = Self::split_mnemonic(mnemonic);
        let a = self.int_regset[rs1];
        match op {
            "fmv" => {
                // suffixes look like "w.x" / "d.x": the int size is the first part.
                let intsize = suffixes.split_once('.').map_or("", |(i, _)| i);
                match intsize {
                    "w" => self.float_regset[rd] = f64::from(f32::from_bits(a as u32)),
                    "d" => self.float_regset[rd] = f64::from_bits(a as u64),
                    other => return Err(MachineError(format!("wacky integer operator size {other:?}"))),
                }
            }
            "fcvt" => {
                // suffixes look like "s.w" / "d.lu" …: float size first, int size second.
                let (floatsize, intsize) = match suffixes.split_once('.') {
                    Some((f, i)) => (f, i),
                    None => return Err(MachineError(format!("wacky fcvt suffix {suffixes:?}"))),
                };
                let value = match intsize {
                    // Converting int32 to float32/float64 is practically the same here,
                    // the float register set is float64 anyway.
                    "w" => f64::from(a as i32),
                    "l" => match floatsize {
                        "s" => f64::from(a as f32),
                        "d" => a as f64,
                        other => return Err(MachineError(format!("wacky float size {other:?}"))),
                    },
                    "wu" => match floatsize {
                        "s" => (a as u32) as f32 as f64,
                        "d" => a as u32 as f64,
                        other => return Err(MachineError(format!("wacky float size {other:?}"))),
                    },
                    "lu" => match floatsize {
                        "s" => (a as u64) as f32 as f64,
                        "d" => a as u64 as f64,
                        other => return Err(MachineError(format!("wacky float size {other:?}"))),
                    },
                    other => return Err(MachineError(format!("wacky integer operator size {other:?}"))),
                };
                self.float_regset[rd] = value;
            }
            other => return Err(MachineError(format!("unknown int2float mnemonic {other:?}"))),
        }
        Ok(())
    }

    /// `feq/flt/fle.* xD,fS1,fS2`.
    pub fn parse_fcompare(
        &mut self,
        mnemonic: &str,
        rd: usize,
        rs1: usize,
        rs2: usize,
    ) -> Result<(), MachineError> {
        self.program_counter += 4;
        let (op, opsize) = Self::split_mnemonic(mnemonic);
        let (a, b) = (self.float_regset[rs1], self.float_regset[rs2]);
        // NaN compares false for all three.
        let result = if a.is_nan() || b.is_nan() {
            0
        } else {
            // Compare in the operand's precision, like the Go big.Float comparison.
            let (a, b) = match opsize {
                "s" => (f64::from(a as f32), f64::from(b as f32)),
                "d" => (a, b),
                other => return Err(MachineError(format!("wacky float operator size {other:?}"))),
            };
            match op {
                "feq" => i64::from(a == b),
                "flt" => i64::from(a < b),
                "fle" => i64::from(a <= b),
                other => return Err(MachineError(format!("unknown fcompare mnemonic {other:?}"))),
            }
        };
        self.int_regset[rd] = result;
        self.reset_x0(rd);
        Ok(())
    }

    /// `fsqrt.*` / `fcvt.s.d` / `fcvt.d.s` — `fD,fS`.
    pub fn parse_float2(&mut self, mnemonic: &str, rd: usize, rs1: usize) -> Result<(), MachineError> {
        self.program_counter += 4;
        let (op, opsize) = Self::split_mnemonic(mnemonic);
        let a = self.float_regset[rs1];
        match op {
            "fsqrt" => match opsize {
                "s" => self.float_regset[rd] = f64::from((a as f32).sqrt()),
                "d" => self.float_regset[rd] = a.sqrt(),
                other => return Err(MachineError(format!("wacky float operator size {other:?}"))),
            },
            "fcvt" => {
                // "s.d" converts float64→float32 (stored as float64), "d.s" is a no-op move.
                let (from, _to) = match opsize.split_once('.') {
                    Some((f, t)) => (f, t),
                    None => return Err(MachineError(format!("wacky fcvt suffix {opsize:?}"))),
                };
                match from {
                    "s" => self.float_regset[rd] = f64::from(a as f32),
                    "d" => self.float_regset[rd] = a,
                    other => return Err(MachineError(format!("wacky float size {other:?}"))),
                }
            }
            other => return Err(MachineError(format!("unknown float2 mnemonic {other:?}"))),
        }
        Ok(())
    }

    /// `fadd/fsub/fmul/fdiv/fsgnj/fsgnjn/fsgnjx/fmin/fmax.* fD,fS1,fS2`.
    pub fn parse_float3(
        &mut self,
        mnemonic: &str,
        rd: usize,
        rs1: usize,
        rs2: usize,
    ) -> Result<(), MachineError> {
        self.program_counter += 4;
        let (op, opsize) = Self::split_mnemonic(mnemonic);
        let (a, b) = (self.float_regset[rs1], self.float_regset[rs2]);
        // Compute in the operand's precision, storing back into the float64 regset.
        match op {
            "fadd" => self.float_regset[rd] = self.float_bin(opsize, a, b, |x, y| x + y)?,
            "fsub" => self.float_regset[rd] = self.float_bin(opsize, a, b, |x, y| x - y)?,
            "fmul" => self.float_regset[rd] = self.float_bin(opsize, a, b, |x, y| x * y)?,
            "fdiv" => self.float_regset[rd] = self.float_bin(opsize, a, b, |x, y| x / y)?,
            "fmin" => self.float_regset[rd] = self.float_bin(opsize, a, b, f64::min)?,
            "fmax" => self.float_regset[rd] = self.float_bin(opsize, a, b, f64::max)?,
            "fsgnj" => self.float_regset[rd] = self.float_bin(opsize, a, b, f64::copysign)?,
            "fsgnjn" => self.float_regset[rd] = self.float_bin(opsize, a, b, |x, y| f64::copysign(x, -y))?,
            "fsgnjx" => {
                let sign_xor = a.is_sign_negative() != b.is_sign_negative();
                let sign = if sign_xor { -1.0 } else { 1.0 };
                self.float_regset[rd] = self.float_bin(opsize, a, b, |x, _| f64::copysign(x, sign))?
            }
            other => return Err(MachineError(format!("unknown float3 mnemonic {other:?}"))),
        }
        Ok(())
    }

    /// Applies `f` in the operand precision ("s" = float32, "d" = float64).
    fn float_bin(
        &self,
        opsize: &str,
        a: f64,
        b: f64,
        f: impl Fn(f64, f64) -> f64,
    ) -> Result<f64, MachineError> {
        match opsize {
            "s" => Ok(f(a as f32 as f64, b as f32 as f64)),
            "d" => Ok(f(a, b)),
            other => Err(MachineError(format!("wacky float operator size {other:?}"))),
        }
    }

    /// `fmadd/fmsub/fnmadd/fnmsub.* fD,fS1,fS2,fS3`.
    pub fn parse_float4(
        &mut self,
        mnemonic: &str,
        rd: usize,
        rs1: usize,
        rs2: usize,
        rs3: usize,
    ) -> Result<(), MachineError> {
        self.program_counter += 4;
        let (op, opsize) = Self::split_mnemonic(mnemonic);
        let (a, b, c) = (
            self.float_regset[rs1],
            self.float_regset[rs2],
            self.float_regset[rs3],
        );
        self.float_regset[rd] = match opsize {
            "s" => {
                let (x, y, z) = (a as f32, b as f32, c as f32);
                f64::from(match op {
                    "fmadd" => x * y + z,
                    "fnmadd" => -(x * y) + z,
                    "fmsub" => x * y - z,
                    "fnmsub" => -(x * y) - z,
                    other => return Err(MachineError(format!("unknown float4 mnemonic {other:?}"))),
                })
            }
            "d" => match op {
                "fmadd" => a * b + c,
                "fnmadd" => -(a * b) + c,
                "fmsub" => a * b - c,
                "fnmsub" => -(a * b) - c,
                other => return Err(MachineError(format!("unknown float4 mnemonic {other:?}"))),
            },
            other => return Err(MachineError(format!("wacky float operator size {other:?}"))),
        };
        Ok(())
    }

    // ------------------------------------------------------------------
    // Memory instructions
    // ------------------------------------------------------------------

    /// Clamps `offset` into the valid memory range around `reg_addr`.
    /// Returns `(new_offset, needs_li)`: `needs_li` is true when the base
    /// register cannot hold an address that reaches valid memory at all.
    fn validate_memory_address(reg_addr: i64, offset: i64) -> (i64, bool) {
        // All address arithmetic wraps, like Go's int64 arithmetic.
        let max_offset = (1i64 << I_TYPE_BITS) - 1;
        let target_addr = reg_addr.wrapping_add(offset);
        if reg_addr.wrapping_sub(max_offset) > UPPER_VALID_MEM
            || reg_addr.wrapping_add(max_offset) < LOWER_VALID_MEM
        {
            return (0, true);
        } else if target_addr < LOWER_VALID_MEM {
            return (offset.wrapping_add(LOWER_VALID_MEM.wrapping_sub(target_addr)), false);
        } else if target_addr > UPPER_VALID_MEM {
            return (offset.wrapping_sub(target_addr.wrapping_sub(UPPER_VALID_MEM)), false);
        }
        (offset, false)
    }

    /// Realigns a misaligned target address, ported from `alignOffset`.
    fn align_offset(target_address: i64, reg_address: i64, alignment: i64) -> i64 {
        let mut aligned_off = (target_address & !(alignment - 1)).wrapping_sub(reg_address);
        if aligned_off < 0 {
            aligned_off += alignment;
        }
        aligned_off
    }

    /// Integer load/store `mnem xD,off(xS)`; returns `(emitted_offset, needs_li)`.
    /// The emitted offset is the (possibly realigned) offset actually used.
    pub fn parse_int_mem(
        &mut self,
        mnemonic: &str,
        rd: usize,
        rs1: usize,
        offset: i64,
    ) -> Result<(i64, bool), MachineError> {
        self.program_counter += 4;
        let offset = clamp_signed(offset, I_TYPE_BITS);
        let reg_addr = self.int_regset[rs1];
        let (new_off, needs_li) = Self::validate_memory_address(reg_addr, offset);
        if needs_li {
            return Ok((0, true));
        }
        let base = self.int_regset[rs1];
        let mut target = base.wrapping_add(new_off);
        let (align, size) = match mnemonic {
            "ld" | "sd" => (DOUBLE_ALIGN, 8),
            "lw" | "lwu" | "sw" => (WORD_ALIGN, 4),
            "lh" | "lhu" | "sh" => (HALF_ALIGN, 2),
            "lb" | "lbu" | "sb" => (1, 1),
            other => return Err(MachineError(format!("unknown int mem mnemonic {other:?}"))),
        };
        let mut off = new_off;
        if size > 1 && target % align != 0 {
            off = Self::align_offset(target, base, align);
            target = base.wrapping_add(off);
        }
        match mnemonic {
            "ld" => self.int_regset[rd] = self.read_mem(target, 8) as i64,
            "lw" => self.int_regset[rd] = self.read_mem(target, 4) as u32 as i32 as i64,
            "lwu" => self.int_regset[rd] = self.read_mem(target, 4) as i64,
            "lh" => self.int_regset[rd] = self.read_mem(target, 2) as u16 as i16 as i64,
            "lhu" => self.int_regset[rd] = self.read_mem(target, 2) as i64,
            "lb" => self.int_regset[rd] = self.mem(target) as i8 as i64,
            "lbu" => self.int_regset[rd] = i64::from(self.mem(target)),
            "sd" => self.write_mem(target, self.int_regset[rd] as u64, 8),
            "sw" => self.write_mem(target, self.int_regset[rd] as u32 as u64, 4),
            "sh" => self.write_mem(target, (self.int_regset[rd] as u16) as u64, 2),
            "sb" => self.set_mem(target, self.int_regset[rd] as u8),
            _ => unreachable!("mnemonic checked above"),
        }
        self.reset_x0(rd);
        Ok((off, false))
    }

    /// Float load/store `mnem fD,off(xS)`; returns `(emitted_offset, needs_li)`.
    pub fn parse_float_mem(
        &mut self,
        mnemonic: &str,
        rd: usize,
        rs1: usize,
        offset: i64,
    ) -> Result<(i64, bool), MachineError> {
        self.program_counter += 4;
        let offset = clamp_signed(offset, I_TYPE_BITS);
        let reg_addr = self.int_regset[rs1];
        let (new_off, needs_li) = Self::validate_memory_address(reg_addr, offset);
        if needs_li {
            return Ok((0, true));
        }
        let base = self.int_regset[rs1];
        let mut target = base.wrapping_add(new_off);
        let align = match mnemonic {
            "fld" | "fsd" => DOUBLE_ALIGN,
            "flw" | "fsw" => WORD_ALIGN,
            other => return Err(MachineError(format!("unknown float mem mnemonic {other:?}"))),
        };
        let mut off = new_off;
        if target % align != 0 {
            off = Self::align_offset(target, base, align);
            target = base.wrapping_add(off);
        }
        match mnemonic {
            "fld" => self.float_regset[rd] = f64::from_bits(self.read_mem(target, 8)),
            "flw" => self.float_regset[rd] = f64::from(f32::from_bits(self.read_mem(target, 4) as u32)),
            "fsd" => self.write_mem(target, self.float_regset[rd].to_bits(), 8),
            "fsw" => self.write_mem(target, (self.float_regset[rd] as f32).to_bits() as u64, 4),
            _ => unreachable!("mnemonic checked above"),
        }
        Ok((off, false))
    }
}

/// High word of the 64×64→128-bit product of the bit patterns, ported from
/// `doMulh`: the Go code computes the unsigned high word via `bits.Mul64` and
/// negates it on mixed signs (Go has no signed 128-bit multiply). Note this
/// is *not* the mathematically correct signed high word for two negative
/// operands — the port reproduces the Go behavior exactly.
fn mulh(a: i64, b: i64) -> i64 {
    let hi = ((a as u64 as u128 * b as u64 as u128) >> 64) as u64 as i64;
    if (a < 0 && b > 0) || (a > 0 && b < 0) {
        hi.wrapping_mul(-1)
    } else {
        hi
    }
}

/// High word with the first operand treated as signed, ported from `doMulhsu`:
/// unsigned high word via `bits.Mul64`, negated when `a` is negative.
fn mulhsu(a: i64, b: i64) -> i64 {
    let hi = ((a as u64 as u128 * b as u64 as u128) >> 64) as u64 as i64;
    if a < 0 {
        hi.wrapping_mul(-1)
    } else {
        hi
    }
}

/// fclass for float32, ported from `ClassifyFloat32`.
fn classify_float32(f: f32) -> i64 {
    if f.is_nan() {
        let bits = f.to_bits();
        if bits & !(1 << 22) == 1 {
            return fclass::QUIET_NAN;
        }
        return fclass::SIG_NAN;
    }
    if f.to_bits() & SUBNORMAL_FLOAT32_MASK == 0 {
        return if f.is_sign_negative() {
            fclass::NEG_SUBNORMAL
        } else {
            fclass::POS_SUBNORMAL
        };
    }
    classify_generic(f64::from(f))
}

/// fclass for float64, ported from `ClassifyFloat64`.
fn classify_float64(f: f64) -> i64 {
    if f.is_nan() {
        let bits = f.to_bits();
        if bits & !(1 << 51) == 1 {
            return fclass::QUIET_NAN;
        }
        return fclass::SIG_NAN;
    }
    if f.to_bits() & SUBNORMAL_FLOAT64_MASK == 0 {
        return if f.is_sign_negative() {
            fclass::NEG_SUBNORMAL
        } else {
            fclass::POS_SUBNORMAL
        };
    }
    classify_generic(f)
}

/// Shared fclass classification, ported from `ClassifyGenericFloat`.
fn classify_generic(f: f64) -> i64 {
    if f == f64::INFINITY {
        fclass::POS_INFTY
    } else if f == f64::NEG_INFINITY {
        fclass::NEG_INFTY
    } else if f == 0.0 && f.is_sign_negative() {
        fclass::NEG_ZERO
    } else if f == 0.0 {
        fclass::POS_ZERO
    } else if f < 0.0 {
        fclass::NEG_NORMAL
    } else if f > 0.0 {
        fclass::POS_NORMAL
    } else {
        fclass::UNKNOWN
    }
}
