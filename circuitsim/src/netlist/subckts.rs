//! Subcircuit definitions and scope flattening: `.subckt` bodies, `X`

//! instantiation, net mapping, and name mangling.

use super::*;
use super::elements::split_b_card;
use super::nodeplan::element_letter;

// ─── Subcircuit definitions ─────────────────────────────────────────────────

/// One `.subckt` definition lifted out of the deck before instantiation.
/// Ports and default parameters are lowercased name text; default parameter
/// *values* stay raw so each instance evaluates them in its own scope.
#[derive(Debug, Clone)]
pub(super) struct SubcktDef {
    pub(super) name: String,
    /// The definition name as first written (display only).
    pub(super) display_name: String,
    pub(super) ports: Vec<String>,
    pub(super) default_params: Vec<(String, String)>,
    pub(super) body: Vec<(usize, String)>,
    /// Source line of the `.subckt` card, for diagnostics.
    pub(super) line_no: usize,
}

/// `.subckt <name> <port…> [PARAMS:] <key=value…>` — ports are the tokens
/// before the first `key=value` pair (LTspice puts default parameter
/// assignments directly on the header, PSpice introduces them with the
/// `PARAMS:` marker; both forms land here).
pub(super) fn parse_subckt_header(
    line_no: usize,
    line: &str,
) -> std::result::Result<(String, String, Vec<String>, Vec<(String, String)>), String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 2 {
        return Err(format!("line {line_no}: .subckt requires a definition name"));
    }
    let display = tokens[1].to_string();
    let name = display.to_ascii_lowercase();

    let mut ports = Vec::new();
    let mut default_params: Vec<(String, String)> = Vec::new();
    let mut after_marker = false;

    for tok in &tokens[2..] {
        let lower = tok.to_ascii_lowercase();

        // Bare `PARAMS`/`PARAMS:` tokens introduce the pair region.
        if lower == "params" || lower == "params:" {
            after_marker = true;
            continue;
        }
        // Fused `PARAMS:key=value` (no whitespace): strip the 7-character
        // marker from the original token — `params:` and its lowercase form
        // have identical ASCII length, so the byte offsets agree.
        if lower.starts_with("params:") {
            let fused = &tok[7..];
            match kv_pair_text(fused) {
                Some((key, value)) => default_params.push((key, value)),
                None => {
                    return Err(format!(
                        "line {line_no}: .subckt '{name}': 'PARAMS:{fused}' must be a key=value pair"
                    ))
                }
            }
            after_marker = true;
            continue;
        }
        match kv_pair_text(tok) {
            Some((key, value)) => default_params.push((key, value)),
            None if after_marker => {
                return Err(format!(
                    "line {line_no}: .subckt '{name}': '{tok}' after PARAMS: must be a key=value pair"
                ))
            }
            None => ports.push(lower),
        }
    }
    Ok((name, display, ports, default_params))
}

/// Split `key=value` into (key lowercased, value text as written) — `None`
/// when the '=' is missing or the key side is empty.
pub(super) fn kv_pair_text(token: &str) -> Option<(String, String)> {
    let eq = token.find('=')?;
    let key = token[..eq].to_ascii_lowercase();
    (!key.is_empty()).then_some((key, token[eq + 1..].to_string()))
}

