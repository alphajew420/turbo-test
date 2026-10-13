use super::*;
use turbo_dom_parser::rtdom::tree::AttributeData;

/// A nonce is independent of its reflected attribute. Read only native ownership/state.
pub fn nonce(scope: &mut v8::PinScope, node: v8::Local<v8::Object>) -> Option<String> {
    let _dom = DomScope::for_object(scope, node);
    let handle = handle_of(scope, node)?;
    if with_tree(|tree| tree.node_type_id(handle)) != Some(1) {
        return None;
    }
    DOM.with(|dom| dom.borrow().as_ref()?.nonces.get(&handle).cloned())
        .or_else(|| {
            with_tree(|tree| tree.get_attribute(handle, "nonce").map(str::to_owned)).flatten()
        })
        .or_else(|| Some(String::new()))
}

pub fn clone_nonces(source: Handle, target: Handle, deep: bool) {
    fn pairs(
        tree: &Tree,
        source: Handle,
        target: Handle,
        deep: bool,
        out: &mut Vec<(Handle, Handle)>,
    ) {
        out.push((source, target));
        if deep {
            for (source, target) in tree.children(source).into_iter().zip(tree.children(target)) {
                pairs(tree, source, target, true, out);
            }
        }
    }
    let mapping = with_tree(|tree| {
        let mut mapping = Vec::new();
        pairs(tree, source, target, deep, &mut mapping);
        mapping
    })
    .unwrap_or_default();
    DOM.with(|dom| {
        let mut dom = dom.borrow_mut();
        let state = dom.as_mut().unwrap();
        for (source, target) in mapping {
            if let Some(value) = state.nonces.get(&source).cloned() {
                state.nonces.insert(target, value);
            }
        }
    });
}

pub fn copy_nonces(
    scope: &mut v8::PinScope,
    source: v8::Local<v8::Object>,
    target: v8::Local<v8::Object>,
    deep: bool,
) {
    fn flatten(tree: &Tree, node: Handle, deep: bool, out: &mut Vec<Handle>) {
        out.push(node);
        if deep {
            for node in tree.children(node) {
                flatten(tree, node, true, out);
            }
        }
    }
    let values = {
        let _source = DomScope::for_object(scope, source);
        let Some(handle) = handle_of(scope, source) else {
            return;
        };
        let mut nodes = Vec::new();
        with_tree(|tree| flatten(tree, handle, deep, &mut nodes));
        DOM.with(|dom| {
            let dom = dom.borrow();
            let state = dom.as_ref().unwrap();
            nodes
                .into_iter()
                .map(|handle| state.nonces.get(&handle).cloned())
                .collect::<Vec<_>>()
        })
    };
    let _target = DomScope::for_object(scope, target);
    let Some(handle) = handle_of(scope, target) else {
        return;
    };
    let mut nodes = Vec::new();
    with_tree(|tree| flatten(tree, handle, deep, &mut nodes));
    DOM.with(|dom| {
        let mut dom = dom.borrow_mut();
        let state = dom.as_mut().unwrap();
        for (node, value) in nodes.into_iter().zip(values) {
            if let Some(value) = value {
                state.nonces.insert(node, value);
            }
        }
    });
}

