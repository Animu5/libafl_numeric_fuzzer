grammar RISCV64G;

// ==== Parser ========

start:
    init_float_registers
    init_x1
    init_x2
    init_x3
    init_x4
    init_x5
    init_x6
    init_x7
    init_x8
    init_x9
    init_x10
    init_x11
    init_x12
    init_x13
    init_x14
    init_x15
    init_x16
    init_x17
    init_x18
    init_x19
    init_x20
    init_x21
    init_x22
    init_x23
    init_x24
    init_x25
    init_x26
    init_x27
    init_x28
    init_x29
    init_x30
    init_x31
    start_label
    (statements)+
    end_loop
    end_label
    data_section
    init_f0_const
    init_f1_const
    init_f2_const
    init_f3_const
    init_f4_const
    init_f5_const
    init_f6_const
    init_f7_const
    init_f8_const
    init_f9_const
    init_f10_const
    init_f11_const
    init_f12_const
    init_f13_const
    init_f14_const
    init_f15_const
    init_f16_const
    init_f17_const
    init_f18_const
    init_f19_const
    init_f20_const
    init_f21_const
    init_f22_const
    init_f23_const
    init_f24_const
    init_f25_const
    init_f26_const
    init_f27_const
    init_f28_const
    init_f29_const
    init_f30_const
    init_f31_const;

start_label: '_start:' NEWLINE;
end_label: '_end:' NEWLINE;

init_x1 : LI_INSTR WHITESPACE X1 ',' run_int_imm2;
init_x2 : LI_INSTR WHITESPACE X2 ',' run_int_imm2;
init_x3 : LI_INSTR WHITESPACE X3 ',' run_int_imm2;
init_x4 : LI_INSTR WHITESPACE X4 ',' run_int_imm2;
init_x5 : LI_INSTR WHITESPACE X5 ',' run_int_imm2;
init_x6 : LI_INSTR WHITESPACE X6 ',' run_int_imm2;
init_x7 : LI_INSTR WHITESPACE X7 ',' run_int_imm2;
init_x8 : LI_INSTR WHITESPACE X8 ',' run_int_imm2;
init_x9 : LI_INSTR WHITESPACE X9 ',' run_int_imm2;
init_x10 : LI_INSTR WHITESPACE X10 ',' run_int_imm2;
init_x11 : LI_INSTR WHITESPACE X11 ',' run_int_imm2;
init_x12 : LI_INSTR WHITESPACE X12 ',' run_int_imm2;
init_x13 : LI_INSTR WHITESPACE X13 ',' run_int_imm2;
init_x14 : LI_INSTR WHITESPACE X14 ',' run_int_imm2;
init_x15 : LI_INSTR WHITESPACE X15 ',' run_int_imm2;
init_x16 : LI_INSTR WHITESPACE X16 ',' run_int_imm2;
init_x17 : LI_INSTR WHITESPACE X17 ',' run_int_imm2;
init_x18 : LI_INSTR WHITESPACE X18 ',' run_int_imm2;
init_x19 : LI_INSTR WHITESPACE X19 ',' run_int_imm2;
init_x20 : LI_INSTR WHITESPACE X20 ',' run_int_imm2;
init_x21 : LI_INSTR WHITESPACE X21 ',' run_int_imm2;
init_x22 : LI_INSTR WHITESPACE X22 ',' run_int_imm2;
init_x23 : LI_INSTR WHITESPACE X23 ',' run_int_imm2;
init_x24 : LI_INSTR WHITESPACE X24 ',' run_int_imm2;
init_x25 : LI_INSTR WHITESPACE X25 ',' run_int_imm2;
init_x26 : LI_INSTR WHITESPACE X26 ',' run_int_imm2;
init_x27 : LI_INSTR WHITESPACE X27 ',' run_int_imm2;
init_x28 : LI_INSTR WHITESPACE X28 ',' run_int_imm2;
init_x29 : LI_INSTR WHITESPACE X29 ',' run_int_imm2;
init_x30 : LI_INSTR WHITESPACE X30 ',' run_int_imm2;
init_x31 : LI_INSTR WHITESPACE X31 ',' run_int_imm2;

