//! The ISA gate: validates and fixes a grammar-generated assembly program.
//!
//! Ported from `atnwalk/cmd/isagate/main.go` in the asltgen-afl-proxy project.
//! The gate replays the program line by line on [`RiscV64Machine`] (the port of
//! the Go `RISCV64GMachine` the original `run_*` hooks execute on), so the
//! emitted text always matches the machine state:
//!
//! * memory operands are clamped into the valid address range and realigned;
//!   when the base register cannot hold a valid address at all, a
//!   `li xN, MIDDLE_VALID_MEM` line is inserted (x0 becomes x10 — it can never
//!   hold an address);
//! * every statement gets a `.Lbr{j}:` label and branch operands are rewritten
//!   to `.Lbr{imm mod n}` — the immediate carried by the parse tree picks the
//!   target statement, forward or backward. gas resolves the label against the
//!   real layout, so with compressed (RVC) instructions the target always lands
//!   on an instruction boundary (the 4-byte PC model would not match the
//!   assembled program, and an absolute target could trap mid-instruction);
//! * immediates are clamped to their instruction field widths;
//! * lines that are not statements (labels, `j _start`, `fld` init,
//!   `.section`/`.align`, `fNconst: .quad …`) pass through unchanged.
//!
//! [`gate`] returns the fixed program, or an error for programs it cannot fix.

use super::machine::{MIDDLE_VALID_MEM, RiscV64Machine};

/// Maximum number of statements the gate executes (defense against pathological
/// inputs; the machine is single-pass so this is just a sanity bound).
const MAX_STATEMENTS: usize = 100_000;

/// Why the gate rejected a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateError(pub String);

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ISA gate rejected the program: {}", self.0)
    }
}

/// Statement kinds, classified from a line's mnemonic and operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Other,
    IntImm2,   // li/lui/auipc xD,imm
    Int2,      // mv xD,xS
    Int3,      // add xD,xS1,xS2
    IntImm3,   // addi xD,xS,imm / shifts / branches
    Float2Int, // fmv.x.w xD,fS / fcvt / fclass
    Int2Float, // fmv.w.x fD,xS / fcvt
    FCompare,  // feq.s xD,fS1,fS2
    Float2,    // fsqrt.s fD,fS / fcvt.s.d
    Float3,    // fadd.s fD,fS1,fS2
    Float4,    // fmadd.s fD,fS1,fS2,fS3
    IntMem,    // lw xD,off(xS)
    FloatMem,  // flw fD,off(xS)
}

/// A classified line.
#[derive(Debug, Clone)]
struct Stmt {
    kind: Kind,
    mnemonic: String,
    rd: usize,
    rs1: usize,
    rs2: usize,
    rs3: usize,
    /// Immediate for IntImm2/IntImm3, offset for the mem kinds.
    imm: i64,
    line: String,
}

/// Parses "x12" or "f3" into the register number. Numbers ≥ 32 are rejected:
/// in the Go original the machine panics on the out-of-range array index and
/// the gate recovers that as an error, so the net behavior is a rejection.
fn parse_reg(s: &str) -> Result<usize, GateError> {
    let (prefix, num) = s.split_at(1);
    if prefix != "x" && prefix != "f" {
        return Err(GateError(format!("bad register {s:?}")));
    }
    match num.parse::<usize>() {
        Ok(n) if n < 32 => Ok(n),
        _ => Err(GateError(format!("bad register {s:?}"))),
    }
}

/// Parses "4(x2)" into offset 4 and base register 2. Per the RISC-V
/// specification a missing offset ("(x2)") is allowed and means 0.
fn parse_mem_operand(s: &str) -> Result<(i64, usize), GateError> {
    let open = s
        .find('(')
        .ok_or_else(|| GateError(format!("bad memory operand {s:?}")))?;
    if !s.ends_with(')') {
        return Err(GateError(format!("bad memory operand {s:?}")));
    }
    let off_text = &s[..open];
    let off = if off_text.is_empty() {
        0
    } else {
        off_text
            .parse::<i64>()
            .map_err(|e| GateError(format!("bad offset in {s:?}: {e}")))?
    };
    let base = parse_reg(&s[open + 1..s.len() - 1])?;
    Ok((off, base))
}

/// Whether a line is program structure, not a statement: labels (`_start:`),
/// the end loop (`j _start`), `fld` register init, assembler directives
/// (`.section`/`.align`) and float constant definitions (`f0const: .quad …`).
fn is_other_line(line: &str) -> bool {
    let mut fields = line.split_whitespace();
    match fields.next() {
        None => true,
        Some(mnem) => {
            mnem.ends_with(':') || mnem == "j" || mnem == "fld" || mnem.starts_with('.')
        }
    }
}

