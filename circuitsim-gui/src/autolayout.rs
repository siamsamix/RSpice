//! Netlist → schematic: where each part goes and how the nets are wired.
//!
//! The importer hands this module a flat list of two-pin [`Part`]s (node ids
//! as the parser numbered them, `0` = ground) and gets back placed
//! [`Component`]s and orthogonal [`Wire`]s. The editor joins pins by *shared
//! grid points only* (a wire end touching the middle of another wire is not a
//! connection), so everything below is built to keep that contract: wires are
//! split at every pin and every T-junction, and wires of different nets only
//! ever cross at points that are an end of neither.
//!
//! Two phases, like a hand-drawn schematic:
//!
//!   1. **Placement — signal flow, left to right.**
//!      Series parts (both pins on real nets) lie flat between node columns;
//!      shunt parts (one pin on ground) hang down from their node with a
//!      ground symbol under them. Columns come from a breadth-first walk over
//!      the series graph starting at the first source's positive node, so a
//!      ladder reads left to right; branches take extra rows; unconnected
//!      islands stack below each other. Parallel parts between the same two
//!      nodes stack as rungs.
//!   2. **Routing — A\* on the grid.**
//!      Each net's pins are joined into a tree, short nets first. Wires avoid
//!      part bodies and the label space around them, prefer few bends, and
//!      cross other nets only straight and perpendicular. A connection that
//!      cannot be routed at all is tied by a shared net label instead, so the
//!      result is never electrically wrong, only less pretty.

use crate::schematic::{CompKind, Component, GridPt, Wire};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet, VecDeque};

// ─── Geometry (grid units; one unit is `GRID_SIZE` pixels) ──────────────────

/// Pin-to-pin length of every two-pin symbol.
const COMP_LEN: i32 = 4;
/// Spacing of side-by-side shunts on one node (room for name + value text).
const PITCH: i32 = 6;
/// Spacing of stacked parallel parts (rungs) between two nodes.
const SLOT_H: i32 = 3;
/// Breathing room kept between one row's lowest feature and the next row.
const ROW_SEP: i32 = 5;
/// Space under a shunt's lower pin that the ground symbol occupies.
const GROUND_ZONE: i32 = 2;
pub const ORIGIN_X: i32 = 5;
pub const ORIGIN_Y: i32 = 6;
/// Free routing border around the placed parts.
const MARGIN: i32 = 6;

// ─── Router costs ───────────────────────────────────────────────────────────

const BEND_COST: u32 = 3;
const CROSS_COST: u32 = 14;
const BODY_COST: u32 = 60;
const HALO_COST: u8 = 6;
const MAX_POPS: usize = 250_000;

type P = (i32, i32);

/// One two-pin part to draw. `a` is the pin that becomes `p1`.
#[derive(Clone, Debug)]
pub struct Part {
    pub kind: CompKind,
    pub a: usize,
    pub b: usize,
    pub val: String,
}

#[derive(Default)]
pub struct Layout {
    /// One per drawn part, in part order, then the ground symbols.
    pub components: Vec<Component>,
    pub wires: Vec<Wire>,
    /// Per non-ground node: a point on the net that is a pin, hence always a
    /// wire end — safe to hang a net label on.
    pub anchors: BTreeMap<usize, GridPt>,
    /// `(node, point)` pairs the router could not wire. Each must carry a
    /// label naming its node so the net still joins by name.
    pub ties: Vec<(usize, GridPt)>,
}

struct Term {
    pt: P,
    /// Outward direction the wire should leave in (0..4), or 4 for none.
    hint: u8,
}

/// Where a part's body sits, for the router's obstacle map.
struct Body {
    a: P,
    b: P,
    /// Cell below the lower pin reserved for a ground symbol.
    ground: Option<P>,
}

