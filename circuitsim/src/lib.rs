//! Analog circuit simulator using modified nodal analysis (MNA).
//!
//! Supports resistors, capacitors, inductors, and DC voltage sources with
//! operating-point (`.op`) and transient (`.tran`) analysis.

pub mod analysis;
pub mod circuit;
pub mod error;
pub mod mna;
pub mod netlist;
pub mod units;
pub mod ac_mna;

pub use analysis::{AcResult, AcCommand, AcScale, run, DcResult, SimulationResult, TranResult};
pub use circuit::Circuit;
pub use error::{Result, SimError};
pub use netlist::{parse, Netlist};

/// Parse a netlist string and run all requested analyses.
pub fn simulate(source: &str) -> Result<SimulationResult> {
    let netlist = parse(source)?;
    analysis::run(&netlist.circuit, &netlist.analysis)
}
