# AGENTS.md

Guidance for AI agents working in this repository (RSpice: a SPICE-style circuit
simulator built in Rust — `circuitsim` core, `circuitsim-cli`, `circuitsim-gui`).

## Agent skills

### Issue tracker

GitHub Issues (`siamsamix/RSpice`), managed with the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Domain docs

Single-context: glossary at `CONTEXT.md`, ADRs at `docs/adr/`. See `docs/agents/domain.md`.

### GUI design reference

The GUI (`circuitsim-gui`) must be similar to LTspice; any GUI-facing change or
LTspice-behavior question goes through the reference install and RE tooling. See
`docs/agents/ltspice.md`.