struct Snapshot {
    old: Handle,
    kind: u8,
    name: String,
    namespace: Option<String>,
    value: String,
    doctype: Option<(String, String)>,
    attributes: Vec<(Handle, AttributeData)>,
    children: Vec<Snapshot>,
    shadow: Option<Box<Snapshot>>,
}
fn capture(tree: &mut Tree, node: Handle) -> Result<Snapshot, &'static str> {
    let kind = tree.node_type_id(node);
    if !matches!(kind, 1 | 2 | 3 | 7 | 8 | 10 | 11) {
        return Err("Adopting this native node kind is unsupported");
    }
    if kind == 1 && tree.local_name(node) == Some("template") {
        return Err("Adopting template content is unsupported");
    }
    let attributes = tree
        .attributes_namespaced(node)
        .into_iter()
        .map(|attribute| {
            let handle = tree
                .attribute_node(node, attribute.namespace.as_deref(), &attribute.local_name)
                .unwrap();
            (handle, attribute)
        })
        .collect();
    let attr = tree.attribute_data(node).cloned();
    let doctype = (kind == 10).then(|| {
        (
            tree.doctype_public_id(node).unwrap_or_default().to_owned(),
            tree.doctype_system_id(node).unwrap_or_default().to_owned(),
        )
    });
    let name = if kind == 7 {
        tree.processing_instruction_target(node)
            .unwrap_or_default()
            .to_owned()
    } else if kind == 10 {
        tree.doctype_name(node).unwrap_or_default().to_owned()
    } else {
        attr.as_ref()
            .map(|attr| attr.qualified_name.to_string())
            .unwrap_or_else(|| tree.qualified_name(node).unwrap_or_default().to_owned())
    };
    let namespace = attr
        .as_ref()
        .and_then(|attr| attr.namespace.as_ref().map(ToString::to_string))
        .or_else(|| tree.namespace_uri(node).map(str::to_owned));
    let value = attr
        .as_ref()
        .map(|attr| attr.value.to_string())
        .unwrap_or_else(|| tree.node_value(node).unwrap_or_default());
    let children = tree
        .children(node)
        .into_iter()
        .map(|node| capture(tree, node))
        .collect::<Result<_, _>>()?;
    let shadow = tree
        .shadow_root(node)
        .map(|node| capture(tree, node).map(Box::new))
        .transpose()?;
    Ok(Snapshot {
        old: node,
        kind,
        name,
        namespace,
        value,
        doctype,
        attributes,
        children,
        shadow,
    })
}

fn materialize(
    tree: &mut Tree,
    source: Snapshot,
    supplied: Option<Handle>,
    mapping: &mut Vec<(Handle, Handle)>,
) -> Handle {
    let target = supplied.unwrap_or_else(|| match source.kind {
        2 => tree.create_attribute(source.namespace.as_deref(), &source.name, &source.value),
        3 => tree.create_text_node(&source.value),
        7 => tree.create_processing_instruction(&source.name, &source.value),
        8 => tree.create_comment(&source.value),
        10 => {
            let (public_id, system_id) = source.doctype.as_ref().unwrap();
            tree.create_document_type(&source.name, public_id, system_id)
        }
        11 => tree.create_document_fragment(),
        _ => tree.create_element_namespace(source.namespace.as_deref(), &source.name),
    });
    mapping.push((source.old, target));
    for (old, attribute) in source.attributes {
        let node = tree.create_attribute(
            attribute.namespace.as_deref(),
            &attribute.qualified_name,
            &attribute.value,
        );
        tree.set_attribute_node(target, node)
            .expect("validated element attribute");
        mapping.push((old, node));
    }
    for child in source.children {
        let child = materialize(tree, child, None, mapping);
        tree.append_child(target, child);
    }
    if let Some(shadow) = source.shadow {
        let root = tree.attach_shadow(target);
        materialize(tree, *shadow, Some(root), mapping);
    }
    target
}

