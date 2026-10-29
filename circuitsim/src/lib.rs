//! Analog circuit simulator using modified nodal analysis (MNA).
//!
//! Supports resistors, capacitors, inductors, voltage and current sources
//! (waveform and AC cards), diodes, and level-1 MOSFETs, with operating-point
//! (`.op`), real DC-sweep (`.dc`), transient (`.tran`), and small-signal
//! (`.ac`) analyses over a parameter language (`.param`, `.func`, `{expr}`).

pub mod analysis;
pub mod behavior;
pub mod circuit;
pub mod error;
pub mod mc;
pub mod mna;
pub mod model;
pub mod netlist;
pub mod params;
pub mod units;

pub use behavior::{Behavior, BehavioralRuntime, BExpr, MathFn, NoiseKind, ProbeBinding, ProbeSpec};

pub use analysis::{AcResult, run, DcResult, DcSweepPoint, DcSweepResult, FourOutput, FourResult,
                   Harmonic, MeasureResult, NoisePoint, NoiseResult, OpPoint, SimulationResult, StepRun,
                   TfResult, TranPoint, TranResult};
pub use netlist::{AcCommand, AcScale, DcCommand, DcSweepSpec, DcSweepValues, MeasAnalysis,
                  FourCommand, NoiseCommand, MeasEdge, MeasFunc, MeasKind, Measurement, NodesetEntry,
                  ParseOptions, StepSpec, StepValues, TfCommand, TfOutput, parse,
                  parse_with_options};
pub use circuit::{AbmForm, AbmMeasure, BehavioralKind, BehavioralSource, Circuit, ControlVar,
                  ControlledSource, CurrentSource, Diode, DiodeModel, GenericModel, Jfet,
                  JfetModel, ModelTolerance, Mosfet, MosfetModel, MutualInductance, OpAmp,
                  OpAmpModel, Switch, SwitchControl, SwitchModel, Tline, Waveform};
pub use error::{Result, SimError};
pub use netlist::{Netlist, parse_with_overrides};

/// Parse a netlist string and run all requested analyses.
///
/// Parameter sweeps (`.dc` over a `.param`) re-expand the deck per sweep
/// point, so the source text travels with the parsed netlist.
pub fn simulate(source: &str) -> Result<SimulationResult> {
    let netlist = parse(source)?;
    analysis::run(&netlist)
}
