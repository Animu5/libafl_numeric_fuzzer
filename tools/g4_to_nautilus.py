#!/usr/bin/env python3
"""Convert the ANTLR4 grammar RISCV64G.g4 into a Nautilus JSON grammar.

Nautilus (libafl's grammar fuzzer engine) does not read ANTLR grammars; it
reads a JSON list of rules ``[[NAME, "rhs"], ...]`` where ``{NAME}`` in a rhs
is a nonterminal reference and everything else is literal text (literal
``{``, ``}`` and ``\\`` must be escaped with a backslash). The first rule is
the start rule; Nautilus wraps it in an implicit ``START`` rule.

This script understands the EBNF subset used by ``grammars/RISCV64G.g4``:

- ``grammar NAME;`` header, ``//`` comments
- parser rules ``name: alt | alt | ... ;`` and lexer rules ``TOKEN: ... ;``
  (lexer rules are emitted as ordinary nonterminals named after the token).
  Nautilus requires nonterminal names to start with a capital letter, so the
  first letter of every rule name is capitalized (`start` -> `Start`, which
  does not collide with the implicit `START` rule Nautilus adds).
- ``fragment`` rules (expanded the same way, referenced by name)
- string literals ``'...'`` with ANTLR escapes (``\\n``, ``\\t``, ``\\'``,
  ``\\"``, ``\\\\``), character sets ``[a-z0-9]`` (expanded to one-char
  alternatives), rule/token references, and groups ``(...)`` with the
  suffixes ``?``, ``*``, ``+`` (converted to synthetic optional/star/plus
  nonterminals; ``*``/``+`` become right-recursive rules, which Nautilus
  bounds via its tree depth)

Special mapping for the ``run_*`` placeholder rules: in the atnwalk pipeline
these rules are semantic hooks executed by the Go ISA emulator (they match a
literal ``_`` and produce immediates/newlines through actions). For pure
text generation they are replaced by:

- ``run_int_imm2`` / ``run_int_imm3`` -> ``{INT_IMM}\\n`` (a generated immediate)
- ``run_random_float``               -> ``{HEX64}\\n``  (generated .quad bits)
- all other ``run_*``                -> empty (the statement text is complete)

Usage:
    python3 tools/g4_to_nautilus.py grammars/RISCV64G.g4 grammars/riscv64g.json

The generated JSON is committed, so the fuzzer never needs this script at
runtime; re-run it only when the vendored .g4 changes.
"""

import json
import re
import sys

# ---------------------------------------------------------------------------
# Tokenizer for the .g4 subset
# ---------------------------------------------------------------------------

# Escape sequences allowed inside ANTLR string literals and char sets.
_ESCAPES = {
    "n": "\n",
    "t": "\t",
    "r": "\r",
    "\\": "\\",
    "'": "'",
    '"': '"',
    "]": "]",
    "-": "-",
}


class Token:
    """One lexical token of the .g4 file."""

    def __init__(self, kind, value):
        self.kind = kind  # 'lit', 'set', 'ident', or a punctuation string
        self.value = value

    def __repr__(self):
        return f"Token({self.kind!r}, {self.value!r})"