// Every already-visible node remains the SAME V8 object. Uncached handles are
// reconstructed in the destination tree; source cache entries cannot alias them.
pub fn adopt<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    document: v8::Local<'s, v8::Object>,
    node: v8::Local<'s, v8::Object>,
) -> Result<v8::Local<'s, v8::Object>, &'static str> {
    let target_id = dom_id_of(scope, document);
    let source_id = dom_id_of(scope, node);
    let source_handle = handle_of(scope, node).ok_or("Illegal invocation")?;
    {
        let _target = DomScope::for_object(scope, document);
        let root = handle_of(scope, document).ok_or("Illegal invocation")?;
        if !matches!(with_tree(|tree| tree.node_type_id(root)), Some(1 | 9)) {
            return Err("Illegal invocation");
        }
    }
    let source = {
        let _source = DomScope::for_object(scope, node);
        if with_tree(|tree| tree.is_shadow_root(source_handle)).unwrap_or(false) {
            return Err("A shadow root cannot be adopted");
        }
        with_tree_mut(|tree| capture(tree, source_handle)).ok_or("Node tree unavailable")??
    };
    if source_id == target_id {
        let _source = DomScope::for_object(scope, node);
        with_tree_mut(|tree| {
            if let Some(owner) = tree.attribute_owner(source_handle) {
                let _ = tree.remove_attribute_node(owner, source_handle);
            } else if let Some(parent) = tree.parent(source_handle) {
                tree.remove_child(parent, source_handle);
            }
        });
        return Ok(node);
    }
    let mapping = {
        let _target = DomScope::for_object(scope, document);
        let mut mapping = Vec::new();
        with_tree_mut(|tree| materialize(tree, source, None, &mut mapping))
            .ok_or("Document tree unavailable")?;
        mapping
    };
    let (cache, styles, listeners, nonces, modes) = {
        let _source = DomScope::for_object(scope, node);
        with_tree_mut(|tree| {
            if let Some(owner) = tree.attribute_owner(source_handle) {
                let _ = tree.remove_attribute_node(owner, source_handle);
            } else if let Some(parent) = tree.parent(source_handle) {
                tree.remove_child(parent, source_handle);
            }
        });
        let mut cache = Vec::new();
        let mut nonces = Vec::new();
        let mut modes = Vec::new();
        DOM.with(|dom| {
            let mut dom = dom.borrow_mut();
            let state = dom.as_mut().unwrap();
            for (old, new) in &mapping {
                if let Some(value) = state.cache.remove(old) {
                    cache.push((*new, value));
                }
                if let Some(value) = state.nonces.remove(old) {
                    nonces.push((*new, value));
                }
                if let Some(value) = state.shadow_modes.remove(old) {
                    modes.push((*new, value));
                }
            }
        });
        let styles = STYLE.with(|styles| {
            let mut styles = styles.borrow_mut();
            mapping
                .iter()
                .filter_map(|(old, new)| styles.remove(old).map(|value| (*new, value)))
                .collect::<Vec<_>>()
        });
        let listeners = LISTENERS.with(|listeners| {
            let mut listeners = listeners.borrow_mut();
            let keys = listeners
                .keys()
                .filter(|(handle, _)| mapping.iter().any(|(old, _)| old == handle))
                .cloned()
                .collect::<Vec<_>>();
            keys.into_iter()
                .map(|key| {
                    let new = mapping.iter().find(|(old, _)| old == &key.0).unwrap().1;
                    let value = listeners.remove(&key).unwrap();
                    ((new, key.1), value)
                })
                .collect::<Vec<_>>()
        });
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            if active.is_some_and(|handle| mapping.iter().any(|(old, _)| old == &handle)) {
                *active = None;
            }
        });
        (cache, styles, listeners, nonces, modes)
    };
    let _target = DomScope::for_object(scope, document);
    for (handle, object) in cache {
        let local = v8::Local::new(scope, &object);
        let handle_value = v8::Integer::new_from_unsigned(scope, handle.raw());
        let id = v8::BigInt::new_from_u64(scope, target_id);
        local.set_internal_field(0, handle_value.into());
        local.set_internal_field(1, id.into());
        DOM.with(|dom| {
            dom.borrow_mut()
                .as_mut()
                .unwrap()
                .cache
                .insert(handle, object);
        });
    }
    STYLE.with(|state| state.borrow_mut().extend(styles));
    LISTENERS.with(|state| state.borrow_mut().extend(listeners));
    DOM.with(|dom| {
        let mut dom = dom.borrow_mut();
        let state = dom.as_mut().unwrap();
        state.nonces.extend(nonces);
        state.shadow_modes.extend(modes);
    });
    Ok(node)
}

