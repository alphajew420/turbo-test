//! Browser environment shim (P3) — the V8 BINDING + browser globals over turbo-dom's pure-Rust `rtdom` (the `turbo-dom-parser` crate, built with
//! `rust-runtime`) to V8 via native rusty-v8 callbacks, so tests get `window`/`document`/Element with
//! NO JS DOM (no `installGlobals`, no `.node` parser, no node_modules turbo-dom).
//!
//! Model: the DOM lives in Rust as one `Tree` per context (handle-based — each node is a `u32`
//! `Handle`). A JS DOM node is a V8 object with the handle in internal field 0; methods/accessors
//! are native callbacks that read the handle, call `Tree`, and wrap result handles back into node
//! objects. A `handle → V8 object` cache preserves identity (`el === el`).
//!
//! This is the P3.2 vertical slice: document/createElement/createTextNode, appendChild/insertBefore/
//! removeChild, get/setAttribute, id/className/tagName/textContent, querySelector(All)/getElementById.
//! Events, cssom/style, classList, forms etc. grow in P3.3. Gated by `TURBO_RUST_DOM`.

#[path = "cssom_parser.rs"]
mod cssom_parser;

use std::cell::RefCell;
use std::collections::HashMap;

use turbo_dom_parser::rtdom::tree::{Handle, Namespace, Tree};
use turbo_dom_parser::rtdom::NodeRef;

/// The all-Rust DOM (rtdom) is now the ONLY DOM environment — the JS turbo-dom bootstrap
/// (installGlobals + the `.node` parser) was removed in v0.3.0. Kept as a function (always true) so
/// the per-file install/reset call sites read intent; `TURBO_RUST_DOM` is no longer consulted.
pub fn enabled() -> bool {
    true
}

/// Whether to RECORD missing-member accesses (debug surface map). The interceptor itself is always
/// installed (for graceful degradation — an unimplemented member returns undefined instead of
/// crashing the file); recording is the only extra cost and is gated here so production pays nothing
/// but a cached bool check + a small fall-through. Cached once (no per-access getenv).
pub fn log_enabled() -> bool {
    use std::sync::OnceLock;
    static LOG: OnceLock<bool> = OnceLock::new();
    *LOG.get_or_init(|| {
        std::env::var("TURBO_RUST_DOM_LOG")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    })
}

/// Names the native DOM binds (template methods/accessors + document own methods) + JS internals —
/// the interceptor falls through for these and only records the REST as "missing".
const KNOWN: &[&str] = &[
    // element methods + accessors
    "appendChild",
    "removeChild",
    "insertBefore",
    "setAttribute",
    "getAttribute",
    "hasAttribute",
    "removeAttribute",
    "querySelector",
    "querySelectorAll",
    "tagName",
    "parentNode",
    "firstChild",
    "nextSibling",
    "textContent",
    "id",
    "className",
    "style",
    "oninput",
    "onchange",
    "onclick",
    "nodeType",
    "nodeName",
    "childNodes",
    "ownerDocument",
    "addEventListener",
    "removeEventListener",
    "dispatchEvent",
    "matches",
    "contains",
    "innerHTML",
    "outerHTML",
    "children",
    "parentElement",
    "firstElementChild",
    "namespaceURI",
    "value",
    "append",
    "prepend",
    "remove",
    "focus",
    "blur",
    "click",
    "scrollIntoView",
    "getBoundingClientRect",
    "createElementNS",
    "createDocumentFragment",
    "createComment",
    "cloneNode",
    "isConnected",
    "attributes",
    "dataset",
    "createRange",
    "getRootNode",
    "getSelection",
    "closest",
    "data",
    "nodeValue",
    "constructor",
    "documentMode",
    "activeElement",
    "focus",
    "blur",
    "selectionStart",
    "selectionEnd",
    "setSelectionRange",
    "getAttributeNode",
    "TEXT_NODE",
    "ELEMENT_NODE",
    "COMMENT_NODE",
    "DOCUMENT_NODE",
    "DOCUMENT_FRAGMENT_NODE",
    // document own methods/props
    "createElement",
    "createTextNode",
    "getElementById",
    "body",
    "documentElement",
    // JS internals V8 / libs probe constantly — never DOM
    "then",
    "catch",
    "finally",
    "constructor",
    "prototype",
    "toString",
    "valueOf",
    "toJSON",
    "length",
    "name",
    "call",
    "apply",
    "bind",
    "hasOwnProperty",
    "nodeName",
];

thread_local! {
    /// debug logger: unknown property name → access count.
    static MISSING: RefCell<HashMap<String, u64>> = RefCell::new(HashMap::new());
}

/// Interceptor getter (debug only): fall through for known/symbol names; record + return undefined
/// for anything unimplemented.
fn missing_getter(
    scope: &mut v8::PinScope,
    name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    let _dom = DomScope::for_object(scope, args.holder());
    // V8 inline-cache hazard with NON_MASKING interceptors: once a load site reads a property while
    // it is ABSENT (we intercept → undefined), V8 may cache "this site goes to the interceptor" and
    // keep routing there EVEN AFTER the property is later added as a real own property — masking it
    // (reads return undefined though getOwnPropertyNames/hasOwnProperty see it). React stores
    // `node.__reactFiber$…`/`__reactProps$…` exactly this way: it reads the key (miss → IC poisoned)
    // before precaching the fiber, then later reads it again and gets undefined → it walks up to the
    // wrong ancestor, so delegated onClick/onChange never fire (only after a prior render primed the
    // IC — a brutal Heisenbug). Guard: when the interceptor fires, first look for a REAL property
    // (interceptors bypassed). If one exists, return IT — so a poisoned IC still yields the truth.
    let this = args.holder();
    if let Some(real) = this.get_real_named_property(scope, name) {
        rv.set(real);
        return v8::Intercepted::kYes;
    }
    let key = name.to_rust_string_lossy(scope);
    // symbols (Symbol.toPrimitive, Symbol.iterator, …) + known names → fall through.
    if key.starts_with("Symbol(") || KNOWN.contains(&key.as_str()) {
        return v8::Intercepted::kNo;
    }
    if log_enabled() {
        MISSING.with(|m| *m.borrow_mut().entry(key).or_insert(0) += 1);
    }
    // graceful degradation: an unimplemented member reads as undefined (no crash).
    rv.set(v8::undefined(scope).into());
    v8::Intercepted::kYes
}

/// Passthrough setter interceptor: NEVER intercept (kNo) → JS property sets create normal own
/// properties. Required so `document.x = …` shims AND React's expando writes on DOM nodes
/// (`node.__reactProps$…`, `_reactListening…`) persist instead of being swallowed by the
/// getter-only interceptor.
fn passthrough_setter<T>(
    _scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    _value: v8::Local<v8::Value>,
    _args: v8::PropertyCallbackArguments,
    _rv: v8::ReturnValue<T>,
) -> v8::Intercepted {
    v8::Intercepted::kNo
}

/// Print + clear the accumulated missing-access log (call between files when debugging).
pub fn dump_missing() {
    let mut items: Vec<(String, u64)> = MISSING.with(|m| m.borrow_mut().drain().collect());
    if items.is_empty() {
        return;
    }
    items.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    eprintln!(
        "[rust-dom] {} missing DOM members (name×accesses):",
        items.len()
    );
    for (name, n) in &items {
        eprintln!("[rust-dom]   {name} ×{n}");
    }
}

struct DomState {
    tree: Tree,
    /// handle → JS node object, for identity (`el === el`).
    cache: HashMap<Handle, v8::Global<v8::Object>>,
    /// shared element object template (internal_field_count = 1).
    el_template: v8::Global<v8::ObjectTemplate>,
    shadow_modes: HashMap<Handle, String>,
}

thread_local! {
    static DOM: RefCell<Option<DomState>> = const { RefCell::new(None) };
}

// Native callbacks may enter through a node from another same-origin frame.
// Keep each context's tree, wrappers, listeners and focus together, and restore
// the caller's DOM on return (including reentrant event dispatch).
#[derive(Clone, Copy)]
struct DomContextId(u64);

#[derive(Default)]
struct SavedDom {
    dom: Option<DomState>,
    styles: HashMap<Handle, v8::Global<v8::Object>>,
    listeners: HashMap<(Handle, String), Vec<(v8::Global<v8::Function>, bool)>>,
    active: Option<Handle>,
}

thread_local! {
    static DOM_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static NEXT_DOM_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    static OTHER_DOMS: RefCell<HashMap<u64, SavedDom>> = RefCell::new(HashMap::new());
    static DOM_CONTEXTS: RefCell<HashMap<u64, v8::Global<v8::Context>>> = RefCell::new(HashMap::new());
}

fn take_dom() -> SavedDom {
    SavedDom {
        dom: DOM.with(|d| d.borrow_mut().take()),
        styles: STYLE.with(|d| std::mem::take(&mut *d.borrow_mut())),
        listeners: LISTENERS.with(|d| std::mem::take(&mut *d.borrow_mut())),
        active: ACTIVE.with(|d| d.borrow_mut().take()),
    }
}

fn put_dom(saved: SavedDom) {
    DOM.with(|d| *d.borrow_mut() = saved.dom);
    STYLE.with(|d| *d.borrow_mut() = saved.styles);
    LISTENERS.with(|d| *d.borrow_mut() = saved.listeners);
    ACTIVE.with(|d| *d.borrow_mut() = saved.active);
}

struct DomScope(u64);
impl DomScope {
    fn enter(id: u64) -> Self {
        let previous = DOM_ID.with(|d| d.replace(id));
        if id != previous {
            let old = take_dom();
            let next = OTHER_DOMS.with(|d| {
                let mut d = d.borrow_mut();
                d.insert(previous, old);
                d.remove(&id).unwrap_or_default()
            });
            put_dom(next);
        }
        Self(previous)
    }

    fn for_object(scope: &mut v8::PinScope, object: v8::Local<v8::Object>) -> Self {
        let context = object
            .get_creation_context(scope)
            .unwrap_or_else(|| scope.get_current_context());
        Self::enter(context.get_slot::<DomContextId>().map_or(0, |id| id.0))
    }
}
impl Drop for DomScope {
    fn drop(&mut self) {
        let current = DOM_ID.with(|d| d.get());
        if current != self.0 {
            let saved = take_dom();
            let previous = OTHER_DOMS.with(|d| {
                let mut d = d.borrow_mut();
                d.insert(current, saved);
                d.remove(&self.0).unwrap_or_default()
            });
            put_dom(previous);
            DOM_ID.with(|d| d.set(self.0));
        }
    }
}

/// Install an independent native document in the current V8 context. The caller's
/// document stays live, and callbacks route by their receiver's creation context.
pub fn install_independent_html(scope: &mut v8::PinScope, html: &str) {
    let context = scope.get_current_context();
    let id = context
        .get_slot::<DomContextId>()
        .map(|id| id.0)
        .unwrap_or_else(|| {
            let id = NEXT_DOM_ID.with(|d| {
                let id = d.get();
                d.set(id + 1);
                id
            });
            context.set_slot(std::rc::Rc::new(DomContextId(id)));
            id
        });
    let _dom = DomScope::enter(id);
    install_with_html(scope, html);
}

/// Read a node object's handle from internal field 0.
fn handle_of(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) -> Option<Handle> {
    let f = obj.get_internal_field(scope, 0)?;
    let v = v8::Local::<v8::Value>::try_from(f).ok()?;
    // Embedder globals can also have internal fields; undefined and pointers
    // must not be coerced into node handle zero.
    if !v.is_uint32() {
        return None;
    }
    v.uint32_value(scope).map(Handle::from_raw)
}

fn receiver_handle(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    kinds: &[u8],
) -> Option<Handle> {
    let handle = handle_of(scope, args.this());
    let valid = handle
        .and_then(|h| with_tree(|tree| kinds.is_empty() || kinds.contains(&tree.node_type_id(h))))
        .unwrap_or(false);
    if valid {
        return handle;
    }
    let message = v8::String::new(scope, "Illegal invocation").unwrap();
    let error = v8::Exception::type_error(scope, message);
    scope.throw_exception(error);
    None
}

/// Wrap a handle as a JS node object (cached for identity).
fn wrap<'s>(scope: &mut v8::PinScope<'s, '_>, handle: Handle) -> v8::Local<'s, v8::Object> {
    if let Some(g) = DOM.with(|d| {
        d.borrow()
            .as_ref()
            .and_then(|s| s.cache.get(&handle).cloned())
    }) {
        return v8::Local::new(scope, &g);
    }
    let owner = DOM_CONTEXTS.with(|d| d.borrow().get(&DOM_ID.with(|id| id.get())).cloned());
    let context = owner
        .as_ref()
        .map(|owner| v8::Local::new(scope, owner))
        .unwrap_or_else(|| scope.get_current_context());
    let scope = &mut v8::ContextScope::new(scope, context);
    let tmpl_g = DOM.with(|d| d.borrow().as_ref().unwrap().el_template.clone());
    let tmpl = v8::Local::new(scope, &tmpl_g);
    let obj = tmpl.new_instance(scope).unwrap();
    let h = v8::Integer::new_from_unsigned(scope, handle.raw());
    obj.set_internal_field(0, h.into());
    // Parser-created controls bypass createElement's JS wrapper. Select their live
    // interface prototype here too, so IDL getters work for query/innerHTML results.
    let tag = with_tree(|t| t.tag_name(handle))
        .flatten()
        .unwrap_or_default();
    let interface = match tag.to_ascii_lowercase().as_str() {
        "input" => Some("HTMLInputElement"),
        "textarea" => Some("HTMLTextAreaElement"),
        "select" => Some("HTMLSelectElement"),
        "option" => Some("HTMLOptionElement"),
        "button" => Some("HTMLButtonElement"),
        "label" => Some("HTMLLabelElement"),
        "canvas" => Some("HTMLCanvasElement"),
        "a" => Some("HTMLAnchorElement"),
        _ => None,
    };
    if let Some(interface) = interface {
        let global = scope.get_current_context().global(scope);
        let key = v8::String::new(scope, interface).unwrap();
        if let Some(ctor) = global
            .get(scope, key.into())
            .and_then(|v| v.to_object(scope))
        {
            let key = v8::String::new(scope, "prototype").unwrap();
            if let Some(proto) = ctor.get(scope, key.into()) {
                if proto.is_object() {
                    obj.set_prototype(scope, proto);
                }
            }
        }
    }
    let g = v8::Global::new(scope, obj);
    DOM.with(|d| {
        if let Some(s) = d.borrow_mut().as_mut() {
            s.cache.insert(handle, g);
        }
    });
    // Projection runs after caching, since accessors such as ownerDocument and
    // childNodes may synchronously wrap this node again.
    let global = context.global(scope);
    let name = v8::String::new(scope, "ts").unwrap();
    let key = v8::Symbol::for_key(scope, name);
    if let Some(namespace) = global
        .get(scope, key.into())
        .and_then(|v| v.to_object(scope))
    {
        let key = v8::String::new(scope, "projectNativeNode").unwrap();
        if let Some(function) = namespace
            .get(scope, key.into())
            .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
        {
            function.call(scope, namespace.into(), &[obj.into()]);
        }
    }
    obj
}

/// Wrap `Option<Handle>` as object-or-null.
fn wrap_opt<'s>(scope: &mut v8::PinScope<'s, '_>, h: Option<Handle>) -> v8::Local<'s, v8::Value> {
    match h {
        Some(h) => wrap(scope, h).into(),
        None => v8::null(scope).into(),
    }
}

fn arg_str(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> String {
    args.get(i).to_rust_string_lossy(scope)
}

/// Resolve argument `i` to a node handle, or `None` for null/undefined/non-node. CRITICAL: guards
/// `to_object()`, which THROWS "Cannot convert undefined or null to object" on null/undefined — and
/// `insertBefore(node, null)` / `removeChild`-style calls pass that constantly (emotion/react-dom).
fn arg_handle(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    i: i32,
) -> Option<Handle> {
    let v = args.get(i);
    if v.is_null_or_undefined() {
        return None;
    }
    let object = v.to_object(scope)?;
    let context = object.get_creation_context(scope)?;
    let owner = context.get_slot::<DomContextId>().map_or(0, |id| id.0);
    if owner != DOM_ID.with(|id| id.get()) {
        let message = v8::String::new(
            scope,
            "Cross-document node adoption is not supported by the native DOM",
        )
        .unwrap();
        let error = v8::Exception::type_error(scope, message);
        scope.throw_exception(error);
        return None;
    }
    handle_of(scope, object)
}

// ---- with-tree helpers (borrow the thread-local Tree) ----------------------------------------

fn with_tree<R>(f: impl FnOnce(&Tree) -> R) -> Option<R> {
    DOM.with(|d| d.borrow().as_ref().map(|s| f(&s.tree)))
}
fn with_tree_mut<R>(f: impl FnOnce(&mut Tree) -> R) -> Option<R> {
    DOM.with(|d| d.borrow_mut().as_mut().map(|s| f(&mut s.tree)))
}

// ---- element methods -------------------------------------------------------------------------

fn throw_dom_error(scope: &mut v8::PinScope, name: &str, message: &str) {
    let text = v8::String::new(scope, message).unwrap();
    let error = v8::Exception::error(scope, text);
    let object = v8::Local::<v8::Object>::try_from(error).unwrap();
    let key = v8::String::new(scope, "name").unwrap();
    let name = v8::String::new(scope, name).unwrap();
    object.set(scope, key.into(), name.into());
    scope.throw_exception(error);
}

fn el_attach_shadow(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(host) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    let Ok(options) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        let text = v8::String::new(scope, "attachShadow requires an options dictionary").unwrap();
        let error = v8::Exception::type_error(scope, text);
        scope.throw_exception(error);
        return;
    };
    let key = v8::String::new(scope, "mode").unwrap();
    let Some(mode) = options.get(scope, key.into()) else {
        return;
    };
    let mode = mode.to_rust_string_lossy(scope);
    if mode != "open" && mode != "closed" {
        let text = v8::String::new(scope, "mode must be open or closed").unwrap();
        let error = v8::Exception::type_error(scope, text);
        scope.throw_exception(error);
        return;
    }
    let allowed = with_tree(|tree| {
        let tag = tree.local_name(host).unwrap_or("");
        tree.namespace_id(host) == 0
            && (tag.contains('-')
                || matches!(
                    tag,
                    "article"
                        | "aside"
                        | "blockquote"
                        | "body"
                        | "div"
                        | "footer"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "header"
                        | "main"
                        | "nav"
                        | "p"
                        | "section"
                        | "span"
                ))
            && tree.shadow_root(host).is_none()
    })
    .unwrap_or(false);
    if !allowed {
        throw_dom_error(
            scope,
            "NotSupportedError",
            "This element cannot attach another shadow root",
        );
        return;
    }
    let root = with_tree_mut(|tree| tree.attach_shadow(host)).unwrap();
    DOM.with(|state| {
        state
            .borrow_mut()
            .as_mut()
            .unwrap()
            .shadow_modes
            .insert(root, mode)
    });
    let node = wrap(scope, root);
    rv.set(node.into());
}

