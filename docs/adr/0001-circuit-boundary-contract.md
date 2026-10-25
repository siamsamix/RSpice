# Circuit's boundary contract: validate() is the single gate

**Status:** accepted (2026-10-25)

Parsing a deck is the only production writer of a `Circuit` (the GUI's
schematic editor builds one to render and round-trip decks, never to simulate
directly), and every cross-reference a device stamp needs is checked by
`Circuit::validate()` — at the end of parse, and again at the top of every
analysis pass, because a per-step `.step temp` value can flip the positivity
checks decided at parse time. The previous arrangement had each matrix-building
site re-check its own model names mid-Newton-iterate; that had accumulated 17
near-identical `ok_or_else` blocks in two files.

**Decision.** `Circuit` documents the contract on the struct: the element-model
maps and the controlled-source/switch families are `pub(crate)` (element
vectors stay public — the schematic editor constructs `R C L V`, the CLI and
GUI count them), `validate()` is the single gate, and matrix stamping trusts
it — model lookups resolve through the one `device_model` helper in `mna.rs`
(error messages preserved), never re-validated per site, never silent-fallback.

## Considered options

- **A "finalize()" step stamps could trust instead** (the audit's original
  name). Rejected: `validate()` already ran at both gates, and there is no
  per-build re-derivation to hoist — stamping is genuinely per-iterate work; a
  frozen deck copy per Newton iteration would cost more than the checks it
  removed.
- **Keep the in-loop checks as defense in depth.** Rejected: the wording had
  already drifted (the noise paths formatted the same failure differently),
  and every new device kind would add another copy rather than one.

## Consequences

- A deck that reaches the stamps through `analysis::run` can never fail a
  model lookup; the stamps' `?`-returning lookups exist for direct matrix
  users (unit tests), not as a second gate.
- The mutual-inductance stamp's silent `continue` on an unknown inductor is
  unreachable through `run()`: `validate()` rejects unknown K targets loudly
  at both gates.
- Two follow-on shapes are this decision applied to neighbouring seams: the
  analysis driver expresses stimulus through `MnaSystem` intent ops
  (`drive_source`/`drive_node`/`solve_transposed`, with the matrix fields
  `pub(crate)`), and real/AC share one generic `BranchCurrents<T>` report
  instead of twin structs.
