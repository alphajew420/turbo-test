//! Native radio grouping keeps state on Tree handles, independently of wrappers.
use super::*;
use turbo_dom_parser::rtdom::tree::FormProp;

pub(super) fn checked(node: Handle) -> bool {
    with_tree(|tree| {
        tree.form_property(node, FormProp::Checked)
            .unwrap_or_else(|| tree.get_attribute(node, "checked").is_some())
    })
    .unwrap_or(false)
}
fn radio(tree: &Tree, node: Handle) -> bool {
    tree.namespace_id(node) == 0
        && tree.local_name(node) == Some("input")
        && tree
            .get_attribute(node, "type")
            .is_some_and(|value| value.eq_ignore_ascii_case("radio"))
}
fn descendants(root: Handle) -> Vec<Handle> {
    with_tree(|tree| {
        let mut result = Vec::new();
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            result.push(node);
            pending.extend(tree.children(node).into_iter().rev());
        }
        result
    })
    .unwrap_or_default()
}
pub(super) fn peers(node: Handle) -> Vec<Handle> {
    let Some((root, name)) = with_tree(|tree| {
        if !radio(tree, node) {
            return None;
        }
        let name = tree.get_attribute_utf16(node, "name")?;
        if name.is_empty() {
            return None;
        }
        let mut root = node;
        while let Some(parent) = tree.parent(root) {
            root = parent;
        }
        Some((root, name))
    })
    .flatten() else {
        return Vec::new();
    };
    let owner = closest_form(node);
    descendants(root)
        .into_iter()
        .filter(|peer| {
            *peer != node
                && with_tree(|tree| {
                    radio(tree, *peer)
                        && tree.get_attribute_utf16(*peer, "name").as_ref() == Some(&name)
                })
                .unwrap_or(false)
                && closest_form(*peer) == owner
        })
        .collect()
}
pub(super) fn group(node: Handle) {
    if !checked(node) {
        return;
    }
    let peers = peers(node)
        .into_iter()
        .filter(|peer| checked(*peer))
        .collect::<Vec<_>>();
    with_tree_mut(|tree| {
        for peer in peers {
            tree.set_form_property(peer, FormProp::Checked, false);
        }
    });
}
pub(super) fn set(node: Handle, value: bool) {
    with_tree_mut(|tree| tree.set_form_property(node, FormProp::Checked, value));
    if value {
        group(node);
    }
}
pub(super) fn attribute_changed(node: Handle, name: &str) {
    if matches!(name, "checked" | "name" | "type" | "form") {
        group(node);
    }
}
pub(super) fn inserted(root: Handle) {
    for node in descendants(root).into_iter().rev() {
        if with_tree(|tree| radio(tree, node)).unwrap_or(false) {
            group(node);
        }
    }
}
pub(super) fn snapshot(node: Handle) -> Vec<(Handle, bool)> {
    let mut rows = peers(node)
        .into_iter()
        .filter(|peer| checked(*peer))
        .collect::<Vec<_>>();
    rows.push(node);
    rows.into_iter().map(|node| (node, checked(node))).collect()
}
pub(super) fn restore(rows: Vec<(Handle, bool)>) {
    with_tree_mut(|tree| {
        for (node, state) in rows {
            tree.set_form_property(node, FormProp::Checked, state)
        }
    });
}