/// Lift `.subckt`/`.ends` definitions out of the deck. Returns the flow of
/// cards outside any definition plus every definition (nested definitions
/// inside bodies join the same global namespace). Everything after the first
/// top-level `.end` is ignored, as SPICE reads, and a `.subckt` without its
/// matching `.ends` (or a stray `.ends`) is a structural error.
pub(super) fn collect_subckts(
    logical: &[(usize, String)],
) -> std::result::Result<(Vec<(usize, String)>, Vec<SubcktDef>), String> {
    let mut flow: Vec<(usize, String)> = Vec::new();
    let mut defs: Vec<SubcktDef> = Vec::new();
    let mut open: Vec<SubcktDef> = Vec::new();

    for (line_no, line) in logical {
        let first = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();

        if open.is_empty() && first == ".end" {
            // Content after `.end` is not part of the deck.
            break;
        }

        match first.as_str() {
            ".subckt" => {
                let (name, display, ports, default_params) = parse_subckt_header(*line_no, line)?;
                open.push(SubcktDef {
                    name,
                    display_name: display,
                    ports,
                    default_params,
                    body: Vec::new(),
                    line_no: *line_no,
                });
            }
            ".ends" => {
                let def = open
                    .pop()
                    .ok_or_else(|| format!("line {line_no}: .ends outside any .subckt"))?;
                defs.push(def);
            }
            _ => match open.last_mut() {
                Some(def) => def.body.push((*line_no, line.to_string())),
                None => flow.push((*line_no, line.to_string())),
            },
        }
    }

    if let Some(def) = open.last() {
        return Err(format!(
            "line {}: .subckt '{}' is missing its .ends",
            def.line_no, def.display_name
        ));
    }
    Ok((flow, defs))
}

// ─── Scope flattening: `X` instantiation and net mapping ────────────────────

/// Shared state while flattening: definition table, global nets, the `.dc`
/// sweep overrides, and diagnostics collected along the way.
pub(super) struct ScopeCtx<'a> {
    pub(super) defs: &'a HashMap<String, SubcktDef>,
    pub(super) globals: &'a HashSet<String>,
    pub(super) overrides: &'a HashMap<String, f64>,
    pub(super) warnings: Vec<String>,
}

/// Node-field positions per element letter — one table for the plan pre-scan
/// and the scope mapper, so they can never drift apart. `K` has no nodes at
/// all (an empty slice stops the generic `[1, 2]` fallback from treating its
/// inductance names as nets).
pub(super) fn node_slot_indices(kind: char) -> Option<&'static [usize]> {
    match kind.to_ascii_uppercase() {
        'M' => Some(&[1, 2, 3, 4]),
        'J' => Some(&[1, 2, 3]),
        'E' | 'G' | 'S' | 'T' => Some(&[1, 2, 3, 4]),
        'R' | 'C' | 'L' | 'V' | 'I' | 'D' | 'F' | 'H' | 'W' | 'B' => Some(&[1, 2]),
        'K' => Some(&[]),
        _ => None,
    }
}

/// Node fields for one card, including the `POLY(n)` control pairs of an
/// `E`/`G` source, whose positions depend on the card's own text.
pub(super) fn node_slots_for_tokens(tokens: &[&str]) -> Vec<usize> {
    let mut slots = node_slot_indices(element_letter(tokens[0]))
        .unwrap_or(&[1, 2])
        .to_vec();
    let kind = element_letter(tokens[0]).to_ascii_uppercase();
    if kind == 'Q' {
        let mut slots = vec![1, 2, 3];
        if q_is_four_terminal(tokens) {
            slots.push(4);
        }
        return slots;
    }
    if matches!(kind, 'E' | 'G') && tokens.get(3).map(|t| t.to_ascii_lowercase()).is_some_and(|t| t.starts_with("poly")) {
        if let Some(n) = poly_count(tokens[3]) {
            for k in 0..n {
                slots.push(4 + 2 * k);
                slots.push(5 + 2 * k);
            }
        }
    }
    slots
}

/// Parse the `n` in `POLY(n)` (the caller already checked the prefix).
pub(super) fn poly_count(token: &str) -> Option<usize> {
    let open = token.find('(')?;
    let close = token[open..].find(')').map(|c| open + c)?;
    token[open + 1..close].trim().parse().ok()
}

