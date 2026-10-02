// Copyright 2021 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file) or the MIT license (found in
// the LICENSE-MIT file), at your option.

// Derived from Chromium's accessibility abstraction.
// Copyright 2018 The Chromium Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE.chromium file.

use core::iter::FusedIterator;

use accesskit::NodeId as LocalNodeId;

use crate::{
    filters::FilterResult,
    node::{Node, NodeId},
    tree::State as TreeState,
};

/// Iterator over child NodeIds, handling both normal nodes and graft nodes.
pub enum ChildIds<'a> {
    Normal {
        parent_id: NodeId,
        children: core::slice::Iter<'a, LocalNodeId>,
    },
    Graft(Option<NodeId>),
}

impl Iterator for ChildIds<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Normal {
                parent_id,
                children,
            } => children
                .next()
                .map(|child| parent_id.with_same_tree(*child)),
            Self::Graft(id) => id.take(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl DoubleEndedIterator for ChildIds<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::Normal {
                parent_id,
                children,
            } => children
                .next_back()
                .map(|child| parent_id.with_same_tree(*child)),
            Self::Graft(id) => id.take(),
        }
    }
}

impl ExactSizeIterator for ChildIds<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Normal { children, .. } => children.len(),
            Self::Graft(id) => usize::from(id.is_some()),
        }
    }
}

impl FusedIterator for ChildIds<'_> {}

fn filtered_sibling<'a, const REVERSE: bool>(
    mut current: Node<'a>,
    filter: &impl Fn(&Node) -> FilterResult,
) -> Option<Node<'a>> {
    let mut consider_children = false;
    loop {
        let candidate = if consider_children {
            if REVERSE {
                current.children().next_back()
            } else {
                current.children().next()
            }
        } else {
            None
        }
        .or_else(|| {
            if REVERSE {
                current.preceding_siblings().next()
            } else {
                current.following_siblings().next()
            }
        });

        if let Some(candidate) = candidate {
            current = candidate;
            match filter(&current) {
                FilterResult::Include => return Some(current),
                FilterResult::ExcludeNode => consider_children = true,
                FilterResult::ExcludeSubtree => consider_children = false,
            }
        } else {
            current = current.parent()?;
            if filter(&current) != FilterResult::ExcludeNode {
                return None;
            }
            consider_children = false;
        }
    }
}

/// A double-ended range of visible siblings, including both endpoints.
pub(crate) struct FilteredNodes<'a, Filter: Fn(&Node) -> FilterResult> {
    filter: Filter,
    remaining: Option<(Node<'a>, Node<'a>)>,
}

impl<'a, Filter: Fn(&Node) -> FilterResult> FilteredNodes<'a, Filter> {
    pub(crate) fn children(parent: Node<'a>, filter: Filter) -> Self {
        let remaining = parent.filtered_child::<false>(&filter).and_then(|front| {
            parent
                .filtered_child::<true>(&filter)
                .map(|back| (front, back))
        });
        Self { filter, remaining }
    }

    pub(crate) fn siblings<const REVERSE: bool>(node: Node<'a>, filter: Filter) -> Self {
        let remaining = filtered_sibling::<REVERSE>(node, &filter).and_then(|near| {
            let parent = node.filtered_parent(&filter)?;
            if REVERSE {
                parent
                    .filtered_child::<false>(&filter)
                    .map(|far| (far, near))
            } else {
                parent
                    .filtered_child::<true>(&filter)
                    .map(|far| (near, far))
            }
        });
        Self { filter, remaining }
    }

    fn next_end<const REVERSE: bool>(&mut self) -> Option<Node<'a>> {
        let (front, back) = self.remaining.take()?;
        let (current, last) = if REVERSE {
            (back, front)
        } else {
            (front, back)
        };
        if current.id() != last.id() {
            self.remaining = filtered_sibling::<REVERSE>(current, &self.filter).map(|next| {
                if REVERSE {
                    (front, next)
                } else {
                    (next, back)
                }
            });
        }
        Some(current)
    }
}

impl<'a, Filter: Fn(&Node) -> FilterResult> Iterator for FilteredNodes<'a, Filter> {
    type Item = Node<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_end::<false>()
    }
}

impl<Filter: Fn(&Node) -> FilterResult> DoubleEndedIterator for FilteredNodes<'_, Filter> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.next_end::<true>()
    }
}

impl<Filter: Fn(&Node) -> FilterResult> FusedIterator for FilteredNodes<'_, Filter> {}

