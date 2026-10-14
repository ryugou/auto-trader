use std::collections::BTreeMap;

use crate::error::SimError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Int,
    Float,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamSpec {
    pub name: String,
    pub kind: ParamKind,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub default: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Int(i64),
    Float(f64),
}

pub type ParamSet = BTreeMap<String, ParamValue>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarState {
    pub t: usize,
    pub position: i8,
    pub entry_price_milli: i64,
    pub bars_held: u32,
}

#[derive(Debug)]
pub enum DeciderError {
    Script(String),
    Fatal(SimError),
}

pub trait Decider {
    fn on_bar(&mut self, state: BarState) -> Result<i8, DeciderError>;
}
