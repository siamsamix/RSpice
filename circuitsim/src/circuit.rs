use crate::error::{Result, SimError};
use std::collections::HashMap;


#[derive(Debug, Clone)]
pub struct DiodeModel {
    pub is: f64,
    pub n: f64,
}

// ─── MOSFET ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum MosfetType {
    Nmos,
    Pmos,
}

/// Level-1 (Shichman-Hodges) MOSFET model parameters.
/// Mirrors standard SPICE .MODEL NMOS/PMOS card syntax.
#[derive(Debug, Clone)]
pub struct MosfetModel {
    pub kind: MosfetType,
    /// Transconductance parameter KP = µ·Cox (A/V²).
    pub kp: f64,
    /// Zero-bias threshold voltage Vth0 (V).
    pub vth0: f64,
    /// Channel-length modulation coefficient λ (1/V). 0 = ideal.
    pub lambda: f64,
    /// Bulk threshold modulation γ (V^0.5).
    pub gamma: f64,
    /// Strong-inversion surface potential 2·φF (V).
    pub phi: f64,
}

impl Default for MosfetModel {
    /// Reasonable level-1 NMOS defaults (matches SPICE internal defaults).
    fn default() -> Self {
        Self {
            kind: MosfetType::Nmos,
            kp: 20e-6,
            vth0: 0.7,
            lambda: 0.0,
            gamma: 0.0,
            phi: 0.6,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Mosfet {
    pub name: String,
    /// Drain node.
    pub nd: NodeId,
    /// Gate node.
    pub ng: NodeId,
    /// Source node.
    pub ns: NodeId,
    /// Bulk / body node.
    pub nb: NodeId,
    /// Name of the referenced .MODEL card.
    pub model: String,
    /// Channel width W (m).
    pub w: f64,
    /// Channel length L (m).
    pub l: f64,
}

// ─── Passive elements ────────────────────────────────────────────────────────

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
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct Pulse {
    pub v1: f64,
    pub v2: f64,
    pub td: f64,
    pub tr: f64,
    pub tf: f64,
    pub pw: f64,
    pub per: f64,
}

impl Pulse {
    pub fn value_at(&self, t: f64) -> f64 {
        if t < self.td {
            return self.v1;
        }
        let t_rel = if self.per > 0.0 {
            (t - self.td) % self.per
        } else {
            t - self.td
        };
        if t_rel < self.tr {
            if self.tr > 0.0 {
                self.v1 + (self.v2 - self.v1) * (t_rel / self.tr)
            } else {
                self.v2
            }
        } else if t_rel < self.tr + self.pw {
            self.v2
        } else if t_rel < self.tr + self.pw + self.tf {
            if self.tf > 0.0 {
                let fall_time = t_rel - self.tr - self.pw;
                self.v2 - (self.v2 - self.v1) * (fall_time / self.tf)
            } else {
                self.v1
            }
        } else {
            self.v1
        }
    }
}

use std::f64::consts::PI;

#[derive(Debug, Clone)]
pub struct Sine {
    pub vo: f64,
    pub va: f64,
    pub freq: f64,
    pub td: f64,
    pub theta: f64,
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
    pub sine: Option<Sine>,
}

impl VoltageSource {
    pub fn value_at(&self, t: f64) -> f64 {
        if let Some(ref pulse) = self.pulse {
            pulse.value_at(t)
        } else if let Some(ref sine) = self.sine {
            sine.value_at(t)
        } else {
            self.voltage
        }
    }
}

// ─── Circuit ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct Circuit {
    pub name: String,
    pub nodes: usize,
    pub resistors: Vec<Resistor>,
    pub capacitors: Vec<Capacitor>,
    pub inductors: Vec<Inductor>,
    pub voltage_sources: Vec<VoltageSource>,
    pub diode_models: HashMap<String, DiodeModel>,
    pub diodes: Vec<Diode>,
    pub mosfet_models: HashMap<String, MosfetModel>,
    pub mosfets: Vec<Mosfet>,
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
                    "resistor {} must have R > 0", r.name
                )));
            }
        }
        for c in &self.capacitors {
            if c.capacitance <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "capacitor {} must have C > 0", c.name
                )));
            }
        }
        for l in &self.inductors {
            if l.inductance <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "inductor {} must have L > 0", l.name
                )));
            }
        }
        for m in &self.mosfets {
            if m.w <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "MOSFET {} must have W > 0", m.name
                )));
            }
            if m.l <= 0.0 {
                return Err(SimError::Circuit(format!(
                    "MOSFET {} must have L > 0", m.name
                )));
            }
            if !self.mosfet_models.contains_key(&m.model) {
                return Err(SimError::Circuit(format!(
                    "MOSFET {} references unknown model '{}'", m.name, m.model
                )));
            }
        }
        for v in &self.voltage_sources {
            if let Some(ref p) = v.pulse {
                if p.td < 0.0 || p.tr < 0.0 || p.tf < 0.0 || p.pw < 0.0 || p.per < 0.0 {
                    return Err(SimError::Circuit(format!(
                        "voltage source {} pulse timings cannot be negative", v.name
                    )));
                }
                if p.per > 0.0 && p.per < (p.tr + p.pw + p.tf) {
                    return Err(SimError::Circuit(format!(
                        "voltage source {} pulse period must be greater than active duration (tr + pw + tf)",
                        v.name
                    )));
                }
            }
            if let Some(ref s) = v.sine {
                if s.freq < 0.0 || s.td < 0.0 {
                    return Err(SimError::Circuit(format!(
                        "voltage source {} sine frequency and delay cannot be negative", v.name
                    )));
                }
            }
        }
        Ok(())
    }
}
