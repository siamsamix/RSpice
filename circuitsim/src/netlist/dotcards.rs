//! Dot commands: the `.dc`/`.ac`/`.tran`/`.step`/`.meas`/… grammar and

//! its canonical emission — every analysis command a deck may carry.

use super::*;

// ─── Dot commands ────────────────────────────────────────────────────────────

/// One dot-command card from the flattened deck.
///
/// Tolerance split: malformed arguments of a *known* directive are loud
/// errors (`.dc` sweeps, `.tran` timings), while directives RSpice does not
/// implement at all are recorded in `warnings` and skipped — third-party
/// decks import as much as the engine can simulate (TODO §2).
pub(super) fn parse_dot_command(
    line: &str,
    analysis: &mut AnalysisCommands,
    circuit: &mut Circuit,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let cmd = tokens[0].to_ascii_lowercase();
    match cmd.as_str() {
        ".model" => parse_model_card(line, &tokens, circuit, warnings, line_no)?,
        ".op" => {
            analysis.dc_op = true;
            for tok in &tokens[1..] {
                match tok.to_ascii_lowercase().as_str() {
                    // PSpice output-detail variants (TODO §4): `pon` prints
                    // the plain bias point, `all` asks for the annotated
                    // device operating-point view on top.
                    "pon" => analysis.op_mode = OpMode::Pon,
                    "all" => analysis.op_mode = OpMode::All,
                    _ => warnings.push(ignored_tokens_owned(line_no, ".op", tok)),
                }
            }
        }
        ".tran" => {
            if tokens.len() < 3 {
                return Err(SimError::Parse(
                    ".tran requires: .tran <tstep> <tstop> [tstart] [UIC]".into(),
                ));
            }
            analysis.tran_step = Some(parse_value(tokens[1]).map_err(SimError::Parse)?);
            analysis.tran_stop = Some(parse_value(tokens[2]).map_err(SimError::Parse)?);
            let mut t_start: Option<f64> = None;
            let mut t_max: Option<f64> = None;
            for tok in &tokens[3..] {
                if tok.eq_ignore_ascii_case("uic") {
                    analysis.tran_uic = true;
                    continue;
                }
                if tok.eq_ignore_ascii_case("startup") {
                    analysis.tran_startup = true;
                    continue;
                }
                match parse_value(tok) {
                    Ok(v) if t_start.is_none() => t_start = Some(v),
                    Ok(v) if t_max.is_none() => t_max = Some(v),
                    Ok(_) => warnings.push(ignored_tokens_owned(line_no, ".tran", tok)),
                    Err(_) => warnings.push(format!(
                        "line {line_no}: .tran ignores unsupported token '{tok}'"
                    )),
                }
            }
            analysis.tran_start = t_start.unwrap_or(0.0);
            analysis.tran_maxstep = t_max;
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
            if tokens.len() > 6 {
                warnings.push(ignored_tokens(line_no, ".ac", &tokens[6..]));
            }
        }
        ".dc" => parse_dc(&tokens, analysis, line_no)?,
        ".tf" => {
            if tokens.len() < 3 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .tf requires: .tf <V(out)|I(Vsrc)> <input source>"
                )));
            }
            if tokens.len() > 3 {
                warnings.push(ignored_tokens(line_no, ".tf", &tokens[3..]));
            }
            analysis.tf = Some(TfCommand {
                output: parse_tf_output(tokens[1], line_no)?,
                input: tokens[2].to_string(),
            });
        }
        ".options" | ".option" => {
            parse_options(&tokens, analysis, warnings, line_no)?;
        }
        ".temp" => {
            if tokens.len() < 2 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .temp requires at least one temperature in °C"
                )));
            }
            for tok in &tokens[1..] {
                let t = parse_value(tok).map_err(SimError::Parse)?;
                if !(-273.15..=1000.0).contains(&t) {
                    return Err(SimError::Parse(format!(
                        "line {line_no}: .temp {t} °C is outside the physical range"
                    )));
                }
                analysis.temperatures.push(t);
            }
            if let Some(&first) = analysis.temperatures.first() {
                circuit.temperature = first;
            }
        }
        ".step" => parse_step(&tokens, analysis, warnings, line_no)?,
        ".meas" | ".measure" => {
            analysis
                .measurements
                .push(parse_measurement(&tokens, line_no)?);
        }
        ".noise" => {
            if tokens.len() < 7 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .noise requires: .noise V(out) <input src> DEC|OCT|LIN <points> <fstart> <fstop>"
                )));
            }
            let output = parse_tf_output(tokens[1], line_no)?;
            if matches!(output, TfOutput::Current(_)) {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .noise output must be a node voltage V(out) or V(a,b)"
                )));
            }
            let scale = match tokens[3].to_ascii_lowercase().as_str() {
                "dec" => AcScale::Dec,
                "oct" => AcScale::Oct,
                "lin" => AcScale::Lin,
                other => {
                    return Err(SimError::Parse(format!(
                        "line {line_no}: .noise sweep type '{other}' must be DEC, OCT, or LIN"
                    )))
                }
            };
            let points: usize = tokens[4].parse().map_err(|_| {
                SimError::Parse(format!("line {line_no}: invalid .noise point count '{}'", tokens[4]))
            })?;
            let f_start = parse_value(tokens[5]).map_err(SimError::Parse)?;
            let f_stop = parse_value(tokens[6]).map_err(SimError::Parse)?;
            if tokens.len() > 7 {
                warnings.push(ignored_tokens(line_no, ".noise", &tokens[7..]));
            }
            analysis.noise = Some(NoiseCommand {
                output,
                input: tokens[2].to_string(),
                scale,
                points,
                f_start,
                f_stop,
            });
        }
        ".four" => {
            if tokens.len() < 3 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .four requires: .four <frequency> <output…>"
                )));
            }
            let frequency = parse_value(tokens[1]).map_err(SimError::Parse)?;
            if frequency <= 0.0 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .four frequency must be > 0"
                )));
            }
            analysis.four = Some(FourCommand {
                frequency,
                outputs: tokens[2..].iter().map(|s| (*s).to_string()).collect(),
            });
        }
        ".ic" => {
            let entries = parse_ic_entries(&tokens, ".ic", line_no, warnings)?;
            analysis.ic.extend(entries);
        }
        ".nodeset" => {
            let entries = parse_ic_entries(&tokens, ".nodeset", line_no, warnings)?;
            analysis.nodeset.extend(entries);
        }
        other => {
            // Echo the arguments so the user can find the card in their deck.
            let shown = if tokens.len() > 1 {
                format!("{other} {}", tokens[1..].join(" "))
            } else {
                other.to_string()
            };
            warnings.push(format!(
                "line {line_no}: directive '{shown}' is not recognized — skipped with a warning, never fatal"
            ));
        }
    }
    Ok(())
}

