//! Element cards: one parser per element family beside the canonical

//! emission of that family's cards.

use super::*;
use super::dotcards::ignored_tokens;
use super::waveforms::waveform_text;
use super::nodeplan::{element_letter, parse_node};
use super::subckts::{poly_count, q_is_four_terminal};
use super::waveforms::parse_source_value_specs;

// ─── Element cards ───────────────────────────────────────────────────────────

/// SPICE syntax:
///   Dname  Anode  Cathode  ModelName  [AREA=<a>] [<a>] [OFF] [M=<m>]
///
/// The junction area multiplier scales the saturation current in the stamp
/// (TODO §2); the bare-numeric form (area as the 5th token) and the
/// `AREA=` keyword are both accepted.
pub(super) fn parse_diode(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 4 {
        return Err(SimError::Parse(
            "Diode requires: Dname Anode Cathode Model [AREA=<a>]".into(),
        ));
    }
    let anode   = parse_node(tokens[1], circuit, plan)?;
    let cathode = parse_node(tokens[2], circuit, plan)?;
    let model = tokens[3].to_ascii_lowercase();

    let mut area = 1.0;
    for tok in &tokens[4..] {
        let lower = tok.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("area=") {
            area = parse_value(rest).map_err(SimError::Parse)?;
        } else if lower.strip_prefix("m=").is_some() {
            warnings.push(format!(
                "line {line_no}: diode {}: the M= multiplier is parsed but not applied",
                tokens[0]
            ));
        } else if lower == "off" {
            warnings.push(format!(
                "line {line_no}: diode {}: OFF is parsed but not applied",
                tokens[0]
            ));
        } else if let Ok(a) = parse_value(tok) {
            area = a;
        } else {
            warnings.push(format!(
                "line {line_no}: diode {}: token '{tok}' is not supported — ignored",
                tokens[0]
            ));
        }
    }

    circuit.diodes.push(Diode { name: tokens[0].to_string(), anode, cathode, model, area });
    Ok(())
}

