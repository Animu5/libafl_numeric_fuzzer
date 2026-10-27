use crate::scope::{Scope, ScopeError};
use std::time::Duration;
use std::{error::Error, fmt};

/// Scope commands: one-time setup, arm for the idle/run capture, and read the mean.
const DO_SETUP: &str = "DISP ON;CHDR OFF;TDIV 50MS";
const DO_IDLE: &str = "ARM;FRTR;WAIT;STO C3,M1;WAIT;*OPC?";
const DO_RUN: &str = "ARM;FRTR;WAIT;*OPC?";
const GET_MEASUREMENT: &str = "F1:PAVA? MEAN";

/// Target state in which a measurement is taken.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeasurementPhase {
    Idle,
    Run,
}

/// Why a measurement failed.
#[derive(Debug)]
pub enum MeasurementError {
    /// VISA communication error.
    Scope(ScopeError),
    /// Response not in the form `type,value,OK`.
    InvalidReading(String),
    /// Value parsed but is NaN or infinite.
    NonFiniteReading(f64),
}

impl fmt::Display for MeasurementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scope(message) => write!(f, "scope communication failed: {message}"),
            Self::InvalidReading(response) => {
                write!(f, "scope returned an invalid measurement: {response:?}")
            }
            Self::NonFiniteReading(value) => {
                write!(f, "scope returned a non-finite measurement: {value}")
            }
        }
    }
}

impl Error for MeasurementError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Scope(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ScopeError> for MeasurementError {
    fn from(e: ScopeError) -> Self {
        Self::Scope(e)
    }
}

/// Source of one scalar reading per target phase.
pub trait Measurement {
    fn measure(&mut self, phase: MeasurementPhase) -> Result<f64, MeasurementError>;
}

/// Oscilloscope measurement. Port the phase-specific arming/readout logic into `measure`.
pub struct ScopeMeasurement {
    #[allow(dead_code)]
    scope: Scope,
    /// How long an arm/acquire command may take to answer.
    acquire_timeout: Duration,
}

impl ScopeMeasurement {
    /// Takes the scope and sends the one-time setup commands.
    pub fn new(mut scope: Scope, acquire_timeout: Duration) -> Result<Self, MeasurementError> {
        scope.write(DO_SETUP)?;
        Ok(Self {
            scope,
            acquire_timeout,
        })
    }

    /// Parses `type,value,OK` into the value.
    fn parse_measurement(&self, response: String) -> Result<f64, MeasurementError> {
        let [_meas_type, measurement, status] = response.split(',').collect::<Vec<&str>>()[..]
        else {
            return Err(MeasurementError::InvalidReading(response));
        };

        if status != "OK" {
            return Err(MeasurementError::InvalidReading(response));
        }

        let value = measurement
            .parse::<f64>()
            .map_err(|_| MeasurementError::InvalidReading(response.clone()))?;
        if !value.is_finite() {
            return Err(MeasurementError::NonFiniteReading(value));
        }
        Ok(value)
    }
}

impl Measurement for ScopeMeasurement {
    /// Arms the scope for `phase`, then reads the measured mean.
    fn measure(&mut self, phase: MeasurementPhase) -> Result<f64, MeasurementError> {
        let command = match phase {
            MeasurementPhase::Idle => DO_IDLE,
            MeasurementPhase::Run => DO_RUN,
        };
        // Arming waits for a complete acquisition, which is much slower than a normal query.
        self.scope
            .query_with_timeout(command, self.acquire_timeout)?;

        let response = self.scope.query(GET_MEASUREMENT)?;
        self.parse_measurement(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn measurement_error_messages_are_descriptive() {
        assert!(MeasurementError::Scope(ScopeError::Read(io::Error::new(
            io::ErrorKind::TimedOut,
            "timeout"
        )))
        .to_string()
        .contains("timeout"));
        assert!(MeasurementError::InvalidReading("N/A".into())
            .to_string()
            .contains("N/A"));
        assert!(MeasurementError::NonFiniteReading(f64::NAN)
            .to_string()
            .contains("non-finite"));
    }
}
