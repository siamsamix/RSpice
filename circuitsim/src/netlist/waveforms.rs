//! Source waveforms: the `PULSE`/`SIN`/`EXP`/`PWL`/`SFFM`/`AM` grammar and

//! its canonical emission.

use super::*;
use super::imports::{find_library_file, open_deck_file};


/// A two-terminal `V`/`I` source card, after the name and the two nodes.
/// The syntax is shared between both element letters, so one grammar owns it:
/// `DC <v>`, bare value, `AC <mag> [phase]`, and the waveform family
/// `PULSE`/`SIN`/`EXP`/`PWL`/`SFFM`/`AM`.
#[derive(Debug, Default)]
pub(super) struct SourceSpec {
    pub(super) dc: Option<f64>,
    pub(super) waveform: Option<Waveform>,
    pub(super) ac: Option<Ac>,
}

pub(super) fn parse_source_value_specs(rest: &[&str], options: &ParseOptions) -> Result<SourceSpec> {
    let mut spec = SourceSpec::default();
    let mut i = 0usize;
    while i < rest.len() {
        let token = rest[i].to_ascii_lowercase();
        if token == "dc" {
            i += 1;
            let value = rest.get(i).copied().ok_or_else(|| {
                SimError::Parse("source missing DC value".into())
            })?;
            spec.dc = Some(parse_value(value).map_err(SimError::Parse)?);
            i += 1;
        } else if token == "ac" {
            i += 1;
            let mag_tok = rest.get(i).copied().ok_or_else(|| {
                SimError::Parse("source missing AC magnitude".into())
            })?;
            let mag = parse_value(mag_tok).map_err(SimError::Parse)?;
            i += 1;
            let mut phase = 0.0;
            if let Some(next) = rest.get(i) {
                if let Ok(p) = parse_value(next) {
                    phase = p;
                    i += 1;
                }
            }
            spec.ac = Some(Ac { mag, phase });
        } else if token.starts_with("pulse") {
            let p = parenthesised_params(&rest[i..], "pulse")?;
            if p.len() < 7 {
                return Err(SimError::Parse(
                    "PULSE source requires 7 parameters: V1 V2 TD TR TF PW PER".into(),
                ));
            }
            let v1  = parse_value(&p[0]).map_err(SimError::Parse)?;
            let v2  = parse_value(&p[1]).map_err(SimError::Parse)?;
            let td  = parse_value(&p[2]).map_err(SimError::Parse)?;
            let tr  = parse_value(&p[3]).map_err(SimError::Parse)?;
            let tf  = parse_value(&p[4]).map_err(SimError::Parse)?;
            let pw  = parse_value(&p[5]).map_err(SimError::Parse)?;
            let per = parse_value(&p[6]).map_err(SimError::Parse)?;
            set_waveform(&mut spec, Waveform::Pulse(Pulse { v1, v2, td, tr, tf, pw, per }))?;
            return Ok(spec);
        } else if token.starts_with("sin") && !token.starts_with("sffm") {
            let s = parenthesised_params(&rest[i..], "sin")?;
            if s.len() < 3 {
                return Err(SimError::Parse(
                    "SIN source requires at least 3 parameters: VO VA FREQ [TD [THETA]]".into(),
                ));
            }
            let vo   = parse_value(&s[0]).map_err(SimError::Parse)?;
            let va   = parse_value(&s[1]).map_err(SimError::Parse)?;
            let freq = parse_value(&s[2]).map_err(SimError::Parse)?;
            let td    = if s.len() > 3 { parse_value(&s[3]).map_err(SimError::Parse)? } else { 0.0 };
            let theta = if s.len() > 4 { parse_value(&s[4]).map_err(SimError::Parse)? } else { 0.0 };
            set_waveform(&mut spec, Waveform::Sine(Sine { vo, va, freq, td, theta }))?;
            return Ok(spec);
        } else if token.starts_with("exp") {
            let e = parenthesised_params(&rest[i..], "exp")?;
            if e.len() < 6 {
                return Err(SimError::Parse(
                    "EXP source requires 6 parameters: V1 V2 TD1 TAU1 TD2 TAU2".into(),
                ));
            }
            let v1 = parse_value(&e[0]).map_err(SimError::Parse)?;
            let v2 = parse_value(&e[1]).map_err(SimError::Parse)?;
            let td1 = parse_value(&e[2]).map_err(SimError::Parse)?;
            let tau1 = parse_value(&e[3]).map_err(SimError::Parse)?;
            let td2 = parse_value(&e[4]).map_err(SimError::Parse)?;
            let tau2 = parse_value(&e[5]).map_err(SimError::Parse)?;
            set_waveform(&mut spec, Waveform::Exp(Exp { v1, v2, td1, tau1, td2, tau2 }))?;
            return Ok(spec);
        } else if token.starts_with("sffm") {
            let s = parenthesised_params(&rest[i..], "sffm")?;
            if s.len() < 5 {
                return Err(SimError::Parse(
                    "SFFM source requires 5 parameters: VO VA FC MDI FS [TD]".into(),
                ));
            }
            let vo = parse_value(&s[0]).map_err(SimError::Parse)?;
            let va = parse_value(&s[1]).map_err(SimError::Parse)?;
            let fc = parse_value(&s[2]).map_err(SimError::Parse)?;
            let mdi = parse_value(&s[3]).map_err(SimError::Parse)?;
            let fs = parse_value(&s[4]).map_err(SimError::Parse)?;
            let td = if s.len() > 5 { parse_value(&s[5]).map_err(SimError::Parse)? } else { 0.0 };
            set_waveform(&mut spec, Waveform::Sffm(Sffm { vo, va, fc, mdi, fs, td }))?;
            return Ok(spec);
        } else if token.starts_with("am") {
            let a = parenthesised_params(&rest[i..], "am")?;
            if a.len() < 4 {
                return Err(SimError::Parse(
                    "AM source requires 4 parameters: VA VO FM FC [TD]".into(),
                ));
            }
            let va = parse_value(&a[0]).map_err(SimError::Parse)?;
            let vo = parse_value(&a[1]).map_err(SimError::Parse)?;
            let fm = parse_value(&a[2]).map_err(SimError::Parse)?;
            let fc = parse_value(&a[3]).map_err(SimError::Parse)?;
            let td = if a.len() > 4 { parse_value(&a[4]).map_err(SimError::Parse)? } else { 0.0 };
            set_waveform(&mut spec, Waveform::Am(Am { va, vo, fm, fc, td }))?;
            return Ok(spec);
        } else if token.starts_with("pwl") {
            let pwl = parse_pwl(&rest[i..], options)?;
            set_waveform(&mut spec, Waveform::Pwl(pwl))?;
            return Ok(spec);
        } else if spec.dc.is_none() {
            // Bare value — SPICE allows the DC keyword to be omitted.
            spec.dc = Some(parse_value(rest[i]).map_err(SimError::Parse)?);
            i += 1;
        } else {
            return Err(SimError::Parse(format!(
                "unexpected token '{}' in source definition", rest[i]
            )));
        }
    }
    Ok(spec)
}