fn get_shadow_root(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let root = handle_of(scope, args.holder())
        .and_then(|host| with_tree(|tree| tree.shadow_root(host)).flatten());
    let root = root.filter(|root| {
        DOM.with(|state| {
            state
                .borrow()
                .as_ref()
                .and_then(|state| state.shadow_modes.get(root))
                .is_some_and(|mode| mode == "open")
        })
    });
    let value = wrap_opt(scope, root);
    rv.set(value);
}

fn get_shadow_host(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let host = handle_of(scope, args.holder())
        .and_then(|root| with_tree(|tree| tree.shadow_host(root)).flatten());
    if let Some(host) = host {
        let value = wrap(scope, host);
        rv.set(value.into());
    }
}

fn get_shadow_mode(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(root) = handle_of(scope, args.holder()) else {
        return;
    };
    let mode = DOM.with(|state| {
        state
            .borrow()
            .as_ref()
            .and_then(|state| state.shadow_modes.get(&root))
            .cloned()
    });
    if let Some(mode) = mode {
        let value = v8::String::new(scope, &mode).unwrap();
        rv.set(value.into());
    }
}

fn get_is_shadow_root(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let is_shadow = handle_of(scope, args.holder())
        .and_then(|h| with_tree(|tree| tree.is_shadow_root(h)))
        .unwrap_or(false);
    rv.set_bool(is_shadow);
}

fn el_append_child(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(parent) = receiver_handle(scope, &args, &[]) else {
        return;
    };
    let Some(child) = arg_handle(scope, &args, 0) else {
        return;
    };
    if with_tree(|t| t.is_shadow_root(child)).unwrap_or(false) {
        throw_dom_error(
            scope,
            "HierarchyRequestError",
            "A shadow root cannot be inserted",
        );
        return;
    }
    with_tree_mut(|t| t.append_child(parent, child));
    rv.set(args.get(0));
}

fn el_remove_child(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(parent) = receiver_handle(scope, &args, &[]) else {
        return;
    };
    let Some(child) = arg_handle(scope, &args, 0) else {
        return;
    };
    with_tree_mut(|t| t.remove_child(parent, child));
    rv.set(args.get(0));
}

// ---- CharacterData methods (text/comment nodes) ----------------------------------------------
fn arg_usize(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, i: i32) -> usize {
    let n = args.get(i).integer_value(scope).unwrap_or(0);
    if n < 0 {
        0
    } else {
        n as usize
    }
}
fn el_insert_data(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[3, 4, 8]) else {
        return;
    };
    let off = arg_usize(scope, &args, 0);
    let data = arg_str(scope, &args, 1);
    with_tree_mut(|t| t.insert_data(h, off, &data));
}
fn el_delete_data(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[3, 4, 8]) else {
        return;
    };
    let off = arg_usize(scope, &args, 0);
    let count = arg_usize(scope, &args, 1);
    with_tree_mut(|t| t.delete_data(h, off, count));
}
fn el_append_data(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[3, 4, 8]) else {
        return;
    };
    let data = arg_str(scope, &args, 0);
    with_tree_mut(|t| t.append_data(h, &data));
}
fn el_replace_data(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[3, 4, 8]) else {
        return;
    };
    let off = arg_usize(scope, &args, 0);
    let count = arg_usize(scope, &args, 1);
    let data = arg_str(scope, &args, 2);
    with_tree_mut(|t| t.replace_data(h, off, count, &data));
}
fn el_substring_data(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[3, 4, 8]) else {
        return;
    };
    let off = arg_usize(scope, &args, 0);
    let count = arg_usize(scope, &args, 1);
    let s = with_tree(|t| t.substring_data(h, off, count)).unwrap_or_default();
    rv.set(v8::String::new(scope, &s).unwrap().into());
}
fn el_split_text(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[3, 4]) else {
        return;
    };
    let off = arg_usize(scope, &args, 0);
    let Some(new_h) = with_tree_mut(|t| t.split_text(h, off)) else {
        return;
    };
    let node = wrap(scope, new_h);
    rv.set(node.into());
}

// rtdom's native style cascade (turbo-dom `cascade::computed_style`) resolves a property's computed
// value WITH inheritance for the inherited set (color, visibility, font-*, …). getComputedStyle's
// JS cascade only sees an element's OWN matched rules + inline style, so an inherited property like
// `visibility: hidden` set on an ancestor never reached descendants — testing-library then treated a
// hidden subtree as query-visible. JS getComputedStyle calls this for those inherited properties.
// (The result is version-cached on the Tree, so repeated calls within a render are cheap.)
fn el_cascade_prop(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    let name = arg_str(scope, &args, 0);
    let val = with_tree(|t| {
        turbo_dom_parser::rtdom::cascade::computed_style(t, h)
            .get(&name)
            .cloned()
    })
    .flatten()
    .unwrap_or_default();
    rv.set(v8::String::new(scope, &val).unwrap().into());
}

// Resolve a run of variadic (Node | string) arguments [start..] into handles, turning bare strings
// into text nodes (per the ChildNode/ParentNode spec). Used by before/after/replaceWith/etc.
fn resolve_nodes(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    start: i32,
) -> Option<Vec<Handle>> {
    let mut out = Vec::new();
    for i in start..args.length() {
        let value = args.get(i);
        if value.is_object() {
            let object = value.to_object(scope)?;
            if handle_of(scope, object).is_some() {
                // Adoption uses the same owner check as appendChild. Equal raw
                // handles from two independent trees must never alias here.
                let node = arg_handle(scope, args, i)?;
                if with_tree(|tree| tree.is_shadow_root(node)).unwrap_or(false) {
                    throw_dom_error(
                        scope,
                        "HierarchyRequestError",
                        "A shadow root cannot be inserted",
                    );
                    return None;
                }
                out.push(node);
                continue;
            }
        }
        let text = value.to_rust_string_lossy(scope);
        out.push(with_tree_mut(|tree| tree.create_text_node(&text))?);
    }
    Some(out)
}

fn el_before(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1, 3, 4, 8, 10]) else {
        return;
    };
    let Some(nodes) = resolve_nodes(scope, &args, 0) else {
        return;
    };
    with_tree_mut(|t| t.before(h, &nodes));
}
fn el_after(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1, 3, 4, 8, 10]) else {
        return;
    };
    let Some(nodes) = resolve_nodes(scope, &args, 0) else {
        return;
    };
    with_tree_mut(|t| t.after(h, &nodes));
}
fn el_replace_with(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1, 3, 4, 8, 10]) else {
        return;
    };
    let Some(nodes) = resolve_nodes(scope, &args, 0) else {
        return;
    };
    with_tree_mut(|t| t.replace_with(h, &nodes));
}
fn el_replace_children(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1, 9, 11]) else {
        return;
    };
    let Some(nodes) = resolve_nodes(scope, &args, 0) else {
        return;
    };
    with_tree_mut(|t| t.replace_children(h, &nodes));
}
fn el_insert_adjacent_element(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    let pos = arg_str(scope, &args, 0);
    let Some(el) = arg_handle(scope, &args, 1) else {
        return;
    };
    let ok = with_tree_mut(|t| t.insert_adjacent_element(h, &pos, el)).unwrap_or(false);
    if ok {
        rv.set(args.get(1));
    } else {
        rv.set(v8::null(scope).into());
    }
}
fn el_insert_adjacent_html(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    let pos = arg_str(scope, &args, 0);
    let html = arg_str(scope, &args, 1);
    with_tree_mut(|t| t.insert_adjacent_html(h, &pos, &html));
}
fn el_toggle_attribute(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    let name = arg_str(scope, &args, 0).to_ascii_lowercase();
    let force = if args.length() > 1 && !args.get(1).is_undefined() {
        Some(args.get(1).boolean_value(scope))
    } else {
        None
    };
    let present = with_tree_mut(|t| t.toggle_attribute(h, &name, force)).unwrap_or(false);
    rv.set(v8::Boolean::new(scope, present).into());
}
fn el_get_attribute_ns(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    let local = arg_str(scope, &args, 1);
    match with_tree(|t| t.get_attribute_ns(h, None, &local)).flatten() {
        Some(s) => rv.set(v8::String::new(scope, &s).unwrap().into()),
        None => rv.set(v8::null(scope).into()),
    }
}

fn el_insert_before(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(parent) = receiver_handle(scope, &args, &[]) else {
        return;
    };
    let Some(child) = arg_handle(scope, &args, 0) else {
        return;
    };
    let reference = arg_handle(scope, &args, 1);
    if with_tree(|t| t.is_shadow_root(child)).unwrap_or(false) {
        throw_dom_error(
            scope,
            "HierarchyRequestError",
            "A shadow root cannot be inserted",
        );
        return;
    }
    with_tree_mut(|t| t.insert_before(parent, child, reference));
    rv.set(args.get(0));
}

fn el_set_attribute(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    let name = arg_str(scope, &args, 0);
    let value = arg_str(scope, &args, 1);
    // HTML setAttribute lowercases the qualified name (React calls setAttribute('colSpan', …); jsdom
    // stores 'colspan'). Lowercase for HTML-namespace elements only (ns 0); SVG (ns 1) keeps camelCase
    // attrs like viewBox.
    let is_html = with_tree(|t| t.namespace_id(h) == 0).unwrap_or(true);
    let key = if is_html {
        name.to_ascii_lowercase()
    } else {
        name.clone()
    };
    with_tree_mut(|t| t.set_attribute(h, &key, &value));

    // When an <input> becomes a checkbox/radio (React sets `type` via setAttribute after createElement),
    // strip the value-change tracker: those inputs track `checked`, not `value`, so a value-tracker
    // would mis-detect changes and break onChange. Removing the own `value`/`_valueTracker` lets the
    // framework use its checked path (we keep the own `checked` accessor).
    if key == "type" {
        let v = value.to_ascii_lowercase();
        if v == "checkbox" || v == "radio" {
            let this = args.this();
            for k in ["value", "_valueTracker"] {
                if let Some(key) = v8::String::new(scope, k) {
                    if this.has_own_property(scope, key.into()).unwrap_or(false) {
                        this.delete(scope, key.into());
                    }
                }
            }
        }
    }
}

/// Attribute lookup with the HTML case-insensitivity rule: try the exact name, then its lowercase
/// form (React writes `tabindex`/`maxlength` lowercased; tests read `tabIndex`/`maxLength`). Exact-
/// first preserves SVG camelCase attributes (`viewBox`).
fn attr_get(t: &Tree, h: Handle, name: &str) -> Option<String> {
    if let Some(v) = t.get_attribute(h, name) {
        return Some(v.to_string());
    }
    let lower = name.to_ascii_lowercase();
    if lower != name {
        if let Some(v) = t.get_attribute(h, &lower) {
            return Some(v.to_string());
        }
    }
    None
}

fn el_get_attribute(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    let name = arg_str(scope, &args, 0);
    let val = with_tree(|t| attr_get(t, h, &name)).flatten();
    match val {
        Some(s) => rv.set(v8::String::new(scope, &s).unwrap().into()),
        None => rv.set(v8::null(scope).into()),
    }
}

fn el_get_attribute_node(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    let name = arg_str(scope, &args, 0);
    let val = with_tree(|t| attr_get(t, h, &name)).flatten();
    match val {
        Some(v) => {
            let o = v8::Object::new(scope);
            for (pk, pv) in [
                ("name", name.as_str()),
                ("nodeName", name.as_str()),
                ("localName", name.as_str()),
                ("value", v.as_str()),
                ("nodeValue", v.as_str()),
            ] {
                let key = v8::String::new(scope, pk).unwrap();
                let s = v8::String::new(scope, pv).unwrap();
                o.set(scope, key.into(), s.into());
            }
            let sk = v8::String::new(scope, "specified").unwrap();
            let st = v8::Boolean::new(scope, true);
            o.set(scope, sk.into(), st.into());
            rv.set(o.into());
        }
        None => rv.set(v8::null(scope).into()),
    }
}

// setAttributeNode(attr) / removeAttributeNode(attr) — operate on an Attr node (the plain object
// el_get_attribute_node returns: { name, value, … }). React's releaseSingletonInstance resets the
// <html>/<head>/<body> singletons during commit via instance.removeAttributeNode(attributes[i]);
// without these it throws "removeAttributeNode is not a function" and aborts the commit.
fn attr_name_value(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<(String, String)> {
    let o = v.to_object(scope)?;
    let name = get_str_prop(scope, o, "name").or_else(|| get_str_prop(scope, o, "nodeName"))?;
    let value = get_str_prop(scope, o, "value")
        .or_else(|| get_str_prop(scope, o, "nodeValue"))
        .unwrap_or_default();
    if name.is_empty() {
        None
    } else {
        Some((name, value))
    }
}
fn el_set_attribute_node(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    if let Some((name, value)) = attr_name_value(scope, args.get(0)) {
        let is_html = with_tree(|t| t.namespace_id(h) == 0).unwrap_or(true);
        let key = if is_html {
            name.to_ascii_lowercase()
        } else {
            name
        };
        with_tree_mut(|t| t.set_attribute(h, &key, &value));
    }
    rv.set(v8::null(scope).into()); // replaced Attr (none modeled) → null
}
fn el_remove_attribute_node(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    if let Some((name, _)) = attr_name_value(scope, args.get(0)) {
        with_tree_mut(|t| t.remove_attribute(h, &name));
        let lower = name.to_ascii_lowercase();
        if lower != name {
            with_tree_mut(|t| t.remove_attribute(h, &lower));
        }
    }
    rv.set(args.get(0)); // spec returns the removed Attr
}

fn el_has_attribute(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    let name = arg_str(scope, &args, 0);
    let lower = name.to_ascii_lowercase();
    let has =
        with_tree(|t| t.has_attribute(h, &name) || (lower != name && t.has_attribute(h, &lower)))
            .unwrap_or(false);
    rv.set(v8::Boolean::new(scope, has).into());
}

fn el_remove_attribute(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    let name = arg_str(scope, &args, 0);
    let lower = name.to_ascii_lowercase();
    with_tree_mut(|t| {
        t.remove_attribute(h, &name);
        if lower != name {
            t.remove_attribute(h, &lower);
        }
    });
}

/// element-scoped querySelector via NodeRef.
fn el_query_selector(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1, 11]) else {
        return;
    };
    let sel = arg_str(scope, &args, 0);
    let found =
        with_tree(|t| NodeRef::new(t, h).query_selector(&sel).map(|n| n.handle())).flatten();
    let v = wrap_opt(scope, found);
    rv.set(v);
}

// ---- innerHTML / outerHTML / children / form props / classList (P3.3 batch) -----------------

fn get_inner_html(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let html = with_tree(|t| turbo_dom_parser::rtdom::serialize::serialize_inner(t, h))
        .unwrap_or_default();
    rv.set(v8::String::new(scope, &html).unwrap().into());
}
fn set_inner_html(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    value: v8::Local<v8::Value>,
    args: v8::PropertyCallbackArguments,
    _rv: v8::ReturnValue<()>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let html = value.to_rust_string_lossy(scope);
    with_tree_mut(|t| t.set_inner_html(h, &html));
}
fn get_outer_html(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let html = with_tree(|t| turbo_dom_parser::rtdom::serialize::serialize_outer(t, h))
        .unwrap_or_default();
    rv.set(v8::String::new(scope, &html).unwrap().into());
}

fn set_outer_html(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    value: v8::Local<v8::Value>,
    args: v8::PropertyCallbackArguments,
    _rv: v8::ReturnValue<()>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(node) = handle_of(scope, args.holder()) else {
        return;
    };
    let parent = with_tree(|tree| {
        NodeRef::new(tree, node)
            .parent()
            .map(|parent| parent.handle())
    })
    .flatten();
    let Some(parent) = parent else {
        return;
    };
    if with_tree(|tree| tree.node_type_id(parent) == 9).unwrap_or(false) {
        throw_dom_error(
            scope,
            "NoModificationAllowedError",
            "The document element cannot be replaced with outerHTML",
        );
        return;
    }
    let html = value.to_rust_string_lossy(scope);
    with_tree_mut(|tree| {
        tree.insert_adjacent_html(node, "beforebegin", &html);
        tree.remove_child(parent, node);
    });
}

/// `children` = element-only child nodes (HTMLCollection-ish array).
fn get_children(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let kids: Vec<Handle> = with_tree(|t| {
        t.children(h)
            .into_iter()
            .filter(|&c| t.node_type_id(c) == 1)
            .collect()
    })
    .unwrap_or_default();
    let arr = v8::Array::new(scope, kids.len() as i32);
    for (i, k) in kids.into_iter().enumerate() {
        let node = wrap(scope, k);
        arr.set_index(scope, i as u32, node.into());
    }
    bind_method(scope, arr.into(), "item", nodelist_item);
    bind_method(scope, arr.into(), "namedItem", nodelist_named_item);
    rv.set(arr.into());
}

fn get_parent_element(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let p = with_tree(|t| {
        NodeRef::new(t, h)
            .parent()
            .map(|x| x.handle())
            .filter(|&ph| t.node_type_id(ph) == 1)
    })
    .flatten();
    let v = wrap_opt(scope, p);
    rv.set(v);
}

fn get_first_element_child(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let c = with_tree(|t| NodeRef::new(t, h).first_element_child().map(|x| x.handle())).flatten();
    let v = wrap_opt(scope, c);
    rv.set(v);
}

fn get_namespace_uri(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    rv.set(
        v8::String::new(scope, "http://www.w3.org/1999/xhtml")
            .unwrap()
            .into(),
    );
}

/// `value`/`checked`/`disabled` etc — stored on the element as expandos by default (React's
/// controlled inputs set them directly); fall back to the attribute for `value`.
fn get_value(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let v = with_tree(|t| t.get_attribute(h, "value").map(|s| s.to_string()))
        .flatten()
        .unwrap_or_default();
    rv.set(v8::String::new(scope, &v).unwrap().into());
}
fn set_value(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    value: v8::Local<v8::Value>,
    args: v8::PropertyCallbackArguments,
    _rv: v8::ReturnValue<()>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let v = value.to_rust_string_lossy(scope);
    with_tree_mut(|t| t.set_attribute(h, "value", &v));
}