pub fn layout(parts: &[Part]) -> Layout {
    let drawn: Vec<usize> = (0..parts.len())
        .filter(|&i| !(parts[i].a == 0 && parts[i].b == 0))
        .collect();
    if drawn.is_empty() {
        return Layout::default();
    }

    let mut nodes: BTreeSet<usize> = BTreeSet::new();
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
    for &i in &drawn {
        let p = &parts[i];
        for n in [p.a, p.b] {
            if n != 0 {
                nodes.insert(n);
            }
        }
        if p.a != 0 && p.b != 0 && p.a != p.b {
            adj.entry(p.a).or_default().push(p.b);
            adj.entry(p.b).or_default().push(p.a);
        }
    }

    // ── 1. Hubs: a (column, row) cell per node ───────────────────────────
    let hub = assign_hubs(parts, &drawn, &nodes, &adj);

    // Shunts per node; sources first so they take the leftmost position.
    let mut shunts: HashMap<usize, Vec<usize>> = HashMap::new();
    for want_source in [true, false] {
        for &i in &drawn {
            let p = &parts[i];
            if (p.a == 0) == (p.b == 0) {
                continue; // series (both real) — ground-to-ground is not drawn
            }
            if (p.kind == CompKind::Voltage) != want_source {
                continue;
            }
            shunts.entry(if p.a == 0 { p.b } else { p.a }).or_default().push(i);
        }
    }

    // Series parts: which gap, which row, which rung.
    struct SeriesAt {
        gap: usize,
        row: usize,
        slot: usize,
        a_left: bool,
    }
    let mut series: HashMap<usize, SeriesAt> = HashMap::new();
    let mut slot_count: HashMap<(usize, usize), usize> = HashMap::new();
    for &i in &drawn {
        let p = &parts[i];
        if p.a == 0 || p.b == 0 {
            continue;
        }
        let (ha, hb) = (hub[&p.a], hub[&p.b]);
        let a_left = ha <= hb;
        let (lo, hi) = if a_left { (ha, hb) } else { (hb, ha) };
        let row = if hi.0 == lo.0 + 1 { hi.1 } else { lo.1 };
        let slot = slot_count.entry((lo.0, row)).or_insert(0);
        series.insert(i, SeriesAt { gap: lo.0, row, slot: *slot, a_left });
        *slot += 1;
    }

    // ── 2. Coordinates ───────────────────────────────────────────────────
    let ncols = hub.values().map(|h| h.0).max().unwrap_or(0) + 2;
    let nrows = hub.values().map(|h| h.1).max().unwrap_or(0) + 1;

    let mut col_k = vec![0usize; ncols];
    for (&n, list) in &shunts {
        let c = hub[&n].0;
        col_k[c] = col_k[c].max(list.len());
    }
    let clear_left = |c: usize| if col_k[c] > 0 { (col_k[c] as i32 - 1) * PITCH + 3 } else { 2 };
    let clear_right = |c: usize| if col_k[c] > 0 { 3 } else { 2 };
    let mut hub_x = vec![ORIGIN_X; ncols];
    for c in 0..ncols - 1 {
        hub_x[c + 1] = hub_x[c] + clear_left(c) + COMP_LEN + clear_right(c + 1);
    }

    let mut row_has_shunt = vec![false; nrows];
    for &n in shunts.keys() {
        row_has_shunt[hub[&n].1] = true;
    }
    let mut row_slots = vec![0usize; nrows];
    for (&(_, r), &k) in &slot_count {
        row_slots[r] = row_slots[r].max(k);
    }
    let mut row_y = vec![ORIGIN_Y; nrows];
    for r in 0..nrows.saturating_sub(1) {
        let shunt_need = if row_has_shunt[r] { COMP_LEN + GROUND_ZONE } else { 0 };
        let slot_need = row_slots[r].saturating_sub(1) as i32 * SLOT_H;
        row_y[r + 1] = row_y[r] + shunt_need.max(slot_need) + ROW_SEP;
    }

    // ── 3. Place parts, collect pins per net ─────────────────────────────
    let mut comps: Vec<Option<Component>> = vec![None; parts.len()];
    let mut grounds: Vec<P> = vec![];
    let mut bodies: Vec<Body> = vec![];
    let mut nets: BTreeMap<usize, Vec<Term>> = BTreeMap::new();
    let add_pin = |nets: &mut BTreeMap<usize, Vec<Term>>, net: usize, pt: P, hint: u8| {
        let list = nets.entry(net).or_default();
        if !list.iter().any(|t| t.pt == pt) {
            list.push(Term { pt, hint });
        }
    };
    let gp = |p: P| GridPt(p.0, p.1);

    for (&node, list) in &shunts {
        let (c, r) = hub[&node];
        for (j, &i) in list.iter().enumerate() {
            let x = hub_x[c] + j as i32 * PITCH;
            let top = (x, row_y[r]);
            let bottom = (x, row_y[r] + COMP_LEN);
            let p = &parts[i];
            let (p1, p2) = if p.a == 0 { (bottom, top) } else { (top, bottom) };
            comps[i] = Some(Component { kind: p.kind, p1: gp(p1), p2: gp(p2), val: p.val.clone(), attrs: Default::default() });
            add_pin(&mut nets, node, top, 3);
            grounds.push(bottom);
            bodies.push(Body { a: top, b: bottom, ground: Some((bottom.0, bottom.1 + 1)) });
        }
    }
    for &i in &drawn {
        let Some(at) = series.get(&i) else { continue };
        let p = &parts[i];
        let y = row_y[at.row] + at.slot as i32 * SLOT_H;
        let x1 = hub_x[at.gap] + clear_left(at.gap);
        let x2 = x1 + COMP_LEN;
        let (left, right) = ((x1, y), (x2, y));
        let (p1, p2, n1, n2, h1, h2) = if at.a_left {
            (left, right, p.a, p.b, 1, 0)
        } else {
            (right, left, p.a, p.b, 0, 1)
        };
        comps[i] = Some(Component { kind: p.kind, p1: gp(p1), p2: gp(p2), val: p.val.clone(), attrs: Default::default() });
        add_pin(&mut nets, n1, p1, h1);
        add_pin(&mut nets, n2, p2, h2);
        bodies.push(Body { a: left, b: right, ground: None });
    }

    // ── 4. Route ─────────────────────────────────────────────────────────
    let mut grid = Grid::new(&nets, &bodies);
    let mut order: Vec<usize> = nets.keys().copied().collect();
    order.sort_by_key(|n| {
        let t = &nets[n];
        let (x0, x1) = (t.iter().map(|t| t.pt.0).min().unwrap(), t.iter().map(|t| t.pt.0).max().unwrap());
        let (y0, y1) = (t.iter().map(|t| t.pt.1).min().unwrap(), t.iter().map(|t| t.pt.1).max().unwrap());
        (x1 - x0 + y1 - y0, *n)
    });

    let mut wires: Vec<Wire> = vec![];
    let mut ties: Vec<(usize, GridPt)> = vec![];
    for net in order {
        let terms = &nets[&net];
        if terms.len() < 2 {
            continue;
        }
        let pins: HashSet<P> = terms.iter().map(|t| t.pt).collect();
        let mut edges: BTreeSet<(P, P)> = BTreeSet::new();
        let root = grid.cell(terms[0].pt);
        grid.used[root] = Use::Net(net, 0);
        let mut tree: Vec<usize> = vec![root];
        let mut pending: Vec<usize> = (1..terms.len()).collect();
        while !pending.is_empty() {
            pending.retain(|&t| !matches!(grid.used[grid.cell(terms[t].pt)], Use::Net(n, _) if n == net));
            let Some((pi, &t)) = pending.iter().enumerate().min_by_key(|&(_, &t)| {
                let c = grid.xy(grid.cell(terms[t].pt));
                tree.iter().map(|&k| manhattan(grid.xy(k), c)).min().unwrap_or(0)
            }) else {
                break;
            };
            let start = grid.cell(terms[t].pt);
            let bbox = grid.bbox(&tree);
            let path = route(&grid, net, start, terms[t].hint, bbox, false)
                .or_else(|| route(&grid, net, start, terms[t].hint, bbox, true));
            match path {
                Some(path) => {
                    grid.commit(net, &path, &mut edges);
                    tree.extend(path);
                }
                None => {
                    ties.push((net, gp(terms[t].pt)));
                    ties.push((net, gp(terms[0].pt)));
                }
            }
            pending.remove(pi);
        }
        wires.extend(segments(&edges, &pins).into_iter().map(|(a, b)| Wire { p1: gp(a), p2: gp(b) }));
    }
    ties.sort_by_key(|&(n, p)| (n, p.0, p.1));
    ties.dedup_by_key(|&mut (n, p)| (n, p.0, p.1));

    // ── 5. Assemble ──────────────────────────────────────────────────────
    let mut components: Vec<Component> = comps.into_iter().flatten().collect();
    for g in grounds {
        components.push(Component { kind: CompKind::Ground, p1: gp(g), p2: gp(g), val: String::new(), attrs: Default::default() });
    }
    let anchors = nets
        .iter()
        .map(|(&n, t)| {
            let best = t.iter().map(|t| t.pt).min_by_key(|&(x, y)| (y, x)).unwrap();
            (n, gp(best))
        })
        .collect();
    Layout { components, wires, anchors, ties }
}