pub(super) fn set_waveform(spec: &mut SourceSpec, waveform: Waveform) -> Result<()> {
    if spec.waveform.is_some() {
        return Err(SimError::Parse(
            "a source card may carry only one waveform (PULSE/SIN/EXP/PWL/SFFM/AM)".into(),
        ));
    }
    spec.waveform = Some(waveform);
    Ok(())
}

/// Parse a `PWL(…)` card, including `PWL FILE=<file>` and LTspice's
/// `REPEAT FOR <n> (…) ENDREPEAT` / `REPEAT FOREVER (…) ENDREPEAT` blocks.
pub(super) fn parse_pwl(rest: &[&str], options: &ParseOptions) -> Result<Pwl> {
    let text = rest.join(" ");
    let trimmed = text.trim();
    let after = if trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("pwl") {
        &trimmed[3..]
    } else {
        return Err(SimError::Parse("broken PWL card".into()));
    };
    let after = after.trim();
    let after = after.strip_prefix('=').unwrap_or(after).trim();

    if after.len() >= 4 && after[..4].eq_ignore_ascii_case("file") {
        let file_part = after[4..].trim_start_matches(['=', ' ', '\t']);
        let name = file_part
            .split(|c: char| c.is_whitespace() || c == ')' || c == ',')
            .next()
            .unwrap_or("")
            .trim_matches(|c| c == '"' || c == '\'');
        if name.is_empty() {
            return Err(SimError::Parse("PWL FILE requires a file name".into()));
        }
        let path = find_library_file(name, options.deck_dir.as_deref(), options)
            .ok_or_else(|| SimError::Parse(format!(
                "PWL FILE '{name}' not found in the deck directory or search paths"
            )))?;
        let (content, _) = open_deck_file(&path).map_err(|e| {
            SimError::Parse(format!("PWL FILE '{}': {e}", path.display()))
        })?;
        let mut points = Vec::new();
        for (n, line) in content.lines().enumerate() {
            let line = strip_comment(line);
            let line = line.trim();
            if line.is_empty() || line.starts_with('.') {
                continue;
            }
            let mut words = line.split(|c: char| c.is_whitespace() || c == ',');
            let t = words.next().unwrap_or("");
            let v = words.next().ok_or_else(|| {
                SimError::Parse(format!(
                    "PWL FILE '{}': line {} needs a time and a value",
                    path.display(),
                    n + 1
                ))
            })?;
            let t = parse_value(t).map_err(|e| {
                SimError::Parse(format!("PWL FILE '{}': line {} time: {e}", path.display(), n + 1))
            })?;
            let v = parse_value(v).map_err(|e| {
                SimError::Parse(format!("PWL FILE '{}': line {} value: {e}", path.display(), n + 1))
            })?;
            points.push((t, v));
        }
        let pwl = Pwl { points, repeat_from: None, repeat_count: None };
        pwl.validate().map_err(SimError::Parse)?;
        return Ok(pwl);
    }

    // Tokenize the inline form: commas/parentheses are separators, and the
    // repeat markers are ordinary words in the stream.
    let cleaned: String = after
        .chars()
        .map(|c| if matches!(c, '(' | ')' | ',') { ' ' } else { c })
        .collect();
    let tokens: Vec<&str> = cleaned.split_whitespace().collect();
    let mut points: Vec<(f64, f64)> = Vec::new();
    let mut repeat_start: Option<usize> = None;
    let mut repeat_count: Option<Option<usize>> = None;
    let mut i = 0usize;
    while i < tokens.len() {
        match tokens[i].to_ascii_lowercase().as_str() {
            "repeat" => {
                i += 1;
                let mut count = None;
                if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("for")) {
                    i += 1;
                }
                if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("forever")) {
                    i += 1;
                } else if let Some(tok) = tokens.get(i) {
                    if let Ok(n) = tok.parse::<usize>() {
                        count = Some(n);
                        i += 1;
                    }
                }
                repeat_start = Some(points.len());
                repeat_count = Some(count);
            }
            "endrepeat" => i += 1,
            _ => {
                let tok = tokens[i];
                let time = parse_value(tok).map_err(|e| {
                    SimError::Parse(format!("PWL time '{tok}': {e}"))
                })?;
                let value_tok = tokens.get(i + 1).ok_or_else(|| {
                    SimError::Parse(format!(
                        "PWL needs a value after time '{tok}' (pairs are time value)"
                    ))
                })?;
                let value = parse_value(value_tok).map_err(|e| {
                    SimError::Parse(format!("PWL value '{value_tok}': {e}"))
                })?;
                points.push((time, value));
                i += 2;
            }
        }
    }

    let repeat_from = match repeat_start {
        Some(start) if start < points.len() => Some(points[start].0),
        Some(_) => {
            return Err(SimError::Parse(
                "PWL REPEAT block contains no (time, value) pairs".into(),
            ))
        }
        None => None,
    };
    let pwl = Pwl { points, repeat_from, repeat_count };
    pwl.validate().map_err(SimError::Parse)?;
    Ok(pwl)
}