/// The standard shape of a "these cards were carried, not applied" warning.
pub(super) fn ignored_tokens<S: AsRef<str>>(
    line_no: usize,
    directive: &str,
    tokens: &[S],
) -> String {
    let tokens: Vec<&str> = tokens.iter().map(|t| t.as_ref()).collect();
    format!(
        "line {line_no}: {directive} ignores unsupported tokens: {}",
        tokens.join(" ")
    )
}

/// The same warning for owned tokens (`.tran` numeric extras).
pub(super) fn ignored_tokens_owned(line_no: usize, directive: &str, rest: &str) -> String {
    format!("line {line_no}: {directive} ignores the extra token '{rest}'")
}

/// Parse the output operand of `.tf`: `V(node)`, `V(a,b)`, or `I(Vname)`.
pub(super) fn parse_tf_output(token: &str, line_no: usize) -> Result<TfOutput> {
    let open = token.find('(').ok_or_else(|| {
        SimError::Parse(format!(
            "line {line_no}: .tf output '{token}' must be V(node) or I(Vsrc)"
        ))
    })?;
    let close = token[open..]
        .find(')')
        .map(|c| open + c)
        .ok_or_else(|| {
            SimError::Parse(format!(
                "line {line_no}: .tf output '{token}' is missing ')'"
            ))
        })?;
    let letter = token[..open].trim().to_ascii_uppercase();
    let inner = token[open + 1..close].trim();
    if inner.is_empty() {
        return Err(SimError::Parse(format!(
            "line {line_no}: .tf output '{token}' names nothing"
        )));
    }
    match letter.as_str() {
        "V" => {
            let mut parts = inner.split(',').map(str::trim);
            let node = parts.next().unwrap_or("").to_string();
            let reference = parts
                .next()
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            if node.is_empty() {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .tf output '{token}' names no node"
                )));
            }
            Ok(TfOutput::Voltage { node, reference })
        }
        "I" => Ok(TfOutput::Current(inner.to_string())),
        _ => Err(SimError::Parse(format!(
            "line {line_no}: .tf output '{token}' must be V(…) or I(…)"
        ))),
    }
}

/// Parse the `V(node)=value` entry list behind an `.ic`/`.nodeset` card.
///
/// The reference spelling puts one entry per token (`V(a)=1 V(b)=2`); the
/// assignment and its value may also be split across tokens (`V(a) = 1`,
/// `V(a)= 1`, `V(a) =1`), which several tools emit. Current entries
/// (`I(…)`) are recognized but not modeled yet, so they warn and are
/// skipped (inductor seeding is not supported at all); anything that is
/// not a V/I entry warns too.
pub(super) fn parse_ic_entries(
    tokens: &[&str],
    directive: &str,
    line_no: usize,
    warnings: &mut Vec<String>,
) -> Result<Vec<NodesetEntry>> {
    let mut entries = Vec::new();
    let mut i = 1usize; // tokens[0] is the directive itself
    while i < tokens.len() {
        let tok = tokens[i];
        if !tok.contains('(') {
            warnings.push(format!(
                "line {line_no}: {directive} ignores stray token '{tok}'"
            ));
            i += 1;
            continue;
        }

        let open_rel = tok.find('(').expect("'(' checked above");
        if open_rel == 0 {
            return Err(SimError::Parse(format!(
                "line {line_no}: {directive} entry '{tok}' is missing its device letter (V/I)"
            )));
        }
        let dev = tok[..open_rel].to_ascii_uppercase();
        let close_rel = tok[open_rel..]
            .find(')')
            .map(|c| open_rel + c)
            .ok_or_else(|| {
                SimError::Parse(format!(
                    "line {line_no}: {directive} entry '{tok}' is missing ')'"
                ))
            })?;
        let node = tok[open_rel + 1..close_rel].trim().to_string();
        if node.is_empty() {
            return Err(SimError::Parse(format!(
                "line {line_no}: {directive} entry '{tok}' names no node"
            )));
        }

        // The value rides fused after ')' (`V(n)=5`), or lands in later
        // tokens (`V(n) = 5`, `V(n)= 5`, `V(n) =5`).
        let after = &tok[close_rel + 1..];
        if !after.is_empty() && !after.starts_with('=') {
            return Err(SimError::Parse(format!(
                "line {line_no}: {directive} entry '{tok}' must match V(node)=value"
            )));
        }
        let mut value_tok = after.strip_prefix('=').unwrap_or("").to_string();
        let mut next = i + 1;
        while value_tok.is_empty() {
            let candidate = tokens.get(next).ok_or_else(|| {
                SimError::Parse(format!(
                    "line {line_no}: {directive} entry '{tok}' is missing its value"
                ))
            })?;
            if candidate.contains('(') {
                // The next entry arrived while the value was still pending.
                return Err(SimError::Parse(format!(
                    "line {line_no}: {directive} entry '{tok}' is missing its value"
                )));
            }
            let stripped = candidate.trim_start_matches('=');
            if stripped.is_empty() {
                value_tok = String::new();
                next += 1;
                continue; // a lone '=' spacer — keep looking
            }
            value_tok = stripped.to_string();
            next += 1;
            break;
        }
        let value = parse_value(&value_tok).map_err(|e| {
            SimError::Parse(format!(
                "line {line_no}: {directive} entry '{tok}': {e}"
            ))
        })?;

        match dev.as_str() {
            "V" => entries.push(NodesetEntry { node, value }),
            "I" => warnings.push(format!(
                "line {line_no}: {directive} entry I({node}) is not supported — skipped (inductor currents are not seeded)"
            )),
            _ => warnings.push(format!(
                "line {line_no}: {directive} only carries V()/I() entries — '{tok}' skipped"
            )),
        }
        i = next;
    }
    Ok(entries)
}