// no-op / stub element methods commonly called during render + queries.
thread_local! {
    /// the focused element handle (document.activeElement). userEvent.type targets it.
    static ACTIVE: RefCell<Option<Handle>> = const { RefCell::new(None) };
}
/// Native `HTMLElement.focus()` — like a real browser / jsdom: blur the previously-focused element
/// (blur + bubbling focusout), make this the activeElement, then fire focus + bubbling focusin. The
/// bubbling focusout/focusin reach React's delegated onBlur/onFocus at the root container. userEvent
/// drives blur-on-tab purely through native focus(), so this must dispatch the events itself.
fn el_focus(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    let prev = ACTIVE.with(|a| *a.borrow());
    if prev == Some(h) {
        return;
    }
    ACTIVE.with(|a| *a.borrow_mut() = Some(h));
    // Only blur a previously-focused element that is still attached. A detached `prev` is a stale
    // pointer left by a prior, now-unmounted tree (testing-library cleanup); dispatching blur/focusout
    // on it walks a detached subtree and would corrupt the focus transition into the new element.
    if let Some(p) = prev.filter(|&p| with_tree(|t| is_connected(t, p)).unwrap_or(false)) {
        dispatch_synthetic(scope, p, "blur", false, false);
        dispatch_synthetic(scope, p, "focusout", true, false);
    }
    dispatch_synthetic(scope, h, "focus", false, false);
    dispatch_synthetic(scope, h, "focusin", true, false);
}
fn el_blur(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    if ACTIVE.with(|a| *a.borrow()) != Some(h) {
        return;
    }
    ACTIVE.with(|a| *a.borrow_mut() = None);
    dispatch_synthetic(scope, h, "blur", false, false);
    dispatch_synthetic(scope, h, "focusout", true, false);
}
/// `document.activeElement` → the focused element, or `<body>`.
fn get_active_element(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    // Real DOM resets focus to <body> when the active element is removed from the tree. If our
    // tracked handle is detached (e.g. the focused input of a prior, now-unmounted React tree —
    // testing-library cleanup leaves the JS ACTIVE pointer dangling), drop it so the stale node
    // never leaks into the next test's focus/userEvent logic.
    let active = ACTIVE.with(|a| *a.borrow());
    let active = active.filter(|&h| with_tree(|t| is_connected(t, h)).unwrap_or(false));
    if active.is_none() {
        ACTIVE.with(|a| *a.borrow_mut() = None);
    }
    let h = active.or_else(|| with_tree(|t| t.query_selector("body")).flatten());
    if let Some(h) = h {
        let node = wrap(scope, h);
        rv.set(node.into());
    }
}
/// Native `HTMLElement.click()` — dispatch a bubbling, cancelable click event so React's delegated
/// onClick at the root container fires. Tests call `el.click()` directly (no userEvent).
fn el_click(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    dispatch_synthetic(scope, h, "click", true, true);
}
fn el_scroll_into_view(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    receiver_handle(scope, &args, &[1]);
}

// Form-control methods with no headless effect (no constraint-validation UI, no text
// selection). They must EXIST though: React's hydration/commit calls `setCustomValidity`
// / `setSelectionRange` on inputs, and without the method the NON_MASKING interceptor
// returns `undefined` → "x is not a function" throws mid-hydration → React discards the
// SSR tree and client-renders the whole root (#425), losing server-rendered content.
fn el_noop(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let _dom = DomScope::for_object(scope, args.this());
    receiver_handle(scope, &args, &[1]);
}

/// Constraint-validation predicates (`checkValidity`/`reportValidity`) → always valid
/// (there is no interactive validation UI to fail against).
fn el_valid_true(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    if receiver_handle(scope, &args, &[1]).is_none() {
        return;
    }
    rv.set(v8::Boolean::new(scope, true).into());
}
fn el_get_bounding_client_rect(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    if receiver_handle(scope, &args, &[1]).is_none() {
        return;
    }
    let o = v8::Object::new(scope);
    for k in [
        "x", "y", "top", "left", "right", "bottom", "width", "height",
    ] {
        let key = v8::String::new(scope, k).unwrap();
        let zero = v8::Number::new(scope, 0.0);
        o.set(scope, key.into(), zero.into());
    }
    rv.set(o.into());
}

/// `append(...nodes)` / `prepend(...)` / `remove()` (DOM ChildNode/ParentNode mixins).
fn el_append(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(parent) = receiver_handle(scope, &args, &[1, 9, 11]) else {
        return;
    };
    for i in 0..args.length() {
        if let Some(child) = arg_handle(scope, &args, i) {
            with_tree_mut(|t| t.append_child(parent, child));
        }
    }
}
fn el_remove_self(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1, 3, 4, 8, 10]) else {
        return;
    };
    let parent = with_tree(|t| NodeRef::new(t, h).parent().map(|p| p.handle())).flatten();
    if let Some(p) = parent {
        with_tree_mut(|t| t.remove_child(p, h));
    }
}

// document native methods (reliable — no JS-bootstrap dependency).
fn doc_create_element_ns(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    if receiver_handle(scope, &args, &[9]).is_none() {
        return;
    }
    let ns = arg_str(scope, &args, 0).to_ascii_lowercase();
    let tag = arg_str(scope, &args, 1);
    // Map the namespace URI to rtdom's ns id (svg=1, math=2, html=0) so SVG/MathML elements keep
    // their namespace + case-preserved attributes (e.g. `viewBox`), unlike HTML which lowercases.
    let ns_id: u8 = if ns.contains("svg") {
        1
    } else if ns.contains("mathml") || ns.contains("math") {
        2
    } else {
        0
    };
    let made = with_tree_mut(|t| {
        if ns_id == 0 {
            t.create_element(&tag)
        } else {
            t.create_element_ns(&tag, Namespace::from_id(ns_id))
        }
    });
    if let Some(h) = made {
        let node = wrap(scope, h);
        rv.set(node.into());
    }
}
fn doc_create_fragment(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    if receiver_handle(scope, &args, &[9]).is_none() {
        return;
    }
    if let Some(h) = with_tree_mut(|t| t.create_document_fragment()) {
        let node = wrap(scope, h);
        rv.set(node.into());
    }
}
fn doc_create_comment(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    if receiver_handle(scope, &args, &[9]).is_none() {
        return;
    }
    let data = arg_str(scope, &args, 0);
    if let Some(h) = with_tree_mut(|t| t.create_comment(&data)) {
        let node = wrap(scope, h);
        rv.set(node.into());
    }
}

/// recursively clone a subtree, returning the new root handle.
fn clone_subtree(t: &mut Tree, h: Handle, deep: bool) -> Handle {
    let nt = t.node_type_id(h);
    let new_h = match nt {
        3 => t.create_text_node(&t.node_value(h).unwrap_or_default()),
        8 => t.create_comment(&t.node_value(h).unwrap_or_default()),
        11 => t.create_document_fragment(),
        _ => {
            let tag = t.tag_name(h).unwrap_or_else(|| "div".into());
            let e = t.create_element(&tag);
            for (k, v) in t.attributes(h) {
                t.set_attribute(e, &k, &v);
            }
            e
        }
    };
    if deep && nt != 3 && nt != 8 {
        for c in t.children(h) {
            let cc = clone_subtree(t, c, true);
            t.append_child(new_h, cc);
        }
    }
    new_h
}

/// `getRootNode()` → the topmost ancestor (the document root if connected). jest-dom's
/// `toBeInTheDocument` compares `ownerDocument === getRootNode()`.
fn el_get_root_node(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[]) else {
        return;
    };
    let composed = v8::Local::<v8::Object>::try_from(args.get(0))
        .ok()
        .is_some_and(|options| {
            let key = v8::String::new(scope, "composed").unwrap();
            options
                .get(scope, key.into())
                .is_some_and(|value| value.boolean_value(scope))
        });
    let root = with_tree(|t| {
        let mut cur = h;
        while let Some(parent) = t
            .parent(cur)
            .or_else(|| composed.then(|| t.shadow_host(cur)).flatten())
        {
            cur = parent;
        }
        cur
    });
    if let Some(r) = root {
        let node = wrap(scope, r);
        rv.set(node.into());
    }
}

fn el_clone_node(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[]) else {
        return;
    };
    if with_tree(|t| t.is_shadow_root(h)).unwrap_or(false) {
        throw_dom_error(scope, "NotSupportedError", "A shadow root cannot be cloned");
        return;
    }
    let deep = args.get(0).boolean_value(scope);
    if let Some(new_h) = with_tree_mut(|t| clone_subtree(t, h, deep)) {
        let node = wrap(scope, new_h);
        // Carry over the source's JS prototype so a cloned control element keeps its interface
        // accessors (type/value/checked/selected). createElement applies these protos, but cloneNode
        // bypasses it; without this a cloned <input type=date> loses date-value validation, which
        // userEvent's isValidDateOrTimeValue (clone, assign, check it stuck) relies on.
        let src_proto = args.this().get_prototype(scope);
        if let Some(p) = src_proto {
            node.set_prototype(scope, p);
        }
        let owner = DOM_CONTEXTS.with(|d| d.borrow().get(&DOM_ID.with(|id| id.get())).cloned());
        if let Some(owner) = owner {
            let context = v8::Local::new(scope, owner);
            let scope = &mut v8::ContextScope::new(scope, context);
            let global = context.global(scope);
            let name = v8::String::new(scope, "ts").unwrap();
            let key = v8::Symbol::for_key(scope, name);
            if let Some(namespace) = global
                .get(scope, key.into())
                .and_then(|v| v.to_object(scope))
            {
                let key = v8::String::new(scope, "cloneControlState").unwrap();
                if let Some(function) = namespace
                    .get(scope, key.into())
                    .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
                {
                    let deep = v8::Boolean::new(scope, deep);
                    function.call(
                        scope,
                        namespace.into(),
                        &[args.this().into(), node.into(), deep.into()],
                    );
                }
            }
        }
        rv.set(node.into());
    }
}

/// whether `h`'s root (walking parents) is the document root → attached to the document.
fn is_connected(t: &Tree, h: Handle) -> bool {
    let root = t.root();
    let mut cur = Some(h);
    while let Some(c) = cur {
        if c == root {
            return true;
        }
        cur = t.parent(c).or_else(|| t.shadow_host(c));
    }
    false
}

fn get_is_connected(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let c = with_tree(|t| is_connected(t, h)).unwrap_or(false);
    rv.set(v8::Boolean::new(scope, c).into());
}

/// `parent.contains(node)` — true if node === parent or a descendant.
fn el_contains_node(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(p) = receiver_handle(scope, &args, &[]) else {
        return;
    };
    let other = arg_handle(scope, &args, 0);
    let yes = match other {
        Some(o) => with_tree(|t| {
            let mut cur = Some(o);
            while let Some(c) = cur {
                if c == p {
                    return true;
                }
                cur = NodeRef::new(t, c).parent().map(|x| x.handle());
            }
            false
        })
        .unwrap_or(false),
        None => false,
    };
    rv.set(v8::Boolean::new(scope, yes).into());
}

/// `attributes` → array of `{ name, value, nodeName, nodeValue }` (NamedNodeMap-lite, indexable).
fn get_attributes(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let attrs = with_tree(|t| t.attributes(h)).unwrap_or_default();
    let arr = v8::Array::new(scope, attrs.len() as i32);
    for (i, (k, v)) in attrs.into_iter().enumerate() {
        let o = v8::Object::new(scope);
        for (pk, pv) in [
            ("name", k.as_str()),
            ("nodeName", k.as_str()),
            ("value", v.as_str()),
            ("nodeValue", v.as_str()),
        ] {
            let key = v8::String::new(scope, pk).unwrap();
            let val = v8::String::new(scope, pv).unwrap();
            o.set(scope, key.into(), val.into());
        }
        arr.set_index(scope, i as u32, o.into());
    }
    // NamedNodeMap accessors (`attributes.item(i)` / `attributes.namedItem(name)`).
    bind_method(scope, arr.into(), "item", nodelist_item);
    bind_method(scope, arr.into(), "namedItem", nodelist_named_item);
    rv.set(arr.into());
}

/// `dataset` → `{ camelCaseKey: value }` for each `data-*` attribute.
fn get_dataset(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let attrs = with_tree(|t| t.attributes(h)).unwrap_or_default();
    let o = v8::Object::new(scope);
    for (k, v) in attrs {
        if let Some(rest) = k.strip_prefix("data-") {
            // kebab → camelCase
            let mut camel = String::new();
            let mut up = false;
            for ch in rest.chars() {
                if ch == '-' {
                    up = true;
                } else if up {
                    camel.extend(ch.to_uppercase());
                    up = false;
                } else {
                    camel.push(ch);
                }
            }
            let key = v8::String::new(scope, &camel).unwrap();
            let val = v8::String::new(scope, &v).unwrap();
            o.set(scope, key.into(), val.into());
        }
    }
    rv.set(o.into());
}

/// `closest(selector)` → nearest ancestor (incl. self) that matches, or null.
fn el_closest(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    let sel = arg_str(scope, &args, 0);
    let found = with_tree(|t| {
        let mut cur = Some(h);
        while let Some(c) = cur {
            if t.node_type_id(c) == 1 && t.matches(c, &sel) {
                return Some(c);
            }
            cur = NodeRef::new(t, c).parent().map(|p| p.handle());
        }
        None
    })
    .flatten();
    let v = wrap_opt(scope, found);
    rv.set(v);
}

fn el_matches(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1]) else {
        return;
    };
    let sel = arg_str(scope, &args, 0);
    let m = with_tree(|t| t.matches(h, &sel)).unwrap_or(false);
    rv.set(v8::Boolean::new(scope, m).into());
}

fn el_contains(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[]) else {
        return;
    };
    let other = arg_handle(scope, &args, 0);
    let contains = match other {
        Some(o) => with_tree(|t| {
            // walk up from `o` to see if `h` is an ancestor (or equal).
            let mut cur = Some(o);
            while let Some(c) = cur {
                if c == h {
                    return true;
                }
                cur = NodeRef::new(t, c).parent().map(|p| p.handle());
            }
            false
        })
        .unwrap_or(false),
        None => false,
    };
    rv.set(v8::Boolean::new(scope, contains).into());
}

/// `node.hasChildNodes()` — true when the node has at least one child. Without this
/// native method the NON_MASKING interceptor returns `undefined`, so React's
/// hydration/commit path (which calls `hasChildNodes()` while diffing) throws
/// "hasChildNodes is not a function" and aborts the client-render fallback.
fn el_has_child_nodes(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[]) else {
        return;
    };
    let has = with_tree(|t| NodeRef::new(t, h).first_child().is_some()).unwrap_or(false);
    rv.set(v8::Boolean::new(scope, has).into());
}

/// `collection.namedItem(name)` for the array-like NodeList/HTMLCollection we hand
/// back — returns the first element whose `id` (or `name`) matches, else null. Bound
/// alongside `.item` so libs that call `namedItem` (React-DOM, form-collection code)
/// don't hit "namedItem is not a function".
fn nodelist_named_item(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let name = arg_str(scope, &args, 0);
    let this = args.this();
    let len = this
        .get(scope, v8::String::new(scope, "length").unwrap().into())
        .and_then(|v| v.uint32_value(scope))
        .unwrap_or(0);
    for i in 0..len {
        let Some(item) = this.get_index(scope, i) else {
            continue;
        };
        let Ok(obj) = v8::Local::<v8::Object>::try_from(item) else {
            continue;
        };
        for attr in ["id", "name"] {
            let key = v8::String::new(scope, attr).unwrap();
            if let Some(val) = obj.get(scope, key.into()) {
                if val.is_string() && val.to_rust_string_lossy(scope) == name {
                    rv.set(item);
                    return;
                }
            }
        }
    }
    rv.set_null();
}

