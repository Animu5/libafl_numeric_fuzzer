use crate::scope::Scope;

pub trait Measurement {
    fn measure(&mut self) -> Result<f64, String>;
}

/// Oscilloscope measurement. Port the arming/readout logic into `measure`.
pub struct ScopeMeasurement {
    #[allow(dead_code)]
    scope: Scope,
}

impl ScopeMeasurement {
    pub fn new(scope: Scope) -> Self {
        Self { scope }
    }
}

impl Measurement for ScopeMeasurement {
    fn measure(&mut self) -> Result<f64, String> {
        // TODO: arm the scope, wait for acquisition and return the mean value
        // (e.g. via self.scope.write(..) / self.scope.query(..)).
        Err("ScopeMeasurement::measure is not implemented yet".to_string())
    }
}