pub(crate) enum LabelledBy<'a, Filter: Fn(&Node) -> FilterResult> {
    FromDescendants(FilteredNodes<'a, Filter>),
    Explicit {
        ids: core::slice::Iter<'a, LocalNodeId>,
        tree_state: &'a TreeState,
        node_id: NodeId,
    },
}

impl<'a, Filter: Fn(&Node) -> FilterResult> Iterator for LabelledBy<'a, Filter> {
    type Item = Node<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::FromDescendants(iter) => iter.next(),
            Self::Explicit {
                ids,
                tree_state,
                node_id,
            } => ids
                .next()
                .map(|id| tree_state.node_by_id(node_id.with_same_tree(*id)).unwrap()),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::FromDescendants(iter) => iter.size_hint(),
            Self::Explicit { ids, .. } => ids.size_hint(),
        }
    }
}

impl<Filter: Fn(&Node) -> FilterResult> DoubleEndedIterator for LabelledBy<'_, Filter> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::FromDescendants(iter) => iter.next_back(),
            Self::Explicit {
                ids,
                tree_state,
                node_id,
            } => ids
                .next_back()
                .map(|id| tree_state.node_by_id(node_id.with_same_tree(*id)).unwrap()),
        }
    }
}

impl<Filter: Fn(&Node) -> FilterResult> FusedIterator for LabelledBy<'_, Filter> {}

#[cfg(test)]
mod tests {
    use crate::{
        filters::common_filter,
        tests::*,
        tree::{ChangeHandler, TreeIndex},
        NodeId,
    };
    use accesskit::{Node, NodeId as LocalNodeId, Role, Tree, TreeId, TreeUpdate, Uuid};
    use alloc::{vec, vec::Vec};

    #[test]
    fn filtered_siblings_skip_hidden_subtrees_after_empty_containers() {
        let mut root = Node::new(Role::Window);
        root.set_children([LocalNodeId(1), LocalNodeId(2), LocalNodeId(7)]);
        let mut transparent = Node::new(Role::GenericContainer);
        transparent.set_children([LocalNodeId(3), LocalNodeId(4), LocalNodeId(6)]);
        let mut hidden = Node::new(Role::Group);
        hidden.set_hidden();
        hidden.set_children([LocalNodeId(5)]);
        let tree = crate::Tree::new(
            TreeUpdate {
                nodes: vec![
                    (LocalNodeId(0), root),
                    (LocalNodeId(1), Node::new(Role::Button)),
                    (LocalNodeId(2), transparent),
                    (LocalNodeId(3), Node::new(Role::GenericContainer)),
                    (LocalNodeId(4), hidden),
                    (LocalNodeId(5), Node::new(Role::Button)),
                    (LocalNodeId(6), Node::new(Role::GenericContainer)),
                    (LocalNodeId(7), Node::new(Role::Button)),
                ],
                tree: Some(Tree::new(LocalNodeId(0))),
                tree_id: TreeId::ROOT,
                focus: LocalNodeId(0),
            },
            false,
        );
        let root = tree.state().root();
        let id = |node: crate::Node<'_>| node.id().to_components().0;
        assert_eq!(
            root.filtered_children(common_filter)
                .map(id)
                .collect::<Vec<_>>(),
            [LocalNodeId(1), LocalNodeId(7)]
        );
        assert_eq!(
            root.filtered_children(common_filter)
                .rev()
                .map(id)
                .collect::<Vec<_>>(),
            [LocalNodeId(7), LocalNodeId(1)]
        );
    }

