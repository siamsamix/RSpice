//! Model cards: the `.model` grammar and its canonical emission.

use super::*;

// ─── Model cards ─────────────────────────────────────────────────────────────

/// `.model <name> <TYPE>(<KEY=VALUE …>)` — the complete card grammar.
///
/// The keyword and the parameter list may run together (`NMOS(KP=…)`), sit
/// apart (`NMOS (…)`) with whitespace inside the list, and `%`/`)`
/// decorations on values are tolerated. Model names are lowercased so the
/// element-card lookup is case-insensitive. Waveform-card conventions:
///
/// * `D` → a [`DiodeModel`] — every SPICE diode parameter is parsed, and
///   the ones the engine stamps (`IS`, `N`) drive the equations.
/// * `NMOS`/`PMOS` → a [`MosfetModel`] — level-1 parameters parsed, plus
///   the `LEVEL` keyword the element validator checks.
/// * anything else (`NPN`, `RES`, …) → a `GenericModel` in
///   `circuit.generic_models`, so decks from other tools import cleanly.
///
/// `DEV=`/`LOT=` spell the PSpice Monte-Carlo tolerance slot on every
/// family (TODO §8 consumes it); unsupported parameters warn and are
/// carried as far as storage allows, but always skip silently *only* at
/// the parameter level — structural breakage (no name, no type, unbalanced
/// parentheses) is an error.
pub(super) fn parse_model_card(
    line: &str,
    tokens: &[&str],
    circuit: &mut Circuit,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 3 {
        return Err(SimError::Parse(
            ".model requires at least a name and a type".into(),
        ));
    }
    let name = tokens[1].to_ascii_lowercase();

    // The type keyword lives after the name and ends at '(' or whitespace.
    // Slice the source line by byte span — a `.find(name)` would also hit
    // earlier text like `.model od D(...)`.
    let name_end = nth_token_span(line, 1).ok_or_else(|| {
        SimError::Parse(format!("line {line_no}: broken .model card: {line}"))
    })?.end;
    let tail = line[name_end..].trim_start();
    if tail.is_empty() {
        return Err(SimError::Parse(
            format!("line {line_no}: .model '{name}' is missing its type"),
        ));
    }
    let (type_word, param_body) = match tail.find('(') {
        Some(open_rel) => {
            let after = &tail[open_rel + 1..];
            let close_rel = after.rfind(')').ok_or_else(|| {
                SimError::Parse(format!(
                    "line {line_no}: .model '{name}': unbalanced parentheses"
                ))
            })?;
            let trailing = after[close_rel + 1..].trim();
            if !trailing.is_empty() {
                warnings.push(format!(
                    "line {line_no}: .model '{name}': tokens after the parameter list are ignored: {trailing}"
                ));
            }
            (tail[..open_rel].trim(), after[..close_rel].to_string())
        }
        // Paren-less form (`D IS=1e-14 N=1.05`): every token after the type
        // word is parameter text.
        None => match tail.find(char::is_whitespace) {
            Some(ws) => (tail[..ws].trim(), tail[ws + 1..].trim().to_string()),
            None => (tail.trim(), String::new()),
        },
    };
    if type_word.is_empty() {
        return Err(SimError::Parse(
            format!("line {line_no}: .model '{name}' is missing its type"),
        ));
    }

    let (mut pairs, strays) = parse_model_params(&param_body);
    // A generic `JFET(NJF …)` card carries the polarity as a bare token —
    // that spelling is legal, so it does not warn as a stray.
    let type_word_lower = type_word.to_ascii_lowercase();
    let jfet_polarity_in_body = type_word_lower == "jfet"
        && strays
            .iter()
            .chain(pairs.iter().filter(|(k, _)| k == "type").map(|(_, v)| v))
            .any(|s| matches!(s.to_ascii_lowercase().as_str(), "njf" | "pjf"));
    for stray in &strays {
        if jfet_polarity_in_body && matches!(stray.to_ascii_lowercase().as_str(), "njf" | "pjf") {
            continue;
        }
        warnings.push(format!(
            "line {line_no}: .model '{name}': token '{stray}' is not a KEY=VALUE pair — ignored"
        ));
    }

    // Value reader with the line context baked in; stray punctuation from
    // the parameter list spelling (`5)` after the closing paren) is
    // tolerated ahead of the numeric parse.
    let value = |raw: &str| {
        parse_model_value(raw).map_err(|e| {
            SimError::Parse(format!("line {line_no}: .model '{name}': {e}"))
        })
    };

    let type_lower = type_word.to_ascii_lowercase();
    if type_lower == "d" || type_lower == "diode" {
        let mut model = DiodeModel::default();
        for (key, raw) in pairs {
            match key.as_str() {
                "is" => model.is = value(&raw)?,
                "n" => model.n = value(&raw)?,
                "rs" => model.rs = value(&raw)?,
                "cjo" => model.cjo = value(&raw)?,
                "vj" => model.vj = value(&raw)?,
                "m" => model.m = value(&raw)?,
                "tt" => model.tt = value(&raw)?,
                "eg" => model.eg = value(&raw)?,
                "xti" => model.xti = value(&raw)?,
                "bv" => model.bv = value(&raw)?,
                "ibv" => model.ibv = value(&raw)?,
                "kf" => model.kf = value(&raw)?,
                "af" => model.af = value(&raw)?,
                "tnom" => model.tnom = value(&raw)?,
                "nbv" => model.nbv = value(&raw)?,
                "fc" => model.fc = value(&raw)?,
                "dev" => model.tolerance.dev = Some(parse_tolerance_value(&raw, &value)),
                "lot" => model.tolerance.lot = Some(parse_tolerance_value(&raw, &value)),
                "level" => warnings.push(format!(
                    "line {line_no}: .model '{name}': LEVEL on a diode model is ignored"
                )),
                other => warnings.push(format!(
                    "line {line_no}: .model '{name}': diode parameter '{other}' is not supported — ignored"
                )),
            }
        }
        circuit.diode_models.insert(name, model);
        return Ok(());
    }

    if type_lower == "nmos" || type_lower == "pmos" {
        let kind = if type_lower == "nmos" {
            MosfetType::Nmos
        } else {
            MosfetType::Pmos
        };
        let mut model = MosfetModel {
            kind,
            ..MosfetModel::default()
        };
        for (key, raw) in pairs {
            match key.as_str() {
                "level" => match raw
                    .trim()
                    .trim_end_matches(|c| c == ')' || c == '%' || c == ',')
                    .parse::<u32>()
                {
                    Ok(level) => model.level = level,
                    Err(_) => {
                        return Err(SimError::Parse(format!(
                            "line {line_no}: .model '{name}': invalid LEVEL '{raw}'"
                        )))
                    }
                },
                "kp" => model.kp = value(&raw)?,
                "vth0" | "vto" => model.vth0 = value(&raw)?,
                "lambda" => model.lambda = value(&raw)?,
                "gamma" => model.gamma = value(&raw)?,
                "phi" => model.phi = value(&raw)?,
                "tnom" => model.tnom = value(&raw)?,
                "tcv" => model.tcv = value(&raw)?,
                "bex" => model.bex = value(&raw)?,
                "tox" => model.tox = Some(value(&raw)?),
                "cgdo" => model.cgdo = Some(value(&raw)?),
                "cgso" => model.cgso = Some(value(&raw)?),
                "cgbo" => model.cgbo = Some(value(&raw)?),
                "cj" => model.cj = Some(value(&raw)?),
                "cjsw" => model.cjsw = Some(value(&raw)?),
                "mj" => model.mj = Some(value(&raw)?),
                "mjsw" => model.mjsw = Some(value(&raw)?),
                "pb" => model.pb = Some(value(&raw)?),
                "fc" => model.fc = Some(value(&raw)?),
                "nsub" => model.nsub = Some(value(&raw)?),
                "nss" => model.nss = Some(value(&raw)?),
                "tpg" => model.tpg = Some(value(&raw)?),
                "dev" => model.tolerance.dev = Some(parse_tolerance_value(&raw, &value)),
                "lot" => model.tolerance.lot = Some(parse_tolerance_value(&raw, &value)),
                other => warnings.push(format!(
                    "line {line_no}: .model '{name}': MOSFET parameter '{other}' is not supported — ignored"
                )),
            }
        }
        circuit.mosfet_models.insert(name, model);
        return Ok(());
    }

    if type_lower == "npn" || type_lower == "pnp" {
        let kind = if type_lower == "npn" {
            BjtType::Npn
        } else {
            BjtType::Pnp
        };
        let mut model = BjtModel { kind, ..BjtModel::default() };
        for (key, raw) in pairs {
            match key.as_str() {
                "is" => model.is = value(&raw)?,
                "bf" => model.bf = value(&raw)?,
                "br" => model.br = value(&raw)?,
                "nf" => model.nf = value(&raw)?,
                "nr" => model.nr = value(&raw)?,
                "vaf" => model.vaf = value(&raw)?,
                "var" => model.var = value(&raw)?,
                "ikf" => model.ikf = value(&raw)?,
                "ikr" => model.ikr = value(&raw)?,
                "ise" => model.ise = value(&raw)?,
                "ne" => model.ne = value(&raw)?,
                "isc" => model.isc = value(&raw)?,
                "nc" => model.nc = value(&raw)?,
                "rb" => model.rb = value(&raw)?,
                "rc" => model.rc = value(&raw)?,
                "re" => model.re = value(&raw)?,
                "cje" => model.cje = value(&raw)?,
                "vje" => model.vje = value(&raw)?,
                "mje" => model.mje = value(&raw)?,
                "cjc" => model.cjc = value(&raw)?,
                "vjc" => model.vjc = value(&raw)?,
                "mjc" => model.mjc = value(&raw)?,
                "tf" => model.tf = value(&raw)?,
                "tr" => model.tr = value(&raw)?,
                "xti" => model.xti = value(&raw)?,
                "xtb" => model.xtb = value(&raw)?,
                "eg" => model.eg = value(&raw)?,
                "tnom" => model.tnom = value(&raw)?,
                "kf" => model.kf = value(&raw)?,
                "af" => model.af = value(&raw)?,
                "dev" => model.tolerance.dev = Some(parse_tolerance_value(&raw, &value)),
                "lot" => model.tolerance.lot = Some(parse_tolerance_value(&raw, &value)),
                "level" => {}
                other => warnings.push(format!(
                    "line {line_no}: .model '{name}': BJT parameter '{other}' is not supported — ignored"
                )),
            }
        }
        circuit.bjt_models.insert(name, model);
        return Ok(());
    }

    // JFET card: either bare `NJF`/`PJF`, or the generic `JFET(...)` spelling
    // where the polarity arrives in the body (a leading stray `NJF`/`PJF`
    // token, or a `TYPE=` pair), defaulting to NJF.
    if type_lower == "njf" || type_lower == "pjf" || type_lower == "jfet" {
        let mut rest_strays = strays;
        let polarity_word = if type_lower == "jfet" {
            // The bare-token spelling (`JFET(NJF VTO=…)`) lost the token in
            // the pairs/strays split; the pairs-only variant keeps `TYPE=`.
            let mut word = "njf".to_string();
            let mut rest_pairs = pairs;
            if let Some(idx) = rest_strays.iter().position(|s| {
                matches!(s.to_ascii_lowercase().as_str(), "njf" | "pjf")
            }) {
                word = rest_strays.remove(idx).to_ascii_lowercase();
            } else if let Some(idx) = rest_pairs.iter().position(|(k, _)| k == "type") {
                let (_, v) = rest_pairs.remove(idx);
                let v = v.to_ascii_lowercase();
                if matches!(v.as_str(), "njf" | "pjf") {
                    word = v;
                }
            }
            // Only `pairs` are consumed by the parameter walk below; the
            // remaining strays were already warned above and stay unread.
            pairs = rest_pairs;
            word
        } else {
            type_lower.clone()
        };
        let kind = if polarity_word == "pjf" {
            JfetType::Pjf
        } else {
            JfetType::Njf
        };
        let mut model = JfetModel { kind, ..JfetModel::default() };
        for (key, raw) in pairs {
            match key.as_str() {
                "vto" => model.vto = value(&raw)?,
                "beta" => model.beta = value(&raw)?,
                "lambda" => model.lambda = value(&raw)?,
                "b" => model.b = value(&raw)?,
                "is" => model.is = value(&raw)?,
                "rd" => model.rd = value(&raw)?,
                "rs" => model.rs = value(&raw)?,
                "cgs" => model.cgs = value(&raw)?,
                "cgd" => model.cgd = value(&raw)?,
                "pb" => model.pb = value(&raw)?,
                "fc" => model.fc = value(&raw)?,
                "xti" => model.xti = value(&raw)?,
                "eg" => model.eg = value(&raw)?,
                "tnom" => model.tnom = value(&raw)?,
                "kf" => model.kf = value(&raw)?,
                "af" => model.af = value(&raw)?,
                "dev" => model.tolerance.dev = Some(parse_tolerance_value(&raw, &value)),
                "lot" => model.tolerance.lot = Some(parse_tolerance_value(&raw, &value)),
                "level" => {}
                other => warnings.push(format!(
                    "line {line_no}: .model '{name}': JFET parameter '{other}' is not supported — ignored"
                )),
            }
        }
        circuit.jfet_models.insert(name, model);
        return Ok(());
    }

    // Passive model cards: `.model m R(R=1k TC1=…)`, `CAP(…)`, `IND(…)`.    // Passive model cards: `.model m R(R=1k TC1=…)`, `CAP(…)`, `IND(…)`.
    let passive = matches!(
        type_lower.as_str(),
        "r" | "res" | "resistor" | "c" | "cap" | "capacitor" | "l" | "ind" | "inductor"
    );
    if passive {
        let mut model = crate::circuit::PassiveModel::default();
        for (key, raw) in pairs {
            match key.as_str() {
                "r" | "res" | "resistance" | "c" | "cap" | "capacitance" | "l" | "ind"
                | "inductance" => model.value = Some(value(&raw)?),
                "tc1" => model.tc1 = value(&raw)?,
                "tc2" => model.tc2 = value(&raw)?,
                // LTspice-style parasitic slots (TODO §5). They are read
                // here and materialized into element arms by
                // `expand_parasitics` after the full deck walk.
                "rser" => model.rser = value(&raw)?,
                "lser" => model.lser = value(&raw)?,
                "cpar" => model.cpar = value(&raw)?,
                "rpar" => model.rpar = value(&raw)?,
                "dev" | "lot" => {}
                other => warnings.push(format!(
                    "line {line_no}: .model '{name}': passive parameter '{other}' is not supported — ignored"
                )),
            }
        }
        match type_lower.as_str() {
            "r" | "res" | "resistor" => {
                circuit.resistor_models.insert(name, model);
            }
            "c" | "cap" | "capacitor" => {
                circuit.capacitor_models.insert(name, model);
            }
            _ => {
                circuit.inductor_models.insert(name, model);
            }
        }
        return Ok(());
    }

    if type_lower == "sw" || type_lower == "switch" || type_lower == "s" {
        let mut model = SwitchModel::default();
        for (key, raw) in pairs {
            match key.as_str() {
                "ron" => model.ron = value(&raw)?,
                "roff" => model.roff = value(&raw)?,
                "vt" | "vth" => model.vt = value(&raw)?,
                "vh" | "vhy" => model.vh = value(&raw)?,
                "dev" | "lot" => {} // tolerance slots are not physical here
                other => warnings.push(format!(
                    "line {line_no}: .model '{name}': switch parameter '{other}' is not supported — ignored"
                )),
            }
        }
        circuit.switch_models.insert(name, model);
        return Ok(());
    }

    // Op-amp model cards: `.model m U(GAIN=… RIN=… ROUT=… VOS=…)`. The
    // documented-interpretation card for RSpice's `U` element (the reference
    // tools reserve no `U` model type); unset keys keep their defaults, so a
    // bare `.model m U()` introduces the default op-amp under that name.
    if type_lower == "u" || type_lower == "opamp" {
        let mut model = crate::circuit::OpAmpModel::default();
        for (key, raw) in pairs {
            match key.as_str() {
                "gain" | "a" | "av" | "aol" => model.gain = value(&raw)?,
                "rin" | "rinp" => model.rin = value(&raw)?,
                "rout" | "zout" => model.rout = value(&raw)?,
                "vos" => model.vos = value(&raw)?,
                "dev" => model.tolerance.dev = Some(parse_tolerance_value(&raw, &value)),
                "lot" => model.tolerance.lot = Some(parse_tolerance_value(&raw, &value)),
                other => warnings.push(format!(
                    "line {line_no}: .model '{name}': op-amp parameter '{other}' is not supported — ignored"
                )),
            }
        }
        circuit.opamp_models.insert(name, model);
        return Ok(());
    }

    // Unknown device family — store the card so the deck still imports, and
    // a future element letter can consume it (TODO §2).
    let mut generic = GenericModel {
        spice_type: type_lower,
        level: None,
        params: Vec::new(),
        tolerance: ModelTolerance::default(),
    };
    for (key, raw) in pairs {
        match key.as_str() {
            "level" => match value(&raw) {
                Ok(l) => generic.level = Some(l as u32),
                Err(_) => warnings.push(format!(
                    "line {line_no}: .model '{name}': non-numeric LEVEL '{raw}' is kept out of the card"
                )),
            },
            "dev" => generic.tolerance.dev = Some(parse_tolerance_value(&raw, &value)),
            "lot" => generic.tolerance.lot = Some(parse_tolerance_value(&raw, &value)),
            _ => match value(&raw) {
                Ok(v) => generic.params.push((key, v)),
                Err(_) => warnings.push(format!(
                    "line {line_no}: .model '{name}': parameter '{key}={raw}' is not numeric — ignored"
                )),
            },
        }
    }
    circuit.generic_models.insert(name, generic);
    Ok(())
}

