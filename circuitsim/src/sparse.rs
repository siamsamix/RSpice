//! Sparse MNA groundwork (TODO §3): the stamp record → structural pattern →
//! symbolic pivot-order pipeline.
//!
//! Nothing here changes numerics yet. [`MnaSystem`](crate::mna::MnaSystem)
//! records which matrix slots each build wrote into through the
//! `stamp_add`/`stamp_sub` seam ([`StampBits`]); the record compiles to a
//! compressed structure ([`CompressedPattern`]); the structure compiles to a
//! symbolic pivot order ([`markowitz_ordering`]). The dense LU keeps solving;
//! a numeric sparse solver slots in behind this seam once it exists, and LU
//! factorization reuse (the measured-out item next to this TODO line) rides
//! the same pattern.
//!
//! The record is deliberately a *superset* of the matrix's structural
//! non-zeros: a slot can be visited yet numerically zero (opposing stamps
//! that cancel, a switch conductance that lands exactly on the stub value).
//! "Visited ⊆ non-zero" would be false; "non-zero ⊆ visited" is the
//! invariant that holds, and it is the sound direction for a symbolic
//! pattern — the compiler may only ever see extra structure, never miss any.

use std::collections::BTreeSet;

/// Which matrix slots one MNA build wrote into: a bitset over the `n²`
/// slots, [`visit`](Self::visit) is O(1) per stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StampBits {
    /// Matrix side length; slot `(row, col)` is bit `row·n + col`.
    n: usize,
    words: Vec<u64>,
}

impl StampBits {
    /// An empty record for an `n × n` system.
    pub fn zeros(size: usize) -> Self {
        Self {
            n: size,
            words: vec![0; size * size / 64 + 1],
        }
    }

    /// Record that slot `(row, col)` was written into. Out-of-range slots
    /// panic the same way an `[index]` write into the matrix would.
    pub fn visit(&mut self, row: usize, col: usize) {
        debug_assert!(row < self.n && col < self.n, "stamp outside the system");
        let bit = row * self.n + col;
        self.words[bit / 64] |= 1 << (bit % 64);
    }

    /// Was slot `(row, col)` written into by any stamp of this build?
    pub fn is_set(&self, row: usize, col: usize) -> bool {
        debug_assert!(row < self.n && col < self.n, "probe outside the system");
        let bit = row * self.n + col;
        self.words[bit / 64] & (1 << (bit % 64)) != 0
    }

    /// Merge another record into this one (same-size systems only, checked
    /// in debug): e.g. the DC record unioned with the AC record to compile a
    /// pattern covering both arms.
    pub fn union(&mut self, other: &Self) {
        debug_assert_eq!(self.n, other.n, "stamp records of different sizes");
        for (word, other) in self.words.iter_mut().zip(&other.words) {
            *word |= *other;
        }
    }

    /// The record as compressed row storage: column indices sorted within
    /// each row, only the visited slots retained.
    pub fn compress(&self) -> CompressedPattern {
        let mut row_ptr = Vec::with_capacity(self.n + 1);
        let mut col_idx = Vec::new();
        for row in 0..self.n {
            row_ptr.push(col_idx.len());
            for col in 0..self.n {
                if self.is_set(row, col) {
                    col_idx.push(col);
                }
            }
        }
        row_ptr.push(col_idx.len());
        CompressedPattern {
            n: self.n,
            row_ptr,
            col_idx,
        }
    }
}

/// A structural pattern in compressed row storage: per row, the sorted
/// column indices that were visited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressedPattern {
    n: usize,
    row_ptr: Vec<usize>,
    col_idx: Vec<usize>,
}

impl CompressedPattern {
    /// Side length.
    pub fn n(&self) -> usize {
        self.n
    }

    /// Stored entries total.
    pub fn nnz(&self) -> usize {
        self.col_idx.len()
    }

    /// The visited columns of one row, in ascending order.
    pub fn columns_of(&self, row: usize) -> &[usize] {
        &self.col_idx[self.row_ptr[row]..self.row_ptr[row + 1]]
    }

    /// Does the pattern contain slot `(row, col)`?
    pub fn contains(&self, row: usize, col: usize) -> bool {
        self.columns_of(row).binary_search(&col).is_ok()
    }
}

/// A symbolic pivot sequence for one structural pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolicOrder {
    /// `(row, col)` of every elimination, in pivot order. Each row index and
    /// each column index appears exactly once: MNA branch rows have no
    /// diagonal in DC, so the ordering is genuinely unsymmetric and the two
    /// permutations are distinct.
    pub order: Vec<(u32, u32)>,
    /// Slots the eliminations create beyond the input pattern.
    pub fill: usize,
}

