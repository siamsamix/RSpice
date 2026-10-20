use crate::circuit::{Capacitor, Circuit, Inductor, Mosfet, MosfetModel, MosfetType, NodeId, Pulse, Resistor, VoltageSource, Sine, Diode, Ac};
use crate::error::{Result, SimError};
use crate::units::parse_value;
use crate::circuit::DiodeModel;

/// Supported sweep scale (mirrors SPICE `.ac` syntax).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AcScale {
    /// Logarithmically spaced points per decade.
    Dec,
    /// Logarithmically spaced points per octave.
    Oct,
    /// Linearly spaced points.
    Lin,
}

/// Parameters extracted from a `.ac` command.
#[derive(Debug, Clone)]
pub struct AcCommand {
    pub scale: AcScale,
    /// Points per decade/octave, or total linear points.
    pub points: usize,
    /// Start frequency (Hz).
    pub f_start: f64,
    /// Stop frequency (Hz).
    pub f_stop: f64,
    /// Index of the voltage source to use as the AC stimulus (0-based into
    /// `circuit.voltage_sources`).  `None` = drive the first source.
    pub stimulus_source: Option<usize>,
}

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
            analysis.ac = Some(AcCommand {
                scale, points, f_start, f_stop, stimulus_source,
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
    let mut ac = None;
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
        }else if token_lower == "ac" {
            i += 1;
            if i >= tokens.len() {
                return Err(SimError::Parse("voltage source missing AC magnitude".into()));
            }
            let mag = parse_value(tokens[i]).map_err(SimError::Parse)?;
            i += 1;

            let mut phase = 0.0;
            // Phase is optional. If the next token is a valid value, parse it as phase.
            if i < tokens.len() {
                if let Ok(p) = parse_value(tokens[i]) {
                    phase = p;
                    i += 1;
                }
            }
            ac = Some(Ac { mag, phase });
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
        ac,
    });
    Ok(())
}