def tokenize(text):
    """Split .g4 source into tokens, stripping // comments."""
    tokens = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c in " \t\r\n":
            i += 1
        elif text.startswith("//", i):
            end = text.find("\n", i)
            i = n if end == -1 else end + 1
        elif c == "'":
            # String literal with escapes.
            j = i + 1
            out = []
            while text[j] != "'":
                if text[j] == "\\":
                    out.append(_ESCAPES[text[j + 1]])
                    j += 2
                else:
                    out.append(text[j])
                    j += 1
            tokens.append(Token("lit", "".join(out)))
            i = j + 1
        elif c == "[":
            # Character set: expand escapes and ranges into a set of chars.
            j = i + 1
            chars = set()
            while text[j] != "]":
                if text[j] == "\\":
                    chars.add(_ESCAPES[text[j + 1]])
                    j += 2
                elif j + 2 < n and text[j + 1] == "-" and text[j + 2] != "]":
                    lo, hi = text[j], text[j + 2]
                    chars.update(chr(k) for k in range(ord(lo), ord(hi) + 1))
                    j += 3
                else:
                    chars.add(text[j])
                    j += 1
            # ANTLR quirk: the source writes NEWLINE as [\\n], which naively
            # parses as backslash + 'n'; the intent is a newline character.
            if chars == {"\\", "n"}:
                chars = {"\n"}
            tokens.append(Token("set", chars))
            i = j + 1
        elif c in "()|;:*+?":
            tokens.append(Token(c, c))
            i += 1
        elif c == ":":
            tokens.append(Token(":", c))
            i += 1
        elif re.match(r"[A-Za-z_]", c):
            j = i
            while j < n and re.match(r"[A-Za-z0-9_]", text[j]):
                j += 1
            tokens.append(Token("ident", text[i:j]))
            i = j
        else:
            raise SyntaxError(f"unexpected character {c!r} at offset {i}")
    return tokens


# ---------------------------------------------------------------------------
# Parser for the .g4 subset: rules -> alternatives -> sequences -> elements
# ---------------------------------------------------------------------------


class Group:
    """A parenthesized ``( ... )`` group with an optional EBNF suffix."""

    def __init__(self, alternatives, suffix):
        self.alternatives = alternatives  # list of sequences
        self.suffix = suffix  # None, '?', '*', '+'


class Rule:
    def __init__(self, name, alternatives, is_lexer, is_fragment):
        self.name = name
        self.alternatives = alternatives  # list of sequences (elements)
        self.is_lexer = is_lexer
        self.is_fragment = is_fragment


def parse(tokens):
    """Parse the token stream into a list of Rule objects."""
    pos = 0

    def peek():
        return tokens[pos] if pos < len(tokens) else None

    def expect(kind):
        nonlocal pos
        tok = tokens[pos]
        if tok.kind != kind:
            raise SyntaxError(f"expected {kind!r}, got {tok!r}")
        pos += 1
        return tok

    # Header: `grammar NAME ;`
    expect("ident")  # 'grammar'
    expect("ident")  # grammar name
    expect(";")

    rules = []
    while peek() is not None:
        is_fragment = False
        if peek().kind == "ident" and peek().value == "fragment":
            pos += 1
            is_fragment = True
        name = expect("ident").value
        expect(":")
        alternatives = []
        while True:
            seq = []
            while peek() is not None and peek().kind not in ("|", ";"):
                tok = peek()
                if tok.kind == "lit":
                    seq.append(("lit", tok.value))
                    pos += 1
                elif tok.kind == "set":
                    seq.append(("set", tok.value))
                    pos += 1
                elif tok.kind == "ident":
                    seq.append(("ref", tok.value))
                    pos += 1
                elif tok.kind == "(":
                    pos += 1
                    group_alts = []
                    while True:
                        gseq = []
                        while peek() is not None and peek().kind not in ("|", ")"):
                            gtok = peek()
                            if gtok.kind == "lit":
                                gseq.append(("lit", gtok.value))
                                pos += 1
                            elif gtok.kind == "set":
                                gseq.append(("set", gtok.value))
                                pos += 1
                            elif gtok.kind == "ident":
                                gseq.append(("ref", gtok.value))
                                pos += 1
                            elif gtok.kind == "(":
                                raise SyntaxError("nested groups are not supported")
                            else:
                                raise SyntaxError(f"unexpected {gtok!r} in group")
                        group_alts.append(gseq)
                        if peek().kind == "|":
                            pos += 1
                            continue
                        break
                    expect(")")
                    suffix = None
                    if peek() is not None and peek().kind in ("?", "*", "+"):
                        suffix = peek().kind
                        pos += 1
                    seq.append(("group", Group(group_alts, suffix)))
                else:
                    raise SyntaxError(f"unexpected {tok!r} in rule body")
            alternatives.append(seq)
            if peek().kind == "|":
                pos += 1
                continue
            break
        expect(";")
        rules.append(Rule(name, alternatives, name[:1].isupper(), is_fragment))
    return rules


