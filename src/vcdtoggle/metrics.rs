//! Derived metrics (vendored from `vcdtogglemeasure`): toggle activity per
//! simulated second and per simulated cycle, bit-level toggle coverage, and the
//! trace map built from the per-signal toggle counts.

use super::engine::ToggleMetrics;
use wellen::Time;

impl ToggleMetrics {
    /// Signal-level toggles per simulated second, normalized by the last
    /// timestamp in the trace (falling back to the last change of a counted
    /// signal). 0.0 if the trace carries no timing.
    pub fn activity(&self) -> f64 {
        let seconds = self.simulated_seconds();
        if seconds > 0.0 {
            self.total_toggles as f64 / seconds
        } else {
            0.0
        }
    }

    /// Signal-level toggles per simulated cycle (rising clock edge). 0.0 if the
    /// clock never rose — scale this by the target clock frequency to get
    /// changes per wall-clock second on the real chip.
    pub fn activity_per_cycle(&self) -> f64 {
        if self.cycles > 0 {
            self.total_toggles as f64 / self.cycles as f64
        } else {
            0.0
        }
    }

    /// Simulated cycles = rising edges of the clock signal.
    pub fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Simulated duration in seconds: the last timestamp in the trace (in
    /// timescale ticks), falling back to the last counted-signal change.
    /// 0.0 if the trace carries no timing.
    pub fn simulated_seconds(&self) -> f64 {
        simulated_seconds(self.last_timestamp.or(self.last_change_time), self.timescale)
    }

    /// Bit-level coverage — the fraction of bits (of counted bit-vector
    /// variables) that changed value at least once. 0.0 if there are no
    /// counted bit-vector bits.
    pub fn coverage(&self) -> f64 {
        if self.total_bits > 0 {
            self.toggled_bits as f64 / self.total_bits as f64
        } else {
            0.0
        }
    }

    /// The trace map: one saturating toggle count per signal slot, to be used
    /// as the primary coverage map for feedback.
    pub fn trace_map(&self) -> Vec<u8> {
        self.per_signal_toggles
            .iter()
            .map(|&toggles| toggles.min(u64::from(u8::MAX)) as u8)
            .collect()
    }
}

fn simulated_seconds(time: Option<Time>, timescale: Option<wellen::Timescale>) -> f64 {
    let Some(time) = time else {
        return 0.0;
    };
    let Some(timescale) = timescale else {
        return 0.0;
    };
    let Some(exponent) = timescale.unit.to_exponent() else {
        return 0.0;
    };
    let seconds_per_tick = timescale.factor as f64 * 10f64.powi(exponent as i32);
    time as f64 * seconds_per_tick
}