init_float_registers:
    FLD_INSTR ' f0,f0const,gp' NEWLINE
    FLD_INSTR ' f1,f1const,gp' NEWLINE
    FLD_INSTR ' f2,f2const,gp' NEWLINE
    FLD_INSTR ' f3,f3const,gp' NEWLINE
    FLD_INSTR ' f4,f4const,gp' NEWLINE
    FLD_INSTR ' f5,f5const,gp' NEWLINE
    FLD_INSTR ' f6,f6const,gp' NEWLINE
    FLD_INSTR ' f7,f7const,gp' NEWLINE
    FLD_INSTR ' f8,f8const,gp' NEWLINE
    FLD_INSTR ' f9,f9const,gp' NEWLINE
    FLD_INSTR ' f10,f10const,gp' NEWLINE
    FLD_INSTR ' f11,f11const,gp' NEWLINE
    FLD_INSTR ' f12,f12const,gp' NEWLINE
    FLD_INSTR ' f13,f13const,gp' NEWLINE
    FLD_INSTR ' f14,f14const,gp' NEWLINE
    FLD_INSTR ' f15,f15const,gp' NEWLINE
    FLD_INSTR ' f16,f16const,gp' NEWLINE
    FLD_INSTR ' f17,f17const,gp' NEWLINE
    FLD_INSTR ' f18,f18const,gp' NEWLINE
    FLD_INSTR ' f19,f19const,gp' NEWLINE
    FLD_INSTR ' f20,f20const,gp' NEWLINE
    FLD_INSTR ' f21,f21const,gp' NEWLINE
    FLD_INSTR ' f22,f22const,gp' NEWLINE
    FLD_INSTR ' f23,f23const,gp' NEWLINE
    FLD_INSTR ' f24,f24const,gp' NEWLINE
    FLD_INSTR ' f25,f25const,gp' NEWLINE
    FLD_INSTR ' f26,f26const,gp' NEWLINE
    FLD_INSTR ' f27,f27const,gp' NEWLINE
    FLD_INSTR ' f28,f28const,gp' NEWLINE
    FLD_INSTR ' f29,f29const,gp' NEWLINE
    FLD_INSTR ' f30,f30const,gp' NEWLINE
    FLD_INSTR ' f31,f31const,gp' NEWLINE;

statements:
    (int2_stmt run_int2
    | int_imm2_stmt
    | int3_stmt run_int3
    | int_imm3_stmt
    | float2int_stmt run_float2int
    | int2float_stmt run_int2float
    | fcompare_stmt run_fcompare
    | float2_stmt run_float2
    | float3_stmt run_float3
    | float4_stmt run_float4
    | int_mem_stmt run_int_mem
    | float_mem_stmt run_float_mem
    );

end_loop: 'j _start' NEWLINE;

data_section:
    '.section ".rodata"' NEWLINE
    '.align 8' NEWLINE;