fn el_query_selector_all(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[1, 11]) else {
        return;
    };
    let sel = arg_str(scope, &args, 0);
    let handles = with_tree(|t| {
        NodeRef::new(t, h)
            .query_selector_all(&sel)
            .into_iter()
            .map(|n| n.handle())
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    let arr = v8::Array::new(scope, handles.len() as i32);
    for (i, hh) in handles.into_iter().enumerate() {
        let node = wrap(scope, hh);
        arr.set_index(scope, i as u32, node.into());
    }
    bind_method(scope, arr.into(), "item", nodelist_item);
    bind_method(scope, arr.into(), "namedItem", nodelist_named_item);
    rv.set(arr.into());
}

// ---- element accessors -----------------------------------------------------------------------
// Getter sig: (scope, name, PropertyCallbackArguments, ReturnValue<Value>).
// Setter sig: (scope, name, Local<Value>, PropertyCallbackArguments, ReturnValue<()>).

fn get_tag_name(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let tag = with_tree(|t| t.tag_name(h))
        .flatten()
        .unwrap_or_default()
        .to_uppercase();
    rv.set(v8::String::new(scope, &tag).unwrap().into());
}

fn get_text_content(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let txt = with_tree(|t| t.text_content(h)).unwrap_or_default();
    rv.set(v8::String::new(scope, &txt).unwrap().into());
}

fn set_text_content(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    value: v8::Local<v8::Value>,
    args: v8::PropertyCallbackArguments,
    _rv: v8::ReturnValue<()>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let txt = value.to_rust_string_lossy(scope);
    with_tree_mut(|t| t.set_text_content(h, &txt));
}

fn get_id(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let id = with_tree(|t| t.get_attribute(h, "id").map(|s| s.to_string()))
        .flatten()
        .unwrap_or_default();
    rv.set(v8::String::new(scope, &id).unwrap().into());
}

fn set_id(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    value: v8::Local<v8::Value>,
    args: v8::PropertyCallbackArguments,
    _rv: v8::ReturnValue<()>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let v = value.to_rust_string_lossy(scope);
    with_tree_mut(|t| t.set_attribute(h, "id", &v));
}

fn get_class_name(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let c = with_tree(|t| t.get_attribute(h, "class").map(|s| s.to_string()))
        .flatten()
        .unwrap_or_default();
    rv.set(v8::String::new(scope, &c).unwrap().into());
}

fn set_class_name(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    value: v8::Local<v8::Value>,
    args: v8::PropertyCallbackArguments,
    _rv: v8::ReturnValue<()>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let v = value.to_rust_string_lossy(scope);
    with_tree_mut(|t| t.set_attribute(h, "class", &v));
}

// `element.contentEditable` IDL property — reflects the `contenteditable` content attribute as the
// enumerated string 'true' | 'false' | 'plaintext-only' | 'inherit'. Lexical reads this property
// (NOT the attribute) to decide the root is editable and to wire up its beforeinput pipeline; with
// it missing (undefined) Lexical treats the editor as non-editable and never emits onChange.
fn get_content_editable(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let attr = with_tree(|t| attr_get(t, h, "contenteditable")).flatten();
    let out = match attr.as_deref() {
        Some("") | Some("true") => "true",
        Some("false") => "false",
        Some("plaintext-only") => "plaintext-only",
        _ => "inherit",
    };
    rv.set(v8::String::new(scope, out).unwrap().into());
}

fn set_content_editable(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    value: v8::Local<v8::Value>,
    args: v8::PropertyCallbackArguments,
    _rv: v8::ReturnValue<()>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let v = value.to_rust_string_lossy(scope);
    with_tree_mut(|t| t.set_attribute(h, "contenteditable", &v));
}

// `element.isContentEditable` — true when the element itself is editable (contenteditable=""|"true")
// or it inherits editability from an ancestor.
fn get_is_content_editable(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(mut h) = handle_of(scope, args.holder()) else {
        return;
    };
    let mut editable = false;
    loop {
        match with_tree(|t| attr_get(t, h, "contenteditable"))
            .flatten()
            .as_deref()
        {
            Some("") | Some("true") | Some("plaintext-only") => {
                editable = true;
                break;
            }
            Some("false") => {
                editable = false;
                break;
            }
            _ => {}
        }
        match with_tree(|t| {
            NodeRef::new(t, h)
                .parent()
                .map(|p| p.handle())
                .filter(|&ph| t.node_type_id(ph) == 1)
        })
        .flatten()
        {
            Some(p) => h = p,
            None => break,
        }
    }
    rv.set(v8::Boolean::new(scope, editable).into());
}

fn get_parent_node(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let found = with_tree(|t| NodeRef::new(t, h).parent().map(|x| x.handle())).flatten();
    let v = wrap_opt(scope, found);
    rv.set(v);
}

fn get_first_child(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let found = with_tree(|t| NodeRef::new(t, h).first_child().map(|x| x.handle())).flatten();
    let v = wrap_opt(scope, found);
    rv.set(v);
}

fn get_next_sibling(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let found = with_tree(|t| NodeRef::new(t, h).next_sibling().map(|x| x.handle())).flatten();
    let v = wrap_opt(scope, found);
    rv.set(v);
}

// ---- events: real dispatch (addEventListener/removeEventListener/dispatchEvent) --------------
// React attaches one delegated listener at the root container and relies on bubbling; fireEvent /
// userEvent call `node.dispatchEvent(new Event(type, {bubbles:true}))`. So we store JS listeners
// per (handle, type) and walk target→ancestors firing them, with target/currentTarget set on the
// event object (the `Event` class lives in the JS bootstrap).

thread_local! {
    /// (node handle, event type) → registered JS listeners (function, capture-phase flag).
    static LISTENERS: RefCell<HashMap<(Handle, String), Vec<(v8::Global<v8::Function>, bool)>>> = RefCell::new(HashMap::new());
}

/// useCapture from the 3rd addEventListener arg (boolean, or `{capture: true}` options object).
fn arg_capture(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> bool {
    let v = args.get(2);
    if v.is_boolean() {
        return v.boolean_value(scope);
    }
    // 2-arg addEventListener leaves arg 2 undefined; to_object() on null/undefined THROWS
    // ("Cannot convert undefined or null to object"). Guard before converting.
    if v.is_null_or_undefined() {
        return false;
    }
    if let Some(o) = v.to_object(scope) {
        if let Some(k) = v8::String::new(scope, "capture") {
            if let Some(c) = o.get(scope, k.into()) {
                return c.boolean_value(scope);
            }
        }
    }
    false
}

fn el_add_event_listener(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[]) else {
        return;
    };
    let ty = arg_str(scope, &args, 0);
    let Some(f) = v8::Local::<v8::Function>::try_from(args.get(1)).ok() else {
        return;
    };
    let capture = arg_capture(scope, &args);
    let g = v8::Global::new(scope, f);
    LISTENERS.with(|m| {
        m.borrow_mut()
            .entry((h, ty))
            .or_default()
            .push((g, capture))
    });
}

fn el_remove_event_listener(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(h) = receiver_handle(scope, &args, &[]) else {
        return;
    };
    let ty = arg_str(scope, &args, 0);
    let Some(f) = v8::Local::<v8::Function>::try_from(args.get(1)).ok() else {
        return;
    };
    LISTENERS.with(|m| {
        if let Some(v) = m.borrow_mut().get_mut(&(h, ty)) {
            v.retain(|(g, _)| !v8::Local::new(scope, g).eq(&f));
        }
    });
}

struct EventRecordAccessor(v8::Global<v8::Function>);
struct HostEventDispatcher(v8::Global<v8::Function>);

fn event_record<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    event: v8::Local<v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    let context = event.get_creation_context(scope)?;
    let accessor = context.get_slot::<EventRecordAccessor>()?;
    let function = v8::Local::new(scope, &accessor.0);
    let receiver = v8::undefined(scope).into();
    v8::Local::<v8::Object>::try_from(function.call(scope, receiver, &[event.into()])?).ok()
}

fn lookup_event_record(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let Ok(event) = v8::Local::<v8::Object>::try_from(args.get(0)) else {
        return;
    };
    if let Some(record) = event_record(scope, event) {
        rv.set(record.into());
    }
}

/// Mark an engine-created event before callbacks inspect it. Script dispatch
/// always clears trust; host dispatch uses a separate private entry point.
pub fn mark_event_trusted(
    scope: &mut v8::PinScope,
    event: v8::Local<v8::Object>,
    timestamp: Option<f64>,
) {
    if let Some(record) = event_record(scope, event) {
        let yes = v8::Boolean::new(scope, true);
        set_prop(scope, record, "trusted", yes.into());
        if let Some(timestamp) = timestamp {
            let timestamp = v8::Number::new(scope, timestamp);
            set_prop(scope, record, "timeStamp", timestamp.into());
        }
    }
}

/// Dispatch an engine-originated event without invoking a guest override of
/// dispatchEvent. Only the host can obtain this dispatch capability.
pub fn dispatch_host_event(
    scope: &mut v8::PinScope,
    event: v8::Local<v8::Object>,
    target: v8::Local<v8::Object>,
) {
    if handle_of(scope, target).is_some() {
        let host = v8::Boolean::new(scope, true);
        if let Some(function) = v8::FunctionTemplate::builder(el_dispatch_event)
            .data(host.into())
            .constructor_behavior(v8::ConstructorBehavior::Throw)
            .build(scope)
            .get_function(scope)
        {
            function.call(scope, target.into(), &[event.into()]);
        }
    } else if let Some(context) = target.get_creation_context(scope) {
        if let Some(dispatcher) = context.get_slot::<HostEventDispatcher>() {
            let function = v8::Local::new(scope, &dispatcher.0);
            let receiver = v8::undefined(scope).into();
            function.call(scope, receiver, &[event.into(), target.into()]);
        }
    }
}

/// Enter or leave an engine-delivered callback on a non-DOM target.
pub fn event_callback_state(
    scope: &mut v8::PinScope,
    event: v8::Local<v8::Object>,
    target: v8::Local<v8::Object>,
    active: bool,
) {
    let Some(record) = event_record(scope, event) else {
        return;
    };
    let flag = v8::Boolean::new(scope, active);
    let phase = v8::Integer::new(scope, if active { 2 } else { 0 });
    let current: v8::Local<v8::Value> = if active {
        target.into()
    } else {
        v8::null(scope).into()
    };
    let path = v8::Array::new(scope, i32::from(active));
    if active {
        path.set_index(scope, 0, target.into());
        set_prop(scope, record, "target", target.into());
        let trusted = v8::Boolean::new(scope, true);
        set_prop(scope, record, "trusted", trusted.into());
    }
    set_prop(scope, record, "currentTarget", current);
    set_prop(scope, record, "eventPhase", phase.into());
    set_prop(scope, record, "path", path.into());
    set_prop(scope, record, "dispatching", flag.into());
    if !active {
        for key in ["stop", "stopImmediate"] {
            set_prop(scope, record, key, flag.into());
        }
    }
}