/// Parse a MOSFET element line.
///
/// SPICE syntax:
///   Mname  Nd  Ng  Ns  Nb  ModelName  [W=<w>]  [L=<l>]
///
/// W and L default to 1µm if not supplied.
pub(super) fn parse_mosfet(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 6 {
        return Err(SimError::Parse(
            "MOSFET requires: Mname Nd Ng Ns Nb ModelName [W=<w>] [L=<l>] \
             [AD=<ad>] [AS=<as>] [PS=<ps>] [PD=<pd>]"
                .into(),
        ));
    }
    let nd = parse_node(tokens[1], circuit, plan)?;
    let ng = parse_node(tokens[2], circuit, plan)?;
    let ns = parse_node(tokens[3], circuit, plan)?;
    let nb = parse_node(tokens[4], circuit, plan)?;
    let model = tokens[5].to_ascii_lowercase();

    let mut w = 1e-6; // 1 µm default
    let mut l = 1e-6;
    // Per-instance junction sizing (TODO §2): parsed and stored for the
    // capacitance work (TODO §5); not stamped yet.
    let mut ad = 0.0;
    let mut asr = 0.0;
    let mut ps = 0.0;
    let mut pd = 0.0;

    for tok in &tokens[6..] {
        let lower = tok.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("w=") {
            w = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Some(rest) = lower.strip_prefix("l=") {
            l = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Some(rest) = lower.strip_prefix("ad=") {
            ad = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Some(rest) = lower.strip_prefix("as=") {
            asr = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Some(rest) = lower.strip_prefix("ps=") {
            ps = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Some(rest) = lower.strip_prefix("pd=") {
            pd = parse_value(rest).map_err(SimError::Parse)?;
        } else if lower.strip_prefix("m=").is_some() {
            warnings.push(format!(
                "line {line_no}: MOSFET {}: the M= multiplier is parsed but not applied",
                tokens[0]
            ));
        } else if lower == "off" {
            warnings.push(format!(
                "line {line_no}: MOSFET {}: OFF is parsed but not applied",
                tokens[0]
            ));
        } else {
            warnings.push(format!(
                "line {line_no}: MOSFET {}: token '{tok}' is not supported — ignored",
                tokens[0]
            ));
        }
    }

    circuit.mosfets.push(Mosfet {
        name: tokens[0].to_string(),
        nd, ng, ns, nb, model, w, l,
        ad, asr, ps, pd,
    });
    Ok(())
}

/// Parse one `E`/`F`/`G`/`H` controlled source, either the plain gain form
/// (`E out+ out- ctrl+ ctrl- gain`) or `POLY(n)` with one control per
/// variable and `n+1` coefficients (constant first).
pub(super) fn parse_controlled_source(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    line_no: usize,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let kind = element_letter(tokens[0]).to_ascii_uppercase();
    if tokens.len() < 5 {
        return Err(SimError::Parse(format!(
            "line {line_no}: {kind} source requires: {kind}name out+ out- <controls> <gain|POLY(n) …>"
        )));
    }
    let out_p = parse_node(tokens[1], circuit, plan)?;
    let out_n = parse_node(tokens[2], circuit, plan)?;
    let voltage_controlled = matches!(kind, 'E' | 'G');

    let is_poly = tokens[3].to_ascii_lowercase().starts_with("poly");
    let (controls, coeffs, consumed): (Vec<ControlVar>, Vec<f64>, usize) = if is_poly {
        let n = poly_count(tokens[3]).ok_or_else(|| {
            SimError::Parse(format!(
                "line {line_no}: {kind} source '{}': malformed POLY(n) token '{}'",
                tokens[0], tokens[3]
            ))
        })?;
        if n == 0 {
            return Err(SimError::Parse(format!(
                "line {line_no}: {kind} source '{}': POLY needs at least one control",
                tokens[0]
            )));
        }
        if voltage_controlled {
            let need = 4 + 2 * n + (n + 1);
            if tokens.len() < need {
                return Err(SimError::Parse(format!(
                    "line {line_no}: {kind} source '{}': POLY({n}) needs {n} node pairs and {} coefficients",
                    tokens[0],
                    n + 1
                )));
            }
            let mut controls = Vec::with_capacity(n);
            for k in 0..n {
                let a = parse_node(tokens[4 + 2 * k], circuit, plan)?;
                let b = parse_node(tokens[5 + 2 * k], circuit, plan)?;
                controls.push(ControlVar::Voltage(a, b));
            }
            let mut coeffs = Vec::with_capacity(n + 1);
            for tok in &tokens[4 + 2 * n..need] {
                coeffs.push(parse_value(tok).map_err(SimError::Parse)?);
            }
            (controls, coeffs, need)
        } else {
            let need = 4 + n + (n + 1);
            if tokens.len() < need {
                return Err(SimError::Parse(format!(
                    "line {line_no}: {kind} source '{}': POLY({n}) needs {n} source name(s) and {} coefficients",
                    tokens[0],
                    n + 1
                )));
            }
            let controls = tokens[4..4 + n]
                .iter()
                .map(|s| ControlVar::Current((*s).to_string()))
                .collect();
            let mut coeffs = Vec::with_capacity(n + 1);
            for tok in &tokens[4 + n..need] {
                coeffs.push(parse_value(tok).map_err(SimError::Parse)?);
            }
            (controls, coeffs, need)
        }
    } else if voltage_controlled {
        if tokens.len() < 6 {
            return Err(SimError::Parse(format!(
                "line {line_no}: {kind} source '{}' requires: {kind}{{name}} out+ out- ctrl+ ctrl- gain",
                tokens[0]
            )));
        }
        let cp = parse_node(tokens[3], circuit, plan)?;
        let cn = parse_node(tokens[4], circuit, plan)?;
        let gain = parse_value(tokens[5]).map_err(SimError::Parse)?;
        (vec![ControlVar::Voltage(cp, cn)], vec![0.0, gain], 6)
    } else {
        if tokens.len() < 5 {
            return Err(SimError::Parse(format!(
                "line {line_no}: {kind} source '{}' requires: {kind}{{name}} out+ out- Vname gain",
                tokens[0]
            )));
        }
        let gain = parse_value(tokens[4]).map_err(SimError::Parse)?;
        (vec![ControlVar::Current(tokens[3].to_string())], vec![0.0, gain], 5)
    };

    if tokens.len() > consumed {
        warnings.push(ignored_tokens(line_no, &format!("{kind} source"), &tokens[consumed..]));
    }

    let source = ControlledSource {
        name: tokens[0].to_string(),
        out_p,
        out_n,
        controls,
        coeffs,
    };
    match kind {
        'E' => circuit.vcvs.push(source),
        'G' => circuit.vccs.push(source),
        'H' => circuit.ccvs.push(source),
        'F' => circuit.cccs.push(source),
        _ => unreachable!(),
    }
    Ok(())
}

/// Parse a bipolar transistor:
/// `Qname c b e [substrate] model [AREA=…] [OFF]`.
pub(super) fn parse_bjt(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 5 {
        return Err(SimError::Parse(format!(
            "line {line_no}: Q requires: Qname c b e [substrate] model [AREA=…]"
        )));
    }
    let nc = parse_node(tokens[1], circuit, plan)?;
    let nb = parse_node(tokens[2], circuit, plan)?;
    let ne = parse_node(tokens[3], circuit, plan)?;
    let four = q_is_four_terminal(tokens);
    let (ns, model_idx) = if four {
        (Some(parse_node(tokens[4], circuit, plan)?), 5)
    } else {
        (None, 4)
    };
    let model = tokens
        .get(model_idx)
        .ok_or_else(|| {
            SimError::Parse(format!("line {line_no}: Q '{}' is missing its model", tokens[0]))
        })?
        .to_ascii_lowercase();
    let mut area = 1.0;
    for tok in &tokens[model_idx + 1..] {
        let lower = tok.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("area=") {
            area = parse_value(rest).map_err(SimError::Parse)?;
        } else if lower == "off" {
            warnings.push(format!(
                "line {line_no}: BJT '{}': OFF is parsed but not applied",
                tokens[0]
            ));
        } else if let Some(rest) = lower.strip_prefix("m=") {
            area = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Ok(a) = parse_value(tok) {
            area = a;
        } else {
            warnings.push(format!(
                "line {line_no}: BJT '{}': token '{tok}' is not supported — ignored",
                tokens[0]
            ));
        }
    }
    circuit.bjts.push(Bjt {
        name: tokens[0].to_string(),
        nc,
        nb,
        ne,
        ns,
        model,
        area,
    });
    Ok(())
}

/// Parse a `J` card: `Jname nd ng ns model [AREA=…]`.
///
/// Trailing tokens mirror the BJT grammar: `AREA=` (also the `M=` multiplier
/// spelling), a bare numeric area, and an ignored `OFF`.
pub(super) fn parse_jfet(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 5 {
        return Err(SimError::Parse(format!(
            "line {line_no}: J requires: Jname drain gate source model [AREA=…]"
        )));
    }
    let nd = parse_node(tokens[1], circuit, plan)?;
    let ng = parse_node(tokens[2], circuit, plan)?;
    let ns = parse_node(tokens[3], circuit, plan)?;
    let model = tokens
        .get(4)
        .ok_or_else(|| {
            SimError::Parse(format!("line {line_no}: J '{}' is missing its model", tokens[0]))
        })?
        .to_ascii_lowercase();
    let mut area = 1.0;
    for tok in &tokens[5..] {
        let lower = tok.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("area=") {
            area = parse_value(rest).map_err(SimError::Parse)?;
        } else if lower == "off" {
            warnings.push(format!(
                "line {line_no}: JFET '{}': OFF is parsed but not applied",
                tokens[0]
            ));
        } else if let Some(rest) = lower.strip_prefix("m=") {
            area = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Ok(a) = parse_value(tok) {
            area = a;
        } else {
            warnings.push(format!(
                "line {line_no}: JFET '{}': token '{tok}' is not supported — ignored",
                tokens[0]
            ));
        }
    }
    circuit.jfets.push(Jfet {
        name: tokens[0].to_string(),
        nd,
        ng,
        ns,
        model,
        area,
    });
    Ok(())
}

/// Parse a voltage-controlled (`S`) or current-controlled (`W`) switch.
///
/// `Sname n+ n- ctrl+ ctrl- model [ON|OFF]`
/// `Wname n+ n- Vname model [ON|OFF]`
pub(super) fn parse_switch(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    line_no: usize,
) -> Result<()> {
    let kind = element_letter(tokens[0]).to_ascii_uppercase();
    let min = if kind == 'S' { 6 } else { 5 };
    if tokens.len() < min {
        return Err(SimError::Parse(if kind == 'S' {
            format!("line {line_no}: S switch requires: Sname n+ n- ctrl+ ctrl- model [ON|OFF]")
        } else {
            format!("line {line_no}: W switch requires: Wname n+ n- Vname model [ON|OFF]")
        }));
    }
    let n1 = parse_node(tokens[1], circuit, plan)?;
    let n2 = parse_node(tokens[2], circuit, plan)?;
    let (control, model_idx) = if kind == 'S' {
        let cp = parse_node(tokens[3], circuit, plan)?;
        let cn = parse_node(tokens[4], circuit, plan)?;
        (SwitchControl::Voltage(cp, cn), 5)
    } else {
        (SwitchControl::Current(tokens[3].to_string()), 4)
    };
    let model = tokens[model_idx].to_ascii_lowercase();
    let mut initial = None;
    for tok in &tokens[model_idx + 1..] {
        if tok.eq_ignore_ascii_case("on") {
            initial = Some(true);
        } else if tok.eq_ignore_ascii_case("off") {
            initial = Some(false);
        } else {
            return Err(SimError::Parse(format!(
                "line {line_no}: switch '{}': unexpected token '{tok}'",
                tokens[0]
            )));
        }
    }
    circuit.switches.push(Switch {
        name: tokens[0].to_string(),
        n1,
        n2,
        control,
        model,
        initial,
    });
    Ok(())
}

/// Parse a mutual-inductance card: `Kname L1 L2 <k>`.
pub(super) fn parse_mutual_inductance(
    tokens: &[&str],
    circuit: &mut Circuit,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 4 {
        return Err(SimError::Parse(format!(
            "line {line_no}: K requires: Kname L1 L2 <coupling>"
        )));
    }
    let coupling = parse_value(tokens[3]).map_err(SimError::Parse)?;
    circuit.mutual_inductances.push(crate::circuit::MutualInductance {
        name: tokens[0].to_string(),
        inductor1: tokens[1].to_string(),
        inductor2: tokens[2].to_string(),
        coupling,
    });
    Ok(())
}

/// Shared R/C/L card grammar: `<name> n1 n2 [value] [model] [TC1=… TC2=…]`.
/// The value may be omitted when a model card supplies `R=`/`C=`/`L=`.
pub(super) fn parse_passive_card<'a>(
    tokens: &'a [&'a str],
    kind: &str,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<(f64, Option<String>)> {
    if tokens.len() < 4 {
        return Err(SimError::Parse(format!(
            "line {line_no}: {kind} requires: {kind}name n1 n2 <value> [model]"
        )));
    }
    let mut value: Option<f64> = None;
    let mut model: Option<String> = None;
    for tok in &tokens[3..] {
        if tok.contains('=') {
            // Per-instance TC overrides are not wired into the model lookup;
            // the model card's TC1/TC2 is the supported spelling.
            warnings.push(format!(
                "line {line_no}: {kind} '{}': parameter '{tok}' is not applied — \
                 put TC1/TC2 on the .model card",
                tokens[0]
            ));
        } else if let Ok(v) = parse_value(tok) {
            if value.is_some() {
                return Err(SimError::Parse(format!(
                    "line {line_no}: {kind} '{}' has more than one value",
                    tokens[0]
                )));
            }
            value = Some(v);
        } else if model.is_none() {
            model = Some(tok.to_ascii_lowercase());
        } else {
            return Err(SimError::Parse(format!(
                "line {line_no}: {kind} '{}': unexpected token '{tok}'",
                tokens[0]
            )));
        }
    }
    Ok((value.unwrap_or(0.0), model))
}

pub(super) fn parse_resistor(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit, plan)?;
    let n2 = parse_node(tokens[2], circuit, plan)?;
    let (resistance, model) = parse_passive_card(tokens, "resistor", warnings, line_no)?;
    circuit.resistors.push(Resistor {
        name: tokens[0].to_string(),
        n1,
        n2,
        resistance,
        model,
    });
    Ok(())
}

pub(super) fn parse_capacitor(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit, plan)?;
    let n2 = parse_node(tokens[2], circuit, plan)?;
    let (capacitance, model) = parse_passive_card(tokens, "capacitor", warnings, line_no)?;
    circuit.capacitors.push(Capacitor {
        name: tokens[0].to_string(),
        n1,
        n2,
        capacitance,
        model,
    });
    Ok(())
}

pub(super) fn parse_inductor(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit, plan)?;
    let n2 = parse_node(tokens[2], circuit, plan)?;
    let (inductance, model) = parse_passive_card(tokens, "inductor", warnings, line_no)?;
    circuit.inductors.push(Inductor {
        name: tokens[0].to_string(),
        n1,
        n2,
        inductance,
        model,
    });
    Ok(())
}

pub(super) fn parse_voltage_source(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    options: &ParseOptions,
) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit, plan)?;
    let n2 = parse_node(tokens[2], circuit, plan)?;
    let spec = parse_source_value_specs(&tokens[3..], options)?;

    let final_voltage = match (spec.dc, &spec.waveform) {
        (Some(v), _) => v,
        (None, Some(w)) => w.start_value(),
        (None, None) => 0.0,
    };

    circuit.voltage_sources.push(VoltageSource {
        name: tokens[0].to_string(),
        n1, n2,
        voltage: final_voltage,
        waveform: spec.waveform,
        ac: spec.ac,
    });
    Ok(())
}

pub(super) fn parse_current_source(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    options: &ParseOptions,
) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit, plan)?;
    let n2 = parse_node(tokens[2], circuit, plan)?;
    let spec = parse_source_value_specs(&tokens[3..], options)?;

    let final_current = match (spec.dc, &spec.waveform) {
        (Some(v), _) => v,
        (None, Some(w)) => w.start_value(),
        (None, None) => 0.0,
    };

    circuit.current_sources.push(CurrentSource {
        name: tokens[0].to_string(),
        n1, n2,
        current: final_current,
        waveform: spec.waveform,
        ac: spec.ac,
    });
    Ok(())
}