init_f0_const:  'f0const: .quad ' run_random_float;
init_f1_const:  'f1const: .quad ' run_random_float;
init_f2_const:  'f2const: .quad ' run_random_float;
init_f3_const:  'f3const: .quad ' run_random_float;
init_f4_const:  'f4const: .quad ' run_random_float;
init_f5_const:  'f5const: .quad ' run_random_float;
init_f6_const:  'f6const: .quad ' run_random_float;
init_f7_const:  'f7const: .quad ' run_random_float;
init_f8_const:  'f8const: .quad ' run_random_float;
init_f9_const:  'f9const: .quad ' run_random_float;
init_f10_const: 'f10const: .quad ' run_random_float;
init_f11_const: 'f11const: .quad ' run_random_float;
init_f12_const: 'f12const: .quad ' run_random_float;
init_f13_const: 'f13const: .quad ' run_random_float;
init_f14_const: 'f14const: .quad ' run_random_float;
init_f15_const: 'f15const: .quad ' run_random_float;
init_f16_const: 'f16const: .quad ' run_random_float;
init_f17_const: 'f17const: .quad ' run_random_float;
init_f18_const: 'f18const: .quad ' run_random_float;
init_f19_const: 'f19const: .quad ' run_random_float;
init_f20_const: 'f20const: .quad ' run_random_float;
init_f21_const: 'f21const: .quad ' run_random_float;
init_f22_const: 'f22const: .quad ' run_random_float;
init_f23_const: 'f23const: .quad ' run_random_float;
init_f24_const: 'f24const: .quad ' run_random_float;
init_f25_const: 'f25const: .quad ' run_random_float;
init_f26_const: 'f26const: .quad ' run_random_float;
init_f27_const: 'f27const: .quad ' run_random_float;
init_f28_const: 'f28const: .quad ' run_random_float;
init_f29_const: 'f29const: .quad ' run_random_float;
init_f30_const: 'f30const: .quad ' run_random_float;
init_f31_const: 'f31const: .quad ' run_random_float;

int_mem_stmt:
    // RISC-V allows a missing memory offset (it then means 0); emit it
    // explicitly so the statement text is complete without the emulator hook.
    INT_MEM_INSTR WHITESPACE int_Reg ',' '0' '(' int_Reg ')' NEWLINE;

float_mem_stmt:
    // Same as above: missing offset means 0.
    FLOAT_MEM_INSTR WHITESPACE FLOAT_REG ',' '0' '(' int_Reg ')' NEWLINE;

int2_stmt:
    INT2_INSTR WHITESPACE int_Reg ',' int_Reg NEWLINE;

int_imm2_stmt:
    INT_IMM2_INSTR WHITESPACE int_Reg ',' run_int_imm2; // run_int_imm2 adds a newline itself.

int3_stmt:
    INT3_INSTR WHITESPACE int_Reg ',' int_Reg ',' int_Reg NEWLINE;

int_imm3_stmt:
    INT_IMM3_INSTR WHITESPACE int_Reg ',' int_Reg ',' run_int_imm3; // run_int_imm3 adds a newline itself.

float2int_stmt:
    FLOAT2INT_INSTR WHITESPACE int_Reg ',' FLOAT_REG NEWLINE;

int2float_stmt:
    INT2FLOAT_INSTR WHITESPACE FLOAT_REG ',' int_Reg NEWLINE;

fcompare_stmt:
    FCOMPARE_INSTR WHITESPACE int_Reg ',' FLOAT_REG ',' FLOAT_REG NEWLINE;

float2_stmt:
    FLOAT2_INSTR WHITESPACE FLOAT_REG ',' FLOAT_REG NEWLINE;

float3_stmt:
    FLOAT3_INSTR WHITESPACE FLOAT_REG ',' FLOAT_REG ',' FLOAT_REG NEWLINE;

float4_stmt:
    FLOAT4_INSTR WHITESPACE FLOAT_REG ',' FLOAT_REG ',' FLOAT_REG ',' FLOAT_REG NEWLINE;

int_Reg: 'x0' | X1 | X2 | X3 | X4 | X5 | X6 | X7 | X8 | X9 | X10 | X11 | X12 | X13 | X14 | X15 | X16 | X17 | X18 | X19 | X20
        | X21 | X22 | X23 | X24 | X25 | X26 | X27 | X28 | X29 | X30 | X31;

// ==== Lexer ===========
INT2_INSTR: 'mv';

LI_INSTR: 'li';
INT_IMM2_INSTR: LI_INSTR | 'lui' | 'auipc';