fn el_dispatch_event(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let Some(target) = receiver_handle(scope, &args, &[]) else {
        rv.set(v8::Boolean::new(scope, true).into());
        return;
    };
    let event = args.get(0);
    if event.is_null_or_undefined() {
        return;
    }
    let Some(event_obj) = event.to_object(scope) else {
        return;
    };
    let Some(record) = event_record(scope, event_obj) else {
        let message = v8::String::new(scope, "Argument must be an Event").unwrap();
        let error = v8::Exception::type_error(scope, message);
        scope.throw_exception(error);
        return;
    };
    let ty = get_str_prop(scope, record, "type").unwrap_or_default();
    if ty.is_empty() || get_bool_prop(scope, record, "dispatching") {
        let message = v8::String::new(
            scope,
            "The event is already being dispatched or uninitialized.",
        )
        .unwrap();
        let name = v8::String::new(scope, "InvalidStateError").unwrap();
        let global = scope.get_current_context().global(scope);
        let key = v8::String::new(scope, "DOMException").unwrap();
        if let Some(ctor) = global
            .get(scope, key.into())
            .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
        {
            if let Some(error) = ctor.new_instance(scope, &[message.into(), name.into()]) {
                scope.throw_exception(error.into());
            }
        }
        return;
    }
    let bubbles = get_bool_prop(scope, record, "bubbles");
    let trusted = args.data().is_true();
    let yes = v8::Boolean::new(scope, true);
    let no = v8::Boolean::new(scope, false);
    let trusted = v8::Boolean::new(scope, trusted);
    set_prop(scope, record, "dispatching", yes.into());
    set_prop(scope, record, "trusted", trusted.into());
    let target_node = wrap(scope, target);
    set_prop(scope, record, "target", target_node.into());

    // PRE-CLICK ACTIVATION: a checkbox/radio toggles `checked` BEFORE the click listeners run (per
    // the activation-behavior spec), so React's ChangeEventPlugin — which detects checkbox change on
    // the `click` event by reading node.checked — sees the new value. Reverted below if the click is
    // canceled. `change`/`input` DOM events fire afterward (also below).
    let click_check = if ty == "click" {
        checkable_kind(target)
    } else {
        None
    };
    let check_was = click_check.map(|_| get_bool_prop(scope, target_node, "checked"));
    if let (Some(kind), Some(was)) = (click_check, check_was) {
        let next = if kind == CheckKind::Radio { true } else { !was };
        let b = v8::Boolean::new(scope, next);
        set_prop(scope, target_node, "checked", b.into());
    }

    // propagation path: ancestors (root … parent), then the target. Bubbling reverses it.
    let mut ancestors = Vec::new();
    let mut cur = with_tree(|t| NodeRef::new(t, target).parent().map(|p| p.handle())).flatten();
    while let Some(c) = cur {
        ancestors.push(c);
        cur = with_tree(|t| NodeRef::new(t, c).parent().map(|p| p.handle())).flatten();
    }

    let path = v8::Array::new(scope, (ancestors.len() + 1) as i32);
    path.set_index(scope, 0, target_node.into());
    for (index, &handle) in ancestors.iter().enumerate() {
        let node = wrap(scope, handle);
        path.set_index(scope, (index + 1) as u32, node.into());
    }
    set_prop(scope, record, "path", path.into());

    // CAPTURE phase: root → target's parent (capture listeners), then TARGET (both), then BUBBLE:
    // target's parent → root (non-capture listeners). React 18 registers some delegated root
    // listeners in capture phase, so a bubble-only dispatch would miss them.
    let mut capture_path: Vec<Handle> = ancestors.iter().rev().copied().collect();
    let bubble_path: Vec<Handle> = if bubbles {
        ancestors.clone()
    } else {
        Vec::new()
    };
    capture_path.push(target); // target fires in both phases (handled by the at_target flag below)

    let fire =
        |scope: &mut v8::PinScope, node_h: Handle, want_capture: bool, at_target: bool| -> bool {
            if get_bool_prop(scope, record, "stop") {
                return false;
            }
            let listeners: Vec<(v8::Global<v8::Function>, bool)> = LISTENERS.with(|m| {
                m.borrow()
                    .get(&(node_h, ty.clone()))
                    .cloned()
                    .unwrap_or_default()
            });
            let cur_node = wrap(scope, node_h);
            set_prop(scope, record, "currentTarget", cur_node.into());
            let phase = v8::Integer::new(
                scope,
                if at_target {
                    2
                } else if want_capture {
                    1
                } else {
                    3
                },
            );
            set_prop(scope, record, "eventPhase", phase.into());
            for (g, capture) in listeners {
                // at the target, both capture and bubble listeners fire; elsewhere only the phase's.
                if !at_target && capture != want_capture {
                    continue;
                }
                if get_bool_prop(scope, record, "stopImmediate") {
                    break;
                }
                let f = v8::Local::new(scope, &g);
                let recv: v8::Local<v8::Value> = cur_node.into();
                f.call(scope, recv, &[event]);
            }

            true
        };

    // capture: root → parent
    for &node_h in capture_path
        .iter()
        .take(capture_path.len().saturating_sub(1))
    {
        if !fire(scope, node_h, true, false) {
            break;
        }
    }
    // at target (both phases)
    fire(scope, target, false, true);
    // bubble: parent → root
    for node_h in bubble_path {
        if !fire(scope, node_h, false, false) {
            break;
        }
    }
    let not_prevented = !get_bool_prop(scope, record, "defaultPrevented");
    let null = v8::null(scope);
    let phase = v8::Integer::new(scope, 0);
    let path = v8::Array::new(scope, 0);
    set_prop(scope, record, "currentTarget", null.into());
    set_prop(scope, record, "eventPhase", phase.into());
    set_prop(scope, record, "path", path.into());
    for key in ["dispatching", "stop", "stopImmediate"] {
        set_prop(scope, record, key, no.into());
    }

    // DEFAULT ACTION: a non-prevented click on a submit control fires the form's `submit` event
    // (jsdom does this natively; userEvent/fireEvent.click rely on it for form submission).
    if not_prevented && ty == "click" && is_submit_control(target) {
        if let Some(form) = closest_form(target) {
            dispatch_synthetic(scope, form, "submit", true, true);
        }
    }

    // Finish the checkbox/radio activation: a canceled click reverts the pre-toggle; otherwise fire
    // the DOM `input` + `change` events (jsdom does this; some libs listen for them directly).
    if let (Some(_kind), Some(was)) = (click_check, check_was) {
        if not_prevented {
            dispatch_synthetic(scope, target, "input", true, false);
            dispatch_synthetic(scope, target, "change", true, false);
        } else {
            let el = wrap(scope, target);
            let b = v8::Boolean::new(scope, was);
            set_prop(scope, el, "checked", b.into());
        }
    }

    rv.set(v8::Boolean::new(scope, not_prevented).into());
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum CheckKind {
    Checkbox,
    Radio,
}

/// `<input type=checkbox|radio>` → its kind, else None (drives the click toggle default action).
fn checkable_kind(h: Handle) -> Option<CheckKind> {
    with_tree(|t| {
        if t.node_type_id(h) != 1
            || t.tag_name(h).unwrap_or_default().to_ascii_uppercase() != "INPUT"
        {
            return None;
        }
        match t
            .get_attribute(h, "type")
            .map(|s| s.to_ascii_lowercase())
            .as_deref()
        {
            Some("checkbox") => Some(CheckKind::Checkbox),
            Some("radio") => Some(CheckKind::Radio),
            _ => None,
        }
    })
    .flatten()
}

/// A `<button>` (type submit or unset — submit is the default) or `<input type=submit>`.
fn is_submit_control(h: Handle) -> bool {
    with_tree(|t| {
        if t.node_type_id(h) != 1 {
            return false;
        }
        let tag = t.tag_name(h).unwrap_or_default().to_ascii_uppercase();
        let ty = t.get_attribute(h, "type").map(|s| s.to_ascii_lowercase());
        match tag.as_str() {
            "BUTTON" => ty.as_deref().map(|t| t == "submit").unwrap_or(true),
            "INPUT" => ty.as_deref() == Some("submit"),
            _ => false,
        }
    })
    .unwrap_or(false)
}

/// nearest ancestor `<form>` (incl. self).
fn closest_form(h: Handle) -> Option<Handle> {
    with_tree(|t| {
        let mut cur = Some(h);
        while let Some(c) = cur {
            if t.tag_name(c)
                .map(|s| s.eq_ignore_ascii_case("form"))
                .unwrap_or(false)
            {
                return Some(c);
            }
            cur = NodeRef::new(t, c).parent().map(|p| p.handle());
        }
        None
    })
    .flatten()
}

/// Construct `new Event(type, {bubbles, cancelable})` and dispatch it on `target` via its native
/// dispatchEvent (re-enters the event system → fires listeners).
fn dispatch_synthetic(
    scope: &mut v8::PinScope,
    target: Handle,
    ty: &str,
    bubbles: bool,
    cancelable: bool,
) {
    let global = scope.get_current_context().global(scope);
    let Some(ctor) = v8::String::new(scope, "Event")
        .and_then(|k| global.get(scope, k.into()))
        .and_then(|v| v8::Local::<v8::Function>::try_from(v).ok())
    else {
        return;
    };
    let init = v8::Object::new(scope);
    for (k, val) in [("bubbles", bubbles), ("cancelable", cancelable)] {
        if let Some(key) = v8::String::new(scope, k) {
            let b = v8::Boolean::new(scope, val);
            init.set(scope, key.into(), b.into());
        }
    }
    let ty_str = v8::String::new(scope, ty).unwrap();
    let Some(ev) = ctor.new_instance(scope, &[ty_str.into(), init.into()]) else {
        return;
    };
    let node = wrap(scope, target);
    dispatch_host_event(scope, ev, node);
}

// small prop helpers
fn get_str_prop(
    scope: &mut v8::PinScope,
    obj: v8::Local<v8::Object>,
    name: &str,
) -> Option<String> {
    let key = v8::String::new(scope, name)?;
    let v = obj.get(scope, key.into())?;
    if v.is_string() {
        Some(v.to_rust_string_lossy(scope))
    } else {
        None
    }
}
fn get_bool_prop(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>, name: &str) -> bool {
    v8::String::new(scope, name)
        .and_then(|k| obj.get(scope, k.into()))
        .map(|v| v.boolean_value(scope))
        .unwrap_or(false)
}
fn set_prop(
    scope: &mut v8::PinScope,
    obj: v8::Local<v8::Object>,
    name: &str,
    val: v8::Local<v8::Value>,
) {
    if let Some(k) = v8::String::new(scope, name) {
        obj.set(scope, k.into(), val);
    }
}

/// `constructor` → the named global DOM constructor for this node's type, so libraries keying on
/// `node.constructor.name` (pretty-format's DOMElementFilter regex `/^((HTML|SVG)\w*)?Element$/`)
/// treat it as a DOM node, not a plain Object they'd recurse into and crash on.
fn get_constructor(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let (nt, tag) = with_tree(|t| (t.node_type_id(h), t.tag_name(h))).unwrap_or((1, None));
    let cname = match nt {
        3 => "Text",
        8 => "Comment",
        9 => "HTMLDocument",
        11 => "DocumentFragment",
        1 => match tag.as_deref().map(|s| s.to_ascii_uppercase()).as_deref() {
            Some("INPUT") => "HTMLInputElement",
            Some("TEXTAREA") => "HTMLTextAreaElement",
            Some("SELECT") => "HTMLSelectElement",
            Some("OPTION") => "HTMLOptionElement",
            // Extra elements whose ctor globals the bootstrap registers — keep `.constructor.name`
            // specific (not generic HTMLElement) so libs keying on it resolve the right interface.
            Some("DIALOG") => "HTMLDialogElement",
            Some("DATALIST") => "HTMLDataListElement",
            Some("FIELDSET") => "HTMLFieldSetElement",
            Some("LEGEND") => "HTMLLegendElement",
            Some("OL") => "HTMLOListElement",
            Some("DL") => "HTMLDListElement",
            Some("PRE") => "HTMLPreElement",
            Some("TR") => "HTMLTableRowElement",
            Some("TD") | Some("TH") => "HTMLTableCellElement",
            Some("THEAD") | Some("TBODY") | Some("TFOOT") => "HTMLTableSectionElement",
            Some("COL") | Some("COLGROUP") => "HTMLTableColElement",
            Some("CAPTION") => "HTMLTableCaptionElement",
            Some("PROGRESS") => "HTMLProgressElement",
            Some("METER") => "HTMLMeterElement",
            Some("DETAILS") => "HTMLDetailsElement",
            Some("PICTURE") => "HTMLPictureElement",
            Some("SOURCE") => "HTMLSourceElement",
            Some("VIDEO") => "HTMLVideoElement",
            Some("AUDIO") => "HTMLAudioElement",
            Some("TEMPLATE") => "HTMLTemplateElement",
            Some("SLOT") => "HTMLSlotElement",
            Some("BODY") => "HTMLBodyElement",
            Some("HTML") => "HTMLHtmlElement",
            Some("HEAD") => "HTMLHeadElement",
            Some("META") => "HTMLMetaElement",
            Some("LINK") => "HTMLLinkElement",
            Some("TITLE") => "HTMLTitleElement",
            Some("BASE") => "HTMLBaseElement",
            Some("BR") => "HTMLBRElement",
            Some("HR") => "HTMLHRElement",
            Some("OPTGROUP") => "HTMLOptGroupElement",
            Some("MAP") => "HTMLMapElement",
            Some("AREA") => "HTMLAreaElement",
            Some("OBJECT") => "HTMLObjectElement",
            Some("EMBED") => "HTMLEmbedElement",
            Some("OUTPUT") => "HTMLOutputElement",
            Some("MENU") => "HTMLMenuElement",
            Some("DATA") => "HTMLDataElement",
            Some("TIME") => "HTMLTimeElement",
            Some("Q") | Some("BLOCKQUOTE") => "HTMLQuoteElement",
            _ => "HTMLElement",
        },
        _ => "HTMLElement",
    };
    let global = scope.get_current_context().global(scope);
    if let Some(key) = v8::String::new(scope, cname) {
        if let Some(ctor) = global.get(scope, key.into()) {
            if !ctor.is_undefined() {
                rv.set(ctor);
            }
        }
    }
}

fn get_node_type(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let nt = with_tree(|t| t.node_type_id(h)).unwrap_or(1);
    rv.set(v8::Integer::new(scope, nt as i32).into());
}

/// `data` / `nodeValue` → text of a text/comment node (null for elements). Serializers call
/// `.replace` on these, so they must be strings (not undefined) for text nodes.
fn get_node_data(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let nt = with_tree(|t| t.node_type_id(h)).unwrap_or(1);
    if nt == 3 || nt == 8 {
        let data = with_tree(|t| t.node_value(h)).flatten().unwrap_or_default();
        rv.set(v8::String::new(scope, &data).unwrap().into());
    } else {
        rv.set(v8::null(scope).into());
    }
}
fn set_node_data(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    value: v8::Local<v8::Value>,
    args: v8::PropertyCallbackArguments,
    _rv: v8::ReturnValue<()>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let data = value.to_rust_string_lossy(scope);
    with_tree_mut(|t| t.set_text_content(h, &data));
}

// `Element.localName` — the lowercase local name (rtdom stores tags lowercased). Non-elements have
// no local name (null). React/Next devtools read node.localName in places.
fn get_local_name(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    match with_tree(|t| t.local_name(h).map(|s| s.to_string())).flatten() {
        Some(ln) => rv.set(v8::String::new(scope, &ln).unwrap().into()),
        None => rv.set(v8::null(scope).into()),
    }
}

fn get_node_name(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let (nt, tag) = with_tree(|t| (t.node_type_id(h), t.tag_name(h))).unwrap_or((1, None));
    let name = match nt {
        3 => "#text".to_string(),
        8 => "#comment".to_string(),
        9 => "#document".to_string(),
        11 => "#document-fragment".to_string(),
        _ => tag.unwrap_or_default().to_uppercase(),
    };
    rv.set(v8::String::new(scope, &name).unwrap().into());
}

fn get_child_nodes(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    let kids = with_tree(|t| t.children(h)).unwrap_or_default();
    let arr = v8::Array::new(scope, kids.len() as i32);
    for (i, k) in kids.into_iter().enumerate() {
        let node = wrap(scope, k);
        arr.set_index(scope, i as u32, node.into());
    }
    bind_method(scope, arr.into(), "item", nodelist_item);
    bind_method(scope, arr.into(), "namedItem", nodelist_named_item);
    rv.set(arr.into());
}

/// `NodeList.item(i)` → the i-th node, or null when out of range. Bound on every node-collection
/// array we hand back (childNodes / querySelectorAll / getElementsBy*), so libs calling `.item(i)`
/// instead of `[i]` (e.g. React-DOM's child reconciliation) don't crash.
fn nodelist_item(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let i = args.get(0).number_value(scope).unwrap_or(f64::NAN);
    if i.is_nan() || i < 0.0 {
        rv.set_null();
        return;
    }
    match args.this().get_index(scope, i as u32) {
        Some(v) if !v.is_undefined() => rv.set(v),
        _ => rv.set_null(),
    }
}

fn get_owner_document(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(handle) = handle_of(scope, args.holder()) else {
        return;
    };
    if with_tree(|tree| tree.node_type_id(handle)) == Some(9) {
        rv.set_null();
        return;
    }
    if let Some(root) = with_tree(|t| t.root()) {
        let doc = wrap(scope, root);
        rv.set(doc.into());
    }
}

thread_local! {
    /// per-element `style` object (a plain JS object React reads/writes); cleared per file.
    static STYLE: RefCell<HashMap<Handle, v8::Global<v8::Object>>> = RefCell::new(HashMap::new());
}

/// `element.style` → a cached plain JS object (CSSStyleDeclaration-lite). React sets `style.color`
/// etc. as own props; `el.style.x === x` works. (Full CSSOM / attribute sync is a later refinement.)
fn get_style(
    scope: &mut v8::PinScope,
    _name: v8::Local<v8::Name>,
    args: v8::PropertyCallbackArguments,
    mut rv: v8::ReturnValue<v8::Value>,
) {
    let _dom = DomScope::for_object(scope, args.holder());
    let Some(h) = handle_of(scope, args.holder()) else {
        return;
    };
    if let Some(g) = STYLE.with(|s| s.borrow().get(&h).cloned()) {
        let o = v8::Local::new(scope, &g);
        rv.set(o.into());
        return;
    }
    let obj = v8::Object::new(scope);
    // setProperty/getPropertyValue/removeProperty so libs using the CSSOM methods don't crash.
    bind_method(scope, obj, "setProperty", style_set_property);
    bind_method(scope, obj, "getPropertyValue", style_get_property);
    bind_method(
        scope,
        obj,
        "getPropertyPriority",
        style_get_property_priority,
    );
    bind_method(scope, obj, "removeProperty", style_remove_property);
    // stash the element handle so the proxy set-trap can reflect to the `style` content attribute.
    if let Some(hk) = v8::String::new(scope, "__h") {
        let hv = v8::Number::new(scope, f64::from(h.raw()));
        obj.set(scope, hk.into(), hv.into());
    }
    // Wrap in a Proxy whose set-trap reflects every style mutation (React writes el.style.x = v) to
    // the `style` content attribute in the tree, so attribute selectors (`[style]`) and
    // getAttribute('style') see it. Reads/methods pass through to the target.
    let handler = v8::Object::new(scope);
    bind_method(scope, handler, "set", style_proxy_set);
    let proxy = v8::Proxy::new(scope, obj, handler)
        .map(|p| p.into())
        .unwrap_or(obj);
    let g = v8::Global::new(scope, proxy);
    STYLE.with(|s| {
        s.borrow_mut().insert(h, g);
    });
    rv.set(proxy.into());
}

/// Proxy `set` trap for `element.style`: set on the target, then serialize the declaration to the
/// element's `style` content attribute so it's observable via attribute selectors / getAttribute.
fn style_proxy_set(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let target = match v8::Local::<v8::Object>::try_from(args.get(0)) {
        Ok(o) => o,
        Err(_) => {
            rv.set_bool(false);
            return;
        }
    };
    let key = args.get(1);
    let mut value = args.get(2);
    // CSSOM values are always strings (jest-dom assigns numbers like `style.fontWeight = 600`; a
    // browser stores "600"). Coerce to string, then normalize hex colors to rgb()/rgba() so
    // el.style.X reads match getComputedStyle's normalized cascade values for jest-dom toHaveStyle.
    if value.is_number() || value.is_boolean() {
        if let Some(s) = value.to_string(scope) {
            value = s.into();
        }
    }
    if value.is_string() {
        if let Some(n) = normalize_css_color(&value.to_rust_string_lossy(scope)) {
            if let Some(s) = v8::String::new(scope, &n) {
                value = s.into();
            }
        }
    }
    target.set(scope, key, value);
    // element handle stashed as __h
    let h = v8::String::new(scope, "__h")
        .and_then(|k| target.get(scope, k.into()))
        .and_then(|v| v.number_value(scope))
        .map(|n| Handle::from_raw(n as u32));
    if let Some(h) = h {
        let css = serialize_style(scope, target);
        with_tree_mut(|t| {
            if css.is_empty() {
                t.remove_attribute(h, "style");
            } else {
                t.set_attribute(h, "style", &css);
            }
        });
    }
    rv.set_bool(true);
}

/// Normalize a `#rgb`/`#rrggbb`/`#rrggbbaa` hex color to `rgb(r, g, b)` / `rgba(r, g, b, a)` (the form
/// a browser's CSS parser stores), so a hex from one source matches a hex from another. Returns None
/// for non-hex values (left untouched).
fn normalize_css_color(v: &str) -> Option<String> {
    let s = v.trim();
    let hex = s.strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let h = |a: u8, b: u8| -> Option<u8> {
        u8::from_str_radix(&format!("{}{}", a as char, b as char), 16).ok()
    };
    let b = hex.as_bytes();
    match hex.len() {
        3 => {
            let (r, g, bl) = (h(b[0], b[0])?, h(b[1], b[1])?, h(b[2], b[2])?);
            Some(format!("rgb({}, {}, {})", r, g, bl))
        }
        6 => {
            let (r, g, bl) = (h(b[0], b[1])?, h(b[2], b[3])?, h(b[4], b[5])?);
            Some(format!("rgb({}, {}, {})", r, g, bl))
        }
        8 => {
            let (r, g, bl, a) = (
                h(b[0], b[1])?,
                h(b[2], b[3])?,
                h(b[4], b[5])?,
                h(b[6], b[7])?,
            );
            let af = (a as f64 / 255.0 * 100.0).round() / 100.0;
            Some(format!("rgba({}, {}, {}, {})", r, g, bl, af))
        }
        _ => None,
    }
}

/// Serialize a style declaration object's own string properties to `prop: value;` cssText (camelCase
/// names → kebab-case), skipping internals (`__*`) and methods.
fn serialize_style(scope: &mut v8::PinScope, obj: v8::Local<v8::Object>) -> String {
    let mut out = String::new();
    let Some(names) = obj.get_own_property_names(scope, Default::default()) else {
        return out;
    };
    for i in 0..names.length() {
        let Some(k) = names.get_index(scope, i) else {
            continue;
        };
        let kn = k.to_rust_string_lossy(scope);
        if kn.starts_with("__") {
            continue;
        }
        let Some(v) = obj.get(scope, k) else { continue };
        if v.is_function() || v.is_undefined() || v.is_null() {
            continue;
        }
        let vs = v.to_rust_string_lossy(scope);
        if vs.is_empty() {
            continue;
        }
        // camelCase -> kebab-case
        let mut prop = String::with_capacity(kn.len() + 4);
        for ch in kn.chars() {
            if ch.is_ascii_uppercase() {
                prop.push('-');
                prop.push(ch.to_ascii_lowercase());
            } else {
                prop.push(ch);
            }
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&prop);
        out.push_str(": ");
        out.push_str(&vs);
        out.push(';');
    }
    out
}

fn style_set_property(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let name = arg_str(scope, &args, 0);
    let value = args.get(1);
    if let Some(key) = v8::String::new(scope, &name) {
        args.this().set(scope, key.into(), value);
    }
    // 3rd arg = priority ("important" | ""). Track it in a non-style `__prio` map for
    // getPropertyPriority (the `__`-prefixed key is skipped by getComputedStyle's inline overlay).
    let prio = arg_str(scope, &args, 2);
    if let Some(pmap) = style_prio_map(scope, args.this()) {
        if let Some(nk) = v8::String::new(scope, &name) {
            if prio.eq_ignore_ascii_case("important") {
                let pv = v8::String::new(scope, "important").unwrap();
                pmap.set(scope, nk.into(), pv.into());
            } else {
                pmap.delete(scope, nk.into());
            }
        }
    }
}

/// the `__prio` object on a style declaration (created on first use) holding per-property priorities.
fn style_prio_map<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    style: v8::Local<v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    let key = v8::String::new(scope, "__prio")?;
    let existing = style.get(scope, key.into())?;
    if let Ok(o) = v8::Local::<v8::Object>::try_from(existing) {
        return Some(o);
    }
    let o = v8::Object::new(scope);
    style.set(scope, key.into(), o.into());
    Some(o)
}

fn style_get_property_priority(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let name = arg_str(scope, &args, 0);
    let prio = style_prio_map(scope, args.this())
        .and_then(|m| v8::String::new(scope, &name).and_then(|k| m.get(scope, k.into())))
        .filter(|v| v.is_string())
        .map(|v| v.to_rust_string_lossy(scope))
        .unwrap_or_default();
    rv.set(v8::String::new(scope, &prio).unwrap().into());
}

fn style_get_property(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let name = arg_str(scope, &args, 0);
    if let Some(key) = v8::String::new(scope, &name) {
        if let Some(v) = args.this().get(scope, key.into()) {
            if !v.is_undefined() {
                rv.set(v);
                return;
            }
        }
    }
    rv.set(v8::String::new(scope, "").unwrap().into());
}

fn style_remove_property(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let name = arg_str(scope, &args, 0);
    if let Some(key) = v8::String::new(scope, &name) {
        args.this().delete(scope, key.into());
    }
}

// ---- document methods ------------------------------------------------------------------------

fn doc_create_element(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    if receiver_handle(scope, &args, &[9]).is_none() {
        return;
    }
    let tag = arg_str(scope, &args, 0);
    let Some(h) = with_tree_mut(|t| t.create_element(&tag)) else {
        return;
    };
    let node = wrap(scope, h);
    rv.set(node.into());
}

fn doc_create_text_node(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    if receiver_handle(scope, &args, &[9]).is_none() {
        return;
    }
    let data = arg_str(scope, &args, 0);
    let Some(h) = with_tree_mut(|t| t.create_text_node(&data)) else {
        return;
    };
    let node = wrap(scope, h);
    rv.set(node.into());
}

fn doc_get_element_by_id(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    if receiver_handle(scope, &args, &[9]).is_none() {
        return;
    }
    let id = arg_str(scope, &args, 0);
    let found = with_tree(|t| t.get_element_by_id(&id)).flatten();
    let v = wrap_opt(scope, found);
    rv.set(v);
}

fn doc_query_selector(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    if receiver_handle(scope, &args, &[9]).is_none() {
        return;
    }
    let sel = arg_str(scope, &args, 0);
    let found = with_tree(|t| t.query_selector(&sel)).flatten();
    let v = wrap_opt(scope, found);
    rv.set(v);
}

fn doc_query_selector_all(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    if receiver_handle(scope, &args, &[9]).is_none() {
        return;
    }
    let sel = arg_str(scope, &args, 0);
    let handles: Vec<Handle> =
        with_tree(|t| t.query_selector_all(&sel).to_vec()).unwrap_or_default();
    let arr = v8::Array::new(scope, handles.len() as i32);
    for (i, hh) in handles.into_iter().enumerate() {
        let node = wrap(scope, hh);
        arr.set_index(scope, i as u32, node.into());
    }
    bind_method(scope, arr.into(), "item", nodelist_item);
    bind_method(scope, arr.into(), "namedItem", nodelist_named_item);
    rv.set(arr.into());
}

// ---- install ---------------------------------------------------------------------------------

fn method_arity(name: &str) -> i32 {
    match name {
        "appendChild"
        | "removeChild"
        | "getAttribute"
        | "getAttributeNode"
        | "setAttributeNode"
        | "removeAttributeNode"
        | "hasAttribute"
        | "removeAttribute"
        | "querySelector"
        | "querySelectorAll"
        | "dispatchEvent"
        | "matches"
        | "closest"
        | "contains"
        | "setCustomValidity"
        | "setRangeText"
        | "appendData"
        | "splitText"
        | "toggleAttribute"
        | "item"
        | "namedItem"
        | "getPropertyValue"
        | "getPropertyPriority"
        | "removeProperty"
        | "createElement"
        | "createTextNode"
        | "getElementById"
        | "createComment"
        | "__cascadeProp"
        | "__parseCssNative"
        | "containsNode"
        | "attachShadow" => 1,
        "insertBefore"
        | "setAttribute"
        | "addEventListener"
        | "removeEventListener"
        | "setSelectionRange"
        | "insertData"
        | "deleteData"
        | "substringData"
        | "insertAdjacentElement"
        | "insertAdjacentHTML"
        | "getAttributeNS"
        | "setProperty"
        | "createElementNS" => 2,
        "replaceData" | "set" => 3,
        _ => 0,
    }
}

fn method_template<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    f: impl v8::MapFnTo<v8::FunctionCallback>,
) -> v8::Local<'s, v8::FunctionTemplate> {
    // DOM operations are callable methods, not constructors. Enforce that in
    // V8 itself rather than removing a property from a constructible function.
    let template = v8::FunctionTemplate::builder(f)
        .constructor_behavior(v8::ConstructorBehavior::Throw)
        .length(method_arity(name))
        .build(scope);
    let name = v8::String::new(scope, name).unwrap();
    template.set_class_name(name);
    template.remove_prototype();
    template
}

fn bind_method(
    scope: &mut v8::PinScope,
    obj: v8::Local<v8::Object>,
    name: &str,
    f: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let tmpl = method_template(scope, name, f);
    let func = tmpl.get_function(scope).unwrap();
    let key = v8::String::new(scope, name).unwrap();
    func.set_name(key);
    obj.set(scope, key.into(), func.into());
}