    #[test]
    fn filtered_iterators_support_mixed_ends_and_stay_exhausted() {
        fn check<'a, I: DoubleEndedIterator<Item = crate::Node<'a>>>(
            make_iter: impl Fn() -> I,
            expected: &[LocalNodeId],
        ) {
            for mask in 0..(1 << expected.len()) {
                let mut iter = make_iter();
                let mut remaining = expected.iter();
                for step in 0..expected.len() {
                    let (actual, expected) = if mask & (1 << step) == 0 {
                        (iter.next(), remaining.next())
                    } else {
                        (iter.next_back(), remaining.next_back())
                    };
                    assert_eq!(
                        actual.map(|node| node.id().to_components().0),
                        expected.copied()
                    );
                }
                assert!(iter.next().is_none());
                assert!(iter.next_back().is_none());
                assert!(iter.next().is_none());
            }
        }

        let tree = test_tree();
        let root = tree.state().root();
        let first = tree.state().node_by_id(nid(PARAGRAPH_0_ID)).unwrap();
        let last = tree.state().node_by_id(nid(BUTTON_3_2_ID)).unwrap();
        let expected = [
            PARAGRAPH_0_ID,
            LABEL_1_1_ID,
            PARAGRAPH_2_ID,
            LABEL_3_1_0_ID,
            BUTTON_3_2_ID,
        ];
        check(|| root.filtered_children(test_tree_filter), &expected);
        check(
            || first.following_filtered_siblings(test_tree_filter),
            &expected[1..],
        );
        check(
            || last.preceding_filtered_siblings(test_tree_filter),
            &[LABEL_3_1_0_ID, PARAGRAPH_2_ID, LABEL_1_1_ID, PARAGRAPH_0_ID],
        );
        check(|| root.following_filtered_siblings(test_tree_filter), &[]);
        check(|| root.preceding_filtered_siblings(test_tree_filter), &[]);
        check(|| first.filtered_children(test_tree_filter), &[]);
    }

    #[test]
    fn empty_filtered_ranges_do_not_search_the_opposite_end() {
        use core::cell::Cell;

        let tree = test_tree();
        let root = tree.state().root();
        let calls = Cell::new(0);
        let mut children = root.filtered_children(|_| {
            calls.set(calls.get() + 1);
            crate::FilterResult::ExcludeSubtree
        });
        assert!(children.next().is_none());
        assert!(children.next_back().is_none());
        assert_eq!(calls.get(), root.child_ids().len());

        let include = |_: &crate::Node<'_>| {
            calls.set(calls.get() + 1);
            crate::FilterResult::Include
        };
        calls.set(0);
        let first = root.children().next().unwrap();
        assert!(first
            .preceding_filtered_siblings(include)
            .next_back()
            .is_none());
        assert_eq!(calls.get(), 1);
        calls.set(0);
        let last = root.children().next_back().unwrap();
        assert!(last.following_filtered_siblings(include).next().is_none());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn siblings_keep_exact_lengths_when_consumed_from_both_ends() {
        fn check(
            mut iter: impl DoubleEndedIterator<Item = NodeId> + ExactSizeIterator,
            expected: [LocalNodeId; 3],
        ) {
            assert_eq!(iter.len(), 3);
            assert_eq!(iter.next().unwrap().to_components().0, expected[0]);
            assert_eq!(iter.len(), 2);
            assert_eq!(iter.next_back().unwrap().to_components().0, expected[2]);
            assert_eq!(iter.len(), 1);
            assert_eq!(iter.next_back().unwrap().to_components().0, expected[1]);
            assert_eq!(iter.size_hint(), (0, Some(0)));
            assert!(iter.next().is_none());
            assert!(iter.next_back().is_none());
        }

        let tree = test_tree();
        let first = tree.state().node_by_id(nid(PARAGRAPH_0_ID)).unwrap();
        let last = tree
            .state()
            .node_by_id(nid(PARAGRAPH_3_IGNORED_ID))
            .unwrap();
        check(
            first.following_sibling_ids(),
            [
                PARAGRAPH_1_IGNORED_ID,
                PARAGRAPH_2_ID,
                PARAGRAPH_3_IGNORED_ID,
            ],
        );
        check(
            last.preceding_sibling_ids(),
            [PARAGRAPH_2_ID, PARAGRAPH_1_IGNORED_ID, PARAGRAPH_0_ID],
        );
    }

    #[test]
    fn following_siblings() {
        let tree = test_tree();
        assert!(tree.state().root().following_siblings().next().is_none());
        assert_eq!(0, tree.state().root().following_siblings().len());
        assert_eq!(
            [
                PARAGRAPH_1_IGNORED_ID,
                PARAGRAPH_2_ID,
                PARAGRAPH_3_IGNORED_ID
            ],
            tree.state()
                .node_by_id(nid(PARAGRAPH_0_ID))
                .unwrap()
                .following_sibling_ids()
                .map(|id| id.to_components().0)
                .collect::<Vec<LocalNodeId>>()[..]
        );
        assert_eq!(
            3,
            tree.state()
                .node_by_id(nid(PARAGRAPH_0_ID))
                .unwrap()
                .following_siblings()
                .len()
        );
        assert!(tree
            .state()
            .node_by_id(nid(PARAGRAPH_3_IGNORED_ID))
            .unwrap()
            .following_siblings()
            .next()
            .is_none());
        assert_eq!(
            0,
            tree.state()
                .node_by_id(nid(PARAGRAPH_3_IGNORED_ID))
                .unwrap()
                .following_siblings()
                .len()
        );
    }

