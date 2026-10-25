//! Chrome's own-key order for every Web IDL interface object and prototype, applied once the
//! realm's interfaces are complete. The order is data, captured from Chrome 156 by the
//! key_order_probe.js collector run (key_order_chrome156.json); key_order.js moves keys
//! without changing their descriptors. An embedder that defines further interfaces after the
//! binding (turbo-surf) defers the step and finishes it after its own installs.
use super::*;
use std::sync::OnceLock;

struct Interface {
    name: String,
    interface: (Vec<String>, Vec<String>),
    prototype: (Vec<String>, Vec<String>),
}

/// The embedder finishes the key order itself after its own interfaces exist.
struct Deferred;

fn strings(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn table() -> &'static [Interface] {
    static TABLE: OnceLock<Vec<Interface>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("key_order_chrome156.json"))
                .expect("key order fixture is JSON");
        let shape = |entry: &serde_json::Value| (strings(&entry["keys"]), strings(&entry["fixed"]));
        fixture["result"]
            .as_object()
            .map(|interfaces| {
                interfaces
                    .iter()
                    .map(|(name, entry)| Interface {
                        name: name.clone(),
                        interface: shape(&entry["interface"]),
                        prototype: shape(&entry["prototype"]),
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

fn array<'s>(scope: &mut v8::PinScope<'s, '_>, items: &[String]) -> v8::Local<'s, v8::Value> {
    let values: Vec<v8::Local<v8::Value>> = items
        .iter()
        .map(|item| v8::String::new(scope, item).unwrap().into())
        .collect();
    v8::Array::new_with_elements(scope, &values).into()
}

fn arrange(scope: &mut v8::PinScope) {
    run_js(scope, include_str!("key_order.js"));
    let global = scope.get_current_context().global(scope);
    let key = v8::String::new(scope, "__captureKeyOrder").unwrap();
    let arranger = global
        .get(scope, key.into())
        .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok());
    global.delete(scope, key.into());
    let Some(arranger) = arranger else {
        return;
    };
    let mut entries = Vec::new();
    for interface in table() {
        let name = v8::String::new(scope, &interface.name).unwrap();
        if !global.has_own_property(scope, name.into()).unwrap_or(false) {
            continue;
        }
        let entry = [
            name.into(),
            array(scope, &interface.interface.0),
            array(scope, &interface.interface.1),
            array(scope, &interface.prototype.0),
            array(scope, &interface.prototype.1),
        ];
        entries.push(v8::Array::new_with_elements(scope, &entry).into());
    }
    let entries = v8::Array::new_with_elements(scope, &entries);
    let receiver = v8::undefined(scope).into();
    arranger.call(scope, receiver, &[entries.into()]);
}

/// Called at the end of every binding install.
pub(super) fn installed(scope: &mut v8::PinScope) {
    if scope.get_current_context().get_slot::<Deferred>().is_none() {
        arrange(scope);
    }
}

/// Keep the current context's interfaces in definition order until [`finish`].
pub fn defer(scope: &mut v8::PinScope) {
    scope
        .get_current_context()
        .set_slot(std::rc::Rc::new(Deferred));
}

/// Arrange the current context's interfaces in Chrome's order and freeze its constants.
pub fn finish(scope: &mut v8::PinScope) {
    scope.get_current_context().remove_slot::<Deferred>();
    arrange(scope);
}
