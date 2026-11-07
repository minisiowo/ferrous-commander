//! Tiling layout for browser panels, in the spirit of Hyprland's "dwindle" layout:
//! a new panel splits the active one along its longer side; dividers drag to resize.

use egui::{Rect, pos2};
use serde::{Deserialize, Serialize};

/// Gap between panels (also the divider's grab area).
pub const GAP: f32 = 4.0;
/// A split never gives a side less than this share.
const MIN_RATIO: f32 = 0.12;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Tile {
    Leaf(usize),
    /// `vertical`: children are stacked top/bottom; otherwise side by side.
    Split {
        vertical: bool,
        ratio: f32,
        a: Box<Tile>,
        b: Box<Tile>,
    },
}

/// A divider between the two children of the split at `path` (false = a, true = b).
pub struct Divider {
    pub path: Vec<bool>,
    pub rect: Rect,
    pub vertical: bool,
    /// The split's whole rectangle, to turn a pointer position into a ratio.
    pub area: Rect,
}

fn halves(r: Rect, vertical: bool, ratio: f32) -> (Rect, Rect, Rect) {
    if vertical {
        let y = r.min.y + (r.height() - GAP) * ratio;
        (
            Rect::from_min_max(r.min, pos2(r.max.x, y)),
            Rect::from_min_max(pos2(r.min.x, y), pos2(r.max.x, y + GAP)),
            Rect::from_min_max(pos2(r.min.x, y + GAP), r.max),
        )
    } else {
        let x = r.min.x + (r.width() - GAP) * ratio;
        (
            Rect::from_min_max(r.min, pos2(x, r.max.y)),
            Rect::from_min_max(pos2(x, r.min.y), pos2(x + GAP, r.max.y)),
            Rect::from_min_max(pos2(x + GAP, r.min.y), r.max),
        )
    }
}

impl Tile {
    /// Panel rectangles and the dividers between them.
    pub fn layout(
        &self,
        r: Rect,
        out: &mut Vec<(usize, Rect)>,
        divs: &mut Vec<Divider>,
        path: &mut Vec<bool>,
    ) {
        match self {
            Tile::Leaf(i) => out.push((*i, r)),
            Tile::Split {
                vertical,
                ratio,
                a,
                b,
            } => {
                let (ra, d, rb) = halves(r, *vertical, *ratio);
                divs.push(Divider {
                    path: path.clone(),
                    rect: d,
                    vertical: *vertical,
                    area: r,
                });
                path.push(false);
                a.layout(ra, out, divs, path);
                path.pop();
                path.push(true);
                b.layout(rb, out, divs, path);
                path.pop();
            }
        }
    }

    pub fn set_ratio(&mut self, path: &[bool], value: f32) {
        match (self, path.split_first()) {
            (Tile::Split { ratio, .. }, None) => *ratio = value.clamp(MIN_RATIO, 1.0 - MIN_RATIO),
            (Tile::Split { a, b, .. }, Some((side, rest))) => {
                if *side {
                    b.set_ratio(rest, value)
                } else {
                    a.set_ratio(rest, value)
                }
            }
            _ => {}
        }
    }

    /// Replace leaf `target` with a split holding `target` and `new` (new goes second).
    pub fn split(&mut self, target: usize, new: usize, vertical: bool) -> bool {
        match self {
            Tile::Leaf(i) if *i == target => {
                *self = Tile::Split {
                    vertical,
                    ratio: 0.5,
                    a: Box::new(Tile::Leaf(target)),
                    b: Box::new(Tile::Leaf(new)),
                };
                true
            }
            Tile::Leaf(_) => false,
            Tile::Split { a, b, .. } => {
                a.split(target, new, vertical) || b.split(target, new, vertical)
            }
        }
    }