/// Does this `Q` card carry the optional substrate node?
///
/// Three-terminal: `Qname c b e model …`; four-terminal:
/// `Qname c b e substrate model …`. The model token is the first trailing
/// word that is not a number, an `AREA=`/`OFF` field, or `IC=`.
pub(super) fn q_is_four_terminal(tokens: &[&str]) -> bool {
    tokens.len() >= 6 && tokens.get(5).is_some_and(|t| {
        let lower = t.to_ascii_lowercase();
        lower != "off"
            && !lower.contains('=')
            && parse_value(t).is_err()
    })
}

/// Token positions that name *other elements* rather than nodes or values.
/// Inside a subckt these names carry the instance path, like the card's own
/// name (`X1.F1` must control `X1.V1`).
pub(super) fn reference_slots_for_tokens(tokens: &[&str]) -> Vec<usize> {
    let kind = element_letter(tokens[0]).to_ascii_uppercase();
    let is_poly = tokens
        .get(3)
        .is_some_and(|t| t.to_ascii_lowercase().starts_with("poly"));
    match kind {
        'F' | 'H' => {
            if is_poly {
                let n = poly_count(tokens[3]).unwrap_or(0);
                (4..4 + n).collect()
            } else {
                vec![3]
            }
        }
        'W' => vec![3],
        'K' => vec![1, 2],
        _ => Vec::new(),
    }
}

/// Rewrite one node token to its physical net name in the current scope:
/// ports bind to the instance's connection tokens, `.global` nets carry over
/// everywhere, ground is world-wide ground, and anything else is the instance
/// path plus the body's spelling (`X1.XR2.mid`).
pub(super) fn scope_token(
    token: &str,
    bindings: &HashMap<String, String>,
    globals: &HashSet<String>,
    prefix: &str,
) -> String {
    let lower = token.to_ascii_lowercase();
    if lower == "0" || lower == "gnd" {
        return token.to_string();
    }
    if let Some(bound) = bindings.get(&lower) {
        return bound.clone();
    }
    if globals.contains(&lower) {
        return token.to_string();
    }
    if prefix.is_empty() {
        return token.to_string();
    }
    format!("{prefix}.{token}")
}

