use crate::analysis::{AcCommand, AcScale};
use crate::circuit::{
    Capacitor, Circuit, Inductor, Mosfet, MosfetModel, MosfetType, NodeId, Pulse, Resistor,
    VoltageSource, Sine, Diode,
};
use crate::error::{Result, SimError};
use crate::units::parse_value;
use crate::circuit::DiodeModel;

#[derive(Debug, Clone, Default)]
pub struct AnalysisCommands {
    pub dc_op: bool,
    pub tran_step: Option<f64>,
    pub tran_stop: Option<f64>,
    pub tran_start: f64,
    pub ac: Option<AcCommand>,
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
            parse_dot_command(line, &mut analysis, &mut circuit)?;
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
            'D' => parse_diode(&tokens, &mut circuit)?,
            'V' => parse_voltage_source(&tokens, &mut circuit)?,
            'M' => parse_mosfet(&tokens, &mut circuit)?,
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

fn extract_model_param(body: &str, param: &str) -> Option<f64> {
    body.find(param).map(|idx| {
        let start = idx + param.len();
        let end = body[start..]
            .find(|c: char| !c.is_numeric() && c != '.' && c != 'e' && c != '-')
            .unwrap_or(body[start..].len());
        body[start..start + end].parse().unwrap_or(0.0)
    })
}

fn parse_dot_command(
    line: &str,
    analysis: &mut AnalysisCommands,
    circuit: &mut Circuit,
) -> Result<()> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let cmd = tokens[0].to_ascii_lowercase();
    match cmd.as_str() {
        ".model" => {
            if tokens.len() < 3 {
                return Err(SimError::Parse(".model requires at least a name and type".into()));
            }
            let name = tokens[1].to_string();
            let type_token = tokens[2].to_ascii_lowercase();
            let body = tokens[2..].join("").to_ascii_lowercase();

            if type_token.starts_with('d') {
                // Diode model: .model <name> D(IS=1e-14 N=1.0)
                let is = extract_model_param(&body, "is=").unwrap_or(1e-14);
                let n = extract_model_param(&body, "n=").unwrap_or(1.0);
                circuit.diode_models.insert(name, DiodeModel { is, n });
            } else if type_token.starts_with("nmos") || type_token.starts_with("pmos") {
                // MOSFET model: .model <name> NMOS(KP=20e-6 VTH0=0.7 LAMBDA=0.01 ...)
                let kind = if type_token.starts_with("nmos") {
                    MosfetType::Nmos
                } else {
                    MosfetType::Pmos
                };
                let kp     = extract_model_param(&body, "kp=").unwrap_or(20e-6);
                let vth0   = extract_model_param(&body, "vth0=")
                    .or_else(|| extract_model_param(&body, "vto="))
                    .unwrap_or(0.7);
                let lambda = extract_model_param(&body, "lambda=").unwrap_or(0.0);
                let gamma  = extract_model_param(&body, "gamma=").unwrap_or(0.0);
                let phi    = extract_model_param(&body, "phi=").unwrap_or(0.6);
                circuit.mosfet_models.insert(name, MosfetModel { kind, kp, vth0, lambda, gamma, phi });
            } else {
                return Err(SimError::Parse(format!(
                    "unknown .model type '{}' — supported: D, NMOS, PMOS", tokens[2]
                )));
            }
        }
        ".op" => analysis.dc_op = true,
        ".tran" => {
            if tokens.len() < 3 {
                return Err(SimError::Parse(
                    ".tran requires: .tran <tstep> <tstop> [tstart]".into(),
                ));
            }
            analysis.tran_step = Some(parse_value(tokens[1]).map_err(SimError::Parse)?);
            analysis.tran_stop = Some(parse_value(tokens[2]).map_err(SimError::Parse)?);
            if let Some(t) = tokens.get(3) {
                analysis.tran_start = parse_value(t).map_err(SimError::Parse)?;
            }
        }
        ".ac" => {
            if tokens.len() < 5 {
                return Err(SimError::Parse(
                    ".ac requires: .ac <DEC|OCT|LIN> <points> <fstart> <fstop> [src_idx [amplitude]]"
                        .into(),
                ));
            }
            let scale = match tokens[1].to_ascii_lowercase().as_str() {
                "dec" => AcScale::Dec,
                "oct" => AcScale::Oct,
                "lin" => AcScale::Lin,
                other => {
                    return Err(SimError::Parse(format!(
                        ".ac sweep type must be DEC, OCT, or LIN — got '{}'", other
                    )));
                }
            };
            let points: usize = tokens[2]
                .parse()
                .map_err(|_| SimError::Parse(format!("invalid .ac point count '{}'", tokens[2])))?;
            let f_start = parse_value(tokens[3]).map_err(SimError::Parse)?;
            let f_stop  = parse_value(tokens[4]).map_err(SimError::Parse)?;
            let stimulus_source = tokens
                .get(5)
                .map(|s| s.parse::<usize>())
                .transpose()
                .map_err(|_| SimError::Parse("invalid .ac source index".into()))?;
            let stimulus_amplitude = tokens
                .get(6)
                .map(|s| parse_value(s).map_err(SimError::Parse))
                .transpose()?
                .unwrap_or(1.0);
            analysis.ac = Some(AcCommand {
                scale, points, f_start, f_stop, stimulus_source, stimulus_amplitude,
            });
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

fn parse_diode(tokens: &[&str], circuit: &mut Circuit) -> Result<()> {
    if tokens.len() < 4 {
        return Err(SimError::Parse(
            "Diode requires: Dname Anode Cathode Model".into(),
        ));
    }
    let anode   = parse_node(tokens[1], circuit)?;
    let cathode = parse_node(tokens[2], circuit)?;
    let model   = tokens[3].to_string();
    circuit.diodes.push(Diode { name: tokens[0].to_string(), anode, cathode, model });
    Ok(())
}

/// Parse a MOSFET element line.
///
/// SPICE syntax:
///   Mname  Nd  Ng  Ns  Nb  ModelName  [W=<w>]  [L=<l>]
///
/// W and L default to 1µm if not supplied.
fn parse_mosfet(tokens: &[&str], circuit: &mut Circuit) -> Result<()> {
    if tokens.len() < 6 {
        return Err(SimError::Parse(
            "MOSFET requires: Mname Nd Ng Ns Nb ModelName [W=<w>] [L=<l>]".into(),
        ));
    }
    let nd = parse_node(tokens[1], circuit)?;
    let ng = parse_node(tokens[2], circuit)?;
    let ns = parse_node(tokens[3], circuit)?;
    let nb = parse_node(tokens[4], circuit)?;
    let model = tokens[5].to_string();

    let mut w = 1e-6; // 1 µm default
    let mut l = 1e-6;

    for tok in &tokens[6..] {
        let lower = tok.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("w=") {
            w = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Some(rest) = lower.strip_prefix("l=") {
            l = parse_value(rest).map_err(SimError::Parse)?;
        }
    }

    circuit.mosfets.push(Mosfet {
        name: tokens[0].to_string(),
        nd, ng, ns, nb, model, w, l,
    });
    Ok(())
}

fn parse_resistor(tokens: &[&str], circuit: &mut Circuit) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit)?;
    let n2 = parse_node(tokens[2], circuit)?;
    let resistance = parse_value(tokens[3]).map_err(SimError::Parse)?;
    circuit.resistors.push(Resistor { name: tokens[0].to_string(), n1, n2, resistance });
    Ok(())
}

fn parse_capacitor(tokens: &[&str], circuit: &mut Circuit) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit)?;
    let n2 = parse_node(tokens[2], circuit)?;
    let capacitance = parse_value(tokens[3]).map_err(SimError::Parse)?;
    circuit.capacitors.push(Capacitor { name: tokens[0].to_string(), n1, n2, capacitance });
    Ok(())
}