fn manhattan(a: P, b: P) -> i32 {
    (a.0 - b.0).abs() + (a.1 - b.1).abs()
}

/// Columns by breadth-first depth along series edges from each island's
/// start node; rows by branching. Islands stack downward.
fn assign_hubs(
    parts: &[Part],
    drawn: &[usize],
    nodes: &BTreeSet<usize>,
    adj: &HashMap<usize, Vec<usize>>,
) -> HashMap<usize, (usize, usize)> {
    // Start nodes: a source's positive side first, then everything else.
    let mut starts: Vec<usize> = vec![];
    for &i in drawn {
        let p = &parts[i];
        if p.kind == CompKind::Voltage {
            if let Some(n) = [p.a, p.b].into_iter().find(|&n| n != 0) {
                if !starts.contains(&n) {
                    starts.push(n);
                }
            }
        }
    }
    let rest: Vec<usize> = nodes.iter().copied().filter(|n| !starts.contains(n)).collect();
    starts.extend(rest);

    let mut hub: HashMap<usize, (usize, usize)> = HashMap::new();
    let mut occupied: HashSet<(usize, usize)> = HashSet::new();
    let mut next_row = 0usize;
    for start in starts {
        if hub.contains_key(&start) {
            continue;
        }
        hub.insert(start, (0, next_row));
        occupied.insert((0, next_row));
        let mut island_last = next_row;
        let mut queue = VecDeque::from([start]);
        while let Some(n) = queue.pop_front() {
            let (c, r) = hub[&n];
            for &m in adj.get(&n).map(|v| v.as_slice()).unwrap_or(&[]) {
                if hub.contains_key(&m) {
                    continue;
                }
                let mut row = r;
                while occupied.contains(&(c + 1, row)) {
                    row += 1;
                }
                hub.insert(m, (c + 1, row));
                occupied.insert((c + 1, row));
                island_last = island_last.max(row);
                queue.push_back(m);
            }
        }
        next_row = island_last + 1;
    }
    hub
}