    #[test]
    fn following_siblings_reversed() {
        let tree = test_tree();
        assert!(tree
            .state()
            .root()
            .following_siblings()
            .next_back()
            .is_none());
        assert_eq!(
            [
                PARAGRAPH_3_IGNORED_ID,
                PARAGRAPH_2_ID,
                PARAGRAPH_1_IGNORED_ID
            ],
            tree.state()
                .node_by_id(nid(PARAGRAPH_0_ID))
                .unwrap()
                .following_sibling_ids()
                .rev()
                .map(|id| id.to_components().0)
                .collect::<Vec<LocalNodeId>>()[..]
        );
        assert!(tree
            .state()
            .node_by_id(nid(PARAGRAPH_3_IGNORED_ID))
            .unwrap()
            .following_siblings()
            .next_back()
            .is_none());
    }

    #[test]
    fn preceding_siblings() {
        let tree = test_tree();
        assert!(tree.state().root().preceding_siblings().next().is_none());
        assert_eq!(0, tree.state().root().preceding_siblings().len());
        assert_eq!(
            [PARAGRAPH_2_ID, PARAGRAPH_1_IGNORED_ID, PARAGRAPH_0_ID],
            tree.state()
                .node_by_id(nid(PARAGRAPH_3_IGNORED_ID))
                .unwrap()
                .preceding_sibling_ids()
                .map(|id| id.to_components().0)
                .collect::<Vec<LocalNodeId>>()[..]
        );
        assert_eq!(
            3,
            tree.state()
                .node_by_id(nid(PARAGRAPH_3_IGNORED_ID))
                .unwrap()
                .preceding_siblings()
                .len()
        );
        assert!(tree
            .state()
            .node_by_id(nid(PARAGRAPH_0_ID))
            .unwrap()
            .preceding_siblings()
            .next()
            .is_none());
        assert_eq!(
            0,
            tree.state()
                .node_by_id(nid(PARAGRAPH_0_ID))
                .unwrap()
                .preceding_siblings()
                .len()
        );
    }

    #[test]
    fn preceding_siblings_reversed() {
        let tree = test_tree();
        assert!(tree
            .state()
            .root()
            .preceding_siblings()
            .next_back()
            .is_none());
        assert_eq!(
            [PARAGRAPH_0_ID, PARAGRAPH_1_IGNORED_ID, PARAGRAPH_2_ID],
            tree.state()
                .node_by_id(nid(PARAGRAPH_3_IGNORED_ID))
                .unwrap()
                .preceding_sibling_ids()
                .rev()
                .map(|id| id.to_components().0)
                .collect::<Vec<LocalNodeId>>()[..]
        );
        assert!(tree
            .state()
            .node_by_id(nid(PARAGRAPH_0_ID))
            .unwrap()
            .preceding_siblings()
            .next_back()
            .is_none());
    }

    #[test]
    fn following_filtered_siblings() {
        let tree = test_tree();
        assert!(tree
            .state()
            .root()
            .following_filtered_siblings(test_tree_filter)
            .next()
            .is_none());
        assert_eq!(
            [LABEL_1_1_ID, PARAGRAPH_2_ID, LABEL_3_1_0_ID, BUTTON_3_2_ID],
            tree.state()
                .node_by_id(nid(PARAGRAPH_0_ID))
                .unwrap()
                .following_filtered_siblings(test_tree_filter)
                .map(|node| node.id().to_components().0)
                .collect::<Vec<LocalNodeId>>()[..]
        );
        assert_eq!(
            [BUTTON_3_2_ID],
            tree.state()
                .node_by_id(nid(LABEL_3_1_0_ID))
                .unwrap()
                .following_filtered_siblings(test_tree_filter)
                .map(|node| node.id().to_components().0)
                .collect::<Vec<LocalNodeId>>()[..]
        );
        assert!(tree
            .state()
            .node_by_id(nid(PARAGRAPH_3_IGNORED_ID))
            .unwrap()
            .following_filtered_siblings(test_tree_filter)
            .next()
            .is_none());
    }

