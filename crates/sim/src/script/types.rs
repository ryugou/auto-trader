use std::collections::BTreeMap;

use crate::error::SimError;

/// Numeric kind of a script parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// Integer parameter.
    Int,
    /// Floating-point parameter.
    Float,
}

/// Declaration of a script parameter: its range, step and default.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamSpec {
    /// Name of the parameter.
    pub name: String,
    /// Numeric kind of the parameter.
    pub kind: ParamKind,
    /// Smallest value.
    pub min: f64,
    /// Largest value.
    pub max: f64,
    /// Distance between neighbouring values on the grid.
    pub step: f64,
    /// Value used when none is given.
    pub default: f64,
}

/// Value of a script parameter.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    /// Integer value.
    Int(i64),
    /// Floating-point value.
    Float(f64),
}

/// Parameter values of a run, by parameter name.
pub type ParamSet = BTreeMap<String, ParamValue>;

/// State of the position that a script sees on a bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarState {
    /// Index of the bar in the bars of the dataset, warmup bars included.
    pub t: usize,
    /// Current position: 1 for long, -1 for short, 0 for none.
    pub position: i8,
    /// Entry price of the position in milli yen.
    pub entry_price_milli: i64,
    /// Number of bars the position has been held.
    pub bars_held: u32,
}

/// Failure of a decider on a bar.
#[derive(Debug)]
pub enum DeciderError {
    /// The script failed, which ends the run as a script error.
    Script(String),
    /// A failure that is not the script's fault, which aborts the simulation.
    Fatal(SimError),
}

/// Source of the position decisions for the bars of a simulation.
pub trait Decider {
    /// Returns the position to hold after the bar: 1 for long, -1 for short, 0 for none.
    ///
    /// # Errors
    ///
    /// - Returns `DeciderError::Script` if the script fails or returns an unsupported value.
    /// - Returns `DeciderError::Fatal` if a fatal error occurs while the script runs.
    fn on_bar(&mut self, state: BarState) -> Result<i8, DeciderError>;
}
