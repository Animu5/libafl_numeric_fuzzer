//! Single-pass waveform engine (vendored from `vcdtogglemeasure`): wellen parses
//! the header and hierarchy, the [`super::fastvcd`] scanner walks VCD bodies, and
//! anything the fast scanner does not support falls back to wellen's streaming
//! parser, so results never depend on the fast path guessing. Produces the raw
//! per-signal toggle counts that [`super::metrics`] turns into metrics.

use super::excludes::Excluded;
use super::fastvcd;
use super::selection::{keep_signal, lookup_target, unique_signal_refs};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use wellen::stream::{read_from_file as stream_read, Filter, StreamingWaveform};
use wellen::{
    BitVecRef, Hierarchy, LoadOptions, ScopeRef, SignalRef, SignalValue, SignalValueRef, Time,
    Timescale,
};

/// Input for one [`run`]: the waveform file, the dotted target scope path, the
/// exclusion set, and whether the file is VCD (enables the fast scanner).
pub struct EngineConfig<'a> {
    pub input_file: &'a Path,
    pub target_scope: &'a str,
    pub excluded: &'a Excluded,
    pub is_vcd: bool,
}

/// Toggle metrics of one evaluated waveform.
#[derive(Debug)]
pub struct ToggleMetrics {
    /// Signal-level toggles summed over all kept variables (aliases each count).
    pub total_toggles: u64,
    /// Bits of kept bit-vector variables that changed value at least once.
    pub toggled_bits: u64,
    /// Total number of bits of kept bit-vector variables.
    pub total_bits: u64,
    /// Time (in timescale ticks) of the last change of any counted signal.
    pub last_change_time: Option<Time>,
    pub timescale: Option<Timescale>,
    /// Toggles per signal, indexed by signal slot; the trace map is built from this.
    pub per_signal_toggles: Vec<u64>,
}

fn load_options() -> LoadOptions {
    LoadOptions {
        multi_thread: false,
        remove_scopes_with_empty_name: true,
    }
}

fn num_slots(signal_refs: &[SignalRef]) -> usize {
    signal_refs
        .iter()
        .map(|r| r.index())
        .max()
        .map(|m| m + 1)
        .unwrap_or(0)
}

/// Evaluates the toggle metrics with one single-threaded pass over the file.
/// VCD files are handled by the specialised scanner; whatever it does not
/// support goes through wellen's generic streaming parser.
pub fn run(cfg: &EngineConfig) -> Result<ToggleMetrics, String> {
    let mut waveform = stream_read(cfg.input_file, &load_options())
        .map_err(|err| format!("Unable to read file {}: {err:?}", cfg.input_file.display()))?;
    let target_scope = lookup_target(waveform.hierarchy(), cfg.target_scope)?;
    let signal_refs = unique_signal_refs(waveform.hierarchy(), target_scope, cfg.excluded);
    let num_slots = num_slots(&signal_refs);

    let fast = if cfg.is_vcd {
        fastvcd::run(cfg.input_file, waveform.hierarchy(), &signal_refs)
            .inspect_err(|reason| {
                tracing::debug!(
                    "fast VCD scanner not applicable for '{}' ({reason}); using wellen",
                    cfg.input_file.display()
                )
            })
            .ok()
    } else {
        None
    };

    let mut toggles = vec![0u64; num_slots];
    let mut toggled_bits = vec![0u32; num_slots];
    let last_change_time = match fast {
        Some(outcome) => {
            for c in outcome.counts {
                toggles[c.signal.index()] = c.toggles;
                toggled_bits[c.signal.index()] = c.toggled_bits;
            }
            outcome.last_change_time
        }
        None => {
            let mut masks = BitMasks::new(
                waveform.hierarchy(),
                target_scope,
                cfg.excluded,
                num_slots,
            );
            let (counted, last) = count_toggles_stream(&mut waveform, &signal_refs, &mut masks)?;
            toggles = counted;
            for &signal in &signal_refs {
                toggled_bits[signal.index()] = masks.toggled_bits(signal.index());
            }
            last
        }
    };

    let (total_toggles, toggled_bits, total_bits) = aggregate(
        waveform.hierarchy(),
        target_scope,
        cfg.excluded,
        &toggles,
        &toggled_bits,
    );
    Ok(ToggleMetrics {
        total_toggles,
        toggled_bits,
        total_bits,
        last_change_time,
        timescale: waveform.hierarchy().timescale(),
        per_signal_toggles: toggles,
    })
}

