use std::{error::Error, fmt, path::Path, rc::Rc};

/// Evaluates a freshly built candidate. Larger scores are more interesting.
pub trait EvaluationBackend {
    fn evaluate(&mut self, elf: &Path) -> Result<Evaluation, EvaluationError>;
}

/// The meaning of an evaluation's primary score.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    /// Running-minus-idle power, in watts.
    #[cfg_attr(not(feature = "hardware"), allow(dead_code))]
    PowerDelta,
    /// Signal-level toggles per simulated second.
    ToggleActivity,
}

/// A validated score, the optional bit-level toggle coverage, and the trace map.
///
/// For the simulation backend the trace map (one saturating toggle count per
/// traced signal, from the integrated VCD analysis) is the primary feedback;
/// the score (toggle activity) is the secondary metric used for best tracking.
#[derive(Clone, Debug, PartialEq)]
pub struct Evaluation {
    metric: Metric,
    score: f64,
    toggle_coverage: Option<f64>,
    map: Option<Rc<Vec<u8>>>,
}

impl Evaluation {
    #[cfg_attr(not(feature = "hardware"), allow(dead_code))]
    pub fn power_delta(watts: f64) -> Result<Self, EvaluationError> {
        if !watts.is_finite() {
            return Err(EvaluationError::Measurement(format!(
                "power delta must be finite, got {watts}"
            )));
        }
        Ok(Self {
            metric: Metric::PowerDelta,
            score: watts,
            toggle_coverage: None,
            map: None,
        })
    }

    pub fn toggle_metrics(
        activity: f64,
        coverage: f64,
        map: Vec<u8>,
    ) -> Result<Self, EvaluationError> {
        if !activity.is_finite() || activity < 0.0 {
            return Err(EvaluationError::Measurement(format!(
                "toggle activity must be finite and non-negative, got {activity}"
            )));
        }
        if !coverage.is_finite() || !(0.0..=1.0).contains(&coverage) {
            return Err(EvaluationError::Measurement(format!(
                "toggle coverage must be finite and between 0 and 1, got {coverage}"
            )));
        }
        Ok(Self {
            metric: Metric::ToggleActivity,
            score: activity,
            toggle_coverage: Some(coverage),
            map: Some(Rc::new(map)),
        })
    }

    pub fn metric(&self) -> Metric {
        self.metric
    }

    pub fn score(&self) -> f64 {
        self.score
    }

    /// Fraction of selected bit-vector bits that changed at least once (simulation only).
    pub fn toggle_coverage(&self) -> Option<f64> {
        self.toggle_coverage
    }

    /// Trace map of the last run: one saturating toggle count per traced signal
    /// (simulation only); `None` for the hardware backend.
    pub fn trace_map(&self) -> Option<&[u8]> {
        self.map.as_ref().map(|map| &***map)
    }
}

/// Failures are classified so the caller can apply backend-appropriate recovery.
#[derive(Debug)]
pub enum EvaluationError {
    /// The candidate failed; it may be skipped after any necessary recovery.
    Candidate(String),
    /// A measurement failed; repeated failures can abort the campaign.
    Measurement(String),
    /// Configuration, infrastructure, or recovery failed and the campaign must stop.
    Fatal(String),
}

impl fmt::Display for EvaluationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Candidate(message) => write!(f, "candidate failed: {message}"),
            Self::Measurement(message) => write!(f, "measurement failed: {message}"),
            Self::Fatal(message) => write!(f, "fatal evaluation error: {message}"),
        }
    }
}

impl Error for EvaluationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_metric_boundaries_and_negative_power_delta() {
        let power = Evaluation::power_delta(-0.5).unwrap();
        assert_eq!(power.metric(), Metric::PowerDelta);
        assert_eq!(power.score(), -0.5);
        assert_eq!(power.toggle_coverage(), None);
        assert!(power.trace_map().is_none());
        for coverage in [0.0, 1.0] {
            let result = Evaluation::toggle_metrics(1200.0, coverage, vec![0, 3, 255]).unwrap();
            assert_eq!(result.metric(), Metric::ToggleActivity);
            assert_eq!(result.score(), 1200.0);
            assert_eq!(result.toggle_coverage(), Some(coverage));
            assert_eq!(result.trace_map(), Some(&[0, 3, 255][..]));
        }
    }

    #[test]
    fn rejects_invalid_metric_values() {
        for activity in [-0.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(Evaluation::toggle_metrics(activity, 0.5, vec![1]).is_err());
        }
        for coverage in [-0.01, 1.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(Evaluation::toggle_metrics(1.0, coverage, vec![1]).is_err());
        }
        for watts in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(Evaluation::power_delta(watts).is_err());
        }
    }

    #[test]
    fn error_categories_remain_distinguishable() {
        let errors = [
            EvaluationError::Candidate("target crashed".into()),
            EvaluationError::Measurement("analysis failed".into()),
            EvaluationError::Fatal("tool missing".into()),
        ];
        assert!(errors[0].to_string().contains("target crashed"));
        assert!(errors[1].to_string().contains("analysis failed"));
        assert!(errors[2].to_string().contains("tool missing"));
    }
}