/// Join the tail of a source card from `start`, strip the function name,
/// parentheses and commas, and split into parameters.
pub(super) fn parenthesised_params(rest: &[&str], name: &'static str) -> Result<Vec<String>> {
    let remainder = rest.join(" ");
    let cleaned = remainder
        .to_ascii_lowercase()
        .replace(name, "")
        .replace('(', "")
        .replace(')', "")
        .replace(',', " ");
    Ok(cleaned.split_whitespace().map(|s| s.to_string()).collect())
}

/// Canonical text for one source waveform (the inverse of the source-card
/// grammar). PWL repeat blocks are emitted as a linear point list — the
/// waveform values are identical even though the block markers are lost.
pub(super) fn waveform_text(w: &Waveform) -> String {
    match w {
        Waveform::Pulse(p) => format!(
            "PULSE({} {} {} {} {} {} {})",
            p.v1, p.v2, p.td, p.tr, p.tf, p.pw, p.per
        ),
        Waveform::Sine(s) => format!("SIN({} {} {} {} {})", s.vo, s.va, s.freq, s.td, s.theta),
        Waveform::Exp(e) => format!(
            "EXP({} {} {} {} {} {})",
            e.v1, e.v2, e.td1, e.tau1, e.td2, e.tau2
        ),
        Waveform::Pwl(p) => {
            let mut text = String::from("PWL(");
            for (i, (t, v)) in p.points.iter().enumerate() {
                if i > 0 {
                    text.push(' ');
                }
                text.push_str(&format!("{t} {v}"));
            }
            text.push(')');
            text
        }
        Waveform::Sffm(s) => format!(
            "SFFM({} {} {} {} {} {})",
            s.vo, s.va, s.fc, s.mdi, s.fs, s.td
        ),
        Waveform::Am(a) => format!("AM({} {} {} {} {})", a.va, a.vo, a.fm, a.fc, a.td),
    }
}
