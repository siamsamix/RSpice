use crate::circuit::{Capacitor, Circuit, Inductor, NodeId, Resistor, VoltageSource};
use crate::error::{Result, SimError};
use crate::units::parse_value;

#[derive(Debug, Clone, Default)]
pub struct AnalysisCommands {
    pub dc_op: bool,
    pub tran_step: Option<f64>,
    pub tran_stop: Option<f64>,
    pub tran_start: f64,
}

#[derive(Debug, Clone)]
pub struct Netlist {
    pub circuit: Circuit,
    pub analysis: AnalysisCommands,
}

pub fn parse(source: &str) -> Result<Netlist> {
    let mut circuit = Circuit::new("untitled");
    let mut analysis = AnalysisCommands::default();
    let mut line_no = 0usize;

    for raw in source.lines() {
        line_no += 1;
        let stripped = strip_comment(raw);
        let line = stripped.trim();
        if line.is_empty() {
            continue;
        }

        let lower = line.to_ascii_lowercase();
        if lower == ".end" {
            break;
        }
        if lower.starts_with('.') {
            parse_dot_command(line, &mut analysis)?;
            continue;
        }

        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 4 {
            return Err(SimError::Parse(format!(
                "line {line_no}: expected at least 4 fields"
            )));
        }

        let kind = tokens[0]
            .chars()
            .next()
            .ok_or_else(|| SimError::Parse(format!("line {line_no}: empty element")))?;
        match kind.to_ascii_uppercase() {
            'R' => parse_resistor(&tokens, &mut circuit)?,
            'C' => parse_capacitor(&tokens, &mut circuit)?,
            'L' => parse_inductor(&tokens, &mut circuit)?,
            'V' => parse_voltage_source(&tokens, &mut circuit)?,
            _ => {
                return Err(SimError::Parse(format!(
                    "line {line_no}: unsupported element '{kind}'"
                )));
            }
        }
    }

    circuit.validate()?;
    Ok(Netlist { circuit, analysis })
}

fn strip_comment(line: &str) -> String {
    let mut out = String::new();
    let mut in_string = false;
    for ch in line.chars() {
        match ch {
            '"' if !in_string => in_string = true,
            '"' if in_string => in_string = false,
            '*' | ';' if !in_string => return out,
            _ => out.push(ch),
        }
    }
    out
}

fn parse_dot_command(line: &str, analysis: &mut AnalysisCommands) -> Result<()> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let cmd = tokens[0].to_ascii_lowercase();
    match cmd.as_str() {
        ".op" => analysis.dc_op = true,
        ".tran" => {
            if tokens.len() < 3 {
                return Err(SimError::Parse(".tran requires: .tran <tstep> <tstop> [tstart]".into()));
            }
            analysis.tran_step = Some(parse_value(tokens[1]).map_err(SimError::Parse)?);
            analysis.tran_stop = Some(parse_value(tokens[2]).map_err(SimError::Parse)?);
            if let Some(t) = tokens.get(3) {
                analysis.tran_start = parse_value(t).map_err(SimError::Parse)?;
            }
        }
        ".dc" => analysis.dc_op = true,
        _ => return Err(SimError::Parse(format!("unknown command '{}'", tokens[0]))),
    }
    Ok(())
}

fn parse_node(token: &str, circuit: &mut Circuit) -> Result<NodeId> {
    let id: usize = token
        .parse()
        .map_err(|_| SimError::Parse(format!("invalid node '{token}'")))?;
    circuit.ensure_node(id);
    Ok(NodeId(id))
}

fn parse_resistor(tokens: &[&str], circuit: &mut Circuit) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit)?;
    let n2 = parse_node(tokens[2], circuit)?;
    let resistance = parse_value(tokens[3]).map_err(SimError::Parse)?;
    circuit.resistors.push(Resistor {
        name: tokens[0].to_string(),
        n1,
        n2,
        resistance,
    });
    Ok(())
}

fn parse_capacitor(tokens: &[&str], circuit: &mut Circuit) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit)?;
    let n2 = parse_node(tokens[2], circuit)?;
    let capacitance = parse_value(tokens[3]).map_err(SimError::Parse)?;
    circuit.capacitors.push(Capacitor {
        name: tokens[0].to_string(),
        n1,
        n2,
        capacitance,
    });
    Ok(())
}

fn parse_inductor(tokens: &[&str], circuit: &mut Circuit) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit)?;
    let n2 = parse_node(tokens[2], circuit)?;
    let inductance = parse_value(tokens[3]).map_err(SimError::Parse)?;
    circuit.inductors.push(Inductor {
        name: tokens[0].to_string(),
        n1,
        n2,
        inductance,
    });
    Ok(())
}

fn parse_voltage_source(tokens: &[&str], circuit: &mut Circuit) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit)?;
    let n2 = parse_node(tokens[2], circuit)?;
    let mut voltage = 0.0;
    let mut i = 3;
    while i < tokens.len() {
        if tokens[i].eq_ignore_ascii_case("dc") {
            i += 1;
            if i >= tokens.len() {
                return Err(SimError::Parse("voltage source missing DC value".into()));
            }
            voltage = parse_value(tokens[i]).map_err(SimError::Parse)?;
            break;
        }
        if tokens[i].eq_ignore_ascii_case("pulse")
            || tokens[i].eq_ignore_ascii_case("sin")
            || tokens[i].eq_ignore_ascii_case("exp")
        {
            return Err(SimError::Parse(
                "time-varying sources not yet supported; use DC".into(),
            ));
        }
        i += 1;
    }
    if i >= tokens.len() && tokens.len() == 4 {
        voltage = parse_value(tokens[3]).map_err(SimError::Parse)?;
    }
    circuit.voltage_sources.push(VoltageSource {
        name: tokens[0].to_string(),
        n1,
        n2,
        voltage,
    });
    Ok(())
}