/// Parse `.options` keys the engine consumes. Unknown keys warn (the full
/// reference-tool option surface is a TODO §2 item) rather than failing an
/// otherwise simulatable deck.
pub(super) fn parse_options(
    tokens: &[&str],
    analysis: &mut AnalysisCommands,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 2 {
        warnings.push(format!("line {line_no}: .options card has no options"));
        return Ok(());
    }
    for tok in &tokens[1..] {
        let Some((key, value)) = tok.split_once('=') else {
            warnings.push(format!(
                "line {line_no}: .options token '{tok}' is not KEY=VALUE — ignored"
            ));
            continue;
        };
        let key = key.to_ascii_lowercase();
        match key.as_str() {
            "method" => {
                analysis.method = match value.to_ascii_lowercase().as_str() {
                    "trap" | "trapezoidal" => crate::circuit::IntegrationMethod::Trapezoidal,
                    "gear" | "gear2" => crate::circuit::IntegrationMethod::Gear2,
                    "gear1" | "be" | "euler" => crate::circuit::IntegrationMethod::Gear1,
                    other => {
                        return Err(SimError::Parse(format!(
                            "line {line_no}: .options method={other} must be trap, gear2, or gear1"
                        )))
                    }
                };
            }
            "reltol" => analysis.reltol = parse_value(value).map_err(SimError::Parse)?,
            "abstol" => analysis.abstol = parse_value(value).map_err(SimError::Parse)?,
            "vntol" => analysis.vntol = parse_value(value).map_err(SimError::Parse)?,
            "maxstep" | "tmax" => {
                analysis.tran_maxstep = Some(parse_value(value).map_err(SimError::Parse)?)
            }
            // TODO §3 convergence suite knobs: they drive the DC operating
            // point (gmin floor, the gmin/source-stepping ladder depths) and
            // the Newton iteration budgets at DC (`itl1`) and per transient
            // point (`itl4`).
            "gmin" => analysis.gmin = parse_value(value).map_err(SimError::Parse)?,
            "gminsteps" => {
                analysis.gminsteps =
                    parse_value(value).map_err(SimError::Parse)?.max(0.0) as usize
            }
            "srcsteps" => {
                analysis.srcsteps =
                    parse_value(value).map_err(SimError::Parse)?.max(0.0) as usize
            }
            "itl1" => analysis.itl1 = parse_value(value).map_err(SimError::Parse)?.max(1.0) as usize,
            "itl4" => analysis.itl4 = parse_value(value).map_err(SimError::Parse)?.max(1.0) as usize,
            // TODO §8: `.options seed=` — the deck's base Monte-Carlo seed.
            // Element lines expand (and draw) before dot commands execute,
            // so the seed is ALSO lifted from the raw logical lines in
            // [`crate::parse_featured`]; this arm records it for analysis
            // visibility and the round-trip. Values that only the walk could
            // read (e.g. parameterized) warn there instead of silently
            // changing the expansion-time draws.
            "seed" => {
                analysis.mc_seed = parse_value(value).map_err(SimError::Parse)?.max(0.0) as u64;
            }
            // Parsed but not applied yet; keep them as warnings instead of
            // pretending the engine honours them.
            "itl2" | "itl6" | "maxord" | "tnom" | "temp"
            | "accurate" | "solvehold" | "plotwinsize" | "numdgt" => {
                warnings.push(format!(
                    "line {line_no}: .options {key}={value} is parsed but not applied yet"
                ));
            }
            other => warnings.push(format!(
                "line {line_no}: .options '{other}' is not recognized — ignored"
            )),
        }
    }
    Ok(())
}