/// Emit the canonical textual form of a parsed netlist — the inverse of
/// [`parse`]: model cards, element cards, dot commands, `.end`.
///
/// Numeric values print through `f64` `Display` (plain decimal notation,
/// always re-parseable by `parse_value`, unlike a value the user typed with a
/// suffix); non-GUI source waveforms (`PULSE`, `SIN`) carry their stored
/// parameters so a round trip preserves them. Model cards are emitted sorted
/// by name so the output is deterministic despite the HashMap storage.
pub fn emit(netlist: &Netlist) -> String {
    let circuit = &netlist.circuit;
    let analysis = &netlist.analysis;
    let mut out = String::from("* Canonical netlist\n");

    // ── Model cards (sorted for deterministic output) ────────────────────
    let mut diode_names: Vec<&String> = circuit.diode_models.keys().collect();
    diode_names.sort();
    for name in diode_names {
        let m = &circuit.diode_models[name];
        out.push_str(&format!(".model {} D(IS={} N={})\n", name, m.is, m.n));
    }
    let mut mosfet_names: Vec<&String> = circuit.mosfet_models.keys().collect();
    mosfet_names.sort();
    for name in mosfet_names {
        let m = &circuit.mosfet_models[name];
        let kind = match m.kind {
            MosfetType::Nmos => "NMOS",
            MosfetType::Pmos => "PMOS",
        };
        out.push_str(&format!(
            ".model {} {}(KP={} VTH0={} LAMBDA={} GAMMA={} PHI={})\n",
            name, kind, m.kp, m.vth0, m.lambda, m.gamma, m.phi
        ));
    }

    // ── Element cards ────────────────────────────────────────────────────
    for r in &circuit.resistors {
        out.push_str(&format!("{} {} {} {}\n", r.name, r.n1.0, r.n2.0, r.resistance));
    }
    for c in &circuit.capacitors {
        out.push_str(&format!("{} {} {} {}\n", c.name, c.n1.0, c.n2.0, c.capacitance));
    }
    for l in &circuit.inductors {
        out.push_str(&format!("{} {} {} {}\n", l.name, l.n1.0, l.n2.0, l.inductance));
    }
    for d in &circuit.diodes {
        out.push_str(&format!("{} {} {} {}\n", d.name, d.anode.0, d.cathode.0, d.model));
    }
    for m in &circuit.mosfets {
        out.push_str(&format!(
            "{} {} {} {} {} {} W={} L={}\n",
            m.name, m.nd.0, m.ng.0, m.ns.0, m.nb.0, m.model, m.w, m.l
        ));
    }
    for v in &circuit.voltage_sources {
        // `DC <value>` is redundant for pulse/sine sources (the value equals
        // their starting level) but keeps every source's operating point.
        out.push_str(&format!("{} {} {} DC {}", v.name, v.n1.0, v.n2.0, v.voltage));
        if let Some(ac) = &v.ac {
            out.push_str(&format!(" AC {}", ac.mag));
            if ac.phase != 0.0 {
                out.push_str(&format!(" {}", ac.phase));
            }
        }
        if let Some(p) = &v.pulse {
            out.push_str(&format!(
                " PULSE({} {} {} {} {} {} {})",
                p.v1, p.v2, p.td, p.tr, p.tf, p.pw, p.per
            ));
        }
        if let Some(s) = &v.sine {
            out.push_str(&format!(" SIN({} {} {} {} {})", s.vo, s.va, s.freq, s.td, s.theta));
        }
        out.push('\n');
    }

    // ── Analysis commands ────────────────────────────────────────────────
    if analysis.dc_op {
        out.push_str(".op\n");
    }
    if let (Some(step), Some(stop)) = (analysis.tran_step, analysis.tran_stop) {
        if analysis.tran_start != 0.0 {
            out.push_str(&format!(".tran {} {} {}\n", step, stop, analysis.tran_start));
        } else {
            out.push_str(&format!(".tran {} {}\n", step, stop));
        }
    }
    if let Some(ac) = &analysis.ac {
        let scale = match ac.scale {
            AcScale::Dec => "DEC",
            AcScale::Oct => "OCT",
            AcScale::Lin => "LIN",
        };
        let src = match ac.stimulus_source {
            Some(i) => format!(" {}", i),
            None => String::new(),
        };
        out.push_str(&format!(".ac {} {} {} {}{}\n", scale, ac.points, ac.f_start, ac.f_stop, src));
    }
    out.push_str(".end\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parse(emit(x))` preserves the language-relevant facts of a
    /// representative netlist: elements, model cards, waveforms, commands.
    #[test]
    fn emit_round_trips_through_parse() {
        let src = "* roundtrip\n\
                   .model dino D(IS=1e-14 N=1.05)\n\
                   .model nh NMOS(KP=2e-4 VTH0=0.7 LAMBDA=0.02 GAMMA=0.5 PHI=0.6)\n\
                   R1 1 0 1k\n\
                   C1 1 2 1u\n\
                   L1 2 0 1m\n\
                   D1 1 0 dino\n\
                   M1 2 1 0 0 nh W=2u L=0.18u\n\
                   VS 1 0 DC 5 AC 1 30 PULSE(0 5 1u 2u 2u 5u 10u)\n\
                   .op\n\
                   .tran 1u 5m\n\
                   .ac DEC 10 1 10k\n\
                   .end\n";
        let first = parse(src).expect("first parse");
        let text = emit(&first);
        let second = parse(&text).expect("re-parse of emitted text");

        assert_eq!(second.circuit.nodes, first.circuit.nodes);
        assert_eq!(second.circuit.resistors.len(), 1);
        assert_eq!(second.circuit.capacitors.len(), 1);
        assert_eq!(second.circuit.inductors.len(), 1);
        assert_eq!(second.circuit.diodes.len(), 1);
        assert_eq!(second.circuit.mosfets.len(), 1);
        assert_eq!(second.circuit.voltage_sources.len(), 1);

        let m = &second.circuit.mosfets[0];
        assert_eq!(m.model, "nh");
        assert_eq!(m.w, parse_value("2u").unwrap());
        assert_eq!(m.l, parse_value("0.18u").unwrap());

        let v = &second.circuit.voltage_sources[0];
        assert_eq!(v.voltage, 5.0);
        let ac = v.ac.as_ref().unwrap();
        assert_eq!(ac.mag, 1.0);
        assert_eq!(ac.phase, 30.0);
        let pulse = v.pulse.as_ref().unwrap();
        assert_eq!(pulse.v1, 0.0);
        assert_eq!(pulse.v2, 5.0);
        // 10×1e-6 is not bit-identical to the 10e-6 literal, so compare against
        // what the parser actually produced: the round trip must be exact.
        assert_eq!(pulse.per, parse_value("10u").unwrap());

        let a = second.analysis.ac.as_ref().unwrap();
        assert!(matches!(a.scale, AcScale::Dec));
        assert_eq!(a.points, 10);
        assert_eq!(a.f_start, 1.0);
        assert_eq!(a.f_stop, parse_value("10k").unwrap());
        assert_eq!(second.analysis.tran_step, Some(parse_value("1u").unwrap()));
        assert_eq!(second.analysis.tran_stop, Some(parse_value("5m").unwrap()));
        assert!(second.analysis.dc_op);
    }
}
