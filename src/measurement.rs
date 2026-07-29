use std::fs::read_to_string;

pub trait Measurement {
    fn measure(&self) -> f64;
}

#[derive(Clone, Debug)]
pub struct FileMeasurement {
    path: String,
}

impl FileMeasurement {
    pub(crate) fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
        }
    }
}

impl Measurement for FileMeasurement {
    fn measure(&self) -> f64 {
        match read_to_string(&self.path) {
            Ok(s) => s.trim().parse::<f64>().unwrap_or(0.0),
            Err(_) => 0.0,
        }
    }
}