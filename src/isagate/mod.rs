//! The ISA gate: an in-process port of the ISA validator/fixer from the
//! asltgen-afl-proxy project (see `atnwalk/grammars/emulator` and
//! `atnwalk/cmd/isagate` there). Every grammar-generated program is replayed on
//! the machine in [`machine`] before it is built; the gate fixes what would
//! crash (memory addresses, alignment, immediate widths) and rejects what it
//! cannot fix. See [`gate`] for the details.

mod gate;
mod machine;

pub use gate::gate;
