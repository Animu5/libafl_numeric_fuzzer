use std::{error::Error, fmt, path::Path};

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
    /// Fraction of selected bit-vector bits that changed at least once.
    ToggleCoverage,
}

/// A validated score and its optional secondary measurement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Evaluation {
    metric: Metric,
    score: f64,
    toggle_activity: Option<f64>,
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
            toggle_activity: None,
        })
    }

    pub fn toggle_coverage(coverage: f64, activity: f64) -> Result<Self, EvaluationError> {
        if !coverage.is_finite() || !(0.0..=1.0).contains(&coverage) {
            return Err(EvaluationError::Measurement(format!(
                "toggle coverage must be finite and between 0 and 1, got {coverage}"
            )));
        }
        if !activity.is_finite() || activity < 0.0 {
            return Err(EvaluationError::Measurement(format!(
                "toggle activity must be finite and non-negative, got {activity}"
            )));
        }
        Ok(Self {
            metric: Metric::ToggleCoverage,
            score: coverage,
            toggle_activity: Some(activity),
        })
    }

    pub fn metric(&self) -> Metric {
        self.metric
    }

    pub fn score(&self) -> f64 {
        self.score
    }

    /// Signal-level changes per simulated second, when reported by the analyzer.
    pub fn toggle_activity_per_second(&self) -> Option<f64> {
        self.toggle_activity
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

/// Parses the two key/value lines printed by vcdtogglerate on stdout.
///
/// The analyzer also reports zero for an empty selection or missing timing; its
/// output does not distinguish those cases from a genuine zero measurement.
pub fn parse_toggle_metrics(stdout: &str) -> Result<Evaluation, EvaluationError> {
    let mut coverage = None;
    let mut activity = None;
    for line in stdout.lines() {
        let (key, value) = line.split_once('=').ok_or_else(|| {
            EvaluationError::Measurement(format!("invalid analyzer output line: {line:?}"))
        })?;
        let slot = match key {
            "toggle_coverage" => &mut coverage,
            "toggle_activity" => &mut activity,
            _ => {
                return Err(EvaluationError::Measurement(format!(
                    "unexpected analyzer output key: {key:?}"
                )));
            }
        };
        if slot.is_some() {
            return Err(EvaluationError::Measurement(format!(
                "duplicate analyzer output key: {key}"
            )));
        }
        *slot = Some(value.parse::<f64>().map_err(|_| {
            EvaluationError::Measurement(format!("invalid {key} value: {value:?}"))
        })?);
    }

    let coverage = coverage.ok_or_else(|| {
        EvaluationError::Measurement("analyzer output is missing toggle_coverage".into())
    })?;
    let activity = activity.ok_or_else(|| {
        EvaluationError::Measurement("analyzer output is missing toggle_activity".into())
    })?;
    Evaluation::toggle_coverage(coverage, activity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_metrics_in_either_order() {
        for output in [
            "toggle_activity=1200.5\ntoggle_coverage=0.25\n",
            "toggle_coverage=0.25\ntoggle_activity=1200.5\n",
        ] {
            let result = parse_toggle_metrics(output).unwrap();
            assert_eq!(result.metric(), Metric::ToggleCoverage);
            assert_eq!(result.score(), 0.25);
            assert_eq!(result.toggle_activity_per_second(), Some(1200.5));
        }
    }

    #[test]
    fn accepts_metric_boundaries_and_negative_power_delta() {
        for coverage in [0.0, 1.0] {
            assert_eq!(
                Evaluation::toggle_coverage(coverage, 0.0).unwrap().score(),
                coverage
            );
        }
        let power = Evaluation::power_delta(-0.5).unwrap();
        assert_eq!(power.metric(), Metric::PowerDelta);
        assert_eq!(power.score(), -0.5);
        assert_eq!(power.toggle_activity_per_second(), None);
    }

    #[test]
    fn rejects_invalid_metric_values() {
        for coverage in [-0.01, 1.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(Evaluation::toggle_coverage(coverage, 0.0).is_err());
        }
        for activity in [-0.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(Evaluation::toggle_coverage(0.5, activity).is_err());
        }
        for watts in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(Evaluation::power_delta(watts).is_err());
        }
    }

    #[test]
    fn rejects_incomplete_duplicate_and_unexpected_output() {
        for output in [
            "",
            "toggle_activity=1\n",
            "toggle_coverage=0.5\n",
            "toggle_activity=1\ntoggle_activity=2\ntoggle_coverage=0.5\n",
            "toggle_activity=1\ntoggle_coverage=0.5\ntoggle_coverage=0.6\n",
            "toggle_activity=1\ntoggle_coverage=0.5\nother=1\n",
            "toggle_activity=1\ntoggle_coverage=0.5\nextra line\n",
            "toggle_activity=1\ntoggle_coverage=oops\n",
            "toggle_activity=1\ntoggle_coverage=0.5=1\n",
            "toggle_activity=NaN\ntoggle_coverage=0.5\n",
            "toggle_activity=1\ntoggle_coverage=inf\n",
        ] {
            assert!(parse_toggle_metrics(output).is_err(), "{output:?}");
        }
    }

    #[test]
    fn error_categories_remain_distinguishable() {
        let errors = [
            EvaluationError::Candidate("target crashed".into()),
            EvaluationError::Measurement("analyzer failed".into()),
            EvaluationError::Fatal("tool missing".into()),
        ];
        assert!(errors[0].to_string().contains("target crashed"));
        assert!(errors[1].to_string().contains("analyzer failed"));
        assert!(errors[2].to_string().contains("tool missing"));
    }
}