/// Full Markowitz ordering of a structural pattern (unsymmetric, and MNA
/// needs it: branch rows have no diagonal in DC).
///
/// At every elimination the pivot is the active entry minimizing the full
/// Markowitz cost `(row_len−1)·(col_len−1)`; ties break on the lowest packed
/// index `row·n + col`, which makes the scan order (ascending rows, set
/// iteration ascending within a row) already yield that pivot. The scan is
/// the honest `O(n·nnz)` version — the dict-of-lists bookkeeping that keeps
/// Markowitz incremental is a follow-up behind the numeric solver, not
/// groundwork this slice needs to be correct.
///
/// One guard rides on top of the raw cost: a pivot is rejected when it
/// would strand *another* alive row or column — any other row holding the
/// pivot column as its only entry, or any other column holding the pivot
/// row as its only entry. Without it, greedy cost-minimal pivots happily
/// consume a shared column out from under a row that still needs it and
/// report `None` on patterns that admit an order (MNA is full of these:
/// one-element node pairs share their branch column). The guard is
/// still greedy — a transversal existing does not guarantee `Some`, only
/// makes it far more likely — documented as groundwork behavior.
///
/// Returns `None` when no admissible pivot remains among the active
/// entries: the surviving structure has nothing left to eliminate (no
/// single-entry solve can come out of that shape).
pub fn markowitz_ordering(pattern: &CompressedPattern) -> Option<SymbolicOrder> {
    let n = pattern.n();
    // Active bookkeeping: alive rows' visited columns and alive columns'
    // visited rows, always in sync — a dead row/column leaves both sides.
    let mut rows_alive = vec![true; n];
    let mut cols_alive = vec![true; n];
    let mut row_cols: Vec<BTreeSet<usize>> = (0..n)
        .map(|row| pattern.columns_of(row).iter().copied().collect())
        .collect();
    let mut col_rows: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    for (row, cols) in row_cols.iter().enumerate() {
        for &col in cols {
            col_rows[col].insert(row);
        }
    }

    let mut order = Vec::with_capacity(n);
    let mut fill = 0usize;
    for _ in 0..n {
        // The cheapest active pivot; the strict `<` keeps the first minimum,
        // and candidates arrive in packed order, so ties break lowest.
        let mut best: Option<(usize, usize, usize)> = None;
        for row in 0..n {
            if !rows_alive[row] {
                continue;
            }
            let row_len = row_cols[row].len();
            for &col in &row_cols[row] {
                debug_assert!(cols_alive[col], "dead column revisited");
                // The stranding guard: skip pivots whose elimination empties
                // another alive row or column outright.
                let strands_a_row = col_rows[col]
                    .iter()
                    .any(|&other| other != row && row_cols[other].len() == 1);
                if strands_a_row {
                    continue;
                }
                let strands_a_col = row_cols[row]
                    .iter()
                    .any(|&other| other != col && col_rows[other].len() == 1);
                if strands_a_col {
                    continue;
                }
                let cost = (row_len - 1) * (col_rows[col].len() - 1);
                let better = match best {
                    None => true,
                    Some((best_cost, _, _)) => cost < best_cost,
                };
                if better {
                    best = Some((cost, row, col));
                }
            }
        }
        let (_, pivot_row, pivot_col) = best?;

        // Eliminating (pivot_row, pivot_col) crosses the pivot's column
        // neighbors with its row neighbors: every such pair becomes an
        // entry, and the ones not already present are fill.
        let cols_r: Vec<usize> = row_cols[pivot_row].iter().copied().collect();
        let rows_c: Vec<usize> = col_rows[pivot_col].iter().copied().collect();
        for &row in &rows_c {
            if row == pivot_row {
                continue;
            }
            for &col in &cols_r {
                if col == pivot_col {
                    continue;
                }
                if row_cols[row].insert(col) {
                    fill += 1;
                }
                col_rows[col].insert(row);
            }
        }
        // The pivot's row/column die; scrub them from every active set.
        for &col in &cols_r {
            col_rows[col].remove(&pivot_row);
        }
        for &row in &rows_c {
            row_cols[row].remove(&pivot_col);
        }
        rows_alive[pivot_row] = false;
        cols_alive[pivot_col] = false;
        row_cols[pivot_row].clear();
        col_rows[pivot_col].clear();
        order.push((pivot_row as u32, pivot_col as u32));
    }
    Some(SymbolicOrder { order, fill })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compress a record built with individual visits.
    #[test]
    fn compress_matches_visited_slots() {
        let mut bits = StampBits::zeros(4);
        bits.visit(1, 0);
        bits.visit(1, 3);
        bits.visit(2, 2);
        bits.visit(0, 0);
        let pat = bits.compress();
        assert_eq!(pat.n(), 4);
        assert_eq!(pat.nnz(), 4);
        assert_eq!(pat.columns_of(0), &[0]);
        assert_eq!(pat.columns_of(1), &[0, 3]);
        assert_eq!(pat.columns_of(2), &[2]);
        assert_eq!(pat.columns_of(3), &[] as &[usize]);
        assert!(pat.contains(1, 3));
        assert!(!pat.contains(3, 1));
    }

    /// Visits are idempotent through the set representation.
    #[test]
    fn repeated_visits_stay_single_entries() {
        let mut bits = StampBits::zeros(2);
        bits.visit(0, 1);
        bits.visit(0, 1);
        bits.visit(1, 0);
        assert_eq!(bits.compress().nnz(), 2);
    }

    /// A diagonal-only pattern pivots along the diagonal with no fill.
    #[test]
    fn diagonal_order_is_the_identity() {
        let mut bits = StampBits::zeros(3);
        for i in 0..3 {
            bits.visit(i, i);
        }
        let order = markowitz_ordering(&bits.compress()).unwrap();
        assert_eq!(order.order, vec![(0, 0), (1, 1), (2, 2)]);
        assert_eq!(order.fill, 0);
    }

    /// A full 2×2 has every candidate at cost 1; the tie must land on the
    /// low end of the packed order, the (0,0) pivot.
    #[test]
    fn full_tie_breaks_to_lowest_packed_pivot() {
        let mut bits = StampBits::zeros(2);
        for row in 0..2 {
            for col in 0..2 {
                bits.visit(row, col);
            }
        }
        let order = markowitz_ordering(&bits.compress()).unwrap();
        assert_eq!(order.order, vec![(0, 0), (1, 1)]);
        assert_eq!(order.fill, 0);
    }

    /// Tridiagonal: each pivot's cross product is already present.
    #[test]
    fn tridiagonal_needs_no_fill() {
        let mut bits = StampBits::zeros(3);
        for i in 0..3 {
            bits.visit(i, i);
        }
        bits.visit(0, 1);
        bits.visit(1, 0);
        bits.visit(1, 2);
        bits.visit(2, 1);
        let order = markowitz_ordering(&bits.compress()).unwrap();
        assert_eq!(order.fill, 0);
        assert_eq!(order.order.len(), 3);
    }

    /// A 3-cycle with no diagonals (the pure-MNA shape): every elimination
    /// is a leaf, so the pivots walk the cycle in order at zero fill.
    #[test]
    fn three_cycle_orders_along_the_ring() {
        let mut bits = StampBits::zeros(3);
        bits.visit(0, 1);
        bits.visit(1, 2);
        bits.visit(2, 0);
        let order = markowitz_ordering(&bits.compress()).unwrap();
        assert_eq!(order.order, vec![(0, 1), (1, 2), (2, 0)]);
        assert_eq!(order.fill, 0);
    }

    /// A pattern whose only pivots have both neighbor counts ≥ 2: the first
    /// elimination must synthesize the missing cross entry, fill = 1, and
    /// the rest then pivots down the synthesized diagonal.
    #[test]
    fn forced_cross_pivot_creates_one_fill() {
        let mut bits = StampBits::zeros(3);
        bits.visit(0, 0);
        bits.visit(0, 1);
        bits.visit(1, 0);
        bits.visit(1, 2);
        bits.visit(2, 1);
        bits.visit(2, 2);
        let order = markowitz_ordering(&bits.compress()).unwrap();
        assert_eq!(order.order, vec![(0, 0), (1, 1), (2, 2)]);
        assert_eq!(order.fill, 1);
    }

    /// Every ordering is a permutation on both axes — rows and columns each
    /// used exactly once (the unsymmetric-MNA guarantee).
    #[test]
    fn orderings_are_row_and_column_permutations() {
        let shapes: Vec<(usize, Vec<(usize, usize)>)> = vec![
            // 4×4 diagonal
            (4, vec![(0, 0), (1, 1), (2, 2), (3, 3)]),
            // 4×4 full
            (
                4,
                (0..4).flat_map(|r| (0..4).map(move |c| (r, c))).collect(),
            ),
            // 3×3 band with the forced cross pivot
            (3, vec![(0, 0), (0, 1), (1, 0), (1, 2), (2, 1), (2, 2)]),
        ];
        for (n, shape) in shapes {
            let mut bits = StampBits::zeros(n);
            for (r, c) in &shape {
                bits.visit(*r, *c);
            }
            let order = markowitz_ordering(&bits.compress()).unwrap();
            let mut rows: Vec<u32> = order.order.iter().map(|o| o.0).collect();
            let mut cols: Vec<u32> = order.order.iter().map(|o| o.1).collect();
            rows.sort_unstable();
            cols.sort_unstable();
            assert_eq!(rows, (0..n as u32).collect::<Vec<_>>());
            assert_eq!(cols, (0..n as u32).collect::<Vec<_>>());
        }
    }

    /// Rows alive with no active entry: no ordering exists.
    #[test]
    fn structurally_stuck_patterns_report_none() {
        let mut bits = StampBits::zeros(2);
        bits.visit(0, 0);
        let pat = bits.compress();
        assert!(markowitz_ordering(&pat).is_none());
    }
}