/// Whether a string is an integer register operand.
fn is_x(s: &str) -> bool {
    s.starts_with('x')
}

/// Whether a string is a float register operand.
fn is_f(s: &str) -> bool {
    s.starts_with('f')
}

/// Turns one assembly line into a `Stmt`; non-statement lines are `Kind::Other`,
/// unrecognized statement-like lines are an error.
fn classify(line: &str) -> Result<Stmt, GateError> {
    let mut fields = line.split_whitespace();
    let mnemonic = match fields.next() {
        None => {
            return Ok(Stmt {
                kind: Kind::Other,
                mnemonic: String::new(),
                rd: 0,
                rs1: 0,
                rs2: 0,
                rs3: 0,
                imm: 0,
                line: line.to_string(),
            })
        }
        Some(m) => m,
    };
    if is_other_line(line) {
        return Ok(Stmt {
            kind: Kind::Other,
            mnemonic: mnemonic.to_string(),
            rd: 0,
            rs1: 0,
            rs2: 0,
            rs3: 0,
            imm: 0,
            line: line.to_string(),
        });
    }
    let Some(operands) = fields.next() else {
        return Err(GateError(format!("unrecognized line {line:?}")));
    };
    let ops: Vec<&str> = operands.split(',').collect();
    let mut st = Stmt {
        kind: Kind::Other,
        mnemonic: mnemonic.to_string(),
        rd: 0,
        rs1: 0,
        rs2: 0,
        rs3: 0,
        imm: 0,
        line: line.to_string(),
    };
    let reg = |s: &str| parse_reg(s);
    let imm = |s: &str| {
        // Immediates are decimal (possibly with leading zeros, e.g. -018).
        s.parse::<i64>()
            .map_err(|e| GateError(format!("bad immediate in {line:?}: {e}")))
    };
    match () {
        // Memory: xD,off(xS) or fD,off(xS).
        _ if operands.contains('(') => {
            let (off, base) = parse_mem_operand(ops.last().unwrap())?;
            st.rd = reg(ops[0])?;
            st.rs1 = base;
            st.imm = off;
            st.kind = if is_f(ops[0]) {
                Kind::FloatMem
            } else {
                Kind::IntMem
            };
        }
        _ if is_x(ops[0]) && ops.len() == 2 && is_x(ops[1]) => {
            st.kind = Kind::Int2; // mv
            st.rd = reg(ops[0])?;
            st.rs1 = reg(ops[1])?;
        }
        _ if is_x(ops[0]) && ops.len() == 3 && is_x(ops[1]) && is_x(ops[2]) => {
            st.kind = Kind::Int3;
            st.rd = reg(ops[0])?;
            st.rs1 = reg(ops[1])?;
            st.rs2 = reg(ops[2])?;
        }
        _ if is_x(ops[0]) && ops.len() == 2 && !is_x(ops[1]) && !is_f(ops[1]) => {
            st.kind = Kind::IntImm2; // li/lui/auipc
            st.rd = reg(ops[0])?;
            st.imm = imm(ops[1])?;
        }
        _ if is_x(ops[0]) && ops.len() == 3 && is_x(ops[1]) && !is_x(ops[2]) && !is_f(ops[2]) => {
            st.kind = Kind::IntImm3; // addi xD,xS,imm / shifts / branches
            st.rd = reg(ops[0])?;
            st.rs1 = reg(ops[1])?;
            st.imm = imm(ops[2])?;
        }
        _ if is_x(ops[0]) && ops.len() == 2 && is_f(ops[1]) => {
            st.kind = Kind::Float2Int; // fmv.x.w / fcvt / fclass
            st.rd = reg(ops[0])?;
            st.rs1 = reg(ops[1])?;
        }
        _ if is_x(ops[0]) && ops.len() == 3 && is_f(ops[1]) && is_f(ops[2]) => {
            st.kind = Kind::FCompare; // feq.s xD,fS1,fS2
            st.rd = reg(ops[0])?;
            st.rs1 = reg(ops[1])?;
            st.rs2 = reg(ops[2])?;
        }
        _ if is_f(ops[0]) && ops.len() == 2 && is_x(ops[1]) => {
            st.kind = Kind::Int2Float; // fmv.w.x / fcvt
            st.rd = reg(ops[0])?;
            st.rs1 = reg(ops[1])?;
        }
        _ if is_f(ops[0]) && ops.len() == 2 && is_f(ops[1]) => {
            st.kind = Kind::Float2; // fsqrt / fcvt.s.d
            st.rd = reg(ops[0])?;
            st.rs1 = reg(ops[1])?;
        }
        _ if is_f(ops[0]) && ops.len() == 3 && is_f(ops[1]) && is_f(ops[2]) => {
            st.kind = Kind::Float3;
            st.rd = reg(ops[0])?;
            st.rs1 = reg(ops[1])?;
            st.rs2 = reg(ops[2])?;
        }
        _ if is_f(ops[0]) && ops.len() == 4 => {
            st.kind = Kind::Float4; // fmadd.s fD,fS1,fS2,fS3
            st.rd = reg(ops[0])?;
            st.rs1 = reg(ops[1])?;
            st.rs2 = reg(ops[2])?;
            st.rs3 = reg(ops[3])?;
        }
        _ => return Err(GateError(format!("unrecognized statement {line:?}"))),
    }
    Ok(st)
}

