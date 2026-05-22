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
pub struct VoltageSource {
    pub name: String,
    pub n1: NodeId,
    pub n2: NodeId,
    pub voltage: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Circuit {
    pub name: String,
    pub nodes: usize,
    pub resistors: Vec<Resistor>,
    pub capacitors: Vec<Capacitor>,
    pub inductors: Vec<Inductor>,
    pub voltage_sources: Vec<VoltageSource>,
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
        Ok(())
    }
}