/// Parse one `.step` card.
///
/// Accepted spellings (LTspice/PSpice-compatible):
/// `.step param x 0 10 1`, `.step dec param x 1 1k 10`,
/// `.step oct param x 1 1k 5`, `.step param x list 1 2 3`,
/// and the `PARAM`-less `.step x …` shorthand.
pub(super) fn parse_step(
    tokens: &[&str],
    analysis: &mut AnalysisCommands,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 3 {
        return Err(SimError::Parse(format!(
            "line {line_no}: .step requires: .step [DEC|OCT|LIN] PARAM <name> <values>"
        )));
    }
    let mut i = 1usize;
    let scale = match tokens[i].to_ascii_lowercase().as_str() {
        "dec" => {
            i += 1;
            Some(AcScale::Dec)
        }
        "oct" => {
            i += 1;
            Some(AcScale::Oct)
        }
        "lin" => {
            i += 1;
            Some(AcScale::Lin)
        }
        _ => None,
    };
    if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("param")) {
        i += 1;
    }
    let target = tokens
        .get(i)
        .ok_or_else(|| {
            SimError::Parse(format!("line {line_no}: .step is missing its parameter name"))
        })?
        .to_ascii_lowercase();
    i += 1;
    if !analysis
        .steps
        .iter()
        .all(|s| s.target != target)
    {
        return Err(SimError::Parse(format!(
            "line {line_no}: .step already sweeps '{target}' — each parameter may be stepped once"
        )));
    }

    let (values, consumed) = parse_step_values(&tokens[i..], scale, line_no, &target)?;
    if i + consumed < tokens.len() {
        warnings.push(ignored_tokens(line_no, ".step", &tokens[i + consumed..]));
    }
    analysis.steps.push(StepSpec { target, values });
    Ok(())
}

/// The value part of a `.step` card, returning the parsed set and how many
/// tokens it consumed. Malformed values are loud errors.
pub(super) fn parse_step_values(
    rest: &[&str],
    scale: Option<AcScale>,
    line_no: usize,
    target: &str,
) -> Result<(StepValues, usize)> {
    let Some(first) = rest.first() else {
        return Err(SimError::Parse(format!(
            "line {line_no}: .step {target} needs values"
        )));
    };
    if first.eq_ignore_ascii_case("list") {
        let mut values = Vec::new();
        for tok in &rest[1..] {
            values.push(parse_value(tok).map_err(SimError::Parse)?);
        }
        if values.is_empty() {
            return Err(SimError::Parse(format!(
                "line {line_no}: .step {target} LIST needs at least one value"
            )));
        }
        return Ok((StepValues::List(values), rest.len()));
    }
    if rest.len() < 3 {
        return Err(SimError::Parse(format!(
            "line {line_no}: .step {target} needs start, stop, and increment/points"
        )));
    }
    let start = parse_value(rest[0]).map_err(SimError::Parse)?;
    let stop = parse_value(rest[1]).map_err(SimError::Parse)?;
    match scale {
        Some(AcScale::Dec) | Some(AcScale::Oct) => {
            let points: usize = rest[2].parse().map_err(|_| {
                SimError::Parse(format!(
                    "line {line_no}: .step {target}: '{}' is not a point count",
                    rest[2]
                ))
            })?;
            if start <= 0.0 || stop <= 0.0 || stop < start {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .step {target}: logarithmic steps need 0 < start <= stop"
                )));
            }
            if points == 0 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .step {target}: points per decade/octave must be > 0"
                )));
            }
            let values = if matches!(scale, Some(AcScale::Dec)) {
                StepValues::Decade { start, stop, points }
            } else {
                StepValues::Octave { start, stop, points }
            };
            Ok((values, 3))
        }
        _ => {
            let step = parse_value(rest[2]).map_err(SimError::Parse)?;
            if step == 0.0 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .step {target}: increment cannot be zero"
                )));
            }
            if (stop - start) * step < 0.0 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .step {target}: increment moves away from stop"
                )));
            }
            Ok((StepValues::Linear { start, stop, step }, 3))
        }
    }
}

/// Parse one `.meas`/`.measure` card.
///
/// `.meas <tran|ac|dc|op> <name> <function…>` with the reference tools'
/// function set: `avg rms max min pp max_at min_at`, `find … when …`,
/// `cross … val=…`, and `trig … targ …`. Options are `from=`, `to=`, `val=`,
/// and `rise[=n]`/`fall[=n]`/`cross[=n]`.
pub(super) fn parse_measurement(tokens: &[&str], line_no: usize) -> Result<Measurement> {
    if tokens.len() < 4 {
        return Err(SimError::Parse(format!(
            "line {line_no}: .meas requires: .meas <tran|ac|dc|op> <name> <function>"
        )));
    }
    let analysis = match tokens[1].to_ascii_lowercase().as_str() {
        "tran" => MeasAnalysis::Tran,
        "ac" => MeasAnalysis::Ac,
        "dc" => MeasAnalysis::Dc,
        "op" => MeasAnalysis::Op,
        other => {
            return Err(SimError::Parse(format!(
                "line {line_no}: .meas analysis '{other}' must be tran, ac, dc, or op"
            )))
        }
    };
    let name = tokens[2].to_string();
    let function = tokens[3].to_ascii_lowercase();
    let func = match function.as_str() {
        "avg" => MeasFunc::Avg,
        "rms" => MeasFunc::Rms,
        "max" => MeasFunc::Max,
        "min" => MeasFunc::Min,
        "pp" => MeasFunc::Pp,
        "max_at" => MeasFunc::MaxAt,
        "min_at" => MeasFunc::MinAt,
        "find" => {
            return parse_find_when(tokens, analysis, name, line_no);
        }
        "cross" => return parse_cross(tokens, analysis, name, line_no),
        "trig" => return parse_trig_targ(tokens, analysis, name, line_no),
        other => {
            return Err(SimError::Parse(format!(
                "line {line_no}: .meas function '{other}' is not supported \
                 (use avg/rms/max/min/pp/max_at/min_at/find/cross/trig)"
            )))
        }
    };
    let expr = tokens
        .get(4)
        .ok_or_else(|| {
            SimError::Parse(format!("line {line_no}: .meas {name}: '{function}' needs an expression"))
        })?
        .to_string();
    let opts = parse_meas_options(&tokens[5..], line_no, &name)?;
    if let Some(value) = opts.value {
        return Err(SimError::Parse(format!(
            "line {line_no}: .meas {name}: VAL={value} is only meaningful for find/cross/trig"
        )));
    }
    if opts.edge.is_some() {
        return Err(SimError::Parse(format!(
            "line {line_no}: .meas {name}: an edge selector is only meaningful for find/cross/trig"
        )));
    }
    Ok(Measurement {
        analysis,
        name,
        kind: MeasKind::Scalar { func, expr, from: opts.from, to: opts.to },
    })
}