/// Byte span of the `index`-th whitespace-separated token of `line`.
pub(super) fn nth_token_span(line: &str, index: usize) -> Option<std::ops::Range<usize>> {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    let mut count = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if count == index {
            return Some(start..i);
        }
        count += 1;
    }
    None
}

/// Split a `.model` parameter body into `(key, value_text)` pairs.
///
/// The reference spellings run pairs together (`KP=20e-6 VTH0=0.7`), with
/// optional whitespace (or `,`) either side of `=`; a `=` without value or
/// a stray word lands in the returned "strays" list so the caller can warn
/// with card-level context.
pub(super) fn parse_model_params(body: &str) -> (Vec<(String, String)>, Vec<String>) {
    let cleaned = body.replace(',', " ");
    let bytes = cleaned.as_bytes();
    let mut pairs = Vec::new();
    let mut strays = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == bytes.len() || bytes[i].is_ascii_whitespace() {
            // No '=' before the pair ends: a key-less leftover word.
            strays.push(cleaned[key_start..i].to_string());
            continue;
        }
        let key = cleaned[key_start..i].to_ascii_lowercase();
        i += 1; // the '='
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = cleaned[value_start..i].to_string();
        if value.is_empty() {
            strays.push(key);
        } else {
            pairs.push((key, value));
        }
    }
    (pairs, strays)
}