/// Rewrite the `V(…)`/`I(…)` probe arguments of one `B` expression into the
/// current scope's naming.
///
/// A probe boundary check guards against rewriting function arguments (`int(`
/// / `hypot(` do not become probes): a probe grabs only when the character
/// before the letter is not a name character. `V(…)` arguments are nets —
/// each top-level comma argument scopes like a node token; `I(…)` names an
/// element and follows the element-reference rule (a path prefix rides along,
/// a bare name gains the instance's own). Unbalanced text passes through for
/// the expression parser to reject loudly.
pub(super) fn rewrite_b_probes(
    expr: &str,
    bindings: &HashMap<String, String>,
    globals: &HashSet<String>,
    prefix: &str,
) -> String {
    let chars: Vec<char> = expr.chars().collect();
    let mut out = String::with_capacity(expr.len());
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        let boundary = i == 0
            || !(chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '_');
        if (c == 'v' || c == 'V' || c == 'i' || c == 'I')
            && chars.get(i + 1) == Some(&'(')
            && boundary
        {
            let mut depth = 1usize;
            let mut j = i + 2;
            while j < chars.len() && depth > 0 {
                match chars[j] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            j -= 1; // position of the balanced ')' (or last char when unbalanced)
            if depth == 0 {
                let inner: String = chars[i + 2..j].iter().collect();
                let is_voltage = c.to_ascii_lowercase() == 'v';
                let rewritten = if is_voltage {
                    inner
                        .split(',')
                        .map(|arg| scope_token(arg.trim(), bindings, globals, prefix))
                        .collect::<Vec<String>>()
                        .join(",")
                } else {
                    let arg = inner.trim();
                    if !prefix.is_empty() && !arg.contains('.') {
                        format!("{prefix}.{arg}")
                    } else {
                        arg.to_string()
                    }
                };
                out.push(c);
                out.push('(');
                out.push_str(&rewritten);
                out.push(')');
                i = j + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Expand one scope's lines into physical element/dot cards.
///
/// `.param`/`.func` execute against the scope's own environment (deck order
/// preserved; instance scopes start from their instantiation snapshot and
/// never leak back out). `.global` was registered before flattening and is
/// consumed. `X` cards instantiate their definition in place. Every other
/// card is `{}`-expanded — into the instance scope — with known element
/// kinds having their node tokens rewritten by [`scope_token`].
pub(super) fn flatten_scope(
    lines: &[(usize, String)],
    prefix: &str,
    label: &str,
    bindings: &HashMap<String, String>,
    env: &mut ParamEnv,
    scope: &mut ScopeCtx<'_>,
    skip_keys: Option<&HashSet<String>>,
) -> std::result::Result<Vec<(usize, String)>, String> {
    let mut out = Vec::with_capacity(lines.len());

    for (line_no, line) in lines {
        let lower = line.to_ascii_lowercase();
        match lower.split_whitespace().next().unwrap_or("") {
            ".param" => {
                // Expansion runs first: a multi-token brace/quote value
                // (`x = '1k * gain'`) must resolve before the assignment
                // token split sees the line (the contract on
                // `parse_param_body`).
                let expanded =
                    expand_line(line, env).map_err(|e| format!("line {line_no}: {e}"))?;
                // Instance-scoped assignments may replay here: a body card
                // re-assigning a key the `X` instance line provided is
                // rolled back, because the instance line wins (the same
                // precedence the reference tools give `PARAMS=` values).
                let snapshot = env.params.clone();
                parse_param_body(&expanded[6..], env, scope.overrides)
                    .map_err(|e| format!("line {line_no}: {e}"))?;
                if let Some(skip) = skip_keys {
                    for key in env.params.keys().cloned().collect::<Vec<String>>() {
                        if skip.contains(&key) {
                            match snapshot.get(&key) {
                                Some(value) => { env.params.insert(key, value.clone()); }
                                None => { env.params.remove(&key); }
                            }
                        }
                    }
                }
                continue;
            }
            ".func" => {
                parse_func_body(&line[5..], env)
                    .map_err(|e| format!("line {line_no}: .func: {e}"))?;
                continue;
            }
            ".global" => continue, // registered by collect_global_nets
            ".ic" | ".nodeset" => {
                // Initial conditions carry node names, so inside a body the
                // entries must be translated to the body's net space
                // (`V(mid)=…` → `V(X1.mid)=…`).
                let expanded = expand_line(line, env).map_err(|e| format!("line {line_no}: {e}"))?;
                let expanded = expanded.trim();
                if !expanded.is_empty() {
                    out.push((*line_no, mangle_ic_entries(expanded, bindings, scope.globals, prefix)));
                }
                continue;
            }
            ".end" => {
                if prefix.is_empty() {
                    // The deck terminator travels to the card walk.
                    out.push((*line_no, line.to_string()));
                    continue;
                }
                return Err(format!(
                    "line {line_no}: '.end' inside subckt '{label}' — end definitions with .ends"
                ));
            }
            ".ends" => {
                return Err(format!(
                    "line {line_no}: '.ends' reached while flattening '{label}' — unbalanced .subckt body"
                ))
            }
            _ => {}
        }

        let expanded = expand_line(line, env).map_err(|e| format!("line {line_no}: {e}"))?;
        let expanded = expanded.trim();
        if expanded.is_empty() {
            continue;
        }

        if expanded.starts_with('.') {
            out.push((*line_no, expanded.to_string()));
            continue;
        }

        let tokens: Vec<&str> = expanded.split_whitespace().collect();
        let kind = tokens[0]
            .chars()
            .next()
            .ok_or_else(|| format!("line {line_no}: empty element"))?;

        if kind.eq_ignore_ascii_case(&'x') {
            let child = instantiate_subckt(&tokens, *line_no, prefix, label, bindings, env, scope)?;
            out.extend(child);
            continue;
        }

        // `B` cards carry an expression whose `V(…)`/`I(…)` probes name nets
        // and elements of the same instance; those are rewritten here instead
        // of the generic slot pass (which knows nothing about subexpressions).
        if kind.eq_ignore_ascii_case(&'b') {
            if let Ok(split) = split_b_card(&tokens) {
                let mut head: Vec<String> = Vec::with_capacity(4);
                head.push(if prefix.is_empty() {
                    tokens[0].to_string()
                } else {
                    format!("{prefix}.{}", tokens[0])
                });
                head.push(scope_token(tokens[1], bindings, scope.globals, prefix));
                head.push(scope_token(tokens[2], bindings, scope.globals, prefix));
                let letter = match split.kind {
                    BehavioralKind::Voltage => 'V',
                    BehavioralKind::Current => 'I',
                };
                let expr = rewrite_b_probes(&split.expr, bindings, scope.globals, prefix);
                head.push(format!("{letter}={expr}"));
                out.push((*line_no, head.join(" ")));
                continue;
            }
            // Malformed split: fall through with the generic rewrite so the
            // card walk raises the loud parse error for it.
        }

        if node_slot_indices(kind).is_some() {
            // Rewrite this card's node tokens in the scope's net space; the
            // card text keeps its non-node fields (values, model names,
            // waveform parameters).
            let mut rewritten: Vec<String> = tokens.iter().map(|t| (*t).to_string()).collect();
            if !prefix.is_empty() {
                // Subckt-internal element names carry their instance path
                // (`X1.R1`), so branch reports never confuse two instances'
                // cards that share a name.
                rewritten[0] = format!("{prefix}.{}", rewritten[0]);
            }
            for slot in node_slots_for_tokens(&tokens) {
                if let Some(tok) = rewritten.get_mut(slot) {
                    *tok = scope_token(tok, bindings, scope.globals, prefix);
                }
            }
            // Element references (controlling voltage sources, coupled
            // inductors) live in the same instance namespace as element
            // names, so they carry the instance path too.
            if !prefix.is_empty() {
                for slot in reference_slots_for_tokens(&tokens) {
                    if let Some(tok) = rewritten.get_mut(slot) {
                        if !tok.contains('.') {
                            *tok = format!("{prefix}.{tok}");
                        }
                    }
                }
            }
            out.push((*line_no, rewritten.join(" ")));
        } else {
            out.push((*line_no, expanded.to_string()));
        }
    }
    Ok(out)
}

/// Instantiate one `X` card: locate the definition, bind its ports, build the
/// instance scope's parameter environment (definition defaults, then the
/// instance's own assignments), and flatten the body at this deck position.
///
/// The definition name is the first token that both names a definition and
/// leaves only `key=value` tokens after the node list — the same match the
/// reference tools make once they know a definition's port count.
pub(super) fn instantiate_subckt(
    tokens: &[&str],
    line_no: usize,
    parent_prefix: &str,
    parent_label: &str,
    parent_bindings: &HashMap<String, String>,
    parent_env: &ParamEnv,
    scope: &mut ScopeCtx<'_>,
) -> std::result::Result<Vec<(usize, String)>, String> {
    let instance = tokens[0].to_string();

    // Locate the definition-tag token.
    let mut tag_idx: Option<usize> = None;
    let mut name_hit: Option<String> = None;
    for j in 1..tokens.len() {
        let cand = tokens[j].to_ascii_lowercase();
        if scope.defs.contains_key(&cand) {
            // Remember the leftmost name match for the error text, but only
            // commit to a tag when every trailing token is key=value
            // (i.e. that token sits after the node list).
            if name_hit.is_none() {
                name_hit = Some(cand);
            }
            if tokens[j + 1..].iter().all(|t| t.contains('=')) {
                tag_idx = Some(j);
                break;
            }
        }
    }
    let Some(tag) = tag_idx else {
        let guess = name_hit.unwrap_or_else(|| {
            tokens
                .iter()
                .skip(1)
                .find(|t| !t.contains('='))
                .map(|t| t.to_ascii_lowercase())
                .unwrap_or_else(|| "?".into())
        });
        let _ = parent_label;
        return Err(format!(
            "line {line_no}: instance '{instance}' references an unknown .subckt name ('{guess}')"
        ));
    };
    let def = &scope.defs[&tokens[tag].to_ascii_lowercase()];

    // Port tokens belong to the caller scope — they were subjects of its
    // bindings, may itself be `.global` names, and keep their spelling.
    let ports: Vec<String> = tokens[1..tag]
        .iter()
        .map(|t| scope_token(t, parent_bindings, scope.globals, parent_prefix))
        .collect();

    if ports.len() != def.ports.len() {
        return Err(format!(
            "line {line_no}: instance '{instance}' of '{}' needs {} port(s), got {}",
            def.display_name,
            def.ports.len(),
            ports.len()
        ));
    }

    let bindings_now: HashMap<String, String> = def
        .ports
        .iter()
        .zip(&ports)
        .map(|(p, t)| (p.clone(), t.clone()))
        .collect();

    // Instance scope: caller env snapshot, definition defaults first, then
    // the instance's own assignments — which may reference the defaults.
    let mut env = parent_env.clone();
    for (key, raw) in &def.default_params {
        let value = eval_param_value(raw, &env).map_err(|e| {
            format!(
                "line {line_no}: instance '{instance}': default '{key}={raw}': {e}"
            )
        })?;
        env.params.insert(key.clone(), value);
    }
    // The X line's own assignments, remembered so a body `.param` sandbox
    // replaying for the same name cannot clobber them.
    let mut instance_keys: HashSet<String> = HashSet::new();
    for token in &tokens[(tag + 1)..] {
        let Some((key, value_text)) = kv_pair_text(token) else {
            return Err(format!(
                "line {line_no}: instance '{instance}': parameter '{token}' must be key=value"
            ));
        };
        let value = eval_param_value(&value_text, &env).map_err(|e| {
            format!(
                "line {line_no}: instance '{instance}': parameter '{key}={value_text}': {e}"
            )
        })?;
        env.params.insert(key.clone(), value);
        instance_keys.insert(key);
    }

    // The instance's node prefix for internal nets. Nested instances extend
    // the path, so two instances of one definition cannot share a net.
    let ins_prefix = if parent_prefix.is_empty() {
        instance
    } else {
        format!("{parent_prefix}.{instance}")
    };
    let ins_label = def.display_name.clone();
    let def_body = def.body.clone();
    flatten_scope(
        &def_body,
        &ins_prefix,
        &ins_label,
        &bindings_now,
        &mut env,
        scope,
        Some(&instance_keys),
    )
}

/// Evaluate one parameter-value text (`1k`, `{a*b}`, an identifier) in `env`.
pub(super) fn eval_param_value(
    text: &str,
    env: &ParamEnv,
) -> std::result::Result<ParamValue, String> {
    env.eval(text.trim())
}

/// The `.ic`/`.nodeset` rewrite for one scope: each entry's node text goes
/// through [`scope_token`] so entries written inside a body address the
/// body's nets; the letter (`V`/`I`) and the value text are copied verbatim.
pub(super) fn mangle_ic_entries(
    line: &str,
    bindings: &HashMap<String, String>,
    globals: &HashSet<String>,
    prefix: &str,
) -> String {
    line.split_whitespace()
        .enumerate()
        .map(|(idx, tok)| {
            if idx == 0 || !tok.contains('(') {
                return tok.to_string();
            }
            match (tok.find('('), tok.find(')')) {
                (Some(o), Some(c)) if o > 0 && c > o + 1 => {
                    let inner = &tok[o + 1..c];
                    format!(
                        "{}{}{}",
                        &tok[..=o],
                        scope_token(inner, bindings, globals, prefix),
                        &tok[c..]
                    )
                }
                _ => tok.to_string(),
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