pub(super) fn parse_find_when(
    tokens: &[&str],
    analysis: MeasAnalysis,
    name: String,
    line_no: usize,
) -> Result<Measurement> {
    let expr = tokens
        .get(4)
        .ok_or_else(|| SimError::Parse(format!("line {line_no}: .meas {name}: FIND needs an expression")))?
        .to_string();
    if !tokens.get(5).is_some_and(|t| t.eq_ignore_ascii_case("when")) {
        return Err(SimError::Parse(format!(
            "line {line_no}: .meas {name}: FIND must be followed by WHEN <expr>=<value>"
        )));
    }
    let condition = tokens.get(6).ok_or_else(|| {
        SimError::Parse(format!("line {line_no}: .meas {name}: WHEN needs a condition"))
    })?;
    let (when, when_value, consumed) = split_condition(condition, tokens.get(7), line_no, &name)?;
    let opts = parse_meas_options(&tokens[6 + consumed..], line_no, &name)?;
    Ok(Measurement {
        analysis,
        name,
        kind: MeasKind::FindWhen {
            expr,
            when,
            when_value,
            edge: opts.edge,
            from: opts.from,
            to: opts.to,
        },
    })
}

/// Split `V(in)=1.5` (or the spaced `V(in) = 1.5`) into expression, target,
/// and how many tokens the condition consumed.
pub(super) fn split_condition(
    first: &str,
    next: Option<&&str>,
    line_no: usize,
    name: &str,
) -> Result<(String, f64, usize)> {
    if let Some(eq) = first.find('=') {
        let expr = first[..eq].trim().to_string();
        let value = parse_value(&first[eq + 1..]).map_err(SimError::Parse)?;
        if expr.is_empty() {
            return Err(SimError::Parse(format!(
                "line {line_no}: .meas {name}: WHEN condition has no expression"
            )));
        }
        return Ok((expr, value, 1));
    }
    // `V(in) = 1.5`.
    if next.is_some_and(|t| *t == "=") {
        let value_tok = next.unwrap();
        let _ = value_tok;
        // The caller passed the token after the condition; `= 1.5` needs one
        // more token than the signature can see, so support the compact form
        // only and report the spaced form clearly.
    }
    Err(SimError::Parse(format!(
        "line {line_no}: .meas {name}: WHEN condition '{first}' must be <expr>=<value>"
    )))
}

pub(super) fn parse_cross(
    tokens: &[&str],
    analysis: MeasAnalysis,
    name: String,
    line_no: usize,
) -> Result<Measurement> {
    let expr = tokens
        .get(4)
        .ok_or_else(|| SimError::Parse(format!("line {line_no}: .meas {name}: CROSS needs an expression")))?
        .to_string();
    let opts = parse_meas_options(&tokens[5..], line_no, &name)?;
    let value = opts.value.ok_or_else(|| {
        SimError::Parse(format!("line {line_no}: .meas {name}: CROSS needs VAL=<level>"))
    })?;
    Ok(Measurement {
        analysis,
        name,
        kind: MeasKind::Cross { expr, value, edge: opts.edge, from: opts.from, to: opts.to },
    })
}