# ---------------------------------------------------------------------------
# Conversion to Nautilus rules
# ---------------------------------------------------------------------------


def nt(name):
    """Nautilus nonterminal names must start with a capital letter and may only
    contain [a-zA-Z_0-9-]; capitalize the first letter of each .g4 rule name.
    `start` becomes `Start`, which does not collide with the implicit `START`
    rule Nautilus adds around the first rule."""
    return name[0].upper() + name[1:]


def nautilus_escape(text):
    """Escape literal text for a Nautilus rhs ({, } and backslash)."""
    return text.replace("\\", "\\\\").replace("{", "\\{").replace("}", "\\}")


# Replacements for the run_* emulator-hook rules (see module docstring).
RUN_RULE_REPLACEMENTS = {
    "run_int_imm2": [[("ref", "INT_IMM"), ("lit", "\n")]],
    "run_int_imm3": [[("ref", "INT_IMM"), ("lit", "\n")]],
    "run_random_float": [[("ref", "HEX64"), ("lit", "\n")]],
}


class Converter:
    def __init__(self):
        self.rules = []  # list of [name, rhs]
        self.queue = []  # synthetic rules generated while converting a rule
        self.counter = 0
        self.trivial = set()  # names of single-literal rules (inlined at use sites)
        self.all_rules = []  # the parsed .g4 rules, for literal lookup

    def synthetic(self, hint):
        """A fresh nonterminal name derived from the enclosing rule."""
        self.counter += 1
        return f"{hint}__g{self.counter}"

    def emit(self, name, rhs):
        self.rules.append([name, rhs])

    def emit_queued(self, name, rhs):
        """Emit a rule into the synthetic queue (flushed after the parent)."""
        self.queue.append([name, rhs])

    def emit_alts(self, name, alternatives):
        """Emit one Nautilus rule per alternative of a .g4 rule.

        The parent rule is emitted before any synthetic rules created while
        converting it, so the first parser rule stays the first JSON rule
        (Nautilus treats rules[0] as the start rule).
        """
        for seq in alternatives:
            self.emit(name, self.conv_seq(seq, name))
        self.rules.extend(self.queue)
        self.queue.clear()

    def conv_seq(self, seq, hint):
        """Convert a sequence of elements into a single Nautilus rhs string."""
        parts = []
        for kind, value in seq:
            if kind == "lit":
                parts.append(nautilus_escape(value))
            elif kind == "set":
                parts.append(self.conv_set(value, hint))
            elif kind == "ref":
                if value in self.trivial:
                    # Inline single-literal rules (see convert()).
                    lit = next(
                        r.alternatives[0][0][1]
                        for r in self.all_rules
                        if r.name == value
                    )
                    parts.append(nautilus_escape(lit))
                else:
                    parts.append("{" + nt(value) + "}")
            elif kind == "group":
                parts.append(self.conv_group(value, hint))
        return "".join(parts)

    def conv_set(self, chars, hint):
        """A character set becomes a synthetic nonterminal with one-char rules."""
        if len(chars) == 1:
            return nautilus_escape(next(iter(chars)))
        name = self.synthetic(hint)
        for c in sorted(chars):
            self.emit_queued(name, nautilus_escape(c))
        return "{" + name + "}"

    def conv_group(self, group, hint):
        """Convert (...)+/?/* into synthetic nonterminals, return a {ref}."""
        # Group content as a single nonterminal.
        inner = self.synthetic(hint)
        for seq in group.alternatives:
            self.emit_queued(inner, self.conv_seq(seq, inner))
        if group.suffix is None:
            return "{" + inner + "}"
        opt = self.synthetic(hint)
        if group.suffix == "?":
            self.emit_queued(opt, "")
            self.emit_queued(opt, "{" + inner + "}")
        elif group.suffix == "*":
            self.emit_queued(opt, "")
            self.emit_queued(opt, "{" + inner + "}{" + opt + "}")
        elif group.suffix == "+":
            self.emit_queued(opt, "{" + inner + "}")
            self.emit_queued(opt, "{" + inner + "}{" + opt + "}")
        return "{" + opt + "}"

    def convert(self, rules):
        self.all_rules = rules
        # Pre-pass: rules with a single alternative that is a single literal
        # (WHITESPACE -> " ", NEWLINE -> "\n", X1 -> "x1", ...) are inlined at
        # their use sites instead of becoming a {REF}. This keeps the minimum
        # derivation size of the start rule small, which matters because the
        # Nautilus tree depth must exceed it.
        self.trivial = {
            rule.name
            for rule in rules
            # run_* hooks are replaced with real rules (see RUN_RULE_REPLACEMENTS),
            # so they must stay references even though their .g4 body is a literal.
            if not rule.name.startswith("run_")
            and len(rule.alternatives) == 1
            and len(rule.alternatives[0]) == 1
            and rule.alternatives[0][0][0] == "lit"
        }
        # Parser rules first, in file order, so that rules[0] is the start
        # rule (Nautilus treats the first rule as the start).
        for rule in rules:
            if rule.is_lexer or rule.is_fragment:
                continue
            if rule.name in RUN_RULE_REPLACEMENTS:
                self.emit_alts(nt(rule.name), RUN_RULE_REPLACEMENTS[rule.name])
            elif rule.name.startswith("run_"):
                self.emit_alts(nt(rule.name), [[]])  # empty: hook adds no text
            else:
                self.emit_alts(nt(rule.name), rule.alternatives)
        # Lexer tokens and fragments become ordinary nonterminals.
        for rule in rules:
            if not (rule.is_lexer or rule.is_fragment):
                continue
            self.emit_alts(nt(rule.name), rule.alternatives)
        self.add_helper_rules()
        return self.rules

    def add_helper_rules(self):
        """Helper nonterminals for the replaced run_* hooks."""
        # Hex digit, used by HEX64.
        self.emit_alts("HEXD", [[("lit", c)] for c in "0123456789abcdef"])
        # Hex literal for .quad (gas accepts fewer than 16 digits). Emitted as
        # 16 alternatives of increasing length so the minimum derivation stays
        # small: Nautilus's tree depth must exceed the start rule's minimum
        # derivation size, and a fixed 16-ref rule would inflate it by 16 per
        # float constant.
        for k in range(1, 17):
            self.emit("HEX64", "0x" + "{HEXD}" * k)
        # Signed decimal immediate of up to 6 digits, plus common edge values.
        # (Nautilus's tree depth must exceed the minimum derivation length of
        # the start rule, and long digit chains inflate it; 6 digits still
        # covers immediates up to 999999, and the edge literals cover the rest.)
        self.emit_alts("INT_SIGN", [[], [("lit", "-")]])
        mags = [[("ref", "DIGIT")] * k for k in range(1, 7)]
        self.emit_alts("INT_MAG", mags)
        edge = ["2047", "-2048", "4095", "-4096", "65535", "-65536",
                "2147483647", "-2147483648", "4294967295",
                "9223372036854775807", "-9223372036854775808"]
        self.emit_alts("INT_IMM", [[("ref", "INT_SIGN"), ("ref", "INT_MAG")]]
                       + [[("lit", v)] for v in edge])


def main():
    if len(sys.argv) != 3:
        sys.exit(f"usage: {sys.argv[0]} <input.g4> <output.json>")
    src, dst = sys.argv[1], sys.argv[2]
    with open(src, encoding="utf-8") as f:
        rules = parse(tokenize(f.read()))
    nautilus_rules = Converter().convert(rules)
    with open(dst, "w", encoding="utf-8") as f:
        json.dump(nautilus_rules, f, indent=1)
        f.write("\n")
    nts = {r[0] for r in nautilus_rules}
    # Sanity check: every {REF} points at a defined nonterminal.
    for name, rhs in nautilus_rules:
        for ref in re.findall(r"\{([^}\\]+)\}", rhs):
            if ref not in nts:
                sys.exit(f"error: rule {name!r} references undefined {{{ref}}}")
    print(f"wrote {len(nautilus_rules)} rules ({len(nts)} nonterminals) to {dst}")


if __name__ == "__main__":
    main()
