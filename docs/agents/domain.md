# Domain Docs

How the engineering skills should consume this repo's domain documentation when
exploring the codebase.

## Before exploring, read these

- **`CONTEXT.md`** at the repo root — this repo's glossary ("RSpice — Domain
  Glossary"). `TODO.md` and the codebase assume its vocabulary.
- **`docs/adr/`**: read ADRs that touch the area you're about to work in.

If any of these files don't exist, **proceed silently**. Don't flag their
absence; don't suggest creating them upfront. The `/domain-modeling` skill
(reached via `/grill-with-docs` and `/improve-codebase-architecture`) creates
them lazily when terms or decisions actually get resolved. In this repo, the
glossary already lives at `CONTEXT.md`: extend that file rather than
introducing a separate `GLOSSARY.md`. New ADRs go in `docs/adr/`.

## File structure

Single-context repo (the three crates share one vocabulary):

```
/
├── CONTEXT.md             ← glossary
├── docs/adr/
│   ├── 0001-…md
│   └── 0002-…md
├── circuitsim/            ← core simulation crate
├── circuitsim-cli/
├── circuitsim-gui/
└── TODO.md                ← roadmap: parity gaps vs LTspice / OrCAD PSpice
```

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor
proposal, a hypothesis, a test name), use the term as defined in `CONTEXT.md`.
Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal: either
you're inventing language the project doesn't use (reconsider) or there's a
real gap (note it for `/domain-modeling`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than
silently overriding:

> _Contradicts ADR-0007 (…), but worth reopening because…_