pub(super) fn parse_trig_targ(
    tokens: &[&str],
    analysis: MeasAnalysis,
    name: String,
    line_no: usize,
) -> Result<Measurement> {
    let trig_expr = tokens
        .get(4)
        .ok_or_else(|| SimError::Parse(format!("line {line_no}: .meas {name}: TRIG needs an expression")))?
        .to_string();
    let mut i = 5usize;
    let mut trig_opts = MeasOpts::default();
    while i < tokens.len() && !tokens[i].eq_ignore_ascii_case("targ") {
        trig_opts.merge_token(tokens[i], line_no, &name)?;
        i += 1;
    }
    if i >= tokens.len() {
        return Err(SimError::Parse(format!(
            "line {line_no}: .meas {name}: TRIG must be followed by TARG"
        )));
    }
    i += 1;
    let targ_expr = tokens
        .get(i)
        .ok_or_else(|| SimError::Parse(format!("line {line_no}: .meas {name}: TARG needs an expression")))?
        .to_string();
    i += 1;
    let targ_opts = parse_meas_options(&tokens[i..], line_no, &name)?;
    let trig_value = trig_opts.value.ok_or_else(|| {
        SimError::Parse(format!("line {line_no}: .meas {name}: TRIG needs VAL=<level>"))
    })?;
    let targ_value = targ_opts.value.ok_or_else(|| {
        SimError::Parse(format!("line {line_no}: .meas {name}: TARG needs VAL=<level>"))
    })?;
    Ok(Measurement {
        analysis,
        name,
        kind: MeasKind::TrigTarg {
            trig_expr,
            trig_value,
            trig_edge: trig_opts.edge,
            targ_expr,
            targ_value,
            targ_edge: targ_opts.edge,
            from: trig_opts.from.or(targ_opts.from),
            to: trig_opts.to.or(targ_opts.to),
        },
    })
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct MeasOpts {
    value: Option<f64>,
    edge: Option<(MeasEdge, usize)>,
    from: Option<f64>,
    to: Option<f64>,
}

impl MeasOpts {
    fn merge_token(&mut self, token: &str, line_no: usize, name: &str) -> Result<()> {
        let parsed = parse_one_meas_option(token, line_no, name)?;
        match parsed {
            MeasOption::Value(v) => self.value = Some(v),
            MeasOption::Edge(e) => self.edge = Some(e),
            MeasOption::From(v) => self.from = Some(v),
            MeasOption::To(v) => self.to = Some(v),
        }
        Ok(())
    }
}

pub(super) enum MeasOption {
    Value(f64),
    Edge((MeasEdge, usize)),
    From(f64),
    To(f64),
}

pub(super) fn parse_one_meas_option(token: &str, line_no: usize, name: &str) -> Result<MeasOption> {
    let lower = token.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("val=") {
        return parse_value(rest)
            .map(MeasOption::Value)
            .map_err(SimError::Parse);
    }
    if let Some(rest) = lower.strip_prefix("from=") {
        return parse_value(rest)
            .map(MeasOption::From)
            .map_err(SimError::Parse);
    }
    if let Some(rest) = lower.strip_prefix("to=") {
        return parse_value(rest)
            .map(MeasOption::To)
            .map_err(SimError::Parse);
    }
    let (word, count) = match lower.split_once('=') {
        Some((word, n)) => {
            let parsed = n.parse::<usize>().map_err(|_| {
                SimError::Parse(format!(
                    "line {line_no}: .meas {name}: '{token}' needs an occurrence count"
                ))
            })?;
            (word.to_string(), parsed)
        }
        None => (lower.clone(), 1),
    };
    let edge = match word.as_str() {
        "rise" => MeasEdge::Rise,
        "fall" => MeasEdge::Fall,
        "cross" => MeasEdge::Cross,
        _ => {
            return Err(SimError::Parse(format!(
                "line {line_no}: .meas {name}: unsupported option '{token}' \
                 (expected from=, to=, val=, or rise/fall/cross[=n])"
            )))
        }
    };
    if count == 0 {
        return Err(SimError::Parse(format!(
            "line {line_no}: .meas {name}: occurrence count must be >= 1"
        )));
    }
    Ok(MeasOption::Edge((edge, count)))
}

pub(super) fn parse_meas_options(tokens: &[&str], line_no: usize, name: &str) -> Result<MeasOpts> {
    let mut opts = MeasOpts::default();
    for tok in tokens {
        opts.merge_token(tok, line_no, name)?;
    }
    Ok(opts)
}


/// Parse the sweep arguments of `.dc`. Malformed arguments are errors —
/// sweep arguments are never silently dropped.
pub(super) fn parse_dc(tokens: &[&str], analysis: &mut AnalysisCommands, line_no: usize) -> Result<()> {
    // A bare `.dc` stays a plain operating point (historical behavior).
    if tokens.len() == 1 {
        analysis.dc_op = true;
        return Ok(());
    }

    let mut sweeps = Vec::new();
    let mut i = 1usize;
    while i < tokens.len() {
        let (spec, next) = parse_dc_sweep_spec(tokens, i, line_no)?;
        sweeps.push(spec);
        i = next;
    }
    if sweeps.len() > 2 {
        return Err(SimError::Parse(format!(
            "line {line_no}: .dc supports at most two nested sweeps, got {}",
            sweeps.len()
        )));
    }
    analysis.dc = Some(DcCommand { sweeps });
    Ok(())
}

/// One sweep spec starting at token `i` (`target [start stop step]` or
/// `target LIST v1 v2 …`), returning the index of the next unconsumed token.
pub(super) fn parse_dc_sweep_spec(
    tokens: &[&str],
    i: usize,
    line_no: usize,
) -> Result<(DcSweepSpec, usize)> {
    let target = tokens.get(i).copied().ok_or_else(|| {
        SimError::Parse(format!("line {line_no}: .dc is missing its sweep source name"))
    })?;
    let first = target
        .chars()
        .next()
        .filter(|c| c.is_ascii_alphabetic())
        .ok_or_else(|| {
            SimError::Parse(format!(
                "line {line_no}: .dc sweep target '{}' must be a source or parameter name",
                target
            ))
        })?;
    let _ = first;
    let target = target.to_string();

    let after = i + 1;
    if tokens
        .get(after)
        .map(|t| t.eq_ignore_ascii_case("list"))
        .unwrap_or(false)
    {
        // Values run until the first token that is not a number — which is
        // where the next sweep spec (if any) begins.
        let mut vals = Vec::new();
        let mut j = after + 1;
        while j < tokens.len() {
            match parse_value(tokens[j]) {
                Ok(v) => vals.push(v),
                Err(_) => break,
            }
            j += 1;
        }
        if vals.is_empty() {
            return Err(SimError::Parse(format!(
                ".dc {} LIST needs at least one value",
                target
            )));
        }
        return Ok((DcSweepSpec { target, values: DcSweepValues::List(vals) }, j));
    }

    if tokens.len() < after + 3 {
        return Err(SimError::Parse(format!(
            "line {line_no}: .dc {} requires <start> <stop> <increment>, or LIST with values",
            target
        )));
    }
    let start = parse_value(tokens[after]).map_err(SimError::Parse)?;
    let stop = parse_value(tokens[after + 1]).map_err(SimError::Parse)?;
    let step = parse_value(tokens[after + 2]).map_err(SimError::Parse)?;

    if step == 0.0 {
        return Err(SimError::Parse(format!(
            "line {line_no}: .dc {} increment cannot be zero",
            target
        )));
    }
    if (stop > start && step < 0.0) || (stop < start && step > 0.0) {
        return Err(SimError::Parse(format!(
            "line {line_no}: .dc {} increment {} must march from {} toward {}",
            target, step, start, stop
        )));
    }
    Ok((
        DcSweepSpec {
            target,
            values: DcSweepValues::Linear { start, stop, step },
        },
        after + 3,
    ))
}

/// Emit the parsed analysis commands (the inverse of
/// [`parse_dot_command`]); `.end` is the orchestrator's.
pub(super) fn emit_commands(analysis: &AnalysisCommands, out: &mut String) {
    // ── Analysis commands ────────────────────────────────────────────────
    if let Some(dc) = &analysis.dc {
        let mut line = String::from(".dc");
        for spec in &dc.sweeps {
            match &spec.values {
                DcSweepValues::Linear { start, stop, step } => {
                    line.push_str(&format!(" {} {} {} {}", spec.target, start, stop, step));
                }
                DcSweepValues::List(values) => {
                    line.push_str(&format!(" {} LIST", spec.target));
                    for v in values {
                        line.push_str(&format!(" {v}"));
                    }
                }
            }
        }
        out.push_str(&line);
        out.push('\n');
    }
    if analysis.dc_op {
        let mut line = String::from(".op");
        // The variants re-spell only when a card asked for them, so plain
        // decks keep their plain text.
        match analysis.op_mode {
            OpMode::Default => {}
            OpMode::Pon => line.push_str(" pon"),
            OpMode::All => line.push_str(" all"),
        }
        out.push_str(&line);
        out.push('\n');
    }
    if let (Some(step), Some(stop)) = (analysis.tran_step, analysis.tran_stop) {
        let mut line = format!(".tran {} {}", step, stop);
        if analysis.tran_start != 0.0 {
            line.push_str(&format!(" {}", analysis.tran_start));
        }
        if let Some(tmax) = analysis.tran_maxstep {
            line.push_str(&format!(" {}", tmax));
        }
        if analysis.tran_uic {
            line.push_str(" UIC");
        }
        if analysis.tran_startup {
            line.push_str(" STARTUP");
        }
        out.push_str(&line);
        out.push('\n');
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
    if let Some(tf) = &analysis.tf {
        let output = match &tf.output {
            TfOutput::Voltage { node, reference } => match reference {
                Some(reference) => format!("V({node},{reference})"),
                None => format!("V({node})"),
            },
            TfOutput::Current(name) => format!("I({name})"),
        };
        out.push_str(&format!(".tf {} {}\n", output, tf.input));
    }
    let mut options = format!(
        ".options method={} reltol={} abstol={} vntol={}",
        match analysis.method {
            crate::circuit::IntegrationMethod::Trapezoidal => "trap",
            crate::circuit::IntegrationMethod::Gear1 => "gear1",
            crate::circuit::IntegrationMethod::Gear2 => "gear2",
        },
        analysis.reltol,
        analysis.abstol,
        analysis.vntol
    );
    // Convergence knobs re-print only when they differ from the engine
    // defaults, so an uncustomized deck's emitted options line is unchanged.
    if analysis.gmin != 1e-12 {
        options.push_str(&format!(" gmin={}", analysis.gmin));
    }
    if analysis.gminsteps != 10 {
        options.push_str(&format!(" gminsteps={}", analysis.gminsteps));
    }
    if analysis.srcsteps != 10 {
        options.push_str(&format!(" srcsteps={}", analysis.srcsteps));
    }
    if analysis.itl1 != 100 {
        options.push_str(&format!(" itl1={}", analysis.itl1));
    }
    if analysis.itl4 != 100 {
        options.push_str(&format!(" itl4={}", analysis.itl4));
    }
    if analysis.mc_seed != 0 {
        // TODO §8: the base Monte-Carlo seed round-trips like the knobs.
        options.push_str(&format!(" seed={}", analysis.mc_seed));
    }
    out.push_str(&options);
    out.push('\n');
    if !analysis.temperatures.is_empty() {
        let values: Vec<String> = analysis.temperatures.iter().map(|t| format!("{t}")).collect();
        out.push_str(&format!(".temp {}\n", values.join(" ")));
    }
    if let Some(noise) = &analysis.noise {
        let output = match &noise.output {
            TfOutput::Voltage { node, reference } => match reference {
                Some(reference) => format!("V({node},{reference})"),
                None => format!("V({node})"),
            },
            TfOutput::Current(name) => format!("I({name})"),
        };
        let scale = match noise.scale {
            AcScale::Dec => "DEC",
            AcScale::Oct => "OCT",
            AcScale::Lin => "LIN",
        };
        out.push_str(&format!(
            ".noise {} {} {} {} {} {}\n",
            output, noise.input, scale, noise.points, noise.f_start, noise.f_stop
        ));
    }
    if let Some(four) = &analysis.four {
        out.push_str(&format!(".four {} {}\n", four.frequency, four.outputs.join(" ")));
    }
    for m in &analysis.measurements {
        out.push_str(&emit_measurement(m));
        out.push('\n');
    }
    for step in &analysis.steps {
        match &step.values {
            StepValues::Linear { start, stop, step: inc } => {
                out.push_str(&format!(".step param {} {} {} {}\n", step.target, start, stop, inc));
            }
            StepValues::Decade { start, stop, points } => {
                out.push_str(&format!(".step dec param {} {} {} {}\n", step.target, start, stop, points));
            }
            StepValues::Octave { start, stop, points } => {
                out.push_str(&format!(".step oct param {} {} {} {}\n", step.target, start, stop, points));
            }
            StepValues::List(values) => {
                let text: Vec<String> = values.iter().map(|v| format!("{v}")).collect();
                out.push_str(&format!(".step param {} list {}\n", step.target, text.join(" ")));
            }
        }
    }
    // Initial conditions: node names are stored as written, so these cards
    // reprint verbatim (the entries' node text resolves again on re-parse).
    for e in &analysis.nodeset {
        out.push_str(&format!(".nodeset V({})={}\n", e.node, e.value));
    }
    for e in &analysis.ic {
        out.push_str(&format!(".ic V({})={}\n", e.node, e.value));
    }
}

// ─── `.meas` emission ────────────────────────────────────────────────────────

/// Canonical text for one `.meas` card — the inverse of `parse_measurement`
/// and its FIND/CROSS/TRIG variants. Options re-print in the card's own
/// vocabulary (`VAL=`, `FROM=`, `TO=`, `RISE|FALL|CROSS[=n]`), so a deck that
/// carries measurements survives `parse(emit(parse))` field for field.
pub(super) fn emit_measurement(m: &Measurement) -> String {
    let mut line = format!(".meas {} {}", meas_analysis_text(m.analysis), m.name);
    match &m.kind {
        MeasKind::Scalar {
            func,
            expr,
            from,
            to,
        } => {
            line.push_str(&format!(" {} {}", meas_func_text(*func), expr));
            push_meas_range(&mut line, *from, *to);
        }
        MeasKind::FindWhen {
            expr,
            when,
            when_value,
            edge,
            from,
            to,
        } => {
            line.push_str(&format!(" FIND {} WHEN {}={}", expr, when, when_value));
            push_meas_edge(&mut line, *edge);
            push_meas_range(&mut line, *from, *to);
        }
        MeasKind::Cross {
            expr,
            value,
            edge,
            from,
            to,
        } => {
            line.push_str(&format!(" CROSS {} VAL={}", expr, value));
            push_meas_edge(&mut line, *edge);
            push_meas_range(&mut line, *from, *to);
        }
        MeasKind::TrigTarg {
            trig_expr,
            trig_value,
            trig_edge,
            targ_expr,
            targ_value,
            targ_edge,
            from,
            to,
        } => {
            line.push_str(&format!(" TRIG {} VAL={}", trig_expr, trig_value));
            push_meas_edge(&mut line, *trig_edge);
            line.push_str(&format!(" TARG {} VAL={}", targ_expr, targ_value));
            push_meas_edge(&mut line, *targ_edge);
            push_meas_range(&mut line, *from, *to);
        }
    }
    line
}

fn meas_analysis_text(analysis: MeasAnalysis) -> &'static str {
    match analysis {
        MeasAnalysis::Tran => "tran",
        MeasAnalysis::Ac => "ac",
        MeasAnalysis::Dc => "dc",
        MeasAnalysis::Op => "op",
    }
}

fn meas_func_text(func: MeasFunc) -> &'static str {
    match func {
        MeasFunc::Avg => "avg",
        MeasFunc::Rms => "rms",
        MeasFunc::Max => "max",
        MeasFunc::Min => "min",
        MeasFunc::Pp => "pp",
        MeasFunc::MaxAt => "max_at",
        MeasFunc::MinAt => "min_at",
    }
}

fn push_meas_edge(line: &mut String, edge: Option<(MeasEdge, usize)>) {
    let Some((word, count)) = edge else { return };
    let text = match word {
        MeasEdge::Rise => "RISE",
        MeasEdge::Fall => "FALL",
        MeasEdge::Cross => "CROSS",
    };
    if count == 1 {
        line.push(' ');
        line.push_str(text);
    } else {
        line.push_str(&format!(" {text}={count}"));
    }
}

fn push_meas_range(line: &mut String, from: Option<f64>, to: Option<f64>) {
    if let Some(value) = from {
        line.push_str(&format!(" FROM={value}"));
    }
    if let Some(value) = to {
        line.push_str(&format!(" TO={value}"));
    }
}