/// One recorded output entry from pass 1 of [`gate`].
enum Out {
    /// A non-statement line (labels, `j _start`, directives, …), passed through.
    Other(String),
    /// An emitted statement whose text is final (may be several lines, e.g. a
    /// `li` fix-up followed by the memory instruction).
    Stmt(String),
    /// A branch statement; its operand is rewritten to a label in pass 2.
    Branch {
        mnemonic: String,
        rd: usize,
        rs1: usize,
        /// The raw immediate from the parse tree; selects the target label.
        imm: i64,
    },
}

/// The label name for the statement with index `j`. `.L` prefix: a gas local
/// label, so it never collides with `_start`/`_end`/`fNconst` and stays out of
/// the symbol table.
fn label(j: usize) -> String {
    format!(".Lbr{j}")
}

/// Runs the ISA gate on a candidate program and returns the fixed program.
///
/// Two passes: pass 1 replays every line on the machine (fixing memory operands,
/// `li` insertions and immediate widths) and records what to emit; pass 2 emits
/// a `.Lbr{j}:` label before *every* statement and rewrites each branch operand
/// to `.Lbr{imm mod n}` (`imm` = the branch immediate from the parse tree,
/// `n` = statement count). The fuzz input therefore picks the branch target,
/// forward or backward. Labels are resolved by gas against the real layout, so
/// targets always land on an instruction boundary — with compressed (RVC)
/// instructions the 4-byte PC model would not match the assembled program, and
/// an absolute target could land mid-instruction and trap.
pub fn gate(text: &str) -> Result<String, GateError> {
    // Pass 1: classify and replay every line on the machine, recording the output.
    let mut machine = RiscV64Machine::new();
    let mut entries = Vec::new();
    let mut statements = 0usize;
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let st = classify(line)?;
        if st.kind == Kind::Other {
            entries.push(Out::Other(line.to_string()));
            continue;
        }
        statements += 1;
        if statements > MAX_STATEMENTS {
            return Err(GateError(format!("more than {MAX_STATEMENTS} statements")));
        }
        let m = &st.mnemonic;
        let entry = match st.kind {
            Kind::Other => unreachable!("handled above"),
            Kind::IntImm2 => {
                let emitted = machine
                    .parse_int_imm2(m, st.rd, st.imm)
                    .map_err(|e| GateError(format!("{line}: {e}")))?;
                Out::Stmt(format!("{m} x{},{emitted}\n", st.rd))
            }
            Kind::Int2 => {
                machine
                    .parse_int2(m, st.rd, st.rs1)
                    .map_err(|e| GateError(format!("{line}: {e}")))?;
                Out::Stmt(format!("{}\n", st.line))
            }
            Kind::Int3 => {
                machine
                    .parse_int3(m, st.rd, st.rs1, st.rs2)
                    .map_err(|e| GateError(format!("{line}: {e}")))?;
                Out::Stmt(format!("{}\n", st.line))
            }
            Kind::IntImm3 if RiscV64Machine::is_branch(m) => Out::Branch {
                mnemonic: m.clone(),
                rd: st.rd,
                rs1: st.rs1,
                imm: st.imm,
            },
            Kind::IntImm3 => {
                let emitted = machine
                    .parse_int_imm3(m, st.rd, st.rs1, st.imm)
                    .map_err(|e| GateError(format!("{line}: {e}")))?;
                Out::Stmt(format!("{m} x{},x{},{emitted}\n", st.rd, st.rs1))
            }
            Kind::Float2Int => {
                machine
                    .parse_float2int(m, st.rd, st.rs1)
                    .map_err(|e| GateError(format!("{line}: {e}")))?;
                Out::Stmt(format!("{}\n", st.line))
            }
            Kind::Int2Float => {
                machine
                    .parse_int2float(m, st.rd, st.rs1)
                    .map_err(|e| GateError(format!("{line}: {e}")))?;
                Out::Stmt(format!("{}\n", st.line))
            }
            Kind::FCompare => {
                machine
                    .parse_fcompare(m, st.rd, st.rs1, st.rs2)
                    .map_err(|e| GateError(format!("{line}: {e}")))?;
                Out::Stmt(format!("{}\n", st.line))
            }
            Kind::Float2 => {
                machine
                    .parse_float2(m, st.rd, st.rs1)
                    .map_err(|e| GateError(format!("{line}: {e}")))?;
                Out::Stmt(format!("{}\n", st.line))
            }
            Kind::Float3 => {
                machine
                    .parse_float3(m, st.rd, st.rs1, st.rs2)
                    .map_err(|e| GateError(format!("{line}: {e}")))?;
                Out::Stmt(format!("{}\n", st.line))
            }
            Kind::Float4 => {
                machine
                    .parse_float4(m, st.rd, st.rs1, st.rs2, st.rs3)
                    .map_err(|e| GateError(format!("{line}: {e}")))?;
                Out::Stmt(format!("{}\n", st.line))
            }
            Kind::IntMem | Kind::FloatMem => {
                let (off, needs_li) = if st.kind == Kind::IntMem {
                    machine
                        .parse_int_mem(m, st.rd, st.rs1, st.imm)
                        .map_err(|e| GateError(format!("{line}: {e}")))?
                } else {
                    machine
                        .parse_float_mem(m, st.rd, st.rs1, st.imm)
                        .map_err(|e| GateError(format!("{line}: {e}")))?
                };
                let mut rs1 = st.rs1;
                let mut text = String::new();
                if needs_li {
                    // x0 can never hold a valid address; use x10 instead.
                    if rs1 == 0 {
                        rs1 = 10;
                    }
                    text.push_str(&format!("li x{rs1},{MIDDLE_VALID_MEM}\n"));
                }
                if st.kind == Kind::IntMem {
                    text.push_str(&format!("{m} x{},{off}(x{rs1})\n", st.rd));
                } else {
                    text.push_str(&format!("{m} f{},{off}(x{rs1})\n", st.rd));
                }
                Out::Stmt(text)
            }
        };
        entries.push(entry);
    }

    // Pass 2: emit, placing a label before every statement and rewriting branch
    // operands to the label the input immediate selects (imm mod n).
    let n = statements as i64;
    let mut out = String::with_capacity(text.len() + statements * 8);
    let mut j = 0usize;
    for entry in entries {
        match entry {
            Out::Other(line) => {
                out.push_str(&line);
                out.push('\n');
            }
            Out::Stmt(text) => {
                out.push_str(&label(j));
                out.push_str(":\n");
                out.push_str(&text);
                j += 1;
            }
            Out::Branch {
                mnemonic,
                rd,
                rs1,
                imm,
            } => {
                out.push_str(&label(j));
                out.push_str(":\n");
                let target = imm.rem_euclid(n) as usize;
                out.push_str(&format!("{mnemonic} x{rd},x{rs1},{}\n", label(target)));
                j += 1;
            }
        }
    }
    if out.is_empty() {
        return Err(GateError("gate produced no output".to_string()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The tests below mirror atnwalk/cmd/isagate/main_test.go one-to-one.

    #[test]
    fn passthrough_statements_are_unchanged() {
        // Statement lines pass through unchanged; every statement gets a label.
        let input = "li x1,5\nadd x1,x1,x2\nmv x3,x1\nfadd.s f1,f2,f3\n";
        let expected = ".Lbr0:\nli x1,5\n.Lbr1:\nadd x1,x1,x2\n.Lbr2:\nmv x3,x1\n.Lbr3:\nfadd.s f1,f2,f3\n";
        assert_eq!(gate(input).unwrap(), expected);
    }

    #[test]
    fn li_immediate_round_trips() {
        // 0x80002000 = LOWER_VALID_MEM
        let input = "li x1,2147491840\n";
        assert_eq!(gate(input).unwrap(), ".Lbr0:\nli x1,2147491840\n");
    }

    #[test]
    fn memory_with_zero_base_gets_li_inserted() {
        // x0 can never hold a valid address: the gate must insert li x10, MIDDLE_VALID_MEM.
        let out = gate("lw x2,0(x0)\n").unwrap();
        assert!(
            out.contains(&format!("li x10,{MIDDLE_VALID_MEM}\n")),
            "missing li insertion: {out:?}"
        );
        assert!(out.contains("lw x2,0(x10)\n"), "mem line not rewritten: {out:?}");
    }

    #[test]
    fn memory_offset_is_clamped_into_valid_range() {
        // x1 = 0x80002000 (LOWER_VALID_MEM); -100 would underflow, so the offset is fixed to 0.
        let out = gate("li x1,2147491840\nlw x2,-100(x1)\n").unwrap();
        assert!(out.contains("lw x2,0(x1)\n"), "offset not clamped: {out:?}");
    }

    #[test]
    fn missing_memory_offset_means_zero() {
        // RISC-V allows "lw x2,(x1)"; the missing offset is 0. x1 starts at 0,
        // which cannot hold a valid address, so the li fix-up kicks in too.
        let out = gate("lw x2,(x1)\n").unwrap();
        assert_eq!(
            out,
            format!(".Lbr0:\nli x1,{MIDDLE_VALID_MEM}\nlw x2,0(x1)\n")
        );
    }

    #[test]
    fn branch_target_is_a_forward_label_from_the_input_immediate() {
        // n = 3 statements; the branch (statement 0) has imm 4, target 4 mod 3 = 1.
        let out = gate("beq x1,x2,4\naddi x1,x2,1\naddi x3,x4,2\n").unwrap();
        assert_eq!(
            out,
            ".Lbr0:\nbeq x1,x2,.Lbr1\n.Lbr1:\naddi x1,x2,1\n.Lbr2:\naddi x3,x4,2\n"
        );
    }

    #[test]
    fn branch_target_can_be_backward() {
        // n = 3; the branch (statement 2) has imm -3, target -3 mod 3 = 0.
        let out = gate("addi x1,x2,1\naddi x3,x4,2\nbne x1,x2,-3\n").unwrap();
        assert_eq!(
            out,
            ".Lbr0:\naddi x1,x2,1\n.Lbr1:\naddi x3,x4,2\n.Lbr2:\nbne x1,x2,.Lbr0\n"
        );
    }

    #[test]
    fn single_statement_branch_targets_itself() {
        // n = 1: any immediate maps to the only statement, a self-loop.
        let out = gate("beq x1,x2,7\n").unwrap();
        assert_eq!(out, ".Lbr0:\nbeq x1,x2,.Lbr0\n");
    }

    #[test]
    fn every_statement_gets_exactly_one_label() {
        let out = gate("beq x1,x2,0\nbne x3,x4,1\n").unwrap();
        assert_eq!(out, ".Lbr0:\nbeq x1,x2,.Lbr0\n.Lbr1:\nbne x3,x4,.Lbr1\n");
    }

    #[test]
    fn itype_immediate_is_clamped_to_12_bits() {
        // addi draws a signed 12-bit immediate: 5000 wraps to 904.
        let out = gate("addi x1,x2,5000\n").unwrap();
        assert!(out.contains("addi x1,x2,904\n"), "not clamped: {out:?}");
    }

    #[test]
    fn leading_zero_immediates_are_normalized() {
        // Nautilus can generate leading zeros (e.g. -018); they are decimal.
        // li's high half 0xFFFFFFFF is unrepresentable (Decode returns
        // raw % (2^32-1)), so -18 normalizes to its zero-extended form.
        let out = gate("li x11,-018\naddi x1,x2,007\n").unwrap();
        assert!(out.contains("li x11,4294967278\n"), "li: {out:?}");
        assert!(out.contains("addi x1,x2,7\n"), "addi: {out:?}");
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(gate("this is not assembly\n").is_err());
    }

    #[test]
    fn full_program_shape() {
        let input = "_start:\nli x1,42\nlw x2,0(x0)\naddi x3,x1,-7\nj _start\n_end:\n.section \".rodata\"\n.align 8\nf0const: .quad 0xdeadbeef\n";
        let out = gate(input).unwrap();
        for want in [
            "_start:\n",
            "li x1,42\n",
            &format!("li x10,{MIDDLE_VALID_MEM}\n"),
            "lw x2,0(x10)\n",
            "addi x3,x1,-7\n",
            "j _start\n",
            "f0const: .quad 0xdeadbeef\n",
        ] {
            assert!(out.contains(want), "missing {want:?} in {out:?}");
        }
    }

    #[test]
    fn empty_program_is_rejected() {
        assert!(gate("").is_err());
        assert!(gate("\n").is_err());
    }

    #[test]
    fn unknown_mnemonic_is_rejected() {
        assert!(gate("frobnicate x1,x2\n").is_err());
    }
}
