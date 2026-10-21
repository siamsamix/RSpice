//! Node planning and resolution: global nets, element letters, the node

//! id map, and per-card node parsing.

use super::*;
use super::subckts::node_slots_for_tokens;

// ─── Global nets ────────────────────────────────────────────────────────────

/// Every net named by a `.global` card, anywhere in the deck — definitions
/// inside `.subckt` bodies count, since a body must be able to say "this net
/// is the top-level one". Parameter references (`{name}`) are not collected:
/// those names resolve later, in the scopes that use them.
pub(super) fn collect_global_nets(logical: &[(usize, String)]) -> HashSet<String> {
    let mut set = HashSet::new();
    for (_, line) in logical {
        let lower = line.to_ascii_lowercase();
        let mut words = lower.split_whitespace();
        if words.next() != Some(".global") {
            continue;
        }
        for name in words {
            if name.contains('{') || name.contains('}') {
                continue;
            }
            set.insert(name.to_string());
        }
    }
    set
}

pub(super) fn element_letter(card_name: &str) -> char {
    card_name
        .rsplit('.')
        .next()
        .unwrap_or(card_name)
        .chars()
        .next()
        .unwrap_or('?')
}

/// Reserve node ids up front: numeric nodes own their number, alphanumeric
/// nodes take ids above every numeric reference in first-appearance order,
/// so the two name spaces can never collide however the deck interleaves.
pub(super) fn scan_node_plan(logical: &[(usize, String)]) -> HashMap<String, usize> {
    let mut max_numeric = 0usize;
    let mut named_order: Vec<String> = Vec::new();

    for (_, line) in logical {
        if line.to_ascii_lowercase() == ".end" {
            break;
        }
        if line.starts_with('.') {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        let slots = node_slots_for_tokens(&tokens);
        // Unknown kinds still look node-ish in slots 1–2; whatever the
        // real grammar rejects surfaces again at parse time.
        for slot in slots {
            let Some(tok) = tokens.get(slot) else { continue };
            let lower = tok.to_ascii_lowercase();
            if lower == "gnd" {
                continue;
            }
            if let Ok(n) = lower.parse::<usize>() {
                max_numeric = max_numeric.max(n);
            } else if !named_order.contains(&lower) {
                named_order.push(lower);
            }
        }
    }

    let mut plan = HashMap::new();
    let mut next = max_numeric + 1;
    for name in named_order {
        plan.insert(name, next);
        next += 1;
    }
    plan
}

// ─── Node resolution ─────────────────────────────────────────────────────────

/// Resolve one node token: ground aliases (`0`, `gnd`), numeric ids, or an
/// alphanumeric net name from the pre-scan plan (case-insensitive; the first
/// spelling seen becomes the display name).
pub(super) fn parse_node(
    token: &str,
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
) -> Result<NodeId> {
    let lower = token.to_ascii_lowercase();
    if lower == "0" || lower == "gnd" {
        return Ok(NodeId(0));
    }

    if let Ok(id) = lower.parse::<usize>() {
        circuit.ensure_node(id);
        let existing = circuit.node_name(id).to_string();
        if existing != id.to_string() {
            return Err(SimError::Parse(format!(
                "node '{token}' collides with named node '{existing}'"
            )));
        }
        return Ok(NodeId(id));
    }

    let id = match plan.get(&lower) {
        Some(&id) => id,
        // The pre-scan did not see this token position (unsupported element
        // kind); allocating above everything seen is the safe fallback.
        None => circuit.nodes,
    };
    circuit.ensure_node(id);
    circuit.set_node_name(id, token);
    Ok(NodeId(id))
}

