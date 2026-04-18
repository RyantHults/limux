//! Split tree data model for pane layout management.
//!
//! A binary tree where leaf nodes hold a PaneId (a pane containing one or more
//! tabs) and split nodes divide space horizontally or vertically between two
//! children.

use std::collections::HashMap;

use gtk4;

use crate::workspace::PaneId;

pub type SurfaceId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

impl From<Orientation> for gtk4::Orientation {
    fn from(o: Orientation) -> Self {
        match o {
            Orientation::Horizontal => gtk4::Orientation::Horizontal,
            Orientation::Vertical => gtk4::Orientation::Vertical,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Normalized bounding rectangle in [0,1]×[0,1] space.
#[derive(Debug, Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(u32);

#[derive(Debug, Clone)]
pub enum Node {
    Leaf {
        pane_id: PaneId,
    },
    Split {
        orientation: Orientation,
        ratio: f64, // 0.0..1.0 — proportion of first child
        first: NodeId,
        second: NodeId,
    },
}

/// A binary tree of split panes.
#[derive(Debug)]
pub struct SplitTree {
    nodes: HashMap<NodeId, Node>,
    root: Option<NodeId>,
    next_id: u32,
    focused: Option<PaneId>,
}

impl SplitTree {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            root: None,
            next_id: 0,
            focused: None,
        }
    }

    /// Create a tree with a single pane.
    pub fn new_with_pane(pane_id: PaneId) -> Self {
        let mut tree = Self::new();
        let node_id = tree.alloc_id();
        tree.nodes.insert(node_id, Node::Leaf { pane_id });
        tree.root = Some(node_id);
        tree.focused = Some(pane_id);
        tree
    }

    /// Merge two trees under a new split root.
    /// Used during session restore to reconstruct nested layouts.
    pub fn merge(
        first: SplitTree,
        second: SplitTree,
        orientation: Orientation,
        ratio: f64,
    ) -> Self {
        let mut tree = Self::new();
        // Ensure our next_id is above both subtrees to avoid collisions
        tree.next_id = first.next_id.max(second.next_id);

        // Re-insert all nodes from first tree with remapped IDs
        let mut first_remap = HashMap::new();
        for (old_id, _) in &first.nodes {
            let new_id = tree.alloc_id();
            first_remap.insert(*old_id, new_id);
        }
        for (old_id, node) in first.nodes {
            let new_id = first_remap[&old_id];
            let remapped = remap_node(&node, &first_remap);
            tree.nodes.insert(new_id, remapped);
        }

        // Re-insert all nodes from second tree
        let mut second_remap = HashMap::new();
        for (old_id, _) in &second.nodes {
            let new_id = tree.alloc_id();
            second_remap.insert(*old_id, new_id);
        }
        for (old_id, node) in second.nodes {
            let new_id = second_remap[&old_id];
            let remapped = remap_node(&node, &second_remap);
            tree.nodes.insert(new_id, remapped);
        }

        // Create root split
        let first_root = first.root.and_then(|r| first_remap.get(&r).copied());
        let second_root = second.root.and_then(|r| second_remap.get(&r).copied());

        if let (Some(fr), Some(sr)) = (first_root, second_root) {
            let root_id = tree.alloc_id();
            tree.nodes.insert(root_id, Node::Split {
                orientation,
                ratio,
                first: fr,
                second: sr,
            });
            tree.root = Some(root_id);
        } else if let Some(fr) = first_root {
            tree.root = Some(fr);
        } else if let Some(sr) = second_root {
            tree.root = Some(sr);
        }

        // Focus first pane
        tree.focused = tree.panes().first().copied();
        tree
    }

    fn alloc_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn root(&self) -> Option<NodeId> {
        self.root
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn focused_pane(&self) -> Option<PaneId> {
        self.focused
    }

    pub fn set_focused(&mut self, pane_id: PaneId) {
        self.focused = Some(pane_id);
    }

    /// Update the ratio of a split node.
    pub fn set_ratio(&mut self, node_id: NodeId, new_ratio: f64) {
        if let Some(Node::Split { ratio, .. }) = self.nodes.get_mut(&node_id) {
            *ratio = new_ratio;
        }
    }

    /// Insert a new pane after the focused pane in the workspace chain.
    /// The new pane appears as a sibling at the same level, not nested inside
    /// the focused pane. After inserting, ratios are set for equal distribution.
    pub fn insert_pane_after_focused(
        &mut self,
        new_pane_id: PaneId,
        orientation: Orientation,
    ) -> bool {
        let Some(root) = self.root else {
            // Empty tree — just create a single leaf
            let node_id = self.alloc_id();
            self.nodes.insert(node_id, Node::Leaf { pane_id: new_pane_id });
            self.root = Some(node_id);
            self.focused = Some(new_pane_id);
            return true;
        };

        // If root is a single leaf, create the first split
        if let Some(Node::Leaf { .. }) = self.nodes.get(&root) {
            let new_leaf = self.alloc_id();
            self.nodes.insert(new_leaf, Node::Leaf { pane_id: new_pane_id });
            let split_id = self.alloc_id();
            self.nodes.insert(split_id, Node::Split {
                orientation,
                ratio: 0.5,
                first: root,
                second: new_leaf,
            });
            self.root = Some(split_id);
            self.focused = Some(new_pane_id);
            self.equalize_chain();
            return true;
        }

        // Find the focused pane's leaf node, then insert after it.
        // In the chain structure, the focused leaf is either:
        //   - a `first` child of some split (pane followed by the rest of the chain)
        //   - the deepest `second` child (last pane in the chain)
        //
        // "Insert after" means: find the subtree that comes after the focused leaf
        // (the `second` sibling of the split containing it) and wrap that subtree
        // with a new split: [NEW, old_subtree]. If the focused leaf IS the last
        // pane (deepest second child), just split it into [focused, NEW].

        let focused = self.focused.unwrap_or_else(|| {
            // Fallback: use the last pane
            match self.nodes.get(&self.find_chain_end(root)) {
                Some(Node::Leaf { pane_id }) => *pane_id,
                _ => return new_pane_id, // shouldn't happen
            }
        });

        let focused_leaf = match self.node_id_for_pane(focused) {
            Some(id) => id,
            None => {
                // Focused pane not found — fall back to appending at end
                let last = self.find_chain_end(root);
                self.insert_after_node(last, new_pane_id, orientation);
                self.focused = Some(new_pane_id);
                self.equalize_chain();
                return true;
            }
        };

        self.insert_after_node(focused_leaf, new_pane_id, orientation);
        self.focused = Some(new_pane_id);
        self.equalize_chain();
        true
    }

    /// Insert a new leaf after the given node in the chain.
    /// If the node is a `first` child (has a `second` sibling), wrap the sibling:
    ///   Split(A, REST) → Split(A, Split(NEW, REST))
    /// If the node is a `second` child (last in its sub-chain), split it:
    ///   Split(X, A) → Split(X, Split(A, NEW))
    fn insert_after_node(&mut self, target: NodeId, new_pane_id: PaneId, orientation: Orientation) {
        let new_leaf = self.alloc_id();
        self.nodes.insert(new_leaf, Node::Leaf { pane_id: new_pane_id });

        // Find the parent split that contains target
        let root = match self.root {
            Some(r) => r,
            None => return,
        };

        if root == target {
            // Target is the root leaf (single pane) — shouldn't get here, handled above
            let new_split = self.alloc_id();
            self.nodes.insert(new_split, Node::Split {
                orientation, ratio: 0.5, first: target, second: new_leaf,
            });
            self.root = Some(new_split);
            return;
        }

        let Some((parent_id, is_first)) = self.find_parent(root, match self.nodes.get(&target) {
            Some(Node::Leaf { pane_id }) => *pane_id,
            _ => return,
        }) else { return };

        if is_first {
            // Target is the `first` child — the `second` child is the rest of the chain.
            // Insert NEW between target and the rest: replace `second` with Split(NEW, old_second).
            let old_second = match self.nodes.get(&parent_id) {
                Some(Node::Split { second, .. }) => *second,
                _ => return,
            };
            let new_split = self.alloc_id();
            self.nodes.insert(new_split, Node::Split {
                orientation, ratio: 0.5, first: new_leaf, second: old_second,
            });
            if let Some(Node::Split { second, .. }) = self.nodes.get_mut(&parent_id) {
                *second = new_split;
            }
        } else {
            // Target is the `second` child (end of this sub-chain).
            // Replace target with Split(target, NEW).
            let new_split = self.alloc_id();
            self.nodes.insert(new_split, Node::Split {
                orientation, ratio: 0.5, first: target, second: new_leaf,
            });
            if let Some(Node::Split { second, .. }) = self.nodes.get_mut(&parent_id) {
                *second = new_split;
            }
        }
    }

    /// Find the deepest leaf following the `second` (end) child at each split.
    fn find_chain_end(&self, node_id: NodeId) -> NodeId {
        match self.nodes.get(&node_id) {
            Some(Node::Leaf { .. }) => node_id,
            Some(Node::Split { second, .. }) => self.find_chain_end(*second),
            None => node_id,
        }
    }

    /// Replace a child reference in the tree.
    fn replace_child(&mut self, node_id: NodeId, old_child: NodeId, new_child: NodeId) {
        if let Some(Node::Split { first, second, .. }) = self.nodes.get_mut(&node_id) {
            if *first == old_child {
                *first = new_child;
                return;
            }
            if *second == old_child {
                *second = new_child;
                return;
            }
            // Recurse into children
            let f = *first;
            let s = *second;
            self.replace_child(f, old_child, new_child);
            self.replace_child(s, old_child, new_child);
        }
    }

    /// Set ratios for equal distribution in a right-leaning chain.
    /// For N panes: root ratio = 1/N, next = 1/(N-1), ..., deepest = 1/2.
    pub fn equalize_chain(&mut self) {
        let n = self.panes().len();
        if n <= 1 { return; }
        if let Some(root) = self.root {
            self.set_chain_ratios(root, n);
        }
    }

    fn set_chain_ratios(&mut self, node_id: NodeId, remaining: usize) {
        if remaining <= 1 { return; }
        if let Some(Node::Split { ratio, second, .. }) = self.nodes.get_mut(&node_id) {
            *ratio = 1.0 / remaining as f64;
            let next = *second;
            self.set_chain_ratios(next, remaining - 1);
        }
    }

    /// Get all pane IDs in the tree (in-order traversal).
    pub fn panes(&self) -> Vec<PaneId> {
        let mut result = Vec::new();
        if let Some(root) = self.root {
            self.collect_panes(root, &mut result);
        }
        result
    }

    fn collect_panes(&self, node_id: NodeId, out: &mut Vec<PaneId>) {
        match self.nodes.get(&node_id) {
            Some(Node::Leaf { pane_id }) => out.push(*pane_id),
            Some(Node::Split { first, second, .. }) => {
                self.collect_panes(*first, out);
                self.collect_panes(*second, out);
            }
            None => {}
        }
    }

    /// Split the node containing `pane_id` in the given orientation.
    /// `new_pane_id` is the ID of the new pane to create in the split.
    /// Split the node containing `pane_id`. Returns the new split node's ID.
    pub fn split(
        &mut self,
        pane_id: PaneId,
        new_pane_id: PaneId,
        orientation: Orientation,
    ) -> Option<NodeId> {
        let Some(root) = self.root else {
            return None;
        };
        let Some(parent_and_pos) = self.find_parent(root, pane_id) else {
            // pane_id is the root
            if let Some(Node::Leaf { pane_id: pid }) = self.nodes.get(&root) {
                if *pid != pane_id {
                    return None;
                }
                let new_leaf = self.alloc_id();
                self.nodes.insert(new_leaf, Node::Leaf { pane_id: new_pane_id });
                let old_root = root;
                let split_id = self.alloc_id();
                self.nodes.insert(
                    split_id,
                    Node::Split {
                        orientation,
                        ratio: 0.5,
                        first: old_root,
                        second: new_leaf,
                    },
                );
                self.root = Some(split_id);
                self.focused = Some(new_pane_id);
                return Some(split_id);
            }
            return None;
        };

        let (parent_id, is_first) = parent_and_pos;
        let target_node_id = if is_first {
            match self.nodes.get(&parent_id) {
                Some(Node::Split { first, .. }) => *first,
                _ => return None,
            }
        } else {
            match self.nodes.get(&parent_id) {
                Some(Node::Split { second, .. }) => *second,
                _ => return None,
            }
        };

        let new_leaf = self.alloc_id();
        self.nodes.insert(new_leaf, Node::Leaf { pane_id: new_pane_id });

        let new_split = self.alloc_id();
        self.nodes.insert(
            new_split,
            Node::Split {
                orientation,
                ratio: 0.5,
                first: target_node_id,
                second: new_leaf,
            },
        );

        // Replace the child pointer in the parent
        if let Some(Node::Split { first, second, .. }) = self.nodes.get_mut(&parent_id) {
            if is_first {
                *first = new_split;
            } else {
                *second = new_split;
            }
        }

        self.focused = Some(new_pane_id);
        Some(new_split)
    }

    /// Split a pane, placing the new pane *before* (as first child) instead of after.
    pub fn split_before(
        &mut self,
        pane_id: PaneId,
        new_pane_id: PaneId,
        orientation: Orientation,
    ) -> Option<NodeId> {
        let Some(root) = self.root else {
            return None;
        };
        let Some(parent_and_pos) = self.find_parent(root, pane_id) else {
            // pane_id is the root
            if let Some(Node::Leaf { pane_id: pid }) = self.nodes.get(&root) {
                if *pid != pane_id {
                    return None;
                }
                let new_leaf = self.alloc_id();
                self.nodes.insert(new_leaf, Node::Leaf { pane_id: new_pane_id });
                let old_root = root;
                let split_id = self.alloc_id();
                self.nodes.insert(
                    split_id,
                    Node::Split {
                        orientation,
                        ratio: 0.5,
                        first: new_leaf,
                        second: old_root,
                    },
                );
                self.root = Some(split_id);
                self.focused = Some(new_pane_id);
                return Some(split_id);
            }
            return None;
        };

        let (parent_id, is_first) = parent_and_pos;
        let target_node_id = if is_first {
            match self.nodes.get(&parent_id) {
                Some(Node::Split { first, .. }) => *first,
                _ => return None,
            }
        } else {
            match self.nodes.get(&parent_id) {
                Some(Node::Split { second, .. }) => *second,
                _ => return None,
            }
        };

        let new_leaf = self.alloc_id();
        self.nodes.insert(new_leaf, Node::Leaf { pane_id: new_pane_id });

        let new_split = self.alloc_id();
        self.nodes.insert(
            new_split,
            Node::Split {
                orientation,
                ratio: 0.5,
                first: new_leaf,
                second: target_node_id,
            },
        );

        if let Some(Node::Split { first, second, .. }) = self.nodes.get_mut(&parent_id) {
            if is_first {
                *first = new_split;
            } else {
                *second = new_split;
            }
        }

        self.focused = Some(new_pane_id);
        Some(new_split)
    }

    /// Remove a pane from the tree. Returns true if removed.
    pub fn remove(&mut self, pane_id: PaneId) -> bool {
        let Some(root) = self.root else {
            return false;
        };

        // If root is the target leaf, clear the tree
        if let Some(Node::Leaf { pane_id: pid }) = self.nodes.get(&root) {
            if *pid == pane_id {
                self.nodes.remove(&root);
                self.root = None;
                self.focused = None;
                return true;
            }
        }

        // Find the split that contains this pane as a direct child
        if let Some((parent_id, is_first)) = self.find_parent(root, pane_id) {
            let sibling_id = if is_first {
                match self.nodes.get(&parent_id) {
                    Some(Node::Split { second, .. }) => *second,
                    _ => return false,
                }
            } else {
                match self.nodes.get(&parent_id) {
                    Some(Node::Split { first, .. }) => *first,
                    _ => return false,
                }
            };

            let target_id = if is_first {
                match self.nodes.get(&parent_id) {
                    Some(Node::Split { first, .. }) => *first,
                    _ => return false,
                }
            } else {
                match self.nodes.get(&parent_id) {
                    Some(Node::Split { second, .. }) => *second,
                    _ => return false,
                }
            };

            // Remove the target leaf and the parent split
            self.nodes.remove(&target_id);

            // Replace parent with sibling
            if self.root == Some(parent_id) {
                self.root = Some(sibling_id);
                self.nodes.remove(&parent_id);
            } else if let Some((grandparent_id, gp_is_first)) =
                self.find_parent_of_node(root, parent_id)
            {
                if let Some(Node::Split { first, second, .. }) =
                    self.nodes.get_mut(&grandparent_id)
                {
                    if gp_is_first {
                        *first = sibling_id;
                    } else {
                        *second = sibling_id;
                    }
                }
                self.nodes.remove(&parent_id);
            }

            // Update focus to a remaining pane
            if self.focused == Some(pane_id) {
                self.focused = self.panes().first().copied();
            }

            return true;
        }

        false
    }

    /// Navigate to the next/previous pane relative to the focused one.
    pub fn navigate(&mut self, forward: bool) -> Option<PaneId> {
        let panes = self.panes();
        let focused = self.focused?;
        let idx = panes.iter().position(|p| *p == focused)?;
        let new_idx = if forward {
            (idx + 1) % panes.len()
        } else {
            (idx + panes.len() - 1) % panes.len()
        };
        let new_focus = panes[new_idx];
        self.focused = Some(new_focus);
        Some(new_focus)
    }

    /// Find the NodeId of the leaf containing the given pane_id.
    pub fn node_id_for_pane(&self, pane_id: PaneId) -> Option<NodeId> {
        self.root.and_then(|root| self.find_leaf(root, pane_id))
    }

    fn find_leaf(&self, node_id: NodeId, pane_id: PaneId) -> Option<NodeId> {
        match self.nodes.get(&node_id)? {
            Node::Leaf { pane_id: pid } => {
                if *pid == pane_id { Some(node_id) } else { None }
            }
            Node::Split { first, second, .. } => {
                self.find_leaf(*first, pane_id)
                    .or_else(|| self.find_leaf(*second, pane_id))
            }
        }
    }

    /// Find the parent split of a leaf with the given pane_id.
    /// Returns (parent_node_id, is_first_child).
    fn find_parent(&self, node_id: NodeId, pane_id: PaneId) -> Option<(NodeId, bool)> {
        match self.nodes.get(&node_id)? {
            Node::Leaf { .. } => None,
            Node::Split { first, second, .. } => {
                if let Some(Node::Leaf { pane_id: pid }) = self.nodes.get(first) {
                    if *pid == pane_id {
                        return Some((node_id, true));
                    }
                }
                if let Some(Node::Leaf { pane_id: pid }) = self.nodes.get(second) {
                    if *pid == pane_id {
                        return Some((node_id, false));
                    }
                }
                self.find_parent(*first, pane_id)
                    .or_else(|| self.find_parent(*second, pane_id))
            }
        }
    }

    /// Find the parent of a specific node (by NodeId).
    fn find_parent_of_node(&self, from: NodeId, target: NodeId) -> Option<(NodeId, bool)> {
        match self.nodes.get(&from)? {
            Node::Leaf { .. } => None,
            Node::Split { first, second, .. } => {
                if *first == target {
                    return Some((from, true));
                }
                if *second == target {
                    return Some((from, false));
                }
                self.find_parent_of_node(*first, target)
                    .or_else(|| self.find_parent_of_node(*second, target))
            }
        }
    }

    /// Compute normalized bounding rectangles for all panes.
    fn pane_bounds(&self) -> Vec<(PaneId, Rect)> {
        let mut result = Vec::new();
        if let Some(root) = self.root {
            let full = Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };
            self.compute_bounds(root, full, &mut result);
        }
        result
    }

    fn compute_bounds(&self, node_id: NodeId, rect: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self.nodes.get(&node_id) {
            Some(Node::Leaf { pane_id }) => {
                out.push((*pane_id, rect));
            }
            Some(Node::Split { orientation, ratio, first, second }) => {
                let (r1, r2) = match orientation {
                    Orientation::Horizontal => {
                        let w1 = rect.w * ratio;
                        (
                            Rect { x: rect.x, y: rect.y, w: w1, h: rect.h },
                            Rect { x: rect.x + w1, y: rect.y, w: rect.w - w1, h: rect.h },
                        )
                    }
                    Orientation::Vertical => {
                        let h1 = rect.h * ratio;
                        (
                            Rect { x: rect.x, y: rect.y, w: rect.w, h: h1 },
                            Rect { x: rect.x, y: rect.y + h1, w: rect.w, h: rect.h - h1 },
                        )
                    }
                };
                self.compute_bounds(*first, r1, out);
                self.compute_bounds(*second, r2, out);
            }
            None => {}
        }
    }

    /// Navigate to the pane in the given direction from the focused pane.
    /// Returns the new focused PaneId if navigation succeeded.
    pub fn navigate_directional(&mut self, direction: Direction) -> Option<PaneId> {
        let focused = self.focused?;
        let bounds = self.pane_bounds();

        let focused_rect = bounds.iter()
            .find(|(pid, _)| *pid == focused)
            .map(|(_, r)| *r)?;

        // Center of focused pane
        let cx = focused_rect.x + focused_rect.w / 2.0;
        let cy = focused_rect.y + focused_rect.h / 2.0;

        let mut best: Option<(PaneId, f64)> = None;
        let eps = 0.001;

        for &(pid, rect) in &bounds {
            if pid == focused { continue; }

            let candidate_cx = rect.x + rect.w / 2.0;
            let candidate_cy = rect.y + rect.h / 2.0;

            let valid = match direction {
                Direction::Left => rect.x + rect.w <= focused_rect.x + eps,
                Direction::Right => rect.x >= focused_rect.x + focused_rect.w - eps,
                Direction::Up => rect.y + rect.h <= focused_rect.y + eps,
                Direction::Down => rect.y >= focused_rect.y + focused_rect.h - eps,
            };

            if !valid { continue; }

            // Distance: primary axis distance + secondary axis penalty
            let dist = match direction {
                Direction::Left | Direction::Right => {
                    (candidate_cx - cx).abs() + (candidate_cy - cy).abs() * 0.1
                }
                Direction::Up | Direction::Down => {
                    (candidate_cy - cy).abs() + (candidate_cx - cx).abs() * 0.1
                }
            };

            if best.map_or(true, |(_, d)| dist < d) {
                best = Some((pid, dist));
            }
        }

        if let Some((pid, _)) = best {
            self.focused = Some(pid);
            Some(pid)
        } else {
            None
        }
    }

    /// Set all split ratios to 0.5 (equal distribution).
    pub fn equalize(&mut self) {
        let node_ids: Vec<NodeId> = self.nodes.keys().copied().collect();
        for nid in node_ids {
            if let Some(Node::Split { ratio, .. }) = self.nodes.get_mut(&nid) {
                *ratio = 0.5;
            }
        }
    }

    /// Move a pane to be adjacent to another pane in the tree.
    /// Removes the source pane, then inserts it before or after the target.
    /// Returns true if successful.
    pub fn move_pane_adjacent(
        &mut self,
        source_pane: PaneId,
        target_pane: PaneId,
        before: bool,
        orientation: Orientation,
    ) -> bool {
        // Can't move to itself
        if source_pane == target_pane {
            return false;
        }

        // Both must exist in the tree
        if self.node_id_for_pane(source_pane).is_none()
            || self.node_id_for_pane(target_pane).is_none()
        {
            return false;
        }

        // Remove source pane from tree
        if !self.remove(source_pane) {
            return false;
        }

        // Now insert source adjacent to target
        let target_node = match self.node_id_for_pane(target_pane) {
            Some(id) => id,
            None => return false,
        };

        if before {
            // Insert before target: create Split(source, target) replacing target
            let source_leaf = self.alloc_id();
            self.nodes.insert(source_leaf, Node::Leaf { pane_id: source_pane });

            let new_split = self.alloc_id();
            self.nodes.insert(new_split, Node::Split {
                orientation,
                ratio: 0.5,
                first: source_leaf,
                second: target_node,
            });

            // Replace target_node with new_split in parent
            if self.root == Some(target_node) {
                self.root = Some(new_split);
            } else if let Some(root) = self.root {
                self.replace_child(root, target_node, new_split);
            }
        } else {
            // Insert after target: create Split(target, source) replacing target
            let source_leaf = self.alloc_id();
            self.nodes.insert(source_leaf, Node::Leaf { pane_id: source_pane });

            let new_split = self.alloc_id();
            self.nodes.insert(new_split, Node::Split {
                orientation,
                ratio: 0.5,
                first: target_node,
                second: source_leaf,
            });

            if self.root == Some(target_node) {
                self.root = Some(new_split);
            } else if let Some(root) = self.root {
                self.replace_child(root, target_node, new_split);
            }
        }

        self.focused = Some(source_pane);
        self.equalize_chain();
        true
    }
}

/// Remap NodeId references inside a Node using the given ID mapping.
fn remap_node(node: &Node, remap: &HashMap<NodeId, NodeId>) -> Node {
    match node {
        Node::Leaf { pane_id } => Node::Leaf { pane_id: *pane_id },
        Node::Split { orientation, ratio, first, second } => Node::Split {
            orientation: *orientation,
            ratio: *ratio,
            first: remap.get(first).copied().unwrap_or(*first),
            second: remap.get(second).copied().unwrap_or(*second),
        },
    }
}
