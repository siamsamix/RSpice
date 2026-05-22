use crate::circuit::{Capacitor, Circuit, Inductor, NodeId, Pulse, Resistor, VoltageSource};
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

    let mut dc_voltage = None;
    let mut pulse = None;
    let mut i = 3;

    while i < tokens.len() {
        let token_lower = tokens[i].to_ascii_lowercase();
        if token_lower == "dc" {
            i += 1;
            if i >= tokens.len() {
                return Err(SimError::Parse("voltage source missing DC value".into()));
            }
            dc_voltage = Some(parse_value(tokens[i]).map_err(SimError::Parse)?);
            i += 1;
        } else if token_lower.starts_with("pulse") {
            // Join remainder of the tokens to accurately capture the parameter arguments
            let remainder = tokens[i..].join(" ");
            let cleaned = remainder
            .to_ascii_lowercase()
            .replace("pulse", "")
            .replace("(", "")
            .replace(")", "")
            .replace(",", " ");

            let p_tokens: Vec<&str> = cleaned.split_whitespace().collect();
            if p_tokens.len() < 7 {
                return Err(SimError::Parse(
                    "PULSE source requires 7 parameters: V1 V2 TD TR TF PW PER".into(),
                ));
            }

            let v1 = parse_value(p_tokens[0]).map_err(SimError::Parse)?;
            let v2 = parse_value(p_tokens[1]).map_err(SimError::Parse)?;
            let td = parse_value(p_tokens[2]).map_err(SimError::Parse)?;
            let tr = parse_value(p_tokens[3]).map_err(SimError::Parse)?;
            let tf = parse_value(p_tokens[4]).map_err(SimError::Parse)?;
            let pw = parse_value(p_tokens[5]).map_err(SimError::Parse)?;
            let per = parse_value(p_tokens[6]).map_err(SimError::Parse)?;

            pulse = Some(Pulse { v1, v2, td, tr, tf, pw, per });
            break; // The pulse specification consumes the rest of the source tokens
        } else if token_lower.starts_with("sin") || token_lower.starts_with("exp") {
            return Err(SimError::Parse(format!(
                "waveform '{}' is not yet supported; use DC or PULSE",
                tokens[i]
            )));
        } else {
            // Fall back to supporting positional raw numbers (e.g. `V1 1 0 5V`)
            if i == 3 && tokens.len() == 4 {
                dc_voltage = Some(parse_value(tokens[3]).map_err(SimError::Parse)?);
                break;
            } else {
                return Err(SimError::Parse(format!(
                    "unexpected token '{}' in voltage source definition",
                    tokens[i]
                )));
            }
        }
    }

    // Determine the baseline value used during DC operation points (.op)
    let final_voltage = match (dc_voltage, &pulse) {
        (Some(v), _) => v,
        (None, Some(p)) => p.v1, // Standard SPICE fallback rule uses V1 if DC keyword is absent
        (None, None) => 0.0,
    };

    circuit.voltage_sources.push(VoltageSource {
        name: tokens[0].to_string(),
                                 n1,
                                 n2,
                                 voltage: final_voltage,
                                 pulse,
    });
    Ok(())
}