    #[test]
    fn following_filtered_siblings_reversed() {
        let tree = test_tree();
        assert!(tree
            .state()
            .root()
            .following_filtered_siblings(test_tree_filter)
            .next_back()
            .is_none());
        assert_eq!(
            [BUTTON_3_2_ID, LABEL_3_1_0_ID, PARAGRAPH_2_ID, LABEL_1_1_ID],
            tree.state()
                .node_by_id(nid(PARAGRAPH_0_ID))
                .unwrap()
                .following_filtered_siblings(test_tree_filter)
                .rev()
                .map(|node| node.id().to_components().0)
                .collect::<Vec<LocalNodeId>>()[..]
        );
        assert_eq!(
            [BUTTON_3_2_ID,],
            tree.state()
                .node_by_id(nid(LABEL_3_1_0_ID))
                .unwrap()
                .following_filtered_siblings(test_tree_filter)
                .rev()
                .map(|node| node.id().to_components().0)
                .collect::<Vec<LocalNodeId>>()[..]
        );
        assert!(tree
            .state()
            .node_by_id(nid(PARAGRAPH_3_IGNORED_ID))
            .unwrap()
            .following_filtered_siblings(test_tree_filter)
            .next_back()
            .is_none());
    }

    #[test]
    fn preceding_filtered_siblings() {
        let tree = test_tree();
        assert!(tree
            .state()
            .root()
            .preceding_filtered_siblings(test_tree_filter)
            .next()
            .is_none());
        assert_eq!(
            [PARAGRAPH_2_ID, LABEL_1_1_ID, PARAGRAPH_0_ID],
            tree.state()
                .node_by_id(nid(PARAGRAPH_3_IGNORED_ID))
                .unwrap()
                .preceding_filtered_siblings(test_tree_filter)
                .map(|node| node.id().to_components().0)
                .collect::<Vec<LocalNodeId>>()[..]
        );
        assert_eq!(
            [PARAGRAPH_2_ID, LABEL_1_1_ID, PARAGRAPH_0_ID],
            tree.state()
                .node_by_id(nid(LABEL_3_1_0_ID))
                .unwrap()
                .preceding_filtered_siblings(test_tree_filter)
                .map(|node| node.id().to_components().0)
                .collect::<Vec<LocalNodeId>>()[..]
        );
        assert!(tree
            .state()
            .node_by_id(nid(PARAGRAPH_0_ID))
            .unwrap()
            .preceding_filtered_siblings(test_tree_filter)
            .next()
            .is_none());
    }

    #[test]
    fn preceding_filtered_siblings_reversed() {
        let tree = test_tree();
        assert!(tree
            .state()
            .root()
            .preceding_filtered_siblings(test_tree_filter)
            .next_back()
            .is_none());
        assert_eq!(
            [PARAGRAPH_0_ID, LABEL_1_1_ID, PARAGRAPH_2_ID],
            tree.state()
                .node_by_id(nid(PARAGRAPH_3_IGNORED_ID))
                .unwrap()
                .preceding_filtered_siblings(test_tree_filter)
                .rev()
                .map(|node| node.id().to_components().0)
                .collect::<Vec<LocalNodeId>>()[..]
        );
        assert_eq!(
            [PARAGRAPH_0_ID, LABEL_1_1_ID, PARAGRAPH_2_ID],
            tree.state()
                .node_by_id(nid(LABEL_3_1_0_ID))
                .unwrap()
                .preceding_filtered_siblings(test_tree_filter)
                .rev()
                .map(|node| node.id().to_components().0)
                .collect::<Vec<LocalNodeId>>()[..]
        );
        assert!(tree
            .state()
            .node_by_id(nid(PARAGRAPH_0_ID))
            .unwrap()
            .preceding_filtered_siblings(test_tree_filter)
            .next_back()
            .is_none());
    }

    #[test]
    fn filtered_children() {
        let tree = test_tree();
        assert_eq!(
            [
                PARAGRAPH_0_ID,
                LABEL_1_1_ID,
                PARAGRAPH_2_ID,
                LABEL_3_1_0_ID,
                BUTTON_3_2_ID
            ],
            tree.state()
                .root()
                .filtered_children(test_tree_filter)
                .map(|node| node.id().to_components().0)
                .collect::<Vec<LocalNodeId>>()[..]
        );
        assert!(tree
            .state()
            .node_by_id(nid(PARAGRAPH_0_ID))
            .unwrap()
            .filtered_children(test_tree_filter)
            .next()
            .is_none());
        assert!(tree
            .state()
            .node_by_id(nid(LABEL_0_0_IGNORED_ID))
            .unwrap()
            .filtered_children(test_tree_filter)
            .next()
            .is_none());
    }