INT3_INSTR:
    'sll' ('w')?
    | 'srl' ('w')?
    | 'sra' ('w')?
    | 'add' ('w')?
    | 'sub' ('w')?
    | 'xor'
    | 'or'
    | 'and'
    | 'slt' ('u')?
    | 'mul' ('w')?
    | 'mulh'
    | 'mulhsu'
    | 'mulhu'
    | 'div' ('w')?
    | 'divu' ('w')?
    | 'rem' ('w')?
    | 'remu' ('w')?
    ;

INT_IMM3_INSTR:
    'slli' ('w')?
    | 'srli' ('w')?
    | 'srai' ('w')?
    | 'addi' ('w')?
    | 'xori'
    | 'ori'
    | 'andi'
    | 'slti' ('u')?
    | 'beq'
    | 'bne'
    | 'blt' ('u')?
    | 'bge'
    | 'bge' ('u')?
    ;

FLOAT2INT_INSTR:
    'fmv.x.' ('w'|'d')
    | 'fcvt.' ('w' | 'l') ('u')? '.' FLOAT_FORMAT
    | 'fclass.' FLOAT_FORMAT
    ;

INT2FLOAT_INSTR:
    'fmv.' ('w'|'d') '.x'
    | 'fcvt.' FLOAT_FORMAT '.' ('w' | 'l') ('u')?
    ;

FCOMPARE_INSTR:
    'feq.' FLOAT_FORMAT
    | 'flt.' FLOAT_FORMAT
    | 'fle.' FLOAT_FORMAT
    ;

FLOAT2_INSTR:
    'fsqrt.' FLOAT_FORMAT
    | 'fcvt.s.d'
    | 'fcvt.d.s'
    ;

FLOAT3_INSTR:
    'f' ('add' | 'sub' | 'mul' | 'div' | 'sgnj' | 'sgnjn' | 'sgnjx' | 'min' | 'max' ) '.' FLOAT_FORMAT;

FLOAT4_INSTR:
    'f' ('madd' | 'msub' | 'nmadd' | 'nmsub') '.' FLOAT_FORMAT;

INT_MEM_INSTR:
    'lb' ('u')?
    | 'lh' ('u')?
    | 'lw' ('u')?
    | 'ld'
    | 'sb'
    | 'sh'
    | 'sw'
    | 'sd'
    ;

FLD_INSTR: 'fld';
FLOAT_MEM_INSTR: 'flw' | FLD_INSTR | 'fsw' | 'fsd';

X1 : 'x1';
X2 : 'x2';
X3 : 'x3';
X4 : 'x4';
X5 : 'x5';
X6 : 'x6';
X7 : 'x7';
X8 : 'x8';
X9 : 'x9';
X10 : 'x10';
X11 : 'x11';
X12 : 'x12';
X13 : 'x13';
X14 : 'x14';
X15 : 'x15';
X16 : 'x16';
X17 : 'x17';
X18 : 'x18';
X19 : 'x19';
X20 : 'x20';
X21 : 'x21';
X22 : 'x22';
X23 : 'x23';
X24 : 'x24';
X25 : 'x25';
X26 : 'x26';
X27 : 'x27';
X28 : 'x28';
X29 : 'x29';
X30 : 'x30';
X31 : 'x31';
FLOAT_REG: FLOAT_NUM_REGISTER;

WHITESPACE: [ ];
NEWLINE: [\n];

fragment FLOAT_FORMAT: ('s' | 'd');
fragment EXTENDED_FLOAT_FORMAT: ('h' | 's' | 'd');
fragment FLOAT_NUM_REGISTER: 'f' DIGIT | 'f1' DIGIT | 'f2' DIGIT | 'f3'[01];
fragment DIGIT: [0-9];

// Function placeholders
run_int2: '_';
run_int_imm2: '_';
run_int3: '_';
run_int_imm3: '_';
run_float2int: '_';
run_int2float: '_';
run_fcompare: '_';
run_float2: '_';
run_float3: '_';
run_float4: '_';
run_int_mem: '_';
run_float_mem: '_';
run_random_float: '_';