    /// Move a panel beside a target without changing panel identities.
    pub fn relocate(&mut self, source: usize, target: usize, vertical: bool, before: bool) -> bool {
        if source == target {
            return false;
        }
        let mut leaves = Vec::new();
        self.leaves(&mut leaves);
        if !leaves.contains(&source) || !leaves.contains(&target) {
            return false;
        }
        self.remove_inner(source);
        self.insert_at(target, source, vertical, before)
    }

    pub fn insert_at(&mut self, target: usize, source: usize, vertical: bool, before: bool) -> bool {
        match self {
            Tile::Leaf(i) if *i == target => {
                let (a, b) = if before {
                    (source, target)
                } else {
                    (target, source)
                };
                *self = Tile::Split {
                    vertical,
                    ratio: 0.5,
                    a: Box::new(Tile::Leaf(a)),
                    b: Box::new(Tile::Leaf(b)),
                };
                true
            }
            Tile::Split { a, b, .. } => {
                a.insert_at(target, source, vertical, before)
                    || b.insert_at(target, source, vertical, before)
            }
            _ => false,
        }
    }

    /// Remove leaf `target`; its sibling takes the space. Leaves above `target` shift down by one
    /// because the panel list shrinks. Returns false when `target` is the last panel.
    pub fn remove(&mut self, target: usize) -> bool {
        if matches!(self, Tile::Leaf(_)) {
            return false;
        }
        let ok = self.remove_inner(target);
        if ok {
            self.renumber(target);
        }
        ok
    }

    fn remove_inner(&mut self, target: usize) -> bool {
        let Tile::Split { a, b, .. } = self else {
            return false;
        };
        if matches!(**a, Tile::Leaf(i) if i == target) {
            *self = (**b).clone();
            return true;
        }
        if matches!(**b, Tile::Leaf(i) if i == target) {
            *self = (**a).clone();
            return true;
        }
        a.remove_inner(target) || b.remove_inner(target)
    }

    fn renumber(&mut self, removed: usize) {
        match self {
            Tile::Leaf(i) if *i > removed => *i -= 1,
            Tile::Leaf(_) => {}
            Tile::Split { a, b, .. } => {
                a.renumber(removed);
                b.renumber(removed);
            }
        }
    }

    pub fn leaves(&self, out: &mut Vec<usize>) {
        match self {
            Tile::Leaf(i) => out.push(*i),
            Tile::Split { a, b, .. } => {
                a.leaves(out);
                b.leaves(out);
            }
        }
    }

    /// Flip the split that directly holds leaf `target` (Hyprland "togglesplit").
    pub fn toggle_split(&mut self, target: usize) -> bool {
        match self {
            Tile::Leaf(_) => false,
            Tile::Split { vertical, a, b, .. } => {
                if matches!(**a, Tile::Leaf(i) if i == target)
                    || matches!(**b, Tile::Leaf(i) if i == target)
                {
                    *vertical = !*vertical;
                    true
                } else {
                    a.toggle_split(target) || b.toggle_split(target)
                }
            }
        }
    }

    /// A valid tree for `n` panels: keeps this one when it matches, else stacks them in a row.
    pub fn sanitize(self, n: usize) -> Tile {
        let mut l = Vec::new();
        self.leaves(&mut l);
        let mut sorted = l.clone();
        sorted.sort();
        if sorted == (0..n).collect::<Vec<_>>() {
            return self;
        }
        let mut t = Tile::Leaf(0);
        for k in 1..n {
            t.split(k - 1, k, false);
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_and_remove() {
        let mut t = Tile::Leaf(0);
        assert!(t.split(0, 1, false));
        assert!(t.split(1, 2, true));
        let mut l = Vec::new();
        t.leaves(&mut l);
        assert_eq!(l, vec![0, 1, 2]);
        assert!(t.remove(1));
        let mut l = Vec::new();
        t.leaves(&mut l);
        assert_eq!(l, vec![0, 1]);
        assert!(t.remove(0));
        assert!(!t.remove(0));
    }
}
