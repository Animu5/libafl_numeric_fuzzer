use super::engine::ToggleMetrics;
use wellen::Time;

impl ToggleMetrics {
    /// Signal-level toggles per simulated second, normalized by the time of the
    /// last change of any counted signal. 0.0 if no counted signal ever changed.
    pub fn activity(&self) -> f64 {
        let seconds = simulated_seconds(self.last_change_time, self.timescale);
        if seconds > 0.0 {
            self.total_toggles as f64 / seconds
        } else {
            0.0
        }
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

fn simulated_seconds(last_change_time: Option<Time>, timescale: Option<wellen::Timescale>) -> f64 {
    let Some(time) = last_change_time else {
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