/// Single pass over the value-change stream of the included signals.
/// The first change of a signal carries its initial value, not a toggle;
/// repeated identical values are not toggles (same semantics the
/// wavemem encoder applies when storing signals).
fn count_toggles_stream(
    waveform: &mut StreamingWaveform<BufReader<File>>,
    signal_refs: &[SignalRef],
    masks: &mut BitMasks,
) -> Result<(Vec<u64>, Option<Time>), String> {
    let num_signals = num_slots(signal_refs);
    let mut toggles = vec![0u64; num_signals];
    let mut previous: Vec<Option<SignalValue>> = (0..num_signals).map(|_| None).collect();
    let mut last_change_time: Option<Time> = None;

    waveform
        .stream_changes(Filter::include_signals(signal_refs), |time, signal, value| {
            let last = last_change_time.get_or_insert(0);
            if time > *last {
                *last = time;
            }
            let idx = signal.index();
            if previous[idx].is_none() {
                previous[idx] = Some(SignalValue::from(value));
                return Ok::<(), String>(());
            }
            let prev_value = SignalValueRef::from(previous[idx].as_ref().unwrap());
            if prev_value == value {
                return Ok(());
            }
            toggles[idx] += 1;
            if let (SignalValueRef::BitVec(p), SignalValueRef::BitVec(c)) = (prev_value, value) {
                masks.mark_changed(idx, p, c);
            }
            previous[idx] = Some(SignalValue::from(value));
            Ok(())
        })
        .map_err(|err| format!("Unable to stream value changes: {err:?}"))?;

    Ok((toggles, last_change_time))
}

/// Per-signal record of which bits ever changed value, for bit-level coverage.
struct BitMasks {
    /// Word offset per signal slot; length is slot count + 1.
    offsets: Vec<usize>,
    words: Vec<u64>,
}

impl BitMasks {
    /// One mask per kept signal; the width comes from the kept variables
    /// below the scope (aliases share a signal and hence a mask).
    fn new(
        hierarchy: &Hierarchy,
        target_scope: ScopeRef,
        excluded: &Excluded,
        num_signals: usize,
    ) -> Self {
        let mut widths = vec![0u32; num_signals];
        for var_ref in hierarchy[target_scope].all_vars(hierarchy) {
            let var = &hierarchy[var_ref];
            if !keep_signal(hierarchy, var, excluded) || !var.is_bit_vector(hierarchy) {
                continue;
            }
            let idx = var.signal_ref().index();
            if idx < num_signals {
                widths[idx] = widths[idx].max(var.length(hierarchy).unwrap_or(0));
            }
        }

        let mut offsets = Vec::with_capacity(num_signals + 1);
        let mut total_words = 0;
        offsets.push(0);
        for &width in &widths {
            total_words += (width as usize).div_ceil(64);
            offsets.push(total_words);
        }
        BitMasks {
            offsets,
            words: vec![0u64; total_words],
        }
    }

    /// Marks the bits that differ between the previous and the current value.
    fn mark_changed(&mut self, idx: usize, prev: BitVecRef, cur: BitVecRef) {
        let start = self.offsets[idx];
        if start == self.offsets[idx + 1] {
            return; // not a bit vector
        }
        debug_assert_eq!(prev.width(), cur.width());
        match (prev.be_bytes(), cur.be_bytes()) {
            // Fast path: both 2-state. Logical bit b (LSB = 0) is stored at
            // byte index len-1-b/8, bit position b%8 within that byte
            // (see wellen's States::get_bit).
            (Some(p), Some(c)) => {
                let len = p.len();
                for i in 0..len {
                    let mut diff = p[i] ^ c[i];
                    if diff == 0 {
                        continue;
                    }
                    let base = (len - 1 - i) * 8;
                    while diff != 0 {
                        let bit = base + diff.trailing_zeros() as usize;
                        if bit < cur.width() as usize {
                            self.words[start + bit / 64] |= 1 << (bit % 64);
                        }
                        diff &= diff - 1;
                    }
                }
            }
            // Any 4/9-state value involved: compare logical bits one by one.
            _ => {
                for bit in 0..cur.width() {
                    if u8::from(prev.get_bit(bit)) != u8::from(cur.get_bit(bit)) {
                        let bit = bit as usize;
                        self.words[start + bit / 64] |= 1 << (bit % 64);
                    }
                }
            }
        }
    }

    fn toggled_bits(&self, idx: usize) -> u32 {
        self.words[self.offsets[idx]..self.offsets[idx + 1]]
            .iter()
            .map(|w| w.count_ones())
            .sum()
    }
}

/// Aggregates per-signal counts over the kept variables below the scope.
/// Aliased variables each contribute (their bits and toggles), matching the
/// per-variable semantics of the metrics.
fn aggregate(
    hierarchy: &Hierarchy,
    target_scope: ScopeRef,
    excluded: &Excluded,
    toggles: &[u64],
    toggled_bits_per_signal: &[u32],
) -> (u64, u64, u64) {
    let mut total_toggles = 0u64;
    let mut toggled_bits = 0u64;
    let mut total_bits = 0u64;
    for var_ref in hierarchy[target_scope].all_vars(hierarchy) {
        let var = &hierarchy[var_ref];
        if !keep_signal(hierarchy, var, excluded) {
            continue;
        }
        let idx = var.signal_ref().index();
        total_toggles += toggles[idx];
        if var.is_bit_vector(hierarchy) {
            total_bits += var.length(hierarchy).unwrap_or(0) as u64;
            toggled_bits += u64::from(toggled_bits_per_signal[idx]);
        }
    }
    (total_toggles, toggled_bits, total_bits)
}
