# Every `MnaSystem.a` write goes through the stamp seam; the stamp record is a superset

**Status:** accepted (2026-10-29)

Sparse MNA groundwork (TODO §3) does not replace the dense LU yet — it first needed an
unambiguous structural record of what an MNA build writes, so the eventual solver's
symbolic layer can be built and verified while every deck still computes through the
proven dense path. The matrix write surface was 77 sites of `sys.a[(r, c)] +=/-= v`
funnelled through `crate::mna`'s stamp helpers; the record needed all of them, and
`z` needed none of them (the RHS is assigned, not accumulated, and carries no
structure the pattern needs).

**Decision.** The seam lives on `MnaSystem`: `stamp_add(row, col, v)` /
`stamp_sub(row, col, v)` — the only code that may write `a` — record every visited
slot into `MnaSystem.touched` (`crate::sparse::StampBits`, an `n²` bitset; the visit
is O(1) next to the stamp's arithmetic). The invariant is deliberately one-sided:
**every dense non-zero slot is in the record** (the zoo tripwire asserts this per arm
for a deck carrying every element family), while the reverse does not hold — visited
slots may still hold exact 0.0 (cancelling stamps, a switch conductance at the stub
value, zero-bias device conductances). Superset direction is the sound one for a
symbolic pattern: extra structure costs the future solver nothing, missed structure
would be unsound. `sparse.rs` compiles the record to CSR (`CompressedPattern`) and
to an unsymmetric full-Markowitz pivot order (`markowitz_ordering`, no-stranding
guard on the greedy scan) — groundwork behavior, exercised by the zoo test per
analysis arm.

## Considered options

- **Reuse one buffer per Newton frame and clear it per restamp** (the TODO §3
  LU-reuse seam). Rejected on measurement: `fill(0.0)` over the dense `n²` costs
  ~7× the fresh `DMatrix::zeros` calloc path (76.2 µs vs 10.9 µs at n=210) and the
  ladder benchmarks regressed 702→815 ms; recorded as a finding on the open item.
  The record+pattern pipeline picks that work back up behind the sparse solver,
  where reset cost follows the stamp count, not the matrix size.
- **Reset `touched` between builds.** Rejected: a build owns a fresh `MnaSystem`
  anyway (matrix `zeros` rides calloc), so there is no stale record; a reset API
  would only invite half-reset seams.
- **Convert the solver itself to sparse numerics now.** Rejected for this slice:
  landing symbolic bookkeeping and a new numeric core in one change would bury the
  bit-exact regressions the test battery exists to catch. The record layer is
  verified against the dense matrix while numerics stay verbatim.

## Consequences

- The one-writer rule is checkable: `sys.a[` appears only inside
  `stamp_add`/`stamp_sub` (and their doc mentions). New stamp helpers write through
  the seam by construction.
- The zoo tripwire (`zoo_stamp_record_covers_dense_nonzeros_in_every_arm`) fails
  the battery, not production, if a future element family forgets the seam.
- `markowitz_ordering`'s `None` means "no admissible pivot among the active
  entries"; the no-stranding guard keeps greedy scan order from reporting `None`
  on patterns a transversal already orders — it is still greedy, not a guarantee.
- Parity stayed exact this slice: tests 142 lib / 93 integration / 8 regression,
  fmt red-count 2126, clippy errors 22 byte-identical; benchmark numbers within
  machine drift (see the commit message footer).