// ─── Behavioral sources (`B`) ───────────────────────────────────────────────

/// One `B` card split into its output flavour and its raw expression text.
pub(super) struct BCardSplit {
    pub(super) kind: BehavioralKind,
    /// Expression as written after the `V=`/`I=` keyword (the keyword's own
    /// letter is stripped: the compiled grammar treats `V(…)` as a probe).
    pub(super) expr: String,
}

/// Find the `V=`/`I=` keyword in a `B` card and rejoin the expression.
///
/// Both keyword shapes are legal: glued (`V=V(in)*2`, possibly the '=' with
/// the expression in the same token) and split (`V = V(in) * V(out)`), and
/// the expression itself may carry spaces — so everything after the keyword
/// is rejoined with single spaces. Scanning starts at token 3 (name, and two
/// nodes, come first); an expression can never contain '=' otherwise.
pub(super) fn split_b_card(tokens: &[&str]) -> Result<BCardSplit> {
    for k in 3..tokens.len() {
        let lower = tokens[k].to_ascii_lowercase();
        let glued = if lower.starts_with("v=") {
            Some(BehavioralKind::Voltage)
        } else if lower.starts_with("i=") {
            Some(BehavioralKind::Current)
        } else {
            None
        };
        if let Some(kind) = glued {
            let mut expr_parts: Vec<&str> = Vec::with_capacity(tokens.len() - k);
            let tail = &tokens[k][2..];
            if !tail.is_empty() {
                expr_parts.push(tail);
            }
            expr_parts.extend(tokens[k + 1..].iter().copied());
            return Ok(BCardSplit { kind, expr: expr_parts.join(" ") });
        }
        // Split: bare `V` / `I`, then either a bare `=` or one starting with `=`.
        if (lower == "v" || lower == "i")
            && tokens.get(k + 1).is_some_and(|n| n.starts_with('='))
        {
            let kind = if lower == "v" { BehavioralKind::Voltage } else { BehavioralKind::Current };
            let mut expr_parts: Vec<&str> = Vec::with_capacity(tokens.len() - k - 1);
            let after_eq = &tokens[k + 1][1..];
            if !after_eq.is_empty() {
                expr_parts.push(after_eq);
            }
            expr_parts.extend(tokens[k + 2..].iter().copied());
            return Ok(BCardSplit { kind, expr: expr_parts.join(" ") });
        }
    }
    Err(SimError::Parse(format!(
        "behavioral source '{}' has no V=<expr> or I=<expr> output",
        tokens[0]
    )))
}