/// Set a method (FunctionTemplate) on an object template.
fn tmpl_method(
    scope: &mut v8::PinScope,
    tmpl: v8::Local<v8::ObjectTemplate>,
    name: &str,
    f: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let ft = method_template(scope, name, f);
    let key = v8::String::new(scope, name).unwrap();
    tmpl.set(key.into(), ft.into());
}

fn tmpl_getter(
    scope: &mut v8::PinScope,
    tmpl: v8::Local<v8::ObjectTemplate>,
    name: &str,
    g: impl v8::MapFnTo<v8::AccessorNameGetterCallback>,
) {
    let key = v8::String::new(scope, name).unwrap();
    tmpl.set_accessor(key.into(), g);
}

fn tmpl_accessor(
    scope: &mut v8::PinScope,
    tmpl: v8::Local<v8::ObjectTemplate>,
    name: &str,
    g: impl v8::MapFnTo<v8::AccessorNameGetterCallback>,
    s: impl v8::MapFnTo<v8::AccessorNameSetterCallback>,
) {
    let key = v8::String::new(scope, name).unwrap();
    tmpl.set_accessor_with_setter(key.into(), g, s);
}

/// Build the shared element template (methods + accessors).
fn build_el_template<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::ObjectTemplate> {
    let tmpl = v8::ObjectTemplate::new(scope);
    tmpl.set_internal_field_count(1);

    tmpl_method(scope, tmpl, "attachShadow", el_attach_shadow);
    tmpl_getter(scope, tmpl, "shadowRoot", get_shadow_root);
    tmpl_getter(scope, tmpl, "host", get_shadow_host);
    tmpl_getter(scope, tmpl, "mode", get_shadow_mode);
    tmpl_getter(scope, tmpl, "__isShadowRoot", get_is_shadow_root);
    tmpl_method(scope, tmpl, "appendChild", el_append_child);
    tmpl_method(scope, tmpl, "removeChild", el_remove_child);
    tmpl_method(scope, tmpl, "insertBefore", el_insert_before);
    tmpl_method(scope, tmpl, "setAttribute", el_set_attribute);
    tmpl_method(scope, tmpl, "getAttribute", el_get_attribute);
    tmpl_method(scope, tmpl, "getAttributeNode", el_get_attribute_node);
    tmpl_method(scope, tmpl, "setAttributeNode", el_set_attribute_node);
    tmpl_method(scope, tmpl, "removeAttributeNode", el_remove_attribute_node);
    tmpl_method(scope, tmpl, "hasAttribute", el_has_attribute);
    tmpl_method(scope, tmpl, "removeAttribute", el_remove_attribute);
    tmpl_method(scope, tmpl, "querySelector", el_query_selector);
    tmpl_method(scope, tmpl, "querySelectorAll", el_query_selector_all);
    tmpl_method(scope, tmpl, "addEventListener", el_add_event_listener);
    tmpl_method(scope, tmpl, "removeEventListener", el_remove_event_listener);
    tmpl_method(scope, tmpl, "dispatchEvent", el_dispatch_event);
    tmpl_method(scope, tmpl, "matches", el_matches);
    tmpl_method(scope, tmpl, "closest", el_closest);
    tmpl_method(scope, tmpl, "contains", el_contains);
    tmpl_method(scope, tmpl, "hasChildNodes", el_has_child_nodes);
    tmpl_method(scope, tmpl, "cloneNode", el_clone_node);
    tmpl_method(scope, tmpl, "getRootNode", el_get_root_node);
    tmpl_method(scope, tmpl, "containsNode", el_contains_node);
    tmpl_method(scope, tmpl, "append", el_append);
    tmpl_method(scope, tmpl, "prepend", el_append);
    tmpl_method(scope, tmpl, "remove", el_remove_self);
    tmpl_method(scope, tmpl, "focus", el_focus);
    tmpl_method(scope, tmpl, "blur", el_blur);
    tmpl_method(scope, tmpl, "click", el_click);
    tmpl_method(scope, tmpl, "scrollIntoView", el_scroll_into_view);
    // Form-control methods (no headless effect, but must exist so React's commit path
    // doesn't throw "not a function" and discard the SSR tree — see el_noop).
    tmpl_method(scope, tmpl, "setCustomValidity", el_noop);
    tmpl_method(scope, tmpl, "setSelectionRange", el_noop);
    tmpl_method(scope, tmpl, "setRangeText", el_noop);
    tmpl_method(scope, tmpl, "select", el_noop);
    tmpl_method(scope, tmpl, "scrollTo", el_noop);
    tmpl_method(scope, tmpl, "scrollBy", el_noop);
    tmpl_method(scope, tmpl, "checkValidity", el_valid_true);
    tmpl_method(scope, tmpl, "reportValidity", el_valid_true);
    tmpl_method(
        scope,
        tmpl,
        "getBoundingClientRect",
        el_get_bounding_client_rect,
    );
    // CharacterData (text/comment node) mutation — native over rtdom's text storage.
    tmpl_method(scope, tmpl, "insertData", el_insert_data);
    tmpl_method(scope, tmpl, "deleteData", el_delete_data);
    tmpl_method(scope, tmpl, "appendData", el_append_data);
    tmpl_method(scope, tmpl, "replaceData", el_replace_data);
    tmpl_method(scope, tmpl, "substringData", el_substring_data);
    tmpl_method(scope, tmpl, "splitText", el_split_text);
    // native cascade lookup (inherited-property resolution) used by getComputedStyle.
    tmpl_method(scope, tmpl, "__cascadeProp", el_cascade_prop);
    // ChildNode / ParentNode manipulation + insertAdjacent* + toggleAttribute + getAttributeNS (native).
    tmpl_method(scope, tmpl, "before", el_before);
    tmpl_method(scope, tmpl, "after", el_after);
    tmpl_method(scope, tmpl, "replaceWith", el_replace_with);
    tmpl_method(scope, tmpl, "replaceChildren", el_replace_children);
    tmpl_method(
        scope,
        tmpl,
        "insertAdjacentElement",
        el_insert_adjacent_element,
    );
    tmpl_method(scope, tmpl, "insertAdjacentHTML", el_insert_adjacent_html);
    tmpl_method(scope, tmpl, "toggleAttribute", el_toggle_attribute);
    tmpl_method(scope, tmpl, "getAttributeNS", el_get_attribute_ns);

    tmpl_getter(scope, tmpl, "tagName", get_tag_name);
    tmpl_getter(scope, tmpl, "parentNode", get_parent_node);
    tmpl_getter(scope, tmpl, "firstChild", get_first_child);
    tmpl_getter(scope, tmpl, "nextSibling", get_next_sibling);
    tmpl_getter(scope, tmpl, "style", get_style);
    tmpl_getter(scope, tmpl, "constructor", get_constructor);
    tmpl_getter(scope, tmpl, "nodeType", get_node_type);
    tmpl_getter(scope, tmpl, "activeElement", get_active_element);
    tmpl_accessor(scope, tmpl, "data", get_node_data, set_node_data);
    tmpl_accessor(scope, tmpl, "nodeValue", get_node_data, set_node_data);
    tmpl_getter(scope, tmpl, "nodeName", get_node_name);
    tmpl_getter(scope, tmpl, "localName", get_local_name);
    tmpl_getter(scope, tmpl, "childNodes", get_child_nodes);
    tmpl_getter(scope, tmpl, "ownerDocument", get_owner_document);
    tmpl_accessor(scope, tmpl, "outerHTML", get_outer_html, set_outer_html);
    tmpl_getter(scope, tmpl, "children", get_children);
    tmpl_getter(scope, tmpl, "parentElement", get_parent_element);
    tmpl_getter(scope, tmpl, "firstElementChild", get_first_element_child);
    tmpl_getter(scope, tmpl, "namespaceURI", get_namespace_uri);
    tmpl_getter(scope, tmpl, "isConnected", get_is_connected);
    tmpl_getter(scope, tmpl, "attributes", get_attributes);
    tmpl_getter(scope, tmpl, "dataset", get_dataset);
    tmpl_accessor(scope, tmpl, "innerHTML", get_inner_html, set_inner_html);

    tmpl_accessor(
        scope,
        tmpl,
        "textContent",
        get_text_content,
        set_text_content,
    );
    tmpl_accessor(scope, tmpl, "id", get_id, set_id);
    tmpl_accessor(scope, tmpl, "className", get_class_name, set_class_name);
    tmpl_accessor(
        scope,
        tmpl,
        "contentEditable",
        get_content_editable,
        set_content_editable,
    );
    tmpl_getter(scope, tmpl, "isContentEditable", get_is_content_editable);

    // Node-type constants on every node (`node.TEXT_NODE === 3`, …). dom-accessibility-api compares
    // `node.nodeType === node.TEXT_NODE`, so without these the accessible-name walk skips all text
    // nodes → empty names → getByRole({name}) finds nothing.
    for (name, val) in [
        ("ELEMENT_NODE", 1),
        ("ATTRIBUTE_NODE", 2),
        ("TEXT_NODE", 3),
        ("CDATA_SECTION_NODE", 4),
        ("PROCESSING_INSTRUCTION_NODE", 7),
        ("COMMENT_NODE", 8),
        ("DOCUMENT_NODE", 9),
        ("DOCUMENT_TYPE_NODE", 10),
        ("DOCUMENT_FRAGMENT_NODE", 11),
    ] {
        let key = v8::String::new(scope, name).unwrap();
        let v = v8::Integer::new(scope, val);
        tmpl.set(key.into(), v.into());
    }

    // Always-on graceful-degradation interceptor: unimplemented members read as undefined (no
    // crash); recording the access list is gated by log_enabled() (TURBO_RUST_DOM_LOG).
    let handler = v8::NamedPropertyHandlerConfiguration::new()
        .getter(missing_getter)
        .setter(passthrough_setter)
        .flags(
            v8::PropertyHandlerFlags::NON_MASKING
                | v8::PropertyHandlerFlags::ONLY_INTERCEPT_STRINGS,
        );
    tmpl.set_named_property_handler(handler);

    tmpl
}

fn parse_css_native(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let _dom = DomScope::for_object(scope, args.this());
    let source = args.get(0).to_rust_string_lossy(scope);
    let parsed = cssom_parser::parse(&source);
    if let Some(text) = v8::String::new(scope, &parsed) {
        rv.set(text.into());
    }
}

/// Register the native stylesheet grammar parser before running the JS bootstrap.
// A projected prototype accessor must keep using the native tree, rather than
// capturing the instance's initial descriptor value. The uncached view retains
// the original ObjectTemplate callbacks without inheriting projected JS APIs.
fn dom_accessor_dispatch(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let receiver = args.this();
    let _dom = DomScope::for_object(scope, receiver);
    let descriptor = args.data().to_rust_string_lossy(scope);
    let mut fields = descriptor.split('\0');
    let setter = fields.next() == Some("set");
    let owner = fields.next().unwrap_or("Node");
    let property = fields.next().unwrap_or("");
    let handle = handle_of(scope, receiver);
    let valid = handle
        .and_then(|h| {
            with_tree(|tree| {
                let kind = tree.node_type_id(h);
                match owner {
                    "Node" => true,
                    "Element" => kind == 1,
                    "HTMLElement" => kind == 1 && tree.namespace_id(h) == 0,
                    "CharacterData" => matches!(kind, 3 | 4 | 8),
                    "Document" => kind == 9,
                    "DocumentFragment" => kind == 11,
                    "ShadowRoot" => kind == 11 && tree.is_shadow_root(h),
                    _ => false,
                }
            })
        })
        .unwrap_or(false);
    if !valid {
        let message = v8::String::new(scope, "Illegal invocation").unwrap();
        let error = v8::Exception::type_error(scope, message);
        scope.throw_exception(error);
        return;
    }
    let handle = handle.unwrap();
    let Some(context) = receiver.get_creation_context(scope) else {
        return;
    };
    let scope = &mut v8::ContextScope::new(scope, context);
    let template = DOM.with(|d| d.borrow().as_ref().unwrap().el_template.clone());
    let template = v8::Local::new(scope, template);
    let raw = template.new_instance(scope).unwrap();
    let handle = v8::Integer::new_from_unsigned(scope, handle.raw());
    raw.set_internal_field(0, handle.into());
    let key = v8::String::new(scope, property).unwrap();
    if setter {
        raw.set(scope, key.into(), args.get(0));
    } else if let Some(value) = raw.get(scope, key.into()) {
        rv.set(value);
    }
}

fn make_dom_accessor(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let property = args.get(0).to_rust_string_lossy(scope);
    let setter = args.get(1).boolean_value(scope);
    let owner = args.get(2).to_rust_string_lossy(scope);
    let operation = if setter { "set" } else { "get" };
    let data = v8::String::new(scope, &format!("{operation}\0{owner}\0{property}")).unwrap();
    let template = v8::FunctionTemplate::builder(dom_accessor_dispatch)
        .data(data.into())
        .constructor_behavior(v8::ConstructorBehavior::Throw)
        .length(i32::from(setter))
        .build(scope);
    let name = v8::String::new(scope, &format!("{operation} {property}")).unwrap();
    template.set_class_name(name);
    template.remove_prototype();
    if let Some(function) = template.get_function(scope) {
        rv.set(function.into());
    }
}

fn native_dom_function_dispatch(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let Ok(data) = v8::Local::<v8::Array>::try_from(args.data()) else {
        return;
    };
    let Some(delegate) = data
        .get_index(scope, 0)
        .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
    else {
        return;
    };
    let constructor = data
        .get_index(scope, 1)
        .is_some_and(|value| value.boolean_value(scope));
    if constructor && !args.is_construct_call() {
        let message = v8::String::new(scope, "Constructor requires 'new'").unwrap();
        let error = v8::Exception::type_error(scope, message);
        scope.throw_exception(error);
        return;
    }
    let values: Vec<_> = (0..args.length()).map(|index| args.get(index)).collect();
    let class_construct = data
        .get_index(scope, 2)
        .is_some_and(|value| value.boolean_value(scope));
    if class_construct && args.is_construct_call() {
        let Some(construct) = data
            .get_index(scope, 3)
            .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
        else {
            return;
        };
        let arguments = v8::Array::new(scope, values.len() as i32);
        for (index, value) in values.iter().enumerate() {
            arguments.set_index(scope, index as u32, *value);
        }
        let receiver = v8::undefined(scope).into();
        if let Some(value) = construct.call(
            scope,
            receiver,
            &[delegate.into(), arguments.into(), args.new_target()],
        ) {
            rv.set(value);
        }
        return;
    }
    if let Some(value) = delegate.call(scope, args.this().into(), &values) {
        rv.set(value);
    }
}

// The implementation closure stays in V8's private function data. This is a
// native callable over the facade's live guest state, not a source-string mask.
fn make_native_dom_function(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let Ok(delegate) = v8::Local::<v8::Function>::try_from(args.get(0)) else {
        let message = v8::String::new(scope, "Expected a function").unwrap();
        let error = v8::Exception::type_error(scope, message);
        scope.throw_exception(error);
        return;
    };
    let name = args.get(1).to_rust_string_lossy(scope);
    let length = args.get(2).int32_value(scope).unwrap_or(0).max(0);
    let constructor = args.get(3).boolean_value(scope);
    let class_construct = args.get(4).boolean_value(scope);
    let data = v8::Array::new(scope, 4);
    data.set_index(scope, 0, delegate.into());
    let flag = v8::Boolean::new(scope, constructor);
    data.set_index(scope, 1, flag.into());
    let flag = v8::Boolean::new(scope, class_construct);
    data.set_index(scope, 2, flag.into());
    data.set_index(scope, 3, args.data());
    let template = v8::FunctionTemplate::builder(native_dom_function_dispatch)
        .data(data.into())
        .constructor_behavior(if constructor {
            v8::ConstructorBehavior::Allow
        } else {
            v8::ConstructorBehavior::Throw
        })
        .length(length)
        .build(scope);
    let name = v8::String::new(scope, &name).unwrap();
    template.set_class_name(name);
    if !constructor {
        template.remove_prototype();
    }
    if let Some(function) = template.get_function(scope) {
        rv.set(function.into());
    }
}

pub fn register_css_parser(scope: &mut v8::PinScope) {
    let global = scope.get_current_context().global(scope);
    bind_method(scope, global, "__parseCssNative", parse_css_native);
}

/// Install `window`/`document` + the DOM onto `globalThis` for the current context.
pub fn install(scope: &mut v8::PinScope) {
    install_with_html(
        scope,
        "<!DOCTYPE html><html><head></head><body></body></html>",
    );
}

pub fn install_with_html(scope: &mut v8::PinScope, html: &str) {
    // A replacement document is seeded before its new bootstrap probes native
    // methods. Projecting with the previous bootstrap here would delete those
    // raw own methods before their replacement prototypes have been constructed.
    let global = scope.get_current_context().global(scope);
    let name = v8::String::new(scope, "ts").unwrap();
    let key = v8::Symbol::for_key(scope, name);
    if let Some(namespace) = global
        .get(scope, key.into())
        .and_then(|v| v.to_object(scope))
    {
        let key = v8::String::new(scope, "projectNativeNode").unwrap();
        namespace.delete(scope, key.into());
    }
    STYLE.with(|s| s.borrow_mut().clear());
    LISTENERS.with(|s| s.borrow_mut().clear());
    ACTIVE.with(|s| *s.borrow_mut() = None);
    let tree = Tree::parse(html);
    let el_template = {
        let t = build_el_template(scope);
        v8::Global::new(scope, t)
    };
    DOM.with(|d| {
        *d.borrow_mut() = Some(DomState {
            tree,
            cache: HashMap::new(),
            el_template,
            shadow_modes: HashMap::new(),
        });
    });

    let context = scope.get_current_context();
    let global = v8::Global::new(scope, context);
    DOM_CONTEXTS.with(|d| {
        d.borrow_mut().insert(DOM_ID.with(|id| id.get()), global);
    });
    bind_document(scope);

    // Window-level globals that libs read as BARE identifiers (`navigator`, `HTMLElement`, …) — a
    // missing bare global is a ReferenceError the element interceptor can't catch, so they must
    // exist on globalThis. Plain-data / stub surface (refined natively as the logger surfaces hot
    // members); document extras that are pure JS go here too.
    register_css_parser(scope);
    let global = scope.get_current_context().global(scope);
    bind_method(scope, global, "__domAccessor", make_dom_accessor);
    let reflect_key = v8::String::new(scope, "Reflect").unwrap();
    let construct_key = v8::String::new(scope, "construct").unwrap();
    let reflect = global.get(scope, reflect_key.into()).unwrap();
    let reflect = v8::Local::<v8::Object>::try_from(reflect).unwrap();
    let construct = reflect.get(scope, construct_key.into()).unwrap();
    let factory = v8::FunctionTemplate::builder(make_native_dom_function)
        .data(construct)
        .constructor_behavior(v8::ConstructorBehavior::Throw)
        .build(scope)
        .get_function(scope)
        .unwrap();
    let factory_key = v8::String::new(scope, "__nativeDOMFunction").unwrap();
    global.set(scope, factory_key.into(), factory.into());
    bind_method(scope, global, "__nativeEventRecord", lookup_event_record);
    run_js(scope, BOOTSTRAP);
    let key = v8::String::new(scope, "__captureEventRecord").unwrap();
    if let Some(accessor) = global
        .get(scope, key.into())
        .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
    {
        let accessor = v8::Global::new(scope, accessor);
        scope
            .get_current_context()
            .set_slot(std::rc::Rc::new(EventRecordAccessor(accessor)));
        global.delete(scope, key.into());
    }
    let key = v8::String::new(scope, "__captureHostEventDispatch").unwrap();
    if let Some(dispatcher) = global
        .get(scope, key.into())
        .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
    {
        let dispatcher = v8::Global::new(scope, dispatcher);
        scope
            .get_current_context()
            .set_slot(std::rc::Rc::new(HostEventDispatcher(dispatcher)));
        global.delete(scope, key.into());
    }
}

