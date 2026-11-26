//! Vendored from the `vcdtogglemeasure` project (`vcdtogglerate` v0.3.0) and
//! integrated as a library: the VCD analysis no longer runs as a separate
//! executable but in-process, so the per-signal toggle counts can be used
//! directly as a trace map for feedback.
//!
//! - `selection` picks the kept signals below the target scope.
//! - `engine` drives one single-threaded pass over the waveform.
//! - `fastvcd` is the specialised mmap-based VCD body scanner; anything it
//!   does not support falls back to wellen's generic streaming parser.
//! - `metrics` turns the raw counts into activity, coverage, and the trace map.
//! - `excludes` loads the optional signal exclusion file.

pub mod engine;
pub mod excludes;
pub mod fastvcd;
pub mod metrics;
pub mod selection;

use engine::{EngineConfig, ToggleMetrics};
use std::path::Path;
use wellen::{FileFormat, viewers::open_and_detect_file_format};

/// Analyzes one waveform file and returns its toggle metrics, including the
/// per-signal toggle counts the trace map is built from.
pub fn analyze(
    input_file: &Path,
    target_scope: &str,
    excluded: &excludes::Excluded,
) -> Result<ToggleMetrics, String> {
    let is_vcd = match open_and_detect_file_format(input_file) {
        FileFormat::Vcd => true,
        FileFormat::Fst => false,
        _ => return Err(format!("Unsupported waveform format: {}", input_file.display())),
    };
    let config = EngineConfig {
        input_file,
        target_scope,
        excluded,
        is_vcd,
    };
    engine::run(&config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A minimal VCD: `value` toggles twice, the 4-bit `nibble` changes once
    /// (2 of 4 bits), and the 1-bit `clock` rises at #10 and #20 (2 cycles) and
    /// falls at #15 and #25. The last timestamp is #25 while the last counted
    /// change is at #20, with a 1ns timescale.
    const VCD: &str = "\
$timescale 1ns $end
$scope module TOP $end
$scope module dut $end
$var wire 1 ! value $end
$var wire 4 \" nibble $end
$var wire 1 & clock $end
$upscope $end
$upscope $end
$enddefinitions $end
#0
0!
b0000 \"
0&
#10
1!
b0101 \"
1&
#15
0&
#20
0!
1&
#25
0&
";

    fn write_temp(content: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(file, "{content}").unwrap();
        file
    }

    #[test]
    fn computes_activity_coverage_and_trace_map() {
        let file = write_temp(VCD);
        let metrics = analyze(file.path(), "TOP.dut", &Default::default()).unwrap();
        assert_eq!(metrics.total_toggles, 3);
        // 3 toggles over the full trace (#25 = 25ns): 1.2e8 toggles/s, and
        // 3 toggles over 2 clock cycles: 1.5 toggles/cycle.
        // 25 ticks at 1ns; compared with an epsilon because 10^-9 is not exact in binary.
        assert!((metrics.simulated_seconds() - 25e-9).abs() < 1e-20);
        assert_eq!(metrics.last_timestamp, Some(25));
        assert_eq!(metrics.last_change_time, Some(20));
        assert_eq!(metrics.cycles(), 2);
        // 3 toggles / 25ns = 1.2e8/s; epsilon because 10^-9 is not exact in binary.
        assert!((metrics.activity() - 120_000_000.0).abs() < 1e-6);
        assert_eq!(metrics.activity_per_cycle(), 1.5);
        // `value` is a 1-bit vector: 3 of 5 bits changed.
        assert_eq!(metrics.coverage(), 0.6);
        let mut map = metrics.trace_map();
        map.sort_unstable();
        assert_eq!(map, vec![1, 2]);
    }

    #[test]
    fn wellen_fallback_matches_the_fast_scanner() {
        // A `$comment` in the body is outside the fast scanner's strict subset,
        // so the engine falls back to wellen's streaming parser; results must match.
        let commented = VCD.replace("#10\n", "#10\n$comment hello $end\n");
        let fast_file = write_temp(VCD);
        let slow_file = write_temp(&commented);
        let excluded = Default::default();
        let fast = analyze(fast_file.path(), "TOP.dut", &excluded).unwrap();
        let slow = analyze(slow_file.path(), "TOP.dut", &excluded).unwrap();
        assert_eq!(fast.total_toggles, slow.total_toggles);
        assert_eq!(fast.toggled_bits, slow.toggled_bits);
        assert_eq!(fast.total_bits, slow.total_bits);
        assert_eq!(fast.last_change_time, slow.last_change_time);
        assert_eq!(fast.last_timestamp, slow.last_timestamp);
        assert_eq!(fast.cycles, slow.cycles);
        assert_eq!(fast.activity(), slow.activity());
        assert_eq!(fast.activity_per_cycle(), slow.activity_per_cycle());
        assert_eq!(fast.trace_map(), slow.trace_map());
    }

    #[test]
    fn unknown_scope_and_non_waveform_are_errors() {
        let file = write_temp(VCD);
        let err = analyze(file.path(), "TOP.missing", &Default::default()).unwrap_err();
        assert!(err.contains("TOP.missing"), "{err}");
        let garbage = write_temp("not a waveform");
        assert!(analyze(garbage.path(), "TOP.dut", &Default::default()).is_err());
    }

    #[test]
    fn exclusions_remove_signals_from_the_metrics() {
        let file = write_temp(VCD);
        let mut excluded = excludes::Excluded::new();
        excluded.insert("TOP.dut.nibble".to_string());
        let metrics = analyze(file.path(), "TOP.dut", &excluded).unwrap();
        assert_eq!(metrics.total_toggles, 2);
        assert_eq!(metrics.coverage(), 1.0);
        let map = metrics.trace_map();
        assert_eq!(map.iter().copied().filter(|&b| b > 0).collect::<Vec<_>>(), vec![2]);
    }
}