/// One model parameter value: trailing `%`/`)`/`,` decorations are
/// tolerated (the tolerance `5%` keeps its percent flag via
/// [`parse_tolerance_value`]; model values simply drop it).
pub(super) fn parse_model_value(raw: &str) -> std::result::Result<f64, String> {
    parse_value(raw.trim().trim_end_matches(|c| c == ')' || c == '%' || c == ','))
        .map_err(|e| format!("value '{raw}' does not parse: {e}"))
}

/// A `DEV=`/`LOT=` tolerance slot: the number plus whether the `5%`
/// percent-flag spelling was used.
pub(super) fn parse_tolerance_value(
    raw: &str,
    value: &impl Fn(&str) -> std::result::Result<f64, SimError>,
) -> (f64, bool) {
    let percent = raw.trim_end().ends_with('%');
    let parsed = value(raw.trim().trim_end_matches('%')).unwrap_or(0.0);
    (parsed, percent)
}


/// Emit every known model family, sorted by name for deterministic
/// output (the inverse of [`parse_model_card`]).
pub(super) fn emit_models(circuit: &Circuit, out: &mut String) {
    // ── Model cards (sorted for deterministic output) ────────────────────
    let mut diode_names: Vec<&String> = circuit.diode_models.keys().collect();
    diode_names.sort();
    for name in diode_names {
        let m = &circuit.diode_models[name];
        // `RS` is deliberately not reprinted: `expand_diode_series_resistance`
        // has already materialized it as resistor arms, and printing the slot
        // a second time would make `parse(emit(…))` split it twice.
        let mut fields = format!("IS={} N={} CJO={} VJ={} M={} TT={} BV={} IBV={}",
            m.is, m.n, m.cjo, m.vj, m.m, m.tt, m.bv, m.ibv);
        if m.eg != 1.11 { fields.push_str(&format!(" EG={}", m.eg)); }
        if m.xti != 3.0 { fields.push_str(&format!(" XTI={}", m.xti)); }
        if m.kf != 0.0 { fields.push_str(&format!(" KF={} AF={}", m.kf, m.af)); }
        if m.tnom != 27.0 { fields.push_str(&format!(" TNOM={}", m.tnom)); }
        if m.nbv != 1.0 { fields.push_str(&format!(" NBV={}", m.nbv)); }
        if m.fc != 0.5 { fields.push_str(&format!(" FC={}", m.fc)); }
        out.push_str(&format!(".model {} D({})\n", name, fields));
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
            ".model {} {}(KP={} VTH0={} LAMBDA={} GAMMA={} PHI={}",
            name, kind, m.kp, m.vth0, m.lambda, m.gamma, m.phi
        ));
        if m.level != 1 {
            out.push_str(&format!(" LEVEL={}", m.level));
        }
        if m.tnom != 27.0 { out.push_str(&format!(" TNOM={}", m.tnom)); }
        if m.tcv != 2.0e-3 { out.push_str(&format!(" TCV={}", m.tcv)); }
        if m.bex != -1.5 { out.push_str(&format!(" BEX={}", m.bex)); }
        if let Some(tox) = m.tox { out.push_str(&format!(" TOX={tox}")); }
        if let Some(v) = m.cgdo { out.push_str(&format!(" CGDO={v}")); }
        if let Some(v) = m.cgso { out.push_str(&format!(" CGSO={v}")); }
        if let Some(v) = m.cgbo { out.push_str(&format!(" CGBO={v}")); }
        if let Some(v) = m.cj { out.push_str(&format!(" CJ={v}")); }
        if let Some(v) = m.cjsw { out.push_str(&format!(" CJSW={v}")); }
        if let Some(v) = m.mj { out.push_str(&format!(" MJ={v}")); }
        if let Some(v) = m.mjsw { out.push_str(&format!(" MJSW={v}")); }
        if let Some(v) = m.pb { out.push_str(&format!(" PB={v}")); }
        if let Some(v) = m.fc { out.push_str(&format!(" FC={v}")); }
        if let Some(v) = m.nsub { out.push_str(&format!(" NSUB={v}")); }
        if let Some(v) = m.nss { out.push_str(&format!(" NSS={v}")); }
        if let Some(v) = m.tpg { out.push_str(&format!(" TPG={v}")); }
        out.push_str(")\n");
    }
    // Passive model cards. Parasitic slots (`RSER`/`LSER`/`CPAR`/`RPAR`) are
    // deliberately not reprinted: `expand_parasitics` has already materialized
    // them as element arms, and printing the slots a second time would make
    // `parse(emit(…))` split them twice.
    let mut passive_names: Vec<&String> = circuit
        .resistor_models
        .keys()
        .chain(circuit.capacitor_models.keys())
        .chain(circuit.inductor_models.keys())
        .collect();
    passive_names.sort();
    passive_names.dedup();
    for name in passive_names {
        let (type_word, m) = if let Some(m) = circuit.resistor_models.get(name) {
            ("R", m)
        } else if let Some(m) = circuit.capacitor_models.get(name) {
            ("C", m)
        } else {
            ("L", circuit.inductor_models.get(name).expect("key from a passive map"))
        };
        out.push_str(&format!(".model {} {}(", name, type_word));
        let mut fields = Vec::new();
        if let Some(v) = m.value {
            fields.push(format!("{}={v}", type_word));
        }
        if m.tc1 != 0.0 {
            fields.push(format!("TC1={}", m.tc1));
        }
        if m.tc2 != 0.0 {
            fields.push(format!("TC2={}", m.tc2));
        }
        out.push_str(&fields.join(" "));
        out.push_str(")\n");
    }
    // BJT model cards. `VAF`/`VAR` (and `IKF`/`IKR`) print only when finite —
    // the infinite defaults would emit `inf`, which `parse_value` rejects —
    // and `XTB` joins only when it moves β at all.
    let mut bjt_names: Vec<&String> = circuit.bjt_models.keys().collect();
    bjt_names.sort();
    for name in bjt_names {
        let m = &circuit.bjt_models[name];
        let kind = match m.kind {
            BjtType::Npn => "NPN",
            BjtType::Pnp => "PNP",
        };
        let mut fields = vec![
            format!("IS={}", m.is),
            format!("BF={}", m.bf),
            format!("BR={}", m.br),
            format!("NF={}", m.nf),
            format!("NR={}", m.nr),
            format!("ISE={}", m.ise),
            format!("NE={}", m.ne),
            format!("ISC={}", m.isc),
            format!("NC={}", m.nc),
            // `RB`/`RC`/`RE` are deliberately not reprinted: the BJT
            // series-resistance expansion has already materialized them as
            // collector/base/emitter resistor arms, and printing the slots a
            // second time would split them twice.
        ];
        for (label, val) in [("VAF", m.vaf), ("VAR", m.var), ("IKF", m.ikf), ("IKR", m.ikr)] {
            if val.is_finite() {
                fields.push(format!("{label}={val}"));
            }
        }
        fields.extend([
            format!("CJE={}", m.cje),
            format!("VJE={}", m.vje),
            format!("MJE={}", m.mje),
            format!("CJC={}", m.cjc),
            format!("VJC={}", m.vjc),
            format!("MJC={}", m.mjc),
            format!("TF={}", m.tf),
            format!("TR={}", m.tr),
            format!("XTI={}", m.xti),
        ]);
        if m.xtb != 0.0 {
            fields.push(format!("XTB={}", m.xtb));
        }
        fields.extend([
            format!("EG={}", m.eg),
            format!("TNOM={}", m.tnom),
            format!("KF={}", m.kf),
            format!("AF={}", m.af),
        ]);
        out.push_str(&format!(".model {} {}({})\n", name, kind, fields.join(" ")));
    }
    // JFET model cards.
    let mut jfet_names: Vec<&String> = circuit.jfet_models.keys().collect();
    jfet_names.sort();
    for name in jfet_names {
        let m = &circuit.jfet_models[name];
        let kind = match m.kind {
            JfetType::Njf => "NJF",
            JfetType::Pjf => "PJF",
        };
        out.push_str(&format!(
            // `RD`/`RS` are deliberately not reprinted: the series-resistance
            // expansion has already materialized them as resistor arms, and
            // printing the slots a second time would split them twice.
            ".model {} {}(VTO={} BETA={} LAMBDA={} B={} IS={} CGS={} CGD={} PB={} FC={} XTI={} EG={} TNOM={} KF={} AF={})\n",
            name, kind, m.vto, m.beta, m.lambda, m.b, m.is, m.cgs, m.cgd, m.pb,
            m.fc, m.xti, m.eg, m.tnom, m.kf, m.af
        ));
    }
    // Switch model cards.
    let mut switch_names: Vec<&String> = circuit.switch_models.keys().collect();
    switch_names.sort();
    for name in switch_names {
        let m = &circuit.switch_models[name];
        out.push_str(&format!(
            ".model {} SW(RON={} ROFF={} VT={} VH={})\n",
            name, m.ron, m.roff, m.vt, m.vh
        ));
    }
    // Op-amp model cards. `RIN` prints only when finite (the ∞ default would
    // emit `inf`, which `parse_value` rejects) and `VOS`/`ROUT` join only
    // when they depart from their defaults.
    let mut opamp_names: Vec<&String> = circuit.opamp_models.keys().collect();
    opamp_names.sort();
    for name in opamp_names {
        let m = &circuit.opamp_models[name];
        let mut fields = format!("GAIN={}", m.gain);
        if m.rin.is_finite() {
            fields.push_str(&format!(" RIN={}", m.rin));
        }
        if m.rout != 0.0 {
            fields.push_str(&format!(" ROUT={}", m.rout));
        }
        if m.vos != 0.0 {
            fields.push_str(&format!(" VOS={}", m.vos));
        }
        out.push_str(&format!(".model {} U({})\n", name, fields));
    }
    // Device families without an element letter yet: emitted with their
    // stored numeric parameters so a re-parse reproduces the card.
    let mut generic_names: Vec<&String> = circuit.generic_models.keys().collect();
    generic_names.sort();
    for name in generic_names {
        let m = &circuit.generic_models[name];
        let mut params: Vec<String> = Vec::new();
        if let Some(level) = m.level {
            params.push(format!("LEVEL={}", level));
        }
        for (key, value) in &m.params {
            params.push(format!("{}={}", key, value));
        }
        out.push_str(&format!(
            ".model {} {}({})\n",
            name,
            m.spice_type.to_ascii_uppercase(),
            params.join(" ")
        ));
    }
}