fn bind_document(scope: &mut v8::PinScope) {
    let root = with_tree(|t| t.root()).unwrap();
    let body_h = with_tree(|t| t.query_selector("body")).flatten();
    let head_h = with_tree(|t| t.query_selector("head")).flatten();
    let html_h = with_tree(|t| t.query_selector("html")).flatten();

    // document = the root node object + document methods.
    let document = wrap(scope, root);
    bind_method(scope, document, "createElement", doc_create_element);
    bind_method(scope, document, "createTextNode", doc_create_text_node);
    bind_method(scope, document, "getElementById", doc_get_element_by_id);
    bind_method(scope, document, "querySelector", doc_query_selector);
    bind_method(scope, document, "querySelectorAll", doc_query_selector_all);
    bind_method(scope, document, "createElementNS", doc_create_element_ns);
    bind_method(
        scope,
        document,
        "createDocumentFragment",
        doc_create_fragment,
    );
    bind_method(scope, document, "createComment", doc_create_comment);
    if let Some(b) = body_h {
        let body = wrap(scope, b);
        let key = v8::String::new(scope, "body").unwrap();
        document.set(scope, key.into(), body.into());
    }
    if let Some(head) = head_h {
        let head = wrap(scope, head);
        let key = v8::String::new(scope, "head").unwrap();
        document.set(scope, key.into(), head.into());
    }
    if let Some(html) = html_h {
        let de = wrap(scope, html);
        let key = v8::String::new(scope, "documentElement").unwrap();
        document.set(scope, key.into(), de.into());
    }

    let global = scope.get_current_context().global(scope);
    let key = v8::String::new(scope, "document").unwrap();
    global.set(scope, key.into(), document.into());
    // window = globalThis
    let key = v8::String::new(scope, "window").unwrap();
    global.set(scope, key.into(), global.into());
}

/// Compile + run a JS snippet in the current context (best-effort).
fn run_js(scope: &mut v8::PinScope, src: &str) {
    let tc = std::pin::pin!(v8::TryCatch::new(scope));
    let tc = &mut tc.init();
    if let Some(code) = v8::String::new(tc, src) {
        if let Some(s) = v8::Script::compile(tc, code, None) {
            s.run(tc);
        }
    }
    if tc.has_caught() && log_enabled() {
        let msg = tc
            .exception()
            .map(|e| e.to_rust_string_lossy(tc))
            .unwrap_or_default();
        eprintln!("[rust-dom] bootstrap threw: {msg}");
    }
}

/// JS enrichment shims layered over the native rtdom binding — browser globals/constructors
/// (navigator, Event subclasses, DOM-interface ctors with Symbol.hasInstance), CSSOM `.sheet`,
/// and pure-JS document extras. Kept in a sibling .js file (not Rust) so the shims are editable
/// as JS; the Rust side is purely the rtdom↔V8 binding.
const BOOTSTRAP: &str = include_str!("browser_env.js");

