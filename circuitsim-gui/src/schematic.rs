//! The schematic's data layer: the element IR the editor manipulates, plus
//! the union-find node assignment shared by the netlist writer and the
//! on-canvas node labels. One owner for the numbering — previously each
//! replicated the DSU and re-derived node ids, which is exactly how they
//! drift apart.

use egui::Pos2;
use std::collections::{HashMap, HashSet};

/// Canvas grid pitch in screen pixels.
pub const GRID_SIZE: f32 = 20.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridPt(pub i32, pub i32);

impl GridPt {
    pub fn from_pos(pos: Pos2) -> Self {
        Self((pos.x / GRID_SIZE).round() as i32, (pos.y / GRID_SIZE).round() as i32)
    }

    pub fn to_pos(self) -> Pos2 {
        Pos2::new(self.0 as f32 * GRID_SIZE, self.1 as f32 * GRID_SIZE)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CompKind {
    Resistor,
    Capacitor,
    Inductor,
    Voltage,
    Ground,
}

#[derive(Clone)]
pub struct Component {
    pub kind: CompKind,
    pub p1: GridPt,
    pub p2: GridPt,
    pub val: String,
}

#[derive(Clone)]
pub struct Wire {
    pub p1: GridPt,
    pub p2: GridPt,
}

/// SPICE node assignment over the schematic: every grid point maps to a node
/// number (ground roots = 0, all other components' nodes numbered 1..n in
/// sorted-grid order, deterministic frame to frame).
///
/// The union-find itself stays private — the interface is the resulting map.
pub struct NodeAssignment {
    /// The unique grid points, in the canonical sorted order used for
    /// numbering. (The painter iterates this to place one label per node.)
    pub pt_list: Vec<GridPt>,
    pt_to_node: HashMap<GridPt, usize>,
}

impl NodeAssignment {
    /// Assign SPICE node numbers to the schematic's points.
    pub fn of(components: &[Component], wires: &[Wire]) -> Self {
        // 1. Collect all unique grid points touched by components or wires
        //    (a ground's p2 equals its p1 everywhere the editor creates them;
        //    including it here matches the historical writer exactly).
        let mut points = HashSet::new();
        for c in components {
            points.insert(c.p1);
            points.insert(c.p2);
        }
        for w in wires {
            points.insert(w.p1);
            points.insert(w.p2);
        }

        let mut pt_list: Vec<GridPt> = points.into_iter().collect();
        // Sort deterministically so DSU root assignment never changes.
        pt_list.sort_by_key(|p| (p.0, p.1));
        let pt_to_idx: HashMap<GridPt, usize> =
            pt_list.iter().enumerate().map(|(i, &p)| (p, i)).collect();

        // 2. Wires connect nodes.
        let mut dsu = Dsu::new(pt_list.len());
        for w in wires {
            let i1 = pt_to_idx[&w.p1];
            let i2 = pt_to_idx[&w.p2];
            dsu.union(i1, i2);
        }

        // 3. Ground roots are forced to node 0.
        let mut ground_roots = HashSet::new();
        for c in components {
            if c.kind == CompKind::Ground {
                if let Some(&idx) = pt_to_idx.get(&c.p1) {
                    ground_roots.insert(dsu.find(idx));
                }
            }
        }

        // 4. Number the remaining roots 1..n in first-seen (sorted) order.
        let mut root_to_node: HashMap<usize, usize> = HashMap::new();
        let mut next_id = 1usize;
        for i in 0..pt_list.len() {
            let root = dsu.find(i);
            root_to_node.entry(root).or_insert_with(|| {
                if ground_roots.contains(&root) {
                    0
                } else {
                    let id = next_id;
                    next_id += 1;
                    id
                }
            });
        }

        let pt_to_node = pt_list
            .iter()
            .enumerate()
            .map(|(i, &pt)| {
                let root = root_to_node[&dsu.find(i)];
                (pt, root)
            })
            .collect();
        Self { pt_list, pt_to_node }
    }

    /// SPICE node id at a grid point.
    pub fn node_of(&self, pt: GridPt) -> usize {
        self.pt_to_node[&pt]
    }
}

/// Union-find over point indices (path-shortening find, union by root).
struct Dsu {
    parent: Vec<usize>,
}

impl Dsu {
    fn new(size: usize) -> Self {
        Self { parent: (0..size).collect() }
    }

    fn find(&mut self, i: usize) -> usize {
        if self.parent[i] == i {
            i
        } else {
            let p = self.parent[i];
            let root = self.find(p);
            self.parent[i] = root;
            root
        }
    }

    fn union(&mut self, i: usize, j: usize) {
        let root_i = self.find(i);
        let root_j = self.find(j);
        if root_i != root_j {
            self.parent[root_i] = root_j;
        }
    }
}
