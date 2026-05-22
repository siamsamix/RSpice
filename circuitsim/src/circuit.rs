use crate::error::{Result, SimError};

/// Ground is always node 0 and is excluded from the unknown vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub usize);

#[derive(Debug, Clone)]
pub struct Resistor {
    pub name: String,
    pub n1: NodeId,
    pub n2: NodeId,
    pub resistance: f64,
}

#[derive(Debug, Clone)]
pub struct Capacitor {
    pub name: String,
    pub n1: NodeId,
    pub n2: NodeId,
    pub capacitance: f64,
}

#[derive(Debug, Clone)]
pub struct Inductor {
    pub name: String,
    pub n1: NodeId,
    pub n2: NodeId,
    pub inductance: f64,
}

#[derive(Debug, Clone)]
pub struct Diode {
    pub name: String,
    pub anode: NodeId,
    pub cathode: NodeId,
    pub is: f64, // Saturation current (e.g., 1e-14 A)
    pub n: f64,  // Ideality factor (e.g., 1.0 to 2.0)
    pub vt: f64, // Thermal voltage (roughly 0.02585 V at room temp)
}

#[derive(Debug, Clone)]
pub struct Pulse {
    pub v1: f64,   // Initial value (V or A)
    pub v2: f64,   // Pulsed value (V or A)
    pub td: f64,   // Delay time
    pub tr: f64,   // Rise time
    pub tf: f64,   // Fall time
    pub pw: f64,   // Pulse width
    pub per: f64,  // Period
}

impl Pulse {
    pub fn value_at(&self, t: f64) -> f64 {
        if t < self.td {
            return self.v1;
        }

        // Calculate time relative to the start of the current pulse cycle
        let t_rel = if self.per > 0.0 {
            (t - self.td) % self.per
        } else {
            t - self.td
        };

        if t_rel < self.tr {
            // Rising edge
            if self.tr > 0.0 {
                self.v1 + (self.v2 - self.v1) * (t_rel / self.tr)
            } else {
                self.v2
            }
        } else if t_rel < self.tr + self.pw {
            // Pulse plateau
            self.v2
        } else if t_rel < self.tr + self.pw + self.tf {
            // Falling edge
            if self.tf > 0.0 {
                let fall_time = t_rel - self.tr - self.pw;
                self.v2 - (self.v2 - self.v1) * (fall_time / self.tf)
            } else {
                self.v1
            }
        } else {
            // Remaining period plateau
            self.v1
        }
    }
}

use std::f64::consts::PI;

#[derive(Debug, Clone)]
pub struct Sine {
    pub vo: f64,    // Offset voltage (V)
    pub va: f64,    // Amplitude (V)
    pub freq: f64,  // Frequency (Hz)
    pub td: f64,    // Delay time (s)
    pub theta: f64, // Damping factor (1/s)
}

impl Sine {
    pub fn value_at(&self, t: f64) -> f64 {
        if t < self.td {
            return self.vo;
        }

        let t_rel = t - self.td;
        let damping = (-self.theta * t_rel).exp();
        let oscillation = (2.0 * PI * self.freq * t_rel).sin();

        self.vo + self.va * damping * oscillation
    }
}

#[derive(Debug, Clone)]
pub struct VoltageSource {
    pub name: String,
    pub n1: NodeId,
    pub n2: NodeId,
    pub voltage: f64,
    pub pulse: Option<Pulse>,
    pub sine: Option<Sine>, // <-- Add the sine field
}

impl VoltageSource {
    pub fn value_at(&self, t: f64) -> f64 {
        if let Some(ref pulse) = self.pulse {
            pulse.value_at(t)
        } else if let Some(ref sine) = self.sine {
            sine.value_at(t) // <-- Evaluate sine wave if present
        } else {
            self.voltage
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Circuit {
    pub name: String,
    pub nodes: usize,
    pub resistors: Vec<Resistor>,
    pub capacitors: Vec<Capacitor>,
    pub inductors: Vec<Inductor>,
    pub voltage_sources: Vec<VoltageSource>,
    pub diodes: Vec<Diode>,
}

impl Circuit {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            nodes: 1,
            ..Default::default()
        }
    }

    pub fn ensure_node(&mut self, id: usize) {
        if id >= self.nodes {
            self.nodes = id + 1;
        }
    }

    pub fn dc_matrix_size(&self) -> usize {
        let n = self.nodes.saturating_sub(1);
        n + self.voltage_sources.len()
    }

    pub fn tran_matrix_size(&self) -> usize {
        let n = self.nodes.saturating_sub(1);
        n + self.voltage_sources.len() + self.inductors.len()
    }

    pub fn validate(&self) -> Result<()> {
        if self.nodes < 1 {
            return Err(SimError::Circuit("circuit has no nodes".into()));
        }
        for r in &self.resistors {
            if r.resistance <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "resistor {} must have R > 0",
                    r.name
                )));
            }
        }
        for c in &self.capacitors {
            if c.capacitance <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "capacitor {} must have C > 0",
                    c.name
                )));
            }
        }
        for l in &self.inductors {
            if l.inductance <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "inductor {} must have L > 0",
                    l.name
                )));
            }
        }
        for v in &self.voltage_sources {
            if let Some(ref p) = v.pulse {
                if p.td < 0.0 || p.tr < 0.0 || p.tf < 0.0 || p.pw < 0.0 || p.per < 0.0 {
                    return Err(SimError::Circuit(format!(
                        "voltage source {} pulse timings cannot be negative",
                        v.name
                    )));
                }
                if p.per > 0.0 && p.per < (p.tr + p.pw + p.tf) {
                    return Err(SimError::Circuit(format!(
                        "voltage source {} pulse period must be greater than active duration (tr + pw + tf)",
                                                         v.name
                    )));
                }
            }
            // Inside the loop for voltage_sources in Circuit::validate():
            if let Some(ref s) = v.sine {
                if s.freq < 0.0 || s.td < 0.0 {
                    return Err(SimError::Circuit(format!(
                        "voltage source {} sine frequency and delay cannot be negative",
                        v.name
                    )));
                }
            }
        }
        Ok(())
    }
}