/// Reset the per-thread DOM (call between test files when reusing an isolate).
pub fn reset() {
    OTHER_DOMS.with(|d| d.borrow_mut().clear());
    DOM_CONTEXTS.with(|d| d.borrow_mut().clear());
    DOM_ID.with(|d| d.set(0));
    NEXT_DOM_ID.with(|d| d.set(1));
    STYLE.with(|s| s.borrow_mut().clear());
    LISTENERS.with(|s| s.borrow_mut().clear());
    ACTIVE.with(|a| *a.borrow_mut() = None);
    if log_enabled() {
        dump_missing();
    }
    DOM.with(|d| *d.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    static INIT: Once = Once::new();
    fn init_v8() {
        INIT.call_once(|| {
            let platform = v8::new_default_platform(0, false).make_shared();
            v8::V8::initialize_platform(platform);
            v8::V8::initialize();
        });
    }

    #[test]
    fn event_records_preserve_readonly_descriptors_and_dispatch_phases() {
        init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        install_with_html(scope, "<body><main><button>press</button></main></body>");
        let source = v8::String::new(scope, r#"(() => {
          const event=new Event('test',{bubbles:true,cancelable:true,composed:true});
          const descriptor=Object.getOwnPropertyDescriptor(event,'isTrusted');
          let invalid=false;try{descriptor.get.call({})}catch(e){invalid=e instanceof TypeError}
          event.type='changed';event.bubbles=false;event.__trusted=true;
          delete event.__trusted;
          const descriptors=['type','target','currentTarget','eventPhase','bubbles','cancelable','defaultPrevented','composed','timeStamp'].every(name=>{
            const d=Object.getOwnPropertyDescriptor(Event.prototype,name);return d.enumerable&&d.configurable&&typeof d.get==='function'&&!d.set&&!Object.hasOwn(event,name);
          });
          const phases=[],button=document.querySelector('button'),main=document.querySelector('main');
          main.addEventListener('test',e=>phases.push([e.eventPhase,e.currentTarget===main,e.composedPath()[0]===button]),true);
          button.addEventListener('test',e=>{phases.push([e.eventPhase,e.currentTarget===button,e.target===button]);e.preventDefault();});
          main.addEventListener('test',e=>phases.push([e.eventPhase,e.currentTarget===main,e.defaultPrevented]));
          const result=button.dispatchEvent(event);
          return JSON.stringify({keys:Object.keys(event),descriptor:[descriptor.enumerable,descriptor.configurable,!descriptor.set,descriptor.get.name,descriptor.get.length,!Object.hasOwn(descriptor.get,'prototype')],invalid,descriptors,
            readonly:event.type==='test'&&event.bubbles&&event.isTrusted===false,
            stamp:event.timeStamp>=0&&event.timeStamp<1000,tag:Object.prototype.toString.call(event),phases,
            after:[result,event.defaultPrevented,event.currentTarget,event.eventPhase,event.composedPath().length,event.target===button],
            private:!('__nativeEventRecord'in globalThis)&&!('__captureEventRecord'in globalThis)&&!('ttEventRecords'in globalThis[Symbol.for('ts')])});
        })()"#).unwrap();
        let value = v8::Script::compile(scope, source, None)
            .unwrap()
            .run(scope)
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&value.to_rust_string_lossy(scope)).unwrap();
        assert_eq!(value["keys"], serde_json::json!(["isTrusted"]));
        assert_eq!(
            value["descriptor"],
            serde_json::json!([true, false, true, "get isTrusted", 0, true])
        );
        for name in ["invalid", "descriptors", "readonly", "stamp", "private"] {
            assert_eq!(value[name], true, "{value}");
        }
        assert_eq!(value["tag"], "[object Event]");
        assert_eq!(
            value["phases"],
            serde_json::json!([[1, true, true], [2, true, true], [3, true, true]])
        );
        assert_eq!(
            value["after"],
            serde_json::json!([false, true, null, 0, 0, true])
        );
    }

    #[test]
    fn host_event_trust_is_private_and_script_redispatch_clears_it() {
        init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let global_template = v8::ObjectTemplate::new(scope);
        global_template.set_internal_field_count(2);
        let context = v8::Context::new(
            scope,
            v8::ContextOptions {
                global_template: Some(global_template),
                ..Default::default()
            },
        );
        let scope = &mut v8::ContextScope::new(scope, context);
        install(scope);
        let source = v8::String::new(scope, "globalThis.event=new Event('test');event").unwrap();
        let event = v8::Script::compile(scope, source, None)
            .unwrap()
            .run(scope)
            .unwrap();
        let event = v8::Local::<v8::Object>::try_from(event).unwrap();
        let source=v8::String::new(scope,"globalThis.seen=[];document.addEventListener('test',e=>seen.push(e.isTrusted));document").unwrap();
        let target = v8::Script::compile(scope, source, None)
            .unwrap()
            .run(scope)
            .unwrap();
        let target = v8::Local::<v8::Object>::try_from(target).unwrap();
        mark_event_trusted(scope, event, Some(37.5));
        dispatch_host_event(scope, event, target);
        let source = v8::String::new(
            scope,
            r#"(() => {
          event.__trusted=true;document.dispatchEvent(event);
          return JSON.stringify([seen,event.isTrusted,event.timeStamp]);
        })()"#,
        )
        .unwrap();
        let value = v8::Script::compile(scope, source, None)
            .unwrap()
            .run(scope)
            .unwrap();
        assert_eq!(
            value.to_rust_string_lossy(scope),
            "[[true,false],false,37.5]"
        );
        let source=v8::String::new(scope,"globalThis.windowTrust=null;window.addEventListener('host-window',event=>windowTrust=event.isTrusted);new Event('host-window')").unwrap();
        let event = v8::Script::compile(scope, source, None)
            .unwrap()
            .run(scope)
            .unwrap();
        let event = v8::Local::<v8::Object>::try_from(event).unwrap();
        let target = context.global(scope);
        dispatch_host_event(scope, event, target);
        let source = v8::String::new(scope, "String(windowTrust)").unwrap();
        let value = v8::Script::compile(scope, source, None)
            .unwrap()
            .run(scope)
            .unwrap();
        assert_eq!(
            value.to_rust_string_lossy(scope),
            "true",
            "host Window dispatch must use private captured registry"
        );
        let source=v8::String::new(scope,"globalThis.loadSeen=null;window.addEventListener('load',e=>loadSeen=[e.target===document,e.currentTarget===window,e.eventPhase,e.isTrusted]);globalThis.loadEvent=new Event('load');loadEvent").unwrap();
        let event = v8::Script::compile(scope, source, None)
            .unwrap()
            .run(scope)
            .unwrap();
        let event = v8::Local::<v8::Object>::try_from(event).unwrap();
        dispatch_host_event(scope, event, target);
        let source=v8::String::new(scope,"JSON.stringify([loadSeen,loadEvent.currentTarget===window,loadEvent.eventPhase,loadEvent.composedPath().length])").unwrap();
        let value = v8::Script::compile(scope, source, None)
            .unwrap()
            .run(scope)
            .unwrap();
        assert_eq!(
            value.to_rust_string_lossy(scope),
            "[[true,true,2,true],true,2,1]"
        );
        let source=v8::String::new(scope,"try{window.dispatchEvent(loadEvent);}catch(error){globalThis.redispatchError=error.name;}JSON.stringify([loadSeen,redispatchError,loadEvent.currentTarget===window,loadEvent.eventPhase])").unwrap();
        let value = v8::Script::compile(scope, source, None)
            .unwrap()
            .run(scope)
            .unwrap();
        assert_eq!(
            value.to_rust_string_lossy(scope),
            "[[true,true,2,true],\"InvalidStateError\",true,2]"
        );
        install_with_html(scope, "<body><button>new document</button></body>");
        let source=v8::String::new(scope,r#"(() => {const bus=new EventTarget(),e=new Event('x',{cancelable:true});let phase;bus.addEventListener('x',ev=>{phase=ev.eventPhase;ev.returnValue=false;});return JSON.stringify([bus.dispatchEvent(e),phase,e.currentTarget,e.defaultPrevented]);})()"#).unwrap();
        let value = v8::Script::compile(scope, source, None)
            .unwrap()
            .run(scope)
            .unwrap();
        assert_eq!(value.to_rust_string_lossy(scope), "[false,2,null,true]");
    }

    #[test]
    fn native_dom_methods_reject_construction_and_preserve_call_behavior() {
        init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        install(scope);
        let source = v8::String::new(
            scope,
            r#"
          const element = document.createElement('div');
          const methods = [document.createTextNode, element.setAttribute,
            element.getAttribute, element.style.setProperty];
          const notConstructible = methods.every(fn => {
            if (Object.hasOwn(fn, 'prototype')) return false;
            try { Reflect.construct(function(){}, [], fn); return false; }
            catch (error) { return error instanceof TypeError; }
          });
          let rejectsNew = false;
          try { new document.createTextNode('text'); }
          catch (error) { rejectsNew = error instanceof TypeError; }
          element.setAttribute('data-token', 'kept');
          element.style.setProperty('color', 'red');
          const attributes = Object.getOwnPropertyDescriptor(document.createTextNode, 'length');
          JSON.stringify([notConstructible, rejectsNew,
            methods.map(fn => [fn.name, fn.length]),
            element.getAttribute('data-token'), element.style.getPropertyValue('color'),
            new Event('preserved').type,
            [attributes.writable, attributes.enumerable, attributes.configurable]]);
        "#,
        )
        .unwrap();
        let script = v8::Script::compile(scope, source, None).unwrap();
        let result = script.run(scope).unwrap().to_rust_string_lossy(scope);
        assert_eq!(
            result,
            r#"[true,true,[["createTextNode",1],["setAttribute",2],["getAttribute",1],["setProperty",2]],"kept","red","preserved",[false,false,true]]"#
        );
        reset();
    }

    #[test]
    fn projected_native_accessors_are_shared_live_and_receiver_checked() {
        init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        install(scope);
        let source = v8::String::new(scope, r#"
          const element=document.createElement('div');
          const id=Object.getOwnPropertyDescriptor(Element.prototype,'id');
          const type=Object.getOwnPropertyDescriptor(Node.prototype,'nodeType');
          const rejected=[{},document,document.createTextNode('text')].every(receiver=>{
            try{id.get.call(receiver);return false}catch(error){return error instanceof TypeError}
          });
          id.set.call(element,'live');
          element.innerHTML='<span>child</span>';
          const clone=element.cloneNode(true);
          JSON.stringify([!Object.hasOwn(element,'id'),element.getAttribute('id'),id.get.call(element),
            id.get.name,id.get.length,id.set.name,id.set.length,!Object.hasOwn(id.get,'prototype'),
            rejected,type.get.call(element),document.ownerDocument===null,
            Object.getPrototypeOf(element)===HTMLDivElement.prototype,
            element.firstChild.parentNode===element,clone.textContent,
            clone.appendChild===Node.prototype.appendChild]);
        "#).unwrap();
        let script = v8::Script::compile(scope, source, None).unwrap();
        let result = script.run(scope).unwrap().to_rust_string_lossy(scope);
        assert_eq!(
            result,
            r#"[true,"live","live","get id",0,"set id",1,true,true,1,true,true,true,"child",true]"#
        );
        reset();
    }

    #[test]
    fn replacing_a_document_rebuilds_projected_native_methods() {
        init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        for html in [
            "<body><p>first</p></body>",
            "<body><p>second</p></body>",
            "<body><p>third</p></body>",
        ] {
            install_independent_html(scope, html);
            let source = v8::String::new(
                scope,
                r#"(()=>{
              const node=document.createElement('span');node.textContent='new';
              document.body.appendChild(node);
              const style=document.createElement('style');style.textContent='sentinel {}';
              document.head.appendChild(style);
              const rules=style.sheet.cssRules;
              globalThis.retainedSheets=globalThis.retainedSheets||[];
              if(retainedSheets.some(sheet=>sheet.cssRules.length!==1||sheet.cssRules.item(0).selectorText!=='.retained'))return false;
              const constructed=new CSSStyleSheet();constructed.replaceSync('.retained {}');retainedSheets.push(constructed);
              return document.body.querySelector('span')===node &&
                node.appendChild===Node.prototype.appendChild &&
                node.ownerDocument===document && rules.length===1 &&
                rules.item(0).selectorText==='sentinel' &&
                rules instanceof CSSRuleList && document.styleSheets.length===1;
            })()"#,
            )
            .unwrap();
            let script = v8::Script::compile(scope, source, None).unwrap();
            assert!(script.run(scope).unwrap().boolean_value(scope));
        }
        reset();
    }

    #[test]
    fn native_facade_functions_preserve_class_construction_and_live_collections() {
        init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        install(scope);
        let code = r#"(() => {
          const make=globalThis[Symbol.for('ts')].makeNativeFunction;
          class State { constructor(value){if(value==='bad')throw new RangeError('bad');this.value=value;this.target=new.target;} }
          const Wrapped=make(State,'State',1,true,true); Wrapped.prototype=State.prototype;
          class Child extends Wrapped {}
          const construct=Reflect.construct; Reflect.construct=()=>{throw new Error('modified');};
          const instance=new Child(7); Reflect.construct=construct;
          let missingNew=false,error=false;
          try{Wrapped(7)}catch(e){missingNew=e instanceof TypeError}
          try{new Wrapped('bad')}catch(e){error=e instanceof RangeError}
          const sheet=new CSSStyleSheet(),rules=sheet.cssRules;sheet.replaceSync('sentinel {}');
          const getter=Object.getOwnPropertyDescriptor(CSSRuleList.prototype,'length').get;
          let invalid=false;try{getter.call({})}catch(e){invalid=e instanceof TypeError}
          return instance.value===7&&instance.target===Child&&instance instanceof Child&&instance instanceof State&&instance instanceof Wrapped
            &&missingNew&&error&&invalid&&rules===sheet.cssRules&&rules.length===1&&rules.item(0).selectorText==='sentinel'
            &&!Object.hasOwn(rules.item,'prototype')&&!Object.hasOwn(getter,'prototype')&&getter.name==='get length'
            &&Function.prototype.toString.call(rules.item)==='function item() { [native code] }';
        })()"#;
        let src = v8::String::new(scope, code).unwrap();
        let script = v8::Script::compile(scope, src, None).unwrap();
        assert!(script.run(scope).unwrap().boolean_value(scope));
        reset();
    }

    #[test]
    fn native_dom_methods_check_receiver_type_before_tree_access() {
        init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        install(scope);
        let source = v8::String::new(scope, r#"
          const element=document.createElement('div'),text=document.createTextNode('text');
          const calls=[
            ()=>document.querySelector.call(null,'body'),
            ()=>document.querySelector.call(element,'body'),
            ()=>element.querySelector.call(document,'body'),
            ()=>element.getAttribute.call({},'id'),
            ()=>element.getAttribute.call(text,'id'),
            ()=>element.appendChild.call({},text),
            ()=>document.createTextNode.call({},'text'),
          ];
          JSON.stringify([calls.every(call=>{try{call();return false}catch(error){return error instanceof TypeError}}),
            document.querySelector('body')===document.body,
            element.getAttribute('id')===null,
            element.appendChild(text)===text,
            element.firstChild===text]);
        "#).unwrap();
        let script = v8::Script::compile(scope, source, None).unwrap();
        let result = script.run(scope).unwrap().to_rust_string_lossy(scope);
        assert_eq!(result, "[true,true,true,true,true]");
        reset();
    }

    #[test]
    fn independent_context_documents_route_native_nodes_and_reentrant_events() {
        init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let parent = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, parent);
        install(scope);
        let child = v8::Context::new(scope, Default::default());
        child.set_security_token(parent.get_security_token(scope));
        {
            let scope = &mut v8::ContextScope::new(scope, child);
            install_independent_html(scope, "<html><head><title>child</title><style>child-sentinel {}</style></head><body><div id='only-child'>child</div></body></html>");
        }
        let global = parent.global(scope);
        let key = v8::String::new(scope, "child").unwrap();
        let child_global = child.global(scope);
        global.set(scope, key.into(), child_global.into());
        let src = v8::String::new(
            scope,
            r#"
          document.body.innerHTML = '<div id="only-parent">parent</div>';
          const node = child.document.getElementById('only-child');
          const independent = document.getElementById('only-child') === null
            && child.document.getElementById('only-parent') === null;
          node.setAttribute('data-touched', 'yes');
          if (child.document.head.querySelector('title').textContent !== 'child'
              || child.document.querySelectorAll('head').length !== 1) throw new Error('parsed head was replaced');
          if(child.document.styleSheets.length!==1||child.document.styleSheets.item(0).cssRules.length!==1
              ||child.document.styleSheets.item(0).cssRules.item(0).selectorText!=='child-sentinel')throw new Error('child CSSOM receiver identity was lost');
          let nested = '';
          node.addEventListener('probe', () => {
            document.getElementById('only-parent').textContent = 'updated';
            nested = child.document.getElementById('only-child').textContent;
          });
          node.dispatchEvent(new Event('probe'));
          let adoptionRejected = false;
          try { document.body.replaceChildren(node); } catch(error) { adoptionRejected = /Cross-document node adoption/.test(error.message); }
          if (!adoptionRejected || document.getElementById('only-parent').textContent !== 'updated'
              || node.ownerDocument !== child.document) throw new Error('variadic adoption aliased another document');
          const separateApis = child.document.createElement.constructor === child.Function
            && child.Event !== Event && child.HTMLElement !== HTMLElement;
          JSON.stringify([independent, node.ownerDocument === child.document,
            node.getAttribute('data-touched'), nested,
            document.getElementById('only-parent').textContent, separateApis]);
        "#,
        )
        .unwrap();
        let script = v8::Script::compile(scope, src, None).unwrap();
        let out = script.run(scope).unwrap().to_rust_string_lossy(scope);
        assert_eq!(out, r#"[true,true,"yes","child","updated",true]"#);
        reset();
    }

    #[test]
    fn dom_slice_smoke() {
        init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);

        install(scope);

        let run = |scope: &mut v8::PinScope, code: &str| -> String {
            let src = v8::String::new(scope, code).unwrap();
            let script = v8::Script::compile(scope, src, None).unwrap();
            let r = script.run(scope).unwrap();
            r.to_rust_string_lossy(scope)
        };

        // create + append + attribute + textContent + querySelector + identity
        let out = run(
            scope,
            r#"
            const d = document.createElement('div');
            d.setAttribute('id', 'x');
            d.className = 'card';
            d.textContent = 'hello';
            document.body.appendChild(d);
            const found = document.querySelector('#x');
            JSON.stringify({
                tag: d.tagName,
                id: d.id,
                cls: d.className,
                text: found.textContent,
                identity: found === d,
                getAttr: d.getAttribute('id'),
                inBody: document.body.firstChild === d,
            });
        "#,
        );
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["tag"], "DIV", "{out}");
        assert_eq!(v["id"], "x", "{out}");
        assert_eq!(v["cls"], "card", "{out}");
        assert_eq!(v["text"], "hello", "{out}");
        assert_eq!(
            v["identity"], true,
            "querySelector must return the SAME object: {out}"
        );
        assert_eq!(v["getAttr"], "x", "{out}");
        assert_eq!(v["inBody"], true, "{out}");
        reset();
    }

    #[test]
    fn cssom_parses_style_text_and_parser_created_controls_have_idl_values() {
        init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        install(scope);
        let source = v8::String::new(scope, r#"
            document.body.innerHTML='<input id="token" type="hidden" value="abc&amp;def"><textarea id="q">search</textarea><style>body { color: red; } sentinel {}</style>';
            const input=document.querySelector('#token'), textarea=document.querySelector('#q');
            const style=document.querySelector('style'), sheet=style.sheet;
            const parsed=sheet.cssRules.length===2 && sheet.cssRules[1].selectorText==='sentinel' && sheet.cssRules[1] instanceof CSSStyleRule;
            const value=input.value==='abc&def' && input instanceof HTMLInputElement && textarea.value==='search';
            const text=style.textContent;
            sheet.insertRule('a { color: blue; }',1);
            const inserted=sheet.cssRules.length===3 && sheet.cssRules[1].selectorText==='a' && style.textContent===text;
            style.textContent='/*comment*/ h1 { content: "}"; } @media screen { p { color: red; } }';
            const replacement=style.sheet;
            const updated=replacement!==sheet && sheet.cssRules.length===3 && sheet.ownerNode===null && replacement.cssRules.length===2 && replacement.cssRules[0].style.getPropertyValue('content')==='"}"' && replacement.cssRules[1].cssRules[0].selectorText==='p';
            const registered=document.styleSheets.length===1 && document.styleSheets[0]===replacement;
            style.remove();
            const removed=document.styleSheets.length===0 && style.sheet===null;
            JSON.stringify({parsed,value,inserted,updated,registered,removed});
        "#).unwrap();
        let script = v8::Script::compile(scope, source, None).unwrap();
        let result = script.run(scope).unwrap().to_rust_string_lossy(scope);
        let result: serde_json::Value = serde_json::from_str(&result).unwrap();
        for field in [
            "parsed",
            "value",
            "inserted",
            "updated",
            "registered",
            "removed",
        ] {
            assert_eq!(result[field], true, "{field}: {result}");
        }
        reset();
    }

    // A cross-origin <iframe> hosts a bridged second window realm: contentWindow is a
    // distinct object with its own document, and postMessage crosses the realm boundary
    // in BOTH directions carrying the correct source/origin (the reCAPTCHA anchor↔bframe
    // handshake shape). Delivery is async (setTimeout), so a queue shim + drain stands in
    // for the real message loop.
    #[test]
    fn iframe_second_realm_bridges_post_message_both_ways() {
        init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        install(scope);

        let run = |scope: &mut v8::PinScope, code: &str| -> String {
            let src = v8::String::new(scope, code).unwrap();
            let script = v8::Script::compile(scope, src, None).unwrap();
            let r = script.run(scope).unwrap();
            r.to_rust_string_lossy(scope)
        };

        let out = run(
            scope,
            r#"
            // Virtual timer queue so postMessage delivery is drainable (no wall clock).
            const __q = [];
            globalThis.setTimeout = (fn) => { __q.push(fn); return __q.length; };
            const drain = () => { let n = 0; while (__q.length && n++ < 1000) { (__q.shift())(); } };
            globalThis.location = { origin: 'https://page.test', href: 'https://page.test/' };

            const iframe = document.createElement('iframe');
            iframe.setAttribute('src', 'https://recaptcha.test/frame');
            const cw = iframe.contentWindow;
            const cd = iframe.contentDocument;

            const distinct = cw !== globalThis && cw.document === cd && cw.self === cw;
            const ownDoc = cd.body != null && cd.body !== document.body;

            // Child builds its own DOM into its own document and queries it back through
            // the child document facade. (The underlying binding has a single native tree,
            // so the child subtree is not tree-level isolated from the parent — the shared
            // object graph noted in browser_env.js; the realm boundary is the window +
            // message channel, which is what a cross-frame protocol relies on.)
            const el = cd.createElement('div'); el.setAttribute('id', 'inchild');
            cd.body.appendChild(el);
            const childFindsOwn = cd.getElementById('inchild') != null &&
                                  cd.querySelector('#inchild') === el;

            // Child listens for a challenge and replies through e.source (→ parent).
            let childSaw = '';
            cw.addEventListener('message', (e) => {
                childSaw = e.data + '|' + e.origin + '|' + (e.source === cw.parent);
                e.source.postMessage('token:CHILD-REPLY', e.origin);
            });
            // Parent listens for the reply and checks source identity + child origin.
            let parentSaw = '';
            addEventListener('message', (e) => {
                parentSaw = e.data + '|' + e.origin + '|' + (e.source === iframe.contentWindow);
            });

            // Parent → child.
            cw.postMessage('challenge:GO', 'https://recaptcha.test');
            drain(); drain();

            JSON.stringify({ distinct, ownDoc, childFindsOwn, childSaw, parentSaw, childOrigin: cw.origin });
        "#,
        );
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["distinct"], true,
            "contentWindow is a distinct realm: {out}"
        );
        assert_eq!(v["ownDoc"], true, "child has its own document body: {out}");
        assert_eq!(
            v["childFindsOwn"], true,
            "child finds its own element via its own document: {out}"
        );
        assert_eq!(v["childOrigin"], "https://recaptcha.test", "{out}");
        // Child received the parent's challenge with the parent origin + a source it can reply to.
        assert_eq!(
            v["childSaw"], "challenge:GO|https://page.test|true",
            "{out}"
        );
        // Parent received the child's reply with source === the iframe + the child origin.
        assert_eq!(
            v["parentSaw"], "token:CHILD-REPLY|https://recaptcha.test|true",
            "cross-realm reply must carry the child origin + iframe source: {out}"
        );
        reset();
    }

    // WebGL emulates a coherent headless-Chrome SwiftShader context: the identity strings
    // (VENDOR/RENDERER + UNMASKED_* via WEBGL_debug_renderer_info), the numeric limits, the
    // extension list, shader precision, and deterministic content-dependent readPixels — the
    // surface a GPU fingerprinter reads. No longer null (a null WebGL is a stronger bot tell).
    #[test]
    fn native_document_fragments_clone_and_move_owned_children() {
        init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        install(scope);
        let code = v8::String::new(scope, r#"
          const f=document.createDocumentFragment();
          const one=document.createElement('b');one.id='one';
          const two=document.createTextNode('two');
          f.appendChild(one);f.appendChild(two);
          const clone=f.cloneNode(true);
          const host=document.createElement('div');
          const ref=document.createElement('i');host.appendChild(ref);
          const returned=host.insertBefore(f,ref);
          const moved=returned===f && f.childNodes.length===0 && host.firstChild===one && one.nextSibling===two && two.nextSibling===ref;
          host.appendChild(clone);
          const own=f.nodeType===11 && f instanceof DocumentFragment && f.ownerDocument===document && f.parentNode===null && f.host===undefined;
          const cloned=clone.nodeType===11 && clone.parentNode===null && clone.childNodes.length===0 && host.childNodes.length===5 && host.childNodes[3]!==one && host.childNodes[3].id==='one';
          JSON.stringify({own,moved,cloned,type:f.nodeType,interface:f instanceof DocumentFragment,owner:f.ownerDocument===document,detached:f.parentNode===null,hostProperty:('host' in f)})
        "#).unwrap();
        let script = v8::Script::compile(scope, code, None).unwrap();
        let result = script.run(scope).unwrap().to_rust_string_lossy(scope);
        let value: serde_json::Value = serde_json::from_str(&result).unwrap();
        for key in ["own", "moved", "cloned"] {
            assert_eq!(value[key], true, "{key}: {result}");
        }
        reset();
    }

    #[test]
    fn native_outer_html_replaces_and_detaches_the_original_element() {
        init_v8();
        let mut isolate = v8::Isolate::new(Default::default());
        v8::scope!(let scope, &mut isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        install_with_html(
            scope,
            "<body><iframe id=old></iframe><b id=after></b></body>",
        );
        let code = v8::String::new(scope, r#"
          const old=document.getElementById('old');
          old.outerHTML='<p id=first>one</p><span id=second>two</span>';
          const detached=document.createElement('div');detached.outerHTML='<b></b>';
          let protectedRoot=false;try{document.documentElement.outerHTML='<p></p>'}catch(e){protectedRoot=e.name==='NoModificationAllowedError'}
          JSON.stringify({old:!old.isConnected&&old.parentNode===null,
            order:Array.from(document.body.children,e=>e.id).join(',')==='first,second,after',
            detached:detached.tagName==='DIV',protectedRoot})
        "#).unwrap();
        let out = v8::Script::compile(scope, code, None)
            .unwrap()
            .run(scope)
            .unwrap()
            .to_rust_string_lossy(scope);
        let result: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            result,
            serde_json::json!({"old":true,"order":true,"detached":true,"protectedRoot":true})
        );
        reset();
    }

    #[test]
    fn native_shadow_roots_have_separate_contents_brand_and_open_closed_modes() {
        init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope,isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        install(scope);
        let code=v8::String::new(scope,r#"
          const host=document.createElement('div');document.body.appendChild(host);
          host.innerHTML='<i id=light>light</i>';
          const root=host.attachShadow({mode:'open'});
          root.innerHTML='<span id=secret>shadow</span>';
          const secret=root.querySelector('#secret');
          const branded=root!==host && root instanceof ShadowRoot && root instanceof DocumentFragment && root.nodeType===11 && root.host===host && root.mode==='open' && root.parentNode===null && host.shadowRoot===root;
          const boundary=secret && secret.parentNode===root && root.getRootNode()===root && secret.getRootNode()===root && secret.getRootNode({composed:true})===document && secret.isConnected && !host.contains(secret) && document.querySelector('#secret')===null && host.querySelector('#secret')===null && host.querySelector('#light')!==null;
          const hidden=document.createElement('div');document.body.appendChild(hidden);
          const closed=hidden.attachShadow({mode:'closed'});
          const modes=hidden.shadowRoot===null && closed.host===hidden && closed.mode==='closed';
          let duplicate=false,invalid=false,insertion=false,clone=false;
          try{hidden.attachShadow({mode:'open'})}catch(e){duplicate=e.name==='NotSupportedError'}
          try{Element.prototype.attachShadow.call({},{mode:'open'})}catch(e){invalid=e instanceof TypeError}
          try{host.appendChild(root)}catch(e){insertion=e.name==='HierarchyRequestError'}
          try{root.cloneNode(true)}catch(e){clone=e.name==='NotSupportedError'}
          JSON.stringify({branded,boundary,modes,duplicate,invalid,insertion,clone})
        "#).unwrap();
        let script = v8::Script::compile(scope, code, None).unwrap();
        let result = script.run(scope).unwrap().to_rust_string_lossy(scope);
        let value: serde_json::Value = serde_json::from_str(&result).unwrap();
        for key in [
            "branded",
            "boundary",
            "modes",
            "duplicate",
            "invalid",
            "insertion",
            "clone",
        ] {
            assert_eq!(value[key], true, "{key}: {result}");
        }
        reset();
    }

    #[test]
    fn webgl_reports_a_coherent_swiftshader_signature() {
        init_v8();
        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        install(scope);

        let run = |scope: &mut v8::PinScope, code: &str| -> String {
            let src = v8::String::new(scope, code).unwrap();
            let script = v8::Script::compile(scope, src, None).unwrap();
            let r = script.run(scope).unwrap();
            r.to_rust_string_lossy(scope)
        };

        let out = run(
            scope,
            r#"
            const c = document.createElement('canvas'); c.width = 256; c.height = 256;
            const gl = c.getContext('webgl');
            const notNull = gl != null;
            const dbg = gl.getExtension('WEBGL_debug_renderer_info');
            const unmaskedVendor = gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL);
            const unmaskedRenderer = gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL);
            const vendor = gl.getParameter(gl.VENDOR);
            const renderer = gl.getParameter(gl.RENDERER);
            const version = gl.getParameter(gl.VERSION);
            const maxTex = gl.getParameter(gl.MAX_TEXTURE_SIZE);
            const exts = gl.getSupportedExtensions();
            const hasDbg = exts.indexOf('WEBGL_debug_renderer_info') >= 0;
            const prec = gl.getShaderPrecisionFormat(gl.FRAGMENT_SHADER || 0x8B30, gl.HIGH_FLOAT);

            // readPixels: deterministic + content-dependent (draw ops change the hash).
            const rp = () => { const px = new Uint8Array(4*16); gl.readPixels(0,0,4,4,0x1908,0x1401,px); return Array.from(px.slice(0,8)).join(','); };
            const a1 = rp();
            gl.clearColor(0.1,0.2,0.3,1.0); gl.clear(0x4000); gl.drawArrays(0x0004,0,3);
            const a2 = rp();
            const a3 = rp(); // same op-state, read again → must be identical (deterministic)

            const gl2 = document.createElement('canvas').getContext('webgl2');
            const version2 = gl2.getParameter(gl2.VERSION);

            JSON.stringify({ notNull, unmaskedVendor, unmaskedRenderer, vendor, renderer, version,
              version2, maxTex, hasDbg, extCount: exts.length,
              precOk: prec.rangeMin === 127 && prec.rangeMax === 127 && prec.precision === 23,
              detStable: a2 === a3, contentDep: a1 !== a2 });
        "#,
        );
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["notNull"], true, "webgl context must not be null: {out}");
        assert_eq!(v["unmaskedVendor"], "Google Inc. (Google)", "{out}");
        assert_eq!(
            v["unmaskedRenderer"],
            "ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (Subzero) (0x0000C0DE)), SwiftShader driver)",
            "{out}"
        );
        assert_eq!(v["vendor"], "WebKit", "{out}");
        assert_eq!(v["renderer"], "WebKit WebGL", "{out}");
        assert_eq!(v["version"], "WebGL 1.0 (OpenGL ES 2.0 Chromium)", "{out}");
        assert_eq!(v["version2"], "WebGL 2.0 (OpenGL ES 3.0 Chromium)", "{out}");
        assert_eq!(v["maxTex"], 8192, "{out}");
        assert_eq!(
            v["hasDbg"], true,
            "WEBGL_debug_renderer_info must be advertised: {out}"
        );
        assert!(
            v["extCount"].as_u64().unwrap() > 20,
            "full extension list: {out}"
        );
        assert_eq!(
            v["precOk"], true,
            "highp float precision is SwiftShader's 127/127/23: {out}"
        );
        assert_eq!(
            v["detStable"], true,
            "identical GL draws hash identically: {out}"
        );
        assert_eq!(
            v["contentDep"], true,
            "different GL draws change readback: {out}"
        );
        reset();
    }
}