// ─── Routing grid ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    Free,
    /// A routed wire of this net; mask bit 1 = horizontal run, 2 = vertical.
    Net(usize, u8),
    /// Two nets cross here: nothing else may use the cell.
    Crossed,
}

struct Grid {
    x0: i32,
    y0: i32,
    w: i32,
    h: i32,
    blocked: Vec<bool>,
    halo: Vec<u8>,
    pin: Vec<i64>,
    used: Vec<Use>,
}

impl Grid {
    fn new(nets: &BTreeMap<usize, Vec<Term>>, bodies: &[Body]) -> Self {
        let pts = nets
            .values()
            .flatten()
            .map(|t| t.pt)
            .chain(bodies.iter().flat_map(|b| [b.a, b.b]));
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for (x, y) in pts {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        x0 -= MARGIN;
        y0 -= MARGIN;
        x1 += MARGIN + GROUND_ZONE;
        y1 += MARGIN + GROUND_ZONE;
        let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
        let n = (w * h) as usize;
        let mut g = Grid {
            x0,
            y0,
            w,
            h,
            blocked: vec![false; n],
            halo: vec![0; n],
            pin: vec![-1; n],
            used: vec![Use::Free; n],
        };
        for (&net, terms) in nets {
            for t in terms {
                let c = g.cell(t.pt);
                g.pin[c] = net as i64;
            }
        }
        for b in bodies {
            let vertical = b.a.0 == b.b.0;
            if vertical {
                let x = b.a.0;
                for y in b.a.1 + 1..b.b.1 {
                    let c = g.cell((x, y));
                    g.blocked[c] = true;
                    for dx in -3..=3 {
                        if dx != 0 {
                            g.add_halo((x + dx, y));
                        }
                    }
                }
            } else {
                let y = b.a.1;
                for x in b.a.0 + 1..b.b.0 {
                    let c = g.cell((x, y));
                    g.blocked[c] = true;
                    for dy in [-1, 1] {
                        g.add_halo((x, y + dy));
                    }
                }
            }
            if let Some((gx, gy)) = b.ground {
                for dy in 0..GROUND_ZONE {
                    let c = g.cell((gx, gy + dy));
                    g.blocked[c] = true;
                }
            }
        }
        g
    }

    fn add_halo(&mut self, p: P) {
        if let Some(c) = self.try_cell(p.0, p.1) {
            self.halo[c] = self.halo[c].saturating_add(HALO_COST);
        }
    }

    fn try_cell(&self, x: i32, y: i32) -> Option<usize> {
        let (dx, dy) = (x - self.x0, y - self.y0);
        (dx >= 0 && dy >= 0 && dx < self.w && dy < self.h).then(|| (dy * self.w + dx) as usize)
    }

    fn cell(&self, p: P) -> usize {
        self.try_cell(p.0, p.1).expect("point inside the routing grid")
    }

    fn xy(&self, c: usize) -> P {
        let c = c as i32;
        (c % self.w + self.x0, c / self.w + self.y0)
    }

    fn bbox(&self, cells: &[usize]) -> (i32, i32, i32, i32) {
        let mut b = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &c in cells {
            let (x, y) = self.xy(c);
            b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
        }
        b
    }

    /// Record a routed path: mark the cells, note the unit edges.
    fn commit(&mut self, net: usize, path: &[usize], edges: &mut BTreeSet<(P, P)>) {
        for (k, &c) in path.iter().enumerate() {
            let mut mask = 0u8;
            if k > 0 {
                mask |= axis(self.xy(path[k - 1]), self.xy(c));
            }
            if k + 1 < path.len() {
                mask |= axis(self.xy(c), self.xy(path[k + 1]));
            }
            self.used[c] = match self.used[c] {
                Use::Free => Use::Net(net, mask),
                Use::Net(n, m) if n == net => Use::Net(net, m | mask),
                _ => Use::Crossed,
            };
        }
        for w in path.windows(2) {
            let (a, b) = (self.xy(w[0]), self.xy(w[1]));
            edges.insert((a.min(b), a.max(b)));
        }
    }
}

fn axis(a: P, b: P) -> u8 {
    if a.1 == b.1 { 1 } else { 2 }
}

/// A\* from `start` to the nearest cell of net `net`'s tree. Directions:
/// 0 right, 1 left, 2 down, 3 up, 4 none.
fn route(g: &Grid, net: usize, start: usize, hint: u8, tree_box: (i32, i32, i32, i32), relaxed: bool) -> Option<Vec<usize>> {
    const DX: [i32; 4] = [1, -1, 0, 0];
    const DY: [i32; 4] = [0, 0, 1, -1];
    let heur = |c: usize| -> u32 {
        let (x, y) = g.xy(c);
        let dx = (tree_box.0 - x).max(0).max(x - tree_box.2);
        let dy = (tree_box.1 - y).max(0).max(y - tree_box.3);
        (dx + dy) as u32
    };

    let mut best: HashMap<usize, (u32, usize)> = HashMap::new();
    let mut heap = BinaryHeap::new();
    let s0 = start * 5 + hint as usize;
    best.insert(s0, (0, usize::MAX));
    heap.push(Reverse((heur(start), 0u32, s0)));
    let mut pops = 0usize;

    while let Some(Reverse((_, cost, st))) = heap.pop() {
        if best[&st].0 < cost {
            continue;
        }
        pops += 1;
        if pops > MAX_POPS {
            return None;
        }
        let (cell, dir) = (st / 5, (st % 5) as u8);
        let here = g.used[cell];
        if cell != start && matches!(here, Use::Net(n, _) if n == net) {
            let mut path = vec![];
            let mut s = st;
            while s != usize::MAX {
                path.push(s / 5);
                s = best[&s].1;
            }
            path.reverse();
            return Some(path);
        }
        // Standing on another net's wire means we are crossing it: go straight.
        let crossing = cell != start && matches!(here, Use::Net(n, _) if n != net);
        let (x, y) = g.xy(cell);
        for nd in 0..4u8 {
            if dir < 4 && nd == dir ^ 1 {
                continue;
            }
            if crossing && nd != dir {
                continue;
            }
            let Some(nc) = g.try_cell(x + DX[nd as usize], y + DY[nd as usize]) else { continue };
            let mut extra = 0u32;
            if g.blocked[nc] {
                if !relaxed {
                    continue;
                }
                extra += BODY_COST;
            }
            if g.pin[nc] >= 0 && g.pin[nc] as usize != net {
                continue;
            }
            match g.used[nc] {
                Use::Free => {}
                Use::Net(n, _) if n == net => {}
                Use::Net(_, m) => {
                    let across = if nd < 2 { 2 } else { 1 };
                    if m != across {
                        continue;
                    }
                    extra += CROSS_COST;
                }
                Use::Crossed => continue,
            }
            if !relaxed {
                extra += g.halo[nc] as u32;
            }
            let bend = if dir < 4 && dir != nd { BEND_COST } else { 0 };
            let ncost = cost + 1 + bend + extra;
            let ns = nc * 5 + nd as usize;
            if best.get(&ns).map_or(true, |&(old, _)| ncost < old) {
                best.insert(ns, (ncost, st));
                heap.push(Reverse((ncost + heur(nc), ncost, ns)));
            }
        }
    }
    None
}

/// Merge a net's unit edges into maximal straight wires, broken at every pin,
/// corner and junction so each connection is a shared wire end.
fn segments(edges: &BTreeSet<(P, P)>, pins: &HashSet<P>) -> Vec<(P, P)> {
    let mut nbrs: BTreeMap<P, Vec<P>> = BTreeMap::new();
    for &(a, b) in edges {
        nbrs.entry(a).or_default().push(b);
        nbrs.entry(b).or_default().push(a);
    }
    let is_break = |p: P| {
        let n = &nbrs[&p];
        n.len() != 2 || pins.contains(&p) || !(n[0].0 == n[1].0 || n[0].1 == n[1].1)
    };
    let mut out: BTreeSet<(P, P)> = BTreeSet::new();
    for (&p, ns) in &nbrs {
        if !is_break(p) {
            continue;
        }
        for &q in ns {
            let (mut prev, mut cur) = (p, q);
            while !is_break(cur) {
                let next = nbrs[&cur].iter().copied().find(|&n| n != prev).unwrap();
                prev = cur;
                cur = next;
            }
            out.insert((p.min(cur), p.max(cur)));
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schematic::{NetLabel, NodeAssignment};
    use std::fmt::Write as _;

    fn part(kind: CompKind, a: usize, b: usize) -> Part {
        let val = match kind {
            CompKind::Resistor => "1k",
            CompKind::Capacitor => "100n",
            CompKind::Inductor => "10m",
            _ => "5",
        };
        Part { kind, a, b, val: val.into() }
    }
    use CompKind::{Capacitor as C, Inductor as L, Resistor as R, Voltage as V};

    fn unit_edges(w: &Wire) -> Vec<(P, P)> {
        let (a, b) = ((w.p1.0, w.p1.1), (w.p2.0, w.p2.1));
        let (a, b) = (a.min(b), a.max(b));
        let mut out = vec![];
        let mut cur = a;
        while cur != b {
            let next = (cur.0 + (b.0 - cur.0).signum(), cur.1 + (b.1 - cur.1).signum());
            out.push((cur, next));
            cur = next;
        }
        out
    }

    /// The layout must reproduce the netlist's connectivity exactly, wire
    /// only along grid axes, never run a wire through a part or over another
    /// wire, and keep parts apart.
    fn check(name: &str, parts: &[Part]) -> Layout {
        let lay = layout(parts);
        let labels: Vec<NetLabel> = lay
            .ties
            .iter()
            .map(|&(n, at)| NetLabel { at, name: format!("_n{n}") })
            .collect();
        let asg = NodeAssignment::of(&lay.components, &lay.wires, &labels);

        // Connectivity: a bijection between netlist nodes and schematic nodes.
        let mut map: HashMap<usize, usize> = HashMap::new();
        let mut back: HashMap<usize, usize> = HashMap::new();
        map.insert(0, 0);
        back.insert(0, 0);
        let drawn: Vec<&Part> = parts.iter().filter(|p| !(p.a == 0 && p.b == 0)).collect();
        let non_ground: Vec<&Component> =
            lay.components.iter().filter(|c| c.kind != CompKind::Ground).collect();
        assert_eq!(drawn.len(), non_ground.len(), "{name}: part count");
        for (p, c) in drawn.iter().zip(&non_ground) {
            assert!(p.kind == c.kind, "{name}: order of parts changed");
            for (orig, pt) in [(p.a, c.p1), (p.b, c.p2)] {
                let got = asg.node_of(pt);
                assert_eq!(*map.entry(orig).or_insert(got), got, "{name}: node {orig} split");
                assert_eq!(*back.entry(got).or_insert(orig), orig, "{name}: nodes merged at {got}");
            }
        }

        // Geometry.
        let mut body: HashMap<P, usize> = HashMap::new();
        let mut pins: HashMap<P, usize> = HashMap::new();
        for (i, c) in non_ground.iter().enumerate() {
            let (a, b) = ((c.p1.0, c.p1.1), (c.p2.0, c.p2.1));
            assert_eq!(manhattan(a, b), COMP_LEN, "{name}: part length");
            assert!(a.0 == b.0 || a.1 == b.1, "{name}: part not axis aligned");
            for q in [a, b] {
                assert!(pins.insert(q, i).is_none() || false, "{name}: two parts share pin {q:?}");
            }
            let inner: HashSet<P> = unit_edges(&Wire { p1: c.p1, p2: c.p2 })
                .into_iter()
                .flat_map(|e| [e.0, e.1])
                .filter(|&q| q != a && q != b)
                .collect();
            for q in inner {
                assert!(body.insert(q, i).is_none(), "{name}: parts overlap at {q:?}");
            }
        }
        let mut seen: HashSet<(P, P)> = HashSet::new();
        for w in &lay.wires {
            assert!(w.p1.0 == w.p2.0 || w.p1.1 == w.p2.1, "{name}: diagonal wire");
            assert!(w.p1 != w.p2, "{name}: empty wire");
            for e in unit_edges(w) {
                assert!(seen.insert(e), "{name}: wires overlap at {e:?}");
                for q in [e.0, e.1] {
                    assert!(!body.contains_key(&q), "{name}: wire through a part at {q:?}");
                }
            }
        }
        if let Ok(dir) = std::env::var("RSPICE_DUMP_DIR") {
            std::fs::write(format!("{dir}/{name}.svg"), svg(&lay)).unwrap();
        }
        lay
    }

    fn svg(lay: &Layout) -> String {
        let (mut mx, mut my) = (0, 0);
        for c in &lay.components {
            mx = mx.max(c.p1.0).max(c.p2.0);
            my = my.max(c.p1.1).max(c.p2.1);
        }
        let k = 20;
        let mut s = String::new();
        let _ = write!(s, "<svg xmlns='http://www.w3.org/2000/svg' width='{}' height='{}' style='background:#fff'>", (mx + 6) * k, (my + 6) * k);
        for w in &lay.wires {
            let _ = write!(s, "<line x1='{}' y1='{}' x2='{}' y2='{}' stroke='#06c' stroke-width='1.5'/>", w.p1.0 * k, w.p1.1 * k, w.p2.0 * k, w.p2.1 * k);
        }
        let names = crate::schematic::instance_names(&lay.components);
        for (c, n) in lay.components.iter().zip(names) {
            let (x1, y1, x2, y2) = (c.p1.0 * k, c.p1.1 * k, c.p2.0 * k, c.p2.1 * k);
            if c.kind == CompKind::Ground {
                let _ = write!(s, "<path d='M{x1} {y1}v12m-9 0h18l-9 11z' stroke='#333' fill='none'/>");
                continue;
            }
            let (cx, cy) = ((x1 + x2) / 2, (y1 + y2) / 2);
            let _ = write!(s, "<line x1='{x1}' y1='{y1}' x2='{x2}' y2='{y2}' stroke='#333'/>");
            let (fill, w, h) = match (c.kind, y1 == y2) {
                (CompKind::Resistor, true) => ("#fdd", 44, 14),
                (CompKind::Resistor, false) => ("#fdd", 14, 44),
                (CompKind::Capacitor, _) => ("#dfd", 14, 14),
                (CompKind::Inductor, true) => ("#ddf", 44, 14),
                (CompKind::Inductor, false) => ("#ddf", 14, 44),
                _ => ("#ffd", 24, 24),
            };
            let _ = write!(s, "<rect x='{}' y='{}' width='{w}' height='{h}' fill='{fill}' stroke='#333'/>", cx - w / 2, cy - h / 2);
            if c.kind == CompKind::Voltage {
                // plus on the p1 side as netlist semantics say
                let (px, py) = (x1 + (cx - x1) / 2, y1 + (cy - y1) / 2);
                let _ = write!(s, "<text x='{px}' y='{}' font-size='11' text-anchor='middle'>+</text>", py + 4);
            }
            let (nx, ny) = if y1 == y2 { (cx, cy - 10) } else { (cx - 18, cy) };
            let (vx, vy) = if y1 == y2 { (cx, cy + 20) } else { (cx + 18, cy) };
            let _ = write!(s, "<text x='{nx}' y='{ny}' font-size='10' text-anchor='middle'>{n}</text><text x='{vx}' y='{vy}' font-size='10' text-anchor='middle'>{}</text>", c.val);
        }
        s.push_str("</svg>");
        s
    }

    #[test]
    fn rc_low_pass() {
        let lay = check("rc", &[part(V, 1, 0), part(R, 1, 2), part(C, 2, 0)]);
        assert!(lay.ties.is_empty());
        // The resistor lies flat; source and cap hang down.
        let r = lay.components.iter().find(|c| c.kind == R).unwrap();
        assert_eq!(r.p1.1, r.p2.1);
    }

    #[test]
    fn two_stage_ladder() {
        check("ladder", &[part(V, 1, 0), part(R, 1, 2), part(C, 2, 0), part(R, 2, 3), part(C, 3, 0)]);
    }

    #[test]
    fn divider_with_load_has_two_shunts_on_one_node() {
        check("divider_load", &[part(V, 1, 0), part(R, 1, 2), part(R, 2, 0), part(R, 2, 0)]);
    }

    #[test]
    fn parallel_parts_stack_as_rungs() {
        check("parallel", &[part(V, 1, 0), part(R, 1, 2), part(C, 1, 2), part(L, 1, 2), part(R, 2, 0)]);
    }

    #[test]
    fn rlc_series_and_branches() {
        check(
            "branches",
            &[part(V, 1, 0), part(R, 1, 2), part(L, 2, 3), part(C, 2, 4), part(R, 3, 0), part(R, 4, 0), part(R, 3, 4)],
        );
    }

    #[test]
    fn feedback_across_several_columns() {
        check(
            "feedback",
            &[part(V, 1, 0), part(R, 1, 2), part(R, 2, 3), part(R, 3, 4), part(C, 4, 0), part(R, 4, 1), part(C, 2, 0)],
        );
    }

    #[test]
    fn disconnected_islands_stack() {
        check("islands", &[part(V, 1, 0), part(R, 1, 2), part(C, 2, 0), part(V, 3, 0), part(R, 3, 4), part(L, 4, 0)]);
    }

    #[test]
    fn floating_source_and_odd_shapes() {
        check("odd", &[part(V, 1, 2), part(R, 1, 3), part(R, 2, 3), part(C, 3, 3), part(R, 0, 0), part(V, 0, 4), part(R, 4, 1)]);
    }

    #[test]
    fn random_circuits_keep_their_connectivity() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move |m: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % m as u64) as usize
        };
        let mut tied = 0;
        for case in 0..60 {
            let n_nodes = 2 + next(14);
            let n_parts = 2 + next(26);
            let mut parts = vec![part(V, 1, 0)];
            for _ in 0..n_parts {
                let kind = [R, C, L, V][next(4)];
                let a = next(n_nodes + 1);
                let b = if next(3) == 0 { 0 } else { next(n_nodes + 1) };
                parts.push(part(kind, a, b));
            }
            let lay = check(&format!("random{case}"), &parts);
            tied += usize::from(!lay.ties.is_empty());
        }
        // Label ties are a safety net, not the normal way nets get joined.
        assert!(tied <= 3, "{tied} of 60 random circuits needed label ties");
    }

    #[test]
    fn a_long_ladder_lays_out_quickly() {
        let mut parts = vec![part(V, 1, 0)];
        for i in 1..120 {
            parts.push(part(R, i, i + 1));
            parts.push(part(C, i + 1, 0));
        }
        let t = std::time::Instant::now();
        check("long_ladder", &parts);
        assert!(t.elapsed().as_secs() < 20, "took {:?}", t.elapsed());
    }
}