pub fn dispatch<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) {
    let operation = args.get(0).to_rust_string_lossy(scope);
    let Ok(node) = v8::Local::<v8::Object>::try_from(args.get(1)) else {
        return type_error(scope);
    };
    let _dom = DomScope::for_object(scope, node);
    let Some(handle) = handle_of(scope, node) else {
        return type_error(scope);
    };
    let kind = with_tree(|tree| tree.node_type_id(handle)).unwrap_or(0);
    match operation.as_str() {
        "stamp" => {
            let stamp =
                with_tree(|tree| format!("{}:{}", DOM_ID.with(|id| id.get()), tree.version))
                    .unwrap_or_default();
            rv.set(v8::String::new(scope, &stamp).unwrap().into());
        }

        "state" if kind == 9 => {
            let existing = DOM.with(|dom| {
                dom.borrow()
                    .as_ref()
                    .and_then(|dom| dom.basics_state.clone())
            });
            let state = existing
                .map(|state| v8::Local::new(scope, state))
                .unwrap_or_else(|| {
                    let state = v8::Object::new(scope);
                    let stored = v8::Global::new(scope, state);
                    DOM.with(|dom| dom.borrow_mut().as_mut().unwrap().basics_state = Some(stored));
                    state
                });
            rv.set(state.into());
        }
        "createPI" if kind == 9 => {
            let target = args.get(2).to_rust_string_lossy(scope);
            let data = args.get(3).to_rust_string_lossy(scope);
            let node =
                with_tree_mut(|tree| tree.create_processing_instruction(&target, &data)).unwrap();
            rv.set(wrap(scope, node).into());
        }
        "piTarget" if kind == 7 => {
            let target = with_tree(|tree| {
                tree.processing_instruction_target(handle)
                    .unwrap_or_default()
                    .to_owned()
            })
            .unwrap();
            rv.set(v8::String::new(scope, &target).unwrap().into());
        }
        "compatMode" if kind == 9 => {
            let mode = with_tree(|tree| tree.compat_mode()).unwrap_or("BackCompat");
            rv.set(v8::String::new(scope, mode).unwrap().into());
        }
        "doctype" if kind == 9 => {
            let found = with_tree(|tree| {
                tree.children(handle)
                    .into_iter()
                    .find(|node| tree.node_type_id(*node) == 10)
            })
            .flatten();
            rv.set(wrap_opt(scope, found));
        }
        "doctypeFields" if kind == 10 => {
            let fields = with_tree(|tree| {
                [
                    tree.doctype_name(handle).unwrap_or_default().to_owned(),
                    tree.doctype_public_id(handle)
                        .unwrap_or_default()
                        .to_owned(),
                    tree.doctype_system_id(handle)
                        .unwrap_or_default()
                        .to_owned(),
                ]
            })
            .unwrap();
            let array = v8::Array::new(scope, 3);
            for (index, value) in fields.iter().enumerate() {
                let value = v8::String::new(scope, value).unwrap();
                array.set_index(scope, index as u32, value.into());
            }
            rv.set(array.into());
        }
        "nonce" if kind == 1 => {
            let value = nonce(scope, node).unwrap_or_default();
            rv.set(v8::String::new(scope, &value).unwrap().into());
        }
        "setNonce" if kind == 1 => {
            let value = args.get(2).to_rust_string_lossy(scope);
            DOM.with(|dom| {
                dom.borrow_mut()
                    .as_mut()
                    .unwrap()
                    .nonces
                    .insert(handle, value);
            });
        }
        "adopt" if kind == 9 => {
            let Ok(source) = v8::Local::<v8::Object>::try_from(args.get(2)) else {
                return type_error(scope);
            };
            match adopt(scope, node, source) {
                Ok(node) => rv.set(node.into()),
                Err(error) => rv.set(v8::String::new(scope, error).unwrap().into()),
            }
        }
        _ => type_error(scope),
    }
}
fn type_error(scope: &mut v8::PinScope) {
    let message = v8::String::new(scope, "Illegal invocation").unwrap();
    let error = v8::Exception::type_error(scope, message);
    scope.throw_exception(error);
}
