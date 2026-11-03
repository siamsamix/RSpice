# Sparse factorizations are cached per plan, keyed on the exact pattern values

**Status:** accepted (2026-11-03)

`ADR 0003` left "factor-once/solve-many" open: `SparsePlan::factor_solve` re-ran
the whole left-looking factor on every call, even when the matrix had not
changed (every Newton iterate of a linear circuit; every constant-`dt`
timestep; the two half-steps of step doubling).

**Decision.** Each plan keeps, per field (`f64` / `Complex64`), the four most
recently used factorizations, each stored with the matrix values it came from
(read along the compiled pattern, CSR order). A solve whose pattern values are
**bit-for-bit equal** (`Field::bits_equal`: `-0.0 ≠ 0.0`, NaN matches itself
when its bits do) to an entry skips the factor and runs only the triangular
solves with the cached factors; anything else factors and evicts the oldest.

**Why exact equality and not a tolerance.** The invariant of `ADR 0003` is that
the sparse layer changes what a solve costs, never what it says. With a bitwise
key a cache hit hands back *the same factors a fresh factorization would
produce*, so results are identical by construction — no "close enough" matrices,
no drift to reason about across a transient. Slots outside the pattern are
structural zeros by the record's invariant (`ADR 0002`), so equal pattern values
mean an equal matrix.

**Why four ways.** A linear Newton loop re-solves one matrix; step doubling
alternates the `dt` and `dt/2` matrices; a handful of ways catches both while a
nonlinear circuit (a fresh Jacobian every iterate) just cycles through the ways
and pays one `Vec` copy plus a mismatch at the first differing value.

**Measured** (release, 2 cores, benchmark suite, median of three, against the
commit before): RC ladder-50 `.tran` 11.3 → 7.0 ms, ladder-200 85 → 60 ms; 593 of
609 sparse solves served from cache. Output of every `examples/*.cir` deck (op,
tran, ac) byte-identical to the baseline CLI. `factor_stats()` exposes the
`(factorizations, reuses)` thread counters the benchmark's `reuse` column and
the unit tests read.

**Not covered.** A nonlinear circuit refactors every iterate; modified Newton
(holding a stale Jacobian) or a partial refactor of only the nonlinear rows
would change *results* (iterate paths), so they need their own decision.