fn parse_inductor(tokens: &[&str], circuit: &mut Circuit) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit)?;
    let n2 = parse_node(tokens[2], circuit)?;
    let inductance = parse_value(tokens[3]).map_err(SimError::Parse)?;
    circuit.inductors.push(Inductor { name: tokens[0].to_string(), n1, n2, inductance });
    Ok(())
}

fn parse_voltage_source(tokens: &[&str], circuit: &mut Circuit) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit)?;
    let n2 = parse_node(tokens[2], circuit)?;

    let mut dc_voltage = None;
    let mut pulse = None;
    let mut sine = None;
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
            let remainder = tokens[i..].join(" ");
            let cleaned = remainder
                .to_ascii_lowercase()
                .replace("pulse", "")
                .replace('(', "")
                .replace(')', "")
                .replace(',', " ");
            let p_tokens: Vec<&str> = cleaned.split_whitespace().collect();
            if p_tokens.len() < 7 {
                return Err(SimError::Parse(
                    "PULSE source requires 7 parameters: V1 V2 TD TR TF PW PER".into(),
                ));
            }
            let v1  = parse_value(p_tokens[0]).map_err(SimError::Parse)?;
            let v2  = parse_value(p_tokens[1]).map_err(SimError::Parse)?;
            let td  = parse_value(p_tokens[2]).map_err(SimError::Parse)?;
            let tr  = parse_value(p_tokens[3]).map_err(SimError::Parse)?;
            let tf  = parse_value(p_tokens[4]).map_err(SimError::Parse)?;
            let pw  = parse_value(p_tokens[5]).map_err(SimError::Parse)?;
            let per = parse_value(p_tokens[6]).map_err(SimError::Parse)?;
            pulse = Some(Pulse { v1, v2, td, tr, tf, pw, per });
            break;
        } else if token_lower.starts_with("sin") {
            let remainder = tokens[i..].join(" ");
            let cleaned = remainder
                .to_ascii_lowercase()
                .replace("sin", "")
                .replace('(', "")
                .replace(')', "")
                .replace(',', " ");
            let s_tokens: Vec<&str> = cleaned.split_whitespace().collect();
            if s_tokens.len() < 3 {
                return Err(SimError::Parse(
                    "SIN source requires at least 3 parameters: VO VA FREQ [TD [THETA]]".into(),
                ));
            }
            let vo   = parse_value(s_tokens[0]).map_err(SimError::Parse)?;
            let va   = parse_value(s_tokens[1]).map_err(SimError::Parse)?;
            let freq = parse_value(s_tokens[2]).map_err(SimError::Parse)?;
            let td    = if s_tokens.len() > 3 { parse_value(s_tokens[3]).map_err(SimError::Parse)? } else { 0.0 };
            let theta = if s_tokens.len() > 4 { parse_value(s_tokens[4]).map_err(SimError::Parse)? } else { 0.0 };
            sine = Some(Sine { vo, va, freq, td, theta });
            break;
        } else if i == 3 && tokens.len() == 4 {
            dc_voltage = Some(parse_value(tokens[3]).map_err(SimError::Parse)?);
            break;
        } else {
            return Err(SimError::Parse(format!(
                "unexpected token '{}' in voltage source definition", tokens[i]
            )));
        }
    }

    let final_voltage = match (dc_voltage, &pulse, &sine) {
        (Some(v), _, _) => v,
        (None, Some(p), _) => p.v1,
        (None, None, Some(s)) => s.vo,
        (None, None, None) => 0.0,
    };

    circuit.voltage_sources.push(VoltageSource {
        name: tokens[0].to_string(),
        n1, n2,
        voltage: final_voltage,
        pulse,
        sine,
    });
    Ok(())
}