    #[test]
    fn filtered_children_reversed() {
        let tree = test_tree();
        assert_eq!(
            [
                BUTTON_3_2_ID,
                LABEL_3_1_0_ID,
                PARAGRAPH_2_ID,
                LABEL_1_1_ID,
                PARAGRAPH_0_ID
            ],
            tree.state()
                .root()
                .filtered_children(test_tree_filter)
                .rev()
                .map(|node| node.id().to_components().0)
                .collect::<Vec<LocalNodeId>>()[..]
        );
        assert!(tree
            .state()
            .node_by_id(nid(PARAGRAPH_0_ID))
            .unwrap()
            .filtered_children(test_tree_filter)
            .next_back()
            .is_none());
        assert!(tree
            .state()
            .node_by_id(nid(LABEL_0_0_IGNORED_ID))
            .unwrap()
            .filtered_children(test_tree_filter)
            .next_back()
            .is_none());
    }

    #[test]
    fn graft_node_without_subtree_has_no_filtered_children() {
        let subtree_id = TreeId(Uuid::from_u128(1));

        let update = TreeUpdate {
            nodes: vec![
                (LocalNodeId(0), {
                    let mut node = Node::new(Role::Window);
                    node.set_children(vec![LocalNodeId(1)]);
                    node
                }),
                (LocalNodeId(1), {
                    let mut node = Node::new(Role::GenericContainer);
                    node.set_tree_id(subtree_id);
                    node
                }),
            ],
            tree: Some(Tree::new(LocalNodeId(0))),
            tree_id: TreeId::ROOT,
            focus: LocalNodeId(0),
        };
        let tree = crate::Tree::new(update, false);

        let graft_node_id = NodeId::new(LocalNodeId(1), TreeIndex(0));
        let graft_node = tree.state().node_by_id(graft_node_id).unwrap();
        assert!(graft_node.filtered_children(common_filter).next().is_none());
    }

    #[test]
    fn filtered_children_crosses_subtree_boundary() {
        struct NoOpHandler;
        impl ChangeHandler for NoOpHandler {
            fn node_added(&mut self, _: &crate::Node) {}
            fn node_updated(&mut self, _: &crate::Node, _: &crate::Node) {}
            fn focus_moved(&mut self, _: Option<&crate::Node>, _: Option<&crate::Node>) {}
            fn node_removed(&mut self, _: &crate::Node) {}
        }

        let subtree_id = TreeId(Uuid::from_u128(1));

        let update = TreeUpdate {
            nodes: vec![
                (LocalNodeId(0), {
                    let mut node = Node::new(Role::Window);
                    node.set_children(vec![LocalNodeId(1)]);
                    node
                }),
                (LocalNodeId(1), {
                    let mut node = Node::new(Role::GenericContainer);
                    node.set_tree_id(subtree_id);
                    node
                }),
            ],
            tree: Some(Tree::new(LocalNodeId(0))),
            tree_id: TreeId::ROOT,
            focus: LocalNodeId(0),
        };
        let mut tree = crate::Tree::new(update, false);

        let subtree_update = TreeUpdate {
            nodes: vec![
                (LocalNodeId(0), {
                    let mut node = Node::new(Role::Document);
                    node.set_children(vec![LocalNodeId(1)]);
                    node
                }),
                (LocalNodeId(1), Node::new(Role::Button)),
            ],
            tree: Some(Tree::new(LocalNodeId(0))),
            tree_id: subtree_id,
            focus: LocalNodeId(0),
        };
        tree.update_and_process_changes(subtree_update, &mut NoOpHandler);

        let root = tree.state().root();
        let filtered_children: Vec<_> = root.filtered_children(common_filter).collect();

        assert_eq!(1, filtered_children.len());
        let subtree_root_id = NodeId::new(LocalNodeId(0), TreeIndex(1));
        assert_eq!(subtree_root_id, filtered_children[0].id());

        let document = &filtered_children[0];
        assert_eq!(document.following_sibling_ids().len(), 0);
        assert_eq!(document.preceding_sibling_ids().len(), 0);
        let doc_children: Vec<_> = document.filtered_children(common_filter).collect();
        assert_eq!(1, doc_children.len());
        let button_id = NodeId::new(LocalNodeId(1), TreeIndex(1));
        assert_eq!(button_id, doc_children[0].id());
    }
}