pub(super) fn parse_behavioral_source(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    line_no: usize,
) -> Result<()> {
    let split = split_b_card(tokens)
        .map_err(|e| SimError::Parse(format!("line {line_no}: {e}")))?;
    let expr = split.expr.trim();
    if expr.is_empty() {
        return Err(SimError::Parse(format!(
            "line {line_no}: behavioral source '{}' has an empty expression",
            tokens[0]
        )));
    }
    let behavior =
        crate::behavior::parse(expr).map_err(|e| SimError::Parse(format!("line {line_no}: {e}")))?;
    let out_p = parse_node(tokens[1], circuit, plan)?;
    let out_n = parse_node(tokens[2], circuit, plan)?;
    circuit.behavioral.push(BehavioralSource {
        name: tokens[0].to_string(),
        out_p,
        out_n,
        kind: split.kind,
        expr: expr.to_string(),
        behavior,
    });
    Ok(())
}

/// Emit every element family, in SPICE card order (the inverse of the
/// element parsers above); sources print their waveform via
/// [`waveform_text`].
pub(super) fn emit_elements(circuit: &Circuit, out: &mut String) {
    // ── Element cards ────────────────────────────────────────────────────
    let name_of = |n: NodeId| circuit.node_name(n.0).to_string();
    for r in &circuit.resistors {
        let mut card = format!(
            "{} {} {} {}",
            r.name, name_of(r.n1), name_of(r.n2), r.resistance
        );
        if let Some(model) = &r.model {
            card.push_str(&format!(" {model}"));
        }
        out.push_str(&card);
        out.push('\n');
    }
    for c in &circuit.capacitors {
        let mut card = format!(
            "{} {} {} {}",
            c.name, name_of(c.n1), name_of(c.n2), c.capacitance
        );
        if let Some(model) = &c.model {
            card.push_str(&format!(" {model}"));
        }
        out.push_str(&card);
        out.push('\n');
    }
    for l in &circuit.inductors {
        let mut card = format!(
            "{} {} {} {}",
            l.name, name_of(l.n1), name_of(l.n2), l.inductance
        );
        if let Some(model) = &l.model {
            card.push_str(&format!(" {model}"));
        }
        out.push_str(&card);
        out.push('\n');
    }
    for d in &circuit.diodes {
        if d.area != 1.0 {
            out.push_str(&format!(
                "{} {} {} {} AREA={}\n",
                d.name, name_of(d.anode), name_of(d.cathode), d.model, d.area
            ));
        } else {
            out.push_str(&format!(
                "{} {} {} {}\n",
                d.name, name_of(d.anode), name_of(d.cathode), d.model
            ));
        }
    }
    for m in &circuit.mosfets {
        let mut line = format!(
            "{} {} {} {} {} {} W={} L={}",
            m.name, name_of(m.nd), name_of(m.ng), name_of(m.ns), name_of(m.nb), m.model, m.w, m.l
        );
        // Junction sizing survives the round trip only when written back.
        if m.ad != 0.0 { line.push_str(&format!(" AD={}", m.ad)); }
        if m.asr != 0.0 { line.push_str(&format!(" AS={}", m.asr)); }
        if m.ps != 0.0 { line.push_str(&format!(" PS={}", m.ps)); }
        if m.pd != 0.0 { line.push_str(&format!(" PD={}", m.pd)); }
        line.push('\n');
        out.push_str(&line);
    }
    for q in &circuit.bjts {
        let substrate = q.ns.map(|n| format!(" {}", name_of(n))).unwrap_or_default();
        let area = if q.area != 1.0 {
            format!(" AREA={}", q.area)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "{} {} {} {}{} {}{}\n",
            q.name,
            name_of(q.nc),
            name_of(q.nb),
            name_of(q.ne),
            substrate,
            q.model,
            area
        ));
    }
    for j in &circuit.jfets {
        let area = if j.area != 1.0 {
            format!(" AREA={}", j.area)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "{} {} {} {} {}{}\n",
            j.name,
            name_of(j.nd),
            name_of(j.ng),
            name_of(j.ns),
            j.model,
            area
        ));
    }
    for v in &circuit.voltage_sources {
        // `DC <value>` is redundant for waveform sources (the value equals
        // their starting level) but keeps every source's operating point.
        out.push_str(&format!(
            "{} {} {} DC {}",
            v.name, name_of(v.n1), name_of(v.n2), v.voltage
        ));
        if let Some(ac) = &v.ac {
            out.push_str(&format!(" AC {}", ac.mag));
            if ac.phase != 0.0 {
                out.push_str(&format!(" {}", ac.phase));
            }
        }
        if let Some(w) = &v.waveform {
            out.push(' ');
            out.push_str(&waveform_text(w));
        }
        out.push('\n');
    }
    for i in &circuit.current_sources {
        out.push_str(&format!(
            "{} {} {} DC {}",
            i.name, name_of(i.n1), name_of(i.n2), i.current
        ));
        if let Some(ac) = &i.ac {
            out.push_str(&format!(" AC {}", ac.mag));
            if ac.phase != 0.0 {
                out.push_str(&format!(" {}", ac.phase));
            }
        }
        if let Some(w) = &i.waveform {
            out.push(' ');
            out.push_str(&waveform_text(w));
        }
        out.push('\n');
    }
    // Behavioral sources: verbatim expressions (the compiled AST recompiles
    // byte-for-byte; emitting any canonicalized form would risk double-scale).
    for b in &circuit.behavioral {
        let letter = match b.kind {
            BehavioralKind::Voltage => 'V',
            BehavioralKind::Current => 'I',
        };
        out.push_str(&format!(
            "{} {} {} {}={}\n",
            b.name,
            name_of(b.out_p),
            name_of(b.out_n),
            letter,
            b.expr
        ));
    }

    // ── Controlled sources, switches, coupled inductors ──────────────────
    for (letter, family) in [
        ('E', &circuit.vcvs),
        ('G', &circuit.vccs),
        ('H', &circuit.ccvs),
        ('F', &circuit.cccs),
    ] {
        for c in family {
            out.push_str(&format!(
                "{} {} {}",
                c.name,
                name_of(c.out_p),
                name_of(c.out_n)
            ));
            if c.is_linear() {
                match &c.controls[0] {
                    ControlVar::Voltage(a, b) => {
                        out.push_str(&format!(" {} {} {}", name_of(*a), name_of(*b), c.coeff(0)));
                    }
                    ControlVar::Current(src) => {
                        out.push_str(&format!(" {src} {}", c.coeff(0)));
                    }
                }
            } else {
                out.push_str(&format!(" POLY({})", c.controls.len()));
                for cv in &c.controls {
                    match cv {
                        ControlVar::Voltage(a, b) => {
                            out.push_str(&format!(" {} {}", name_of(*a), name_of(*b)));
                        }
                        ControlVar::Current(src) => out.push_str(&format!(" {src}")),
                    }
                }
                for coeff in &c.coeffs {
                    out.push_str(&format!(" {coeff}"));
                }
            }
            let _ = letter;
            out.push('\n');
        }
    }
    for s in &circuit.switches {
        let control = match &s.control {
            SwitchControl::Voltage(a, b) => format!("{} {}", name_of(*a), name_of(*b)),
            SwitchControl::Current(src) => src.clone(),
        };
        let state = match s.initial {
            Some(true) => " ON",
            Some(false) => " OFF",
            None => "",
        };
        out.push_str(&format!(
            "{} {} {} {} {}{}\n",
            s.name,
            name_of(s.n1),
            name_of(s.n2),
            control,
            s.model,
            state
        ));
    }
    for k in &circuit.mutual_inductances {
        out.push_str(&format!(
            "{} {} {} {}\n",
            k.name, k.inductor1, k.inductor2, k.coupling
        ));
    }
}
