//! Node-API (napi) host — load native `.node` addons in turbo-test's V8 embedding.
//!
//! Implements the subset of the napi C ABI that real addons call, backed by rusty_v8.
//! This is what Node/Deno/Bun each provide; here it is scoped to the surface turbo-dom's
//! parser addon needs (enumerated from `nm -u` on the .node):
//!   value/object/string/number, arrays, functions + cb_info, arraybuffer/typedarray,
//!   errors/exceptions, references, napi_wrap / unwrap / remove_wrap, classes and property
//!   descriptors (napi_define_class / napi_define_properties / napi_get_new_target, what
//!   node-addon-api's ObjectWrap builds on), threadsafe-fn (stubbed).
//!
//! Bridge: `napi_value` IS a `v8::Local<Value>` (Local is `NonNull<T>` — pointer-sized),
//! so they transmute. Handles are created in the scope stored on the env for the current
//! native call (set by the function trampoline / loader before calling into the addon, and
//! restored when a nested call returns).
//!
//! Semantics follow Node's js_native_api_v8.cc (the status codes, the checks and coercions):
//! every loaded addon gets its own env, owned by its isolate; callbacks may re-enter (native ->
//! JS -> native); calls that can run JS honour a pending exception (NAPI_PREAMBLE) and park
//! what JS throws on the env (`js_call`), from where napi_get_and_clear_last_exception can take
//! it or the trampoline throws it to the JS caller. No entrypoint panics across the FFI boundary.

#![allow(non_camel_case_types, non_upper_case_globals, dead_code, clippy::missing_safety_doc)]

use std::ffi::c_void;
use std::os::raw::c_char;
use std::path::Path;
use std::ptr::NonNull;

pub type napi_status = i32;
pub const napi_ok: napi_status = 0;
pub const napi_invalid_arg: napi_status = 1;
pub const napi_object_expected: napi_status = 2;
pub const napi_string_expected: napi_status = 3;
pub const napi_name_expected: napi_status = 4;
pub const napi_number_expected: napi_status = 6;
pub const napi_boolean_expected: napi_status = 7;
pub const napi_array_expected: napi_status = 8;
pub const napi_generic_failure: napi_status = 9;
pub const napi_pending_exception: napi_status = 10;

pub type napi_value = *mut c_void;
pub type napi_env = *mut Env;
pub type napi_ref = *mut c_void;
pub type napi_callback_info = *mut CbInfo;
pub type napi_callback = unsafe extern "C" fn(napi_env, napi_callback_info) -> napi_value;
pub type napi_finalize = unsafe extern "C" fn(napi_env, *mut c_void, *mut c_void);

/// napi_property_attributes bits.
pub const napi_writable: i32 = 1;
pub const napi_enumerable: i32 = 1 << 1;
pub const napi_configurable: i32 = 1 << 2;
pub const napi_static: i32 = 1 << 10;

/// One property for napi_define_class / napi_define_properties: named by `utf8name`, else by
/// the `name` value; a `method`, a `getter` / `setter` pair, or a plain `value`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct napi_property_descriptor {
    pub utf8name: *const c_char,
    pub name: napi_value,
    pub method: Option<napi_callback>,
    pub getter: Option<napi_callback>,
    pub setter: Option<napi_callback>,
    pub value: napi_value,
    pub attributes: i32,
    pub data: *mut c_void,
}

#[repr(i32)]
pub enum napi_valuetype {
    Undefined = 0,
    Null = 1,
    Boolean = 2,
    Number = 3,
    String = 4,
    Symbol = 5,
    Object = 6,
    Function = 7,
    External = 8,
    Bigint = 9,
}

/// One loaded addon's napi environment (Node creates one `napi_env` per addon instance), owned
/// by its isolate (`AddonEnvs`). `scope` points at the live rusty_v8 scope for the current
/// native call (the only window during which napi value calls happen).
pub struct Env {
    pub isolate: v8::UnsafeRawIsolatePtr,
    pub scope: *mut c_void, // *mut v8::PinScope<'static,'static>, valid only during a call
    pub context: v8::Global<v8::Context>,
    pub last_exception: Option<v8::Global<v8::Value>>,
    /// One record per napi_create_function; each function's data is an External pointing at
    /// its record, so a function always dispatches into the env (addon) that created it.
    pub fns: Vec<*mut Callback>,
    pub refs: Vec<Option<Ref>>,
    /// The live finalizer records of this env (napi_wrap, napi_add_finalizer), by creation order.
    pub finalizers: std::collections::BTreeMap<u64, *mut Finalizer>,
    pub next_finalizer: u64,
    /// The env is being torn down: napi value calls work, JS cannot run (Node's
    /// !can_call_into_js()).
    pub tearing_down: bool,
}

impl Drop for Env {
    /// Env teardown (the isolate is being disposed). As Node's napi_env__::DeleteMe, the
    /// finalizers still pending (napi_wrap, napi_add_finalizer) run now, newest first.
    fn drop(&mut self) {
        let env: napi_env = self;
        unsafe {
            if !(*env).finalizers.is_empty() {
                teardown_finalizers(env);
            }
        }
        for &f in &self.fns {
            drop(unsafe { Box::from_raw(f) });
        }
    }
}

/// Run the env's remaining finalizers, newest first, the way Node runs them at teardown: in a
/// handle scope of the env's context, so value calls (references, creating values) work, but
/// with JS disabled, so a call that could run JS is napi_pending_exception (`js_call`). The
/// isolate is mid-disposal (OwnedIsolate::drop has exited it before dropping its slots, the env
/// included) but not yet disposed: it is entered again for the scope.
unsafe fn teardown_finalizers(env: napi_env) {
    let isolate = &mut v8::Isolate::from_raw_isolate_ptr((*env).isolate);
    isolate.enter();
    {
        v8::scope!(let hs, isolate);
        let context = v8::Local::new(hs, &(*env).context);
        let cs: &mut v8::PinScope = &mut v8::ContextScope::new(hs, context);
        (*env).scope = cs as *mut v8::PinScope as *mut c_void;
        (*env).tearing_down = true;
        while let Some((_, fin)) = (*env).finalizers.pop_last() {
            run_finalizer(env, &Box::from_raw(fin));
        }
        (*env).scope = std::ptr::null_mut();
    }
    isolate.exit();
}

/// A napi_ref, as Node's v8impl::Reference: a value with a reference count. Above 0 the value
/// is held strongly; at 0 weakly (it goes when collected), which only objects and symbols can be
/// (any other value is dropped). The references napi_wrap and napi_add_finalizer hand back start
/// at 0, so they do not keep the object alive.
pub struct Ref {
    count: u32,
    value: RefValue,
}

pub enum RefValue {
    Strong(v8::Global<v8::Value>),
    Weak(v8::Weak<v8::Value>),
    /// No value any more: collected, dropped at count 0, or the reference of a finalizer record
    /// whose finalizer has run (or is running), as Node's Reference::Finalize resets it first.
    Empty,
}

impl Ref {
    /// A reference to `value` with `count`, weak at 0 (Node's Reference::New).
    fn new(s: &mut v8::PinScope<'_, '_>, value: v8::Local<v8::Value>, count: u32) -> Ref {
        let value = if count > 0 {
            RefValue::Strong(v8::Global::new(s, value))
        } else {
            weak_value(s, value)
        };
        Ref { count, value }
    }

    /// The value, None when it is gone.
    fn get<'s>(&self, s: &v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>> {
        match &self.value {
            RefValue::Strong(g) => Some(v8::Local::new(s, g)),
            RefValue::Weak(w) => w.to_local(s),
            RefValue::Empty => None,
        }
    }

    /// Reference::Ref: the new count, 0 (unchanged) once the value is gone; at 1 the value
    /// becomes strong.
    fn inc(&mut self, s: &mut v8::PinScope<'_, '_>) -> u32 {
        let Some(value) = self.get(s) else {
            self.value = RefValue::Empty;
            return 0;
        };
        self.count += 1;
        if self.count == 1 {
            self.value = RefValue::Strong(v8::Global::new(s, value));
        }
        self.count
    }

    /// Reference::Unref of a count above 0: the new count; at 0 the value becomes weak.
    fn dec(&mut self, s: &mut v8::PinScope<'_, '_>) -> u32 {
        let Some(value) = self.get(s) else {
            self.value = RefValue::Empty;
            return 0;
        };
        self.count -= 1;
        if self.count == 0 {
            self.value = weak_value(s, value);
        }
        self.count
    }
}

/// `value` held weakly: objects (functions included) and symbols, what V8 can hold weakly (Node's
/// CanBeHeldWeakly); any other value is dropped (Reference::SetWeak resets it).
fn weak_value(s: &mut v8::PinScope<'_, '_>, value: v8::Local<v8::Value>) -> RefValue {
    if value.is_object() || value.is_symbol() {
        RefValue::Weak(v8::Weak::new(s, value))
    } else {
        RefValue::Empty
    }
}

/// A native pointer tied to a JS object's lifetime, with its optional finalizer, which runs
/// once, when the object is collected or at env teardown. napi_wrap stores one on the object (an
/// External under the isolate's wrap private key points here; napi_remove_wrap drops it without
/// finalizing); napi_add_finalizer attaches any number to an object.
pub struct Finalizer {
    env: napi_env,
    id: u64,
    native: *mut c_void,
    finalize: Option<napi_finalize>,
    hint: *mut c_void,
    /// Weak handle on the object; its GC finalizer (`finalizer_collected`) frees this record.
    weak: Option<v8::Weak<v8::Object>>,
    /// The id of the result reference napi_wrap / napi_add_finalizer handed back (0: none).
    own_ref: usize,
}

/// Run `fin`'s finalizer (if any) in the env's current scope: addon code, so under the lock.
/// The env's pending exception is set aside for it, and what the finalizer leaves pending is
/// dropped (Node reports it as an uncaught exception; there is no JS caller to throw it to).
unsafe fn run_finalizer(env: napi_env, fin: &Finalizer) {
    if let Some(cb) = fin.finalize {
        // as Node's Reference::Finalize: the record's own reference loses its value first (a
        // deleted one stays deleted)
        let refs = &mut (*env).refs;
        if let Some(slot) = refs.get_mut(fin.own_ref.wrapping_sub(1)) {
            if let Some(r) = slot {
                r.value = RefValue::Empty;
            }
        }
        let pending = (*env).last_exception.take();
        let _ = call_addon(|| cb(env, fin.native, fin.hint));
        (*env).last_exception = pending;
    }
}

/// What a napi-created function calls: the addon callback, its data, and the env it belongs to.
#[derive(Clone, Copy)]
pub struct Callback {
    env: napi_env,
    cb: napi_callback,
    data: *mut c_void,
}

/// The envs of every addon loaded into an isolate, kept in an isolate slot so they are freed
/// with the isolate: the functions pointing into them cannot outlive it either.
struct AddonEnvs(Vec<napi_env>);

impl Drop for AddonEnvs {
    fn drop(&mut self) {
        for &env in &self.0 {
            drop(unsafe { Box::from_raw(env) });
        }
    }
}

/// Process-global lock serializing native `.node` addon entry across worker threads (the shared
/// addon's internal state isn't thread-safe). `IN_ADDON` allows same-thread re-entry.
static ADDON_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
thread_local! {
    static IN_ADDON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[inline]
unsafe fn scope<'a>(env: napi_env) -> &'a mut v8::PinScope<'static, 'static> {
    &mut *((*env).scope as *mut v8::PinScope<'static, 'static>)
}

#[inline]
unsafe fn to_napi(l: v8::Local<v8::Value>) -> napi_value {
    std::mem::transmute::<v8::Local<v8::Value>, *mut c_void>(l)
}

#[inline]
unsafe fn to_local(v: napi_value) -> v8::Local<'static, v8::Value> {
    std::mem::transmute::<*mut c_void, v8::Local<v8::Value>>(v)
}

#[inline]
unsafe fn put(out: *mut napi_value, l: v8::Local<v8::Value>) {
    if !out.is_null() {
        *out = to_napi(l);
    }
}

/// A napi call's outcome; `Err(status)` carries a non-`napi_ok` status, so `?` returns it.
type NapiResult = Result<(), napi_status>;

/// Run `f` the way Node runs a napi call that can execute JS (`NAPI_PREAMBLE` plus its
/// `v8impl::TryCatch`): while an exception is pending, or when JS cannot run (env teardown), it
/// returns `napi_pending_exception` without running `f`; otherwise it runs `f` under `catching`.
unsafe fn js_call(
    env: napi_env,
    f: impl FnOnce(&mut v8::PinScope<'static, 'static>) -> NapiResult,
) -> napi_status {
    // env teardown: JS cannot run (Node's !can_call_into_js(), which is napi_pending_exception
    // for an addon without a declared module API version)
    if (*env).last_exception.is_some() || (*env).tearing_down {
        return napi_pending_exception;
    }
    catching(env, f)
}

/// Run `f` in a TryCatch: a JS exception thrown inside it is caught and parked on the env as
/// its pending exception. `napi_is_exception_pending` / `napi_get_and_clear_last_exception`
/// see it, and the trampoline (or the module loader) throws it to JS when the addon returns
/// to its caller. A termination is not caught: it keeps unwinding.
unsafe fn catching(
    env: napi_env,
    f: impl FnOnce(&mut v8::PinScope<'static, 'static>) -> NapiResult,
) -> napi_status {
    let tc = std::pin::pin!(v8::TryCatch::new(scope(env)));
    let tc = &mut tc.init();
    let result = f(tc);
    if !tc.has_terminated() {
        if let Some(exc) = tc.exception() {
            (*env).last_exception = Some(v8::Global::new(tc, exc));
        }
    }
    result.err().unwrap_or(napi_ok)
}

/// ToObject of an object argument, as Node's CHECK_TO_OBJECT: primitives become their wrapper
/// objects; null / undefined throw a TypeError and the call is napi_object_expected.
unsafe fn to_object(
    s: &mut v8::PinScope<'static, 'static>,
    obj: napi_value,
) -> Result<v8::Local<'static, v8::Object>, napi_status> {
    to_local(obj).to_object(s).ok_or(napi_object_expected)
}

/// Callback info passed to a napi_callback during a function call.
pub struct CbInfo {
    pub args: Vec<napi_value>,
    pub this: napi_value,
    pub data: *mut c_void,
    /// `new.target` of a construct call, NULL for a plain call (napi_get_new_target).
    pub new_target: napi_value,
}

// ---- value getters --------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn napi_get_undefined(env: napi_env, out: *mut napi_value) -> napi_status {
    let s = scope(env);
    put(out, v8::undefined(s).into());
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_get_null(env: napi_env, out: *mut napi_value) -> napi_status {
    let s = scope(env);
    put(out, v8::null(s).into());
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_get_global(env: napi_env, out: *mut napi_value) -> napi_status {
    let s = scope(env);
    let g = s.get_current_context().global(s);
    put(out, g.into());
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_get_boolean(
    env: napi_env,
    b: bool,
    out: *mut napi_value,
) -> napi_status {
    let s = scope(env);
    put(out, v8::Boolean::new(s, b).into());
    napi_ok
}

#[no_mangle]
pub unsafe extern "C" fn napi_typeof(
    env: napi_env,
    v: napi_value,
    out: *mut napi_valuetype,
) -> napi_status {
    let val = to_local(v);
    let t = if val.is_undefined() {
        napi_valuetype::Undefined
    } else if val.is_null() {
        napi_valuetype::Null
    } else if val.is_boolean() {
        napi_valuetype::Boolean
    } else if val.is_number() {
        napi_valuetype::Number
    } else if val.is_string() {
        napi_valuetype::String
    } else if val.is_symbol() {
        napi_valuetype::Symbol
    } else if val.is_big_int() {
        napi_valuetype::Bigint
    } else if val.is_function() {
        napi_valuetype::Function
    } else {
        napi_valuetype::Object
    };
    let _ = env;
    if !out.is_null() {
        *out = t;
    }
    napi_ok
}

// ---- numbers / strings ----------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn napi_create_uint32(
    env: napi_env,
    n: u32,
    out: *mut napi_value,
) -> napi_status {
    let s = scope(env);
    put(out, v8::Integer::new_from_unsigned(s, n).into());
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_create_double(
    env: napi_env,
    n: f64,
    out: *mut napi_value,
) -> napi_status {
    let s = scope(env);
    put(out, v8::Number::new(s, n).into());
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_get_value_uint32(
    _env: napi_env,
    v: napi_value,
    out: *mut u32,
) -> napi_status {
    let Some(n) = number_of(v) else {
        return napi_number_expected;
    };
    if !out.is_null() {
        *out = to_uint32(n);
    }
    napi_ok
}

/// The value of a JS number, `None` for anything else: Node's number getters never coerce
/// (they return napi_number_expected).
unsafe fn number_of(v: napi_value) -> Option<f64> {
    v8::Local::<v8::Number>::try_from(to_local(v))
        .ok()
        .map(|n| n.value())
}

/// ECMAScript ToUint32, what V8's Uint32Value (Node's napi_get_value_uint32) computes: NaN and
/// the infinities are 0, anything else is truncated and wrapped modulo 2^32.
fn to_uint32(n: f64) -> u32 {
    if n.is_finite() {
        n.trunc().rem_euclid(4294967296.0) as u32
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_string_utf8(
    env: napi_env,
    str_: *const c_char,
    len: usize,
    out: *mut napi_value,
) -> napi_status {
    let s = scope(env);
    let bytes = utf8_arg(str_, len);
    // V8's String::NewFromUtf8 (what Node uses) replaces invalid sequences with U+FFFD.
    match v8::String::new_from_utf8(s, bytes, v8::NewStringType::Normal) {
        Some(v) => {
            put(out, v.into());
            napi_ok
        }
        None => napi_generic_failure,
    }
}

/// The bytes of a `(const char*, size_t)` string argument; NAPI_AUTO_LENGTH means NUL-terminated.
unsafe fn utf8_arg<'a>(str_: *const c_char, len: usize) -> &'a [u8] {
    if len == usize::MAX {
        std::ffi::CStr::from_ptr(str_).to_bytes()
    } else {
        std::slice::from_raw_parts(str_ as *const u8, len)
    }
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_value_string_utf8(
    env: napi_env,
    v: napi_value,
    buf: *mut c_char,
    bufsize: usize,
    result: *mut usize,
) -> napi_status {
    let s = scope(env);
    let Ok(str_val) = v8::Local::<v8::String>::try_from(to_local(v)) else {
        return napi_string_expected;
    };
    // as Node: a NULL buf is a length query, which needs `result` (CHECK_ARG(env, result))
    if buf.is_null() {
        if result.is_null() {
            return napi_invalid_arg;
        }
        *result = str_val.utf8_length(s);
        return napi_ok;
    }
    // As Node: bufsize 0 writes nothing; otherwise at most bufsize - 1 bytes of whole UTF-8
    // characters (never a split sequence; lone surrogates become U+FFFD) plus the NUL.
    let n = if bufsize == 0 {
        0
    } else {
        let dst = std::slice::from_raw_parts_mut(buf as *mut u8, bufsize - 1);
        let n = str_val.write_utf8_v2(s, dst, v8::WriteFlags::kReplaceInvalidUtf8, None);
        *buf.add(n) = 0;
        n
    };
    if !result.is_null() {
        *result = n;
    }
    napi_ok
}

#[no_mangle]
pub unsafe extern "C" fn napi_coerce_to_string(
    env: napi_env,
    v: napi_value,
    out: *mut napi_value,
) -> napi_status {
    js_call(env, |s| {
        // as Node's CHECK_TO_STRING: a ToString that throws is napi_string_expected
        let str_ = to_local(v).to_string(s).ok_or(napi_string_expected)?;
        put(out, str_.into());
        Ok(())
    })
}

// ---- objects / arrays -----------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn napi_create_object(env: napi_env, out: *mut napi_value) -> napi_status {
    let s = scope(env);
    put(out, v8::Object::new(s).into());
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_create_array_with_length(
    env: napi_env,
    len: usize,
    out: *mut napi_value,
) -> napi_status {
    let s = scope(env);
    put(out, v8::Array::new(s, len as i32).into());
    napi_ok
}

#[no_mangle]
pub unsafe extern "C" fn napi_set_named_property(
    env: napi_env,
    obj: napi_value,
    name: *const c_char,
    value: napi_value,
) -> napi_status {
    js_call(env, |s| {
        let key = std::ffi::CStr::from_ptr(name).to_string_lossy();
        let k = v8::String::new(s, &key).ok_or(napi_generic_failure)?;
        let o = to_object(s, obj)?;
        // V8's Object::Set is Nothing only when it threw (a throwing setter or proxy trap)
        o.set(s, k.into(), to_local(value))
            .ok_or(napi_pending_exception)?;
        Ok(())
    })
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_named_property(
    env: napi_env,
    obj: napi_value,
    name: *const c_char,
    out: *mut napi_value,
) -> napi_status {
    js_call(env, |s| {
        let key = std::ffi::CStr::from_ptr(name).to_string_lossy();
        let k = v8::String::new(s, &key).ok_or(napi_generic_failure)?;
        let o = to_object(s, obj)?;
        put(out, o.get(s, k.into()).ok_or(napi_pending_exception)?);
        Ok(())
    })
}

#[no_mangle]
pub unsafe extern "C" fn napi_set_element(
    env: napi_env,
    obj: napi_value,
    index: u32,
    value: napi_value,
) -> napi_status {
    js_call(env, |s| {
        let o = to_object(s, obj)?;
        o.set_index(s, index, to_local(value))
            .ok_or(napi_pending_exception)?;
        Ok(())
    })
}

// ---- functions ------------------------------------------------------------

/// Trampoline: every napi function shares this rusty_v8 callback. It recovers the function's
/// `Callback` record from the External stored as its data, packages CbInfo, points the
/// owning env's scope at the current scope, and calls the addon callback.
fn napi_fn_trampoline(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let Ok(record) = v8::Local::<v8::External>::try_from(args.data()) else {
        return;
    };
    let Callback {
        env: env_ptr,
        cb,
        data,
    } = unsafe { *(record.value() as *const Callback) };
    let argv: Vec<napi_value> = (0..args.length())
        .map(|i| unsafe { to_napi(args.get(i)) })
        .collect();
    let new_target = if args.is_construct_call() {
        unsafe { to_napi(args.new_target()) }
    } else {
        std::ptr::null_mut()
    };
    let mut info = CbInfo {
        args: argv,
        this: unsafe { to_napi(args.this().into()) },
        data,
        new_target,
    };
    // Point the env at this call's scope for the callback's duration and restore it after: a
    // nested call (the addon calling JS that calls the addon again) must not leave the outer
    // callback with the inner, already-dead scope.
    let here = scope as *mut v8::PinScope as *mut c_void;
    let outer = unsafe { std::mem::replace(&mut (*env_ptr).scope, here) };
    // The .node addon (turbo-dom parser) is one shared library across all worker threads, so
    // its internal (C/Rust) state is process-global and NOT thread-safe — concurrent calls
    // race (intermittent "document is not defined" / load flakiness). Serialize native addon
    // entry across threads with a global lock; a thread-local guard allows same-thread
    // re-entry (the addon calling a JS callback that re-enters) without self-deadlock.
    // Calling the addon's native callback crosses the FFI boundary. A panic there is UB and
    // can crash the process; call_addon converts it into a thrown JS error instead.
    let ret = call_addon(|| unsafe { cb(env_ptr, &mut info as *mut CbInfo) });
    unsafe { (*env_ptr).scope = outer };
    // As Node's CallIntoModule: an exception the callback left pending is thrown to its caller.
    if let Some(exc) = unsafe { (*env_ptr).last_exception.take() } {
        let exc = v8::Local::new(scope, &exc);
        scope.throw_exception(exc);
    }
    if let Some(ret) = callback_return(scope, ret) {
        rv.set(unsafe { to_local(ret) });
    }
}

/// Enter native addon code on this thread: `(reentrant, guard)`. The outermost entry marks the
/// thread as inside the addon and takes the process-global `ADDON_LOCK`; a same-thread re-entry
/// takes no lock (it would self-deadlock). The caller clears `IN_ADDON` when not `reentrant`.
fn enter_addon() -> (bool, Option<std::sync::MutexGuard<'static, ()>>) {
    let reentrant = IN_ADDON.with(|f| f.get());
    let guard = if reentrant {
        None
    } else {
        IN_ADDON.with(|f| f.set(true));
        Some(ADDON_LOCK.lock().unwrap_or_else(|e| e.into_inner()))
    };
    (reentrant, guard)
}

/// Run native addon code (a callback, a module init) under `enter_addon`, catching a Rust panic
/// out of it (one crossing the `extern "C"` boundary is UB) as an `Err`.
fn call_addon<R>(f: impl FnOnce() -> R) -> std::thread::Result<R> {
    let (reentrant, _guard) = enter_addon();
    let ret = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    if !reentrant {
        IN_ADDON.with(|f| f.set(false));
    }
    ret
}

/// The JS-visible outcome of a native callback: a non-null `napi_value` is the return value,
/// NULL leaves the return slot `undefined` (`None`), and a panic is thrown as a JS error.
fn callback_return(
    scope: &mut v8::PinScope,
    ret: std::thread::Result<napi_value>,
) -> Option<napi_value> {
    match ret {
        Ok(ret) if !ret.is_null() => Some(ret),
        Ok(_) => None,
        Err(_) => {
            if let Some(msg) = v8::String::new(scope, "turbo-test: native addon callback panicked") {
                let exc = v8::Exception::error(scope, msg);
                scope.throw_exception(exc);
            }
            None
        }
    }
}

/// A panic out of the addon's module init becomes a load error naming the addon.
fn init_outcome(result: std::thread::Result<napi_value>, path: &Path) -> Result<napi_value, String> {
    result.map_err(|_| format!("native addon panicked during module init: {}", path.display()))
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_function(
    env: napi_env,
    name: *const c_char,
    len: usize,
    cb: napi_callback,
    data: *mut c_void,
    out: *mut napi_value,
) -> napi_status {
    js_call(env, |s| {
        let f = new_function(s, env, cb, data)?;
        // as Node: a non-NULL name becomes the function's `name` (a name V8 cannot hold is
        // napi_generic_failure)
        if !name.is_null() {
            let name = utf8_arg(name, len);
            let name = v8::String::new_from_utf8(s, name, v8::NewStringType::Internalized)
                .ok_or(napi_generic_failure)?;
            f.set_name(name);
        }
        put(out, f.into());
        Ok(())
    })
}

/// A function template dispatching to the addon callback `cb` with `data` (its `Callback`
/// record is owned by the env), optionally with a receiver `signature` — Node's
/// FunctionCallbackWrapper::NewTemplate.
unsafe fn callback_template<'s>(
    s: &mut v8::PinScope<'s, 'static>,
    env: napi_env,
    cb: napi_callback,
    data: *mut c_void,
    signature: Option<v8::Local<'s, v8::Signature>>,
) -> v8::Local<'s, v8::FunctionTemplate> {
    let record = Box::into_raw(Box::new(Callback { env, cb, data }));
    (*env).fns.push(record);
    let data_val = v8::External::new(s, record as *mut c_void);
    let builder = v8::FunctionTemplate::builder(napi_fn_trampoline).data(data_val.into());
    match signature {
        Some(sig) => builder.signature(sig).build(s),
        None => builder.build(s),
    }
}

/// An unnamed function dispatching to `cb` (Node's FunctionCallbackWrapper::NewFunction).
unsafe fn new_function<'s>(
    s: &mut v8::PinScope<'s, 'static>,
    env: napi_env,
    cb: napi_callback,
    data: *mut c_void,
) -> Result<v8::Local<'s, v8::Function>, napi_status> {
    let tmpl = callback_template(s, env, cb, data, None);
    tmpl.get_function(s).ok_or(napi_generic_failure)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_new_target(
    _env: napi_env,
    info: napi_callback_info,
    result: *mut napi_value,
) -> napi_status {
    // as Node: both arguments are required; a plain (non-construct) call gives NULL
    if info.is_null() || result.is_null() {
        return napi_invalid_arg;
    }
    *result = (*info).new_target;
    napi_ok
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_cb_info(
    env: napi_env,
    info: napi_callback_info,
    argc: *mut usize,
    argv: *mut napi_value,
    this: *mut napi_value,
    data: *mut *mut c_void,
) -> napi_status {
    let cb = &*info;
    if !argc.is_null() {
        let want = *argc;
        let have = cb.args.len();
        let n = want.min(have);
        if !argv.is_null() {
            for i in 0..n {
                *argv.add(i) = cb.args[i];
            }
            // as in Node, slots past the actual arguments are filled with `undefined`
            let undefined = to_napi(v8::undefined(scope(env)).into());
            for i in n..want {
                *argv.add(i) = undefined;
            }
        }
        *argc = have;
    }
    if !this.is_null() {
        *this = cb.this;
    }
    if !data.is_null() {
        *data = cb.data;
    }
    napi_ok
}

#[no_mangle]
pub unsafe extern "C" fn napi_call_function(
    env: napi_env,
    recv: napi_value,
    func: napi_value,
    argc: usize,
    argv: *const napi_value,
    out: *mut napi_value,
) -> napi_status {
    js_call(env, |s| {
        // as Node (CHECK_ARG(env, recv)): a receiver is required; an addon passes `undefined`.
        // So is argv when argc > 0 (CHECK_ARG(env, argv)): nothing is called.
        if recv.is_null() || (argc > 0 && argv.is_null()) {
            return Err(napi_invalid_arg);
        }
        let f: v8::Local<v8::Function> = to_local(func).try_into().map_err(|_| napi_invalid_arg)?;
        let args: Vec<v8::Local<v8::Value>> = (0..argc).map(|i| to_local(*argv.add(i))).collect();
        put(
            out,
            f.call(s, to_local(recv), &args)
                .ok_or(napi_pending_exception)?,
        );
        Ok(())
    })
}

// ---- arraybuffer / typedarray (for the parser's SoA buffer) ----------------

#[no_mangle]
pub unsafe extern "C" fn napi_create_arraybuffer(
    env: napi_env,
    len: usize,
    data: *mut *mut c_void,
    out: *mut napi_value,
) -> napi_status {
    js_call(env, |s| {
        let ab = v8::ArrayBuffer::new(s, len);
        if !data.is_null() {
            let store = ab.get_backing_store();
            *data = store
                .data()
                .map(|p| p.as_ptr())
                .unwrap_or(std::ptr::null_mut());
        }
        put(out, ab.into());
        Ok(())
    })
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_typedarray(
    env: napi_env,
    type_: i32,
    length: usize,
    arraybuffer: napi_value,
    byte_offset: usize,
    out: *mut napi_value,
) -> napi_status {
    js_call(env, |s| {
        let ab: v8::Local<v8::ArrayBuffer> = to_local(arraybuffer)
            .try_into()
            .map_err(|_| napi_invalid_arg)?;
        // napi_typedarray_type codes (Node 26: 0..=11): element size and constructor name.
        let (size, name) = match type_ {
            0 => (1, "Int8Array"),
            1 => (1, "Uint8Array"),
            2 => (1, "Uint8ClampedArray"),
            3 => (2, "Int16Array"),
            4 => (2, "Uint16Array"),
            5 => (4, "Int32Array"),
            6 => (4, "Uint32Array"),
            7 => (4, "Float32Array"),
            8 => (8, "Float64Array"),
            9 => (8, "BigInt64Array"),
            10 => (8, "BigUint64Array"),
            11 => (2, "Float16Array"),
            // as Node: an unknown napi_typedarray_type is napi_invalid_arg
            _ => return Err(napi_invalid_arg),
        };
        // As Node (THROW_RANGE_ERROR_IF_FALSE): a misaligned offset or a view running past the
        // buffer throws a RangeError; V8's constructors would not check (or abort). `length` is
        // in ELEMENTS; a product overflowing usize is a length error too.
        if !byte_offset.is_multiple_of(size) {
            let msg = format!("start offset of {name} should be a multiple of {size}");
            return throw_range_error(s, "ERR_NAPI_INVALID_TYPEDARRAY_ALIGNMENT", &msg);
        }
        let end = length
            .checked_mul(size)
            .and_then(|n| n.checked_add(byte_offset));
        if end.is_none_or(|end| end > ab.byte_length()) {
            let code = "ERR_NAPI_INVALID_TYPEDARRAY_LENGTH";
            return throw_range_error(s, code, "Invalid typed array length");
        }
        let (o, n) = (byte_offset, length);
        let ta: Option<v8::Local<v8::Value>> = match type_ {
            0 => v8::Int8Array::new(s, ab, o, n).map(Into::into),
            1 => v8::Uint8Array::new(s, ab, o, n).map(Into::into),
            2 => v8::Uint8ClampedArray::new(s, ab, o, n).map(Into::into),
            3 => v8::Int16Array::new(s, ab, o, n).map(Into::into),
            4 => v8::Uint16Array::new(s, ab, o, n).map(Into::into),
            5 => v8::Int32Array::new(s, ab, o, n).map(Into::into),
            6 => v8::Uint32Array::new(s, ab, o, n).map(Into::into),
            7 => v8::Float32Array::new(s, ab, o, n).map(Into::into),
            8 => v8::Float64Array::new(s, ab, o, n).map(Into::into),
            9 => v8::BigInt64Array::new(s, ab, o, n).map(Into::into),
            // (this V8 binding has no Float16Array -> Value `From`; the upcast is always valid)
            11 => v8::Float16Array::new(s, ab, o, n).map(|t| v8::Local::cast_unchecked(t)),
            _ => v8::BigUint64Array::new(s, ab, o, n).map(Into::into), // 10
        };
        put(out, ta.ok_or(napi_generic_failure)?);
        Ok(())
    })
}

/// Throw a RangeError carrying `code`, as Node's napi_throw_range_error, and fail the call with
/// napi_generic_failure.
unsafe fn throw_range_error(
    s: &mut v8::PinScope<'static, 'static>,
    code: &str,
    msg: &str,
) -> NapiResult {
    let msg = v8::String::new(s, msg).ok_or(napi_generic_failure)?;
    let code = v8::String::new(s, code).ok_or(napi_generic_failure)?;
    let err = v8::Exception::range_error(s, msg);
    set_error_code(s, err, code)?;
    s.throw_exception(err);
    Err(napi_generic_failure)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_external_arraybuffer(
    env: napi_env,
    data: *mut c_void,
    len: usize,
    _finalize: Option<napi_finalize>,
    _hint: *mut c_void,
    out: *mut napi_value,
) -> napi_status {
    js_call(env, |s| {
        // copy into a managed ArrayBuffer (we don't track external lifetime here)
        let ab = v8::ArrayBuffer::new(s, len);
        if let Some(store) = ab.get_backing_store().data() {
            std::ptr::copy_nonoverlapping(data as *const u8, store.as_ptr() as *mut u8, len);
        }
        put(out, ab.into());
        Ok(())
    })
}

// ---- buffers (Node `Buffer` == Uint8Array; addons returning bytes use these) ---
// turbo-html2pdf and most napi-rs addons that hand back binary data (PDF bytes,
// images, etc.) call the buffer family. These were previously UNEXPORTED, so the
// addon's reference resolved (flat namespace, -export_dynamic) to address 0x0 and
// the first call jumped to NULL -> SIGSEGV that killed the whole run with no output.
// Back them with a Uint8Array (the runtime exposes Buffer as a Uint8Array subclass),
// which is what real Buffer-consuming JS expects.

#[no_mangle]
pub unsafe extern "C" fn napi_create_buffer(
    env: napi_env,
    len: usize,
    data: *mut *mut c_void,
    out: *mut napi_value,
) -> napi_status {
    js_call(env, |s| {
        let ab = v8::ArrayBuffer::new(s, len);
        if !data.is_null() {
            *data = ab
                .get_backing_store()
                .data()
                .map(|p| p.as_ptr())
                .unwrap_or(std::ptr::null_mut());
        }
        let buf: v8::Local<v8::Value> = v8::Uint8Array::new(s, ab, 0, len)
            .map(|t| t.into())
            .unwrap_or_else(|| ab.into());
        put(out, buf);
        Ok(())
    })
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_buffer_copy(
    env: napi_env,
    len: usize,
    src: *const c_void,
    result_data: *mut *mut c_void,
    out: *mut napi_value,
) -> napi_status {
    js_call(env, |s| {
        let ab = v8::ArrayBuffer::new(s, len);
        if let Some(store) = ab.get_backing_store().data() {
            if !src.is_null() && len > 0 {
                std::ptr::copy_nonoverlapping(src as *const u8, store.as_ptr() as *mut u8, len);
            }
            if !result_data.is_null() {
                *result_data = store.as_ptr();
            }
        } else if !result_data.is_null() {
            *result_data = std::ptr::null_mut();
        }
        let buf: v8::Local<v8::Value> = v8::Uint8Array::new(s, ab, 0, len)
            .map(|t| t.into())
            .unwrap_or_else(|| ab.into());
        put(out, buf);
        Ok(())
    })
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_external_buffer(
    env: napi_env,
    len: usize,
    data: *mut c_void,
    _finalize: Option<napi_finalize>,
    _hint: *mut c_void,
    out: *mut napi_value,
) -> napi_status {
    js_call(env, |s| {
        // We don't track the external lifetime; copy into a managed buffer.
        let ab = v8::ArrayBuffer::new(s, len);
        if let Some(store) = ab.get_backing_store().data() {
            if !data.is_null() && len > 0 {
                std::ptr::copy_nonoverlapping(data as *const u8, store.as_ptr() as *mut u8, len);
            }
        }
        let buf: v8::Local<v8::Value> = v8::Uint8Array::new(s, ab, 0, len)
            .map(|t| t.into())
            .unwrap_or_else(|| ab.into());
        put(out, buf);
        Ok(())
    })
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_buffer_info(
    env: napi_env,
    v: napi_value,
    data: *mut *mut c_void,
    length: *mut usize,
) -> napi_status {
    let s = scope(env);
    let val = to_local(v);
    // As Node (node::Buffer::HasInstance): any ArrayBufferView (Buffer, Uint8Array, DataView,
    // ...); anything else, a raw ArrayBuffer included, is napi_invalid_arg.
    let Ok(view) = TryInto::<v8::Local<v8::ArrayBufferView>>::try_into(val) else {
        return napi_invalid_arg;
    };
    let len = view.byte_length();
    if !length.is_null() {
        *length = len;
    }
    if !data.is_null() {
        let off = view.byte_offset();
        *data = view
            .buffer(s)
            .and_then(|b| b.get_backing_store().data())
            .map(|p| (p.as_ptr() as *mut u8).add(off) as *mut c_void)
            .unwrap_or(std::ptr::null_mut());
    }
    napi_ok
}

// ---- more value getters (bool/double/int64) -------------------------------

#[no_mangle]
pub unsafe extern "C" fn napi_get_value_bool(
    _env: napi_env,
    v: napi_value,
    out: *mut bool,
) -> napi_status {
    // as Node: only a JS boolean, no ToBoolean coercion
    let Ok(b) = v8::Local::<v8::Boolean>::try_from(to_local(v)) else {
        return napi_boolean_expected;
    };
    if !out.is_null() {
        *out = b.is_true();
    }
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_get_value_double(
    _env: napi_env,
    v: napi_value,
    out: *mut f64,
) -> napi_status {
    let Some(n) = number_of(v) else {
        return napi_number_expected;
    };
    if !out.is_null() {
        *out = n;
    }
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_get_value_int64(
    _env: napi_env,
    v: napi_value,
    out: *mut i64,
) -> napi_status {
    let Some(n) = number_of(v) else {
        return napi_number_expected;
    };
    if !out.is_null() {
        // as Node: NaN and the infinities are 0; finite values truncate, saturating at the
        // int64 range (V8's IntegerValue)
        *out = if n.is_finite() { n as i64 } else { 0 };
    }
    napi_ok
}

// ---- arrays (length / element access / type test / key enumeration) -------

#[no_mangle]
pub unsafe extern "C" fn napi_is_array(
    _env: napi_env,
    v: napi_value,
    out: *mut bool,
) -> napi_status {
    if !out.is_null() {
        *out = to_local(v).is_array();
    }
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_get_array_length(
    env: napi_env,
    v: napi_value,
    out: *mut u32,
) -> napi_status {
    js_call(env, |_| {
        let arr: v8::Local<v8::Array> = to_local(v).try_into().map_err(|_| napi_array_expected)?;
        if !out.is_null() {
            *out = arr.length();
        }
        Ok(())
    })
}
#[no_mangle]
pub unsafe extern "C" fn napi_get_element(
    env: napi_env,
    obj: napi_value,
    index: u32,
    out: *mut napi_value,
) -> napi_status {
    js_call(env, |s| {
        let o = to_object(s, obj)?;
        put(out, o.get_index(s, index).ok_or(napi_pending_exception)?);
        Ok(())
    })
}
#[no_mangle]
pub unsafe extern "C" fn napi_get_property_names(
    env: napi_env,
    obj: napi_value,
    out: *mut napi_value,
) -> napi_status {
    js_call(env, |s| {
        let o = to_object(s, obj)?;
        // as Node: the for-in keys (prototype chain, enumerable, no symbols), indices as strings
        let args = v8::GetPropertyNamesArgs {
            mode: v8::KeyCollectionMode::IncludePrototypes,
            property_filter: v8::PropertyFilter::ONLY_ENUMERABLE | v8::PropertyFilter::SKIP_SYMBOLS,
            index_filter: v8::IndexFilter::IncludeIndices,
            key_conversion: v8::KeyConversionMode::ConvertToString,
        };
        let names = o
            .get_property_names(s, args)
            .ok_or(napi_pending_exception)?;
        put(out, names.into());
        Ok(())
    })
}

// ---- wrap / unwrap / remove_wrap ---------------------------------------------

/// The isolate-wide private key under which an object carries its wrap (as Node's
/// `napi_wrapper` private symbol: every addon of an isolate sees the same wraps).
fn wrap_key<'s>(s: &v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let name = v8::String::new(s, "turbo-test::napi_wrap");
    v8::Private::for_api(s, name)
}

/// The object a wrap call targets: as Node, NULL or a non-object is napi_invalid_arg.
unsafe fn wrap_target(v: napi_value) -> Result<v8::Local<'static, v8::Object>, napi_status> {
    if v.is_null() {
        return Err(napi_invalid_arg);
    }
    to_local(v).try_into().map_err(|_| napi_invalid_arg)
}

#[no_mangle]
pub unsafe extern "C" fn napi_wrap(
    env: napi_env,
    js_object: napi_value,
    native_object: *mut c_void,
    finalize_cb: Option<napi_finalize>,
    finalize_hint: *mut c_void,
    result: *mut napi_ref,
) -> napi_status {
    js_call(env, |s| {
        let obj = wrap_target(js_object)?;
        let key = wrap_key(s);
        // as Node: an object carries one wrap; a result reference needs a finalizer (it is to
        // be deleted from the finalizer)
        if obj.has_private(s, key) == Some(true) || (!result.is_null() && finalize_cb.is_none()) {
            return Err(napi_invalid_arg);
        }
        let wrap = track_finalizer(s, env, obj, native_object, finalize_cb, finalize_hint);
        obj.set_private(s, key, v8::External::new(s, wrap as *mut c_void).into());
        if !result.is_null() {
            *result = weak_ref(s, env, obj);
            (*wrap).own_ref = *result as usize;
        }
        Ok(())
    })
}

/// A new finalizer record for `obj`, held by the env until the object is collected (or the env
/// is torn down).
unsafe fn track_finalizer(
    s: &mut v8::PinScope<'static, 'static>,
    env: napi_env,
    obj: v8::Local<v8::Object>,
    native: *mut c_void,
    finalize: Option<napi_finalize>,
    hint: *mut c_void,
) -> *mut Finalizer {
    let e = &mut *env;
    let id = e.next_finalizer;
    e.next_finalizer += 1;
    let fin = Box::into_raw(Box::new(Finalizer {
        env,
        id,
        native,
        finalize,
        hint,
        weak: None,
        own_ref: 0,
    }));
    let on_gc = Box::new(move |isolate: &mut v8::Isolate| finalizer_collected(isolate, fin));
    (*fin).weak = Some(v8::Weak::with_finalizer(s, obj, on_gc));
    e.finalizers.insert(id, fin);
    fin
}

/// A new weak reference to `obj` (the optional result of napi_wrap / napi_add_finalizer).
unsafe fn weak_ref(
    s: &mut v8::PinScope<'static, 'static>,
    env: napi_env,
    obj: v8::Local<v8::Object>,
) -> napi_ref {
    new_ref(&mut *env, Ref::new(s, obj.into(), 0))
}

/// GC finalizer of an object with a finalizer record: the record leaves its env and its
/// finalizer runs in a fresh handle scope of the env's context (V8 calls this after the
/// collection, where napi calls are allowed).
fn finalizer_collected(isolate: &mut v8::Isolate, fin: *mut Finalizer) {
    unsafe {
        let fin = Box::from_raw(fin);
        let env = fin.env;
        (*env).finalizers.remove(&fin.id);
        v8::scope!(let hs, isolate);
        let context = v8::Local::new(hs, &(*env).context);
        let cs: &mut v8::PinScope = &mut v8::ContextScope::new(hs, context);
        let here = cs as *mut v8::PinScope as *mut c_void;
        let outer = std::mem::replace(&mut (*env).scope, here);
        run_finalizer(env, &fin);
        (*env).scope = outer;
    }
}

/// napi_unwrap (`remove` false) and napi_remove_wrap (`remove` true), as Node's v8impl::Unwrap:
/// the wrapped pointer, and for remove_wrap the wrap is dropped without running its finalizer.
unsafe fn unwrap(
    env: napi_env,
    js_object: napi_value,
    result: *mut *mut c_void,
    remove: bool,
) -> napi_status {
    js_call(env, |s| {
        // napi_unwrap needs `result`; napi_remove_wrap may discard the pointer
        if !remove && result.is_null() {
            return Err(napi_invalid_arg);
        }
        let obj = wrap_target(js_object)?;
        let key = wrap_key(s);
        let held = obj.get_private(s, key).ok_or(napi_pending_exception)?;
        let ext: v8::Local<v8::External> = held.try_into().map_err(|_| napi_invalid_arg)?;
        let wrap = ext.value() as *mut Finalizer;
        if !result.is_null() {
            *result = (*wrap).native;
        }
        if remove {
            obj.delete_private(s, key);
            (*(*wrap).env).finalizers.remove(&(*wrap).id);
            drop(Box::from_raw(wrap));
        }
        Ok(())
    })
}

#[no_mangle]
pub unsafe extern "C" fn napi_unwrap(
    env: napi_env,
    js_object: napi_value,
    result: *mut *mut c_void,
) -> napi_status {
    unwrap(env, js_object, result, false)
}

#[no_mangle]
pub unsafe extern "C" fn napi_remove_wrap(
    env: napi_env,
    js_object: napi_value,
    result: *mut *mut c_void,
) -> napi_status {
    unwrap(env, js_object, result, true)
}

#[no_mangle]
pub unsafe extern "C" fn napi_add_finalizer(
    env: napi_env,
    js_object: napi_value,
    finalize_data: *mut c_void,
    finalize_cb: Option<napi_finalize>,
    finalize_hint: *mut c_void,
    result: *mut napi_ref,
) -> napi_status {
    // as Node: no NAPI_PREAMBLE (it runs no JS, also with an exception pending); the object and
    // the finalizer are required, and the object must be one (napi_invalid_arg)
    let Some(finalize_cb) = finalize_cb else {
        return napi_invalid_arg;
    };
    let Ok(obj) = wrap_target(js_object) else {
        return napi_invalid_arg;
    };
    let s = scope(env);
    let fin = track_finalizer(s, env, obj, finalize_data, Some(finalize_cb), finalize_hint);
    if !result.is_null() {
        *result = weak_ref(s, env, obj);
        (*fin).own_ref = *result as usize;
    }
    napi_ok
}

// ---- type tags ------------------------------------------------------------------

/// A 128-bit type tag (napi_type_tag).
#[repr(C)]
pub struct napi_type_tag {
    pub lower: u64,
    pub upper: u64,
}

/// The isolate-wide private key under which an object carries its type tag (Node's
/// `napi_type_tag` private symbol: one tag per object, whatever its value).
fn type_tag_key<'s>(s: &v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let name = v8::String::new(s, "turbo-test::napi_type_tag");
    v8::Private::for_api(s, name)
}

/// The object a type-tag call targets, as Node's CHECK_TO_OBJECT: NULL is napi_invalid_arg,
/// a primitive is converted with ToObject (null / undefined throw: napi_object_expected).
unsafe fn tag_target(
    s: &mut v8::PinScope<'static, 'static>,
    v: napi_value,
) -> Result<v8::Local<'static, v8::Object>, napi_status> {
    if v.is_null() {
        return Err(napi_invalid_arg);
    }
    to_object(s, v)
}

#[no_mangle]
pub unsafe extern "C" fn napi_type_tag_object(
    env: napi_env,
    object: napi_value,
    type_tag: *const napi_type_tag,
) -> napi_status {
    js_call(env, |s| {
        let obj = tag_target(s, object)?;
        if type_tag.is_null() {
            return Err(napi_invalid_arg);
        }
        // as Node: the tag is a BigInt of the two words, under one private key: an object that
        // already carries a tag is napi_invalid_arg
        let key = type_tag_key(s);
        if obj.has_private(s, key) != Some(false) {
            return Err(napi_invalid_arg);
        }
        let tag = &*type_tag;
        let value = v8::BigInt::new_from_words(s, false, &[tag.lower, tag.upper])
            .ok_or(napi_generic_failure)?;
        obj.set_private(s, key, value.into())
            .filter(|&set| set)
            .ok_or(napi_generic_failure)?;
        Ok(())
    })
}

#[no_mangle]
pub unsafe extern "C" fn napi_check_object_type_tag(
    env: napi_env,
    object: napi_value,
    type_tag: *const napi_type_tag,
    result: *mut bool,
) -> napi_status {
    js_call(env, |s| {
        let obj = tag_target(s, object)?;
        if type_tag.is_null() || result.is_null() {
            return Err(napi_invalid_arg);
        }
        let key = type_tag_key(s);
        let held = obj.get_private(s, key).ok_or(napi_generic_failure)?;
        let tag = &*type_tag;
        *result = v8::Local::<v8::BigInt>::try_from(held).is_ok_and(|held| {
            // as Node: a non-negative BigInt of up to two words, missing high words being 0
            let mut words = [0u64; 2];
            let (negative, _) = held.to_words_array(&mut words);
            !negative && words == [tag.lower, tag.upper]
        });
        Ok(())
    })
}

// ---- new_instance: not supported; surface as a CATCHABLE JS throw ----------
// Constructing a native class instance (napi_new_instance) is not modelled. Rather than leave it
// UNEXPORTED (null pointer -> SIGSEGV when the addon calls it), export it and throw a clear,
// catchable JS error so the require() fails as a normal load-error and the rest of the run
// survives.
unsafe fn throw_unsupported(env: napi_env, name: &str) -> napi_status {
    js_call(env, |s| {
        let msg = format!("turbo-test: N-API {name} is not implemented (native addon called an unsupported entrypoint)");
        if let Some(m) = v8::String::new(s, &msg) {
            let exc = v8::Exception::error(s, m);
            s.throw_exception(exc);
        }
        Err(napi_pending_exception)
    })
}
#[no_mangle]
pub unsafe extern "C" fn napi_new_instance(
    env: napi_env,
    _constructor: napi_value,
    _argc: usize,
    _argv: *const napi_value,
    _result: *mut napi_value,
) -> napi_status {
    throw_unsupported(env, "napi_new_instance")
}

// ---- errors / exceptions --------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn napi_throw(env: napi_env, error: napi_value) -> napi_status {
    js_call(env, |s| {
        s.throw_exception(to_local(error));
        Ok(())
    })
}
#[no_mangle]
pub unsafe extern "C" fn napi_throw_error(
    env: napi_env,
    code: *const c_char,
    msg: *const c_char,
) -> napi_status {
    js_call(env, |s| {
        let m = std::ffi::CStr::from_ptr(msg).to_string_lossy();
        let ms = v8::String::new(s, &m).ok_or(napi_generic_failure)?;
        let exc = v8::Exception::error(s, ms);
        if !code.is_null() {
            let code = std::ffi::CStr::from_ptr(code).to_string_lossy();
            let code = v8::String::new(s, &code).ok_or(napi_generic_failure)?;
            set_error_code(s, exc, code)?;
        }
        s.throw_exception(exc);
        Ok(())
    })
}
#[no_mangle]
pub unsafe extern "C" fn napi_create_error(
    env: napi_env,
    code: napi_value,
    msg: napi_value,
    out: *mut napi_value,
) -> napi_status {
    // No NAPI_PREAMBLE in Node, but setting `code` can run JS (a setter): park what it throws.
    catching(env, |s| {
        let ms: v8::Local<v8::String> =
            to_local(msg).try_into().map_err(|_| napi_string_expected)?;
        let exc = v8::Exception::error(s, ms);
        if !code.is_null() {
            let code: v8::Local<v8::String> = to_local(code)
                .try_into()
                .map_err(|_| napi_string_expected)?;
            set_error_code(s, exc, code)?;
        }
        put(out, exc);
        Ok(())
    })
}

/// `err.code = code`, as Node's set_error_code for napi_throw_error / napi_create_error: a
/// failed (throwing) Set is napi_generic_failure.
unsafe fn set_error_code(
    s: &mut v8::PinScope<'static, 'static>,
    err: v8::Local<v8::Value>,
    code: v8::Local<v8::String>,
) -> NapiResult {
    let err: v8::Local<v8::Object> = err.try_into().map_err(|_| napi_generic_failure)?;
    let key = v8::String::new(s, "code").ok_or(napi_generic_failure)?;
    err.set(s, key.into(), code.into())
        .ok_or(napi_generic_failure)?;
    Ok(())
}
#[no_mangle]
pub unsafe extern "C" fn napi_is_error(
    _env: napi_env,
    v: napi_value,
    out: *mut bool,
) -> napi_status {
    if !out.is_null() {
        *out = to_local(v).is_native_error();
    }
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_is_exception_pending(
    env: napi_env,
    out: *mut bool,
) -> napi_status {
    let e = &mut *env;
    if !out.is_null() {
        *out = e.last_exception.is_some();
    }
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_get_and_clear_last_exception(
    env: napi_env,
    out: *mut napi_value,
) -> napi_status {
    let s = scope(env);
    let e = &mut *env;
    match e.last_exception.take() {
        Some(g) => {
            let l = v8::Local::new(s, &g);
            put(out, l);
        }
        None => put(out, v8::undefined(s).into()),
    }
    napi_ok
}

// ---- references ------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn napi_create_reference(
    env: napi_env,
    v: napi_value,
    initial_refcount: u32,
    out: *mut napi_ref,
) -> napi_status {
    let s = scope(env);
    // as Node for an addon without a declared module API version (8): only objects, functions
    // and symbols can be referenced
    let value = to_local(v);
    if !(value.is_object() || value.is_symbol()) {
        return napi_invalid_arg;
    }
    let r = new_ref(&mut *env, Ref::new(s, value, initial_refcount));
    if !out.is_null() {
        *out = r;
    }
    napi_ok
}

/// Store `r` on the env: its napi_ref is its 1-based id.
fn new_ref(e: &mut Env, r: Ref) -> napi_ref {
    e.refs.push(Some(r));
    e.refs.len() as napi_ref
}
#[no_mangle]
pub unsafe extern "C" fn napi_get_reference_value(
    env: napi_env,
    r: napi_ref,
    out: *mut napi_value,
) -> napi_status {
    let s = scope(env);
    let e = &mut *env;
    let id = r as usize;
    if id == 0 || id > e.refs.len() {
        return napi_invalid_arg;
    }
    let value = match e.refs[id - 1].as_ref() {
        // as Node: a weak reference whose value was collected (or finalized) yields NULL
        Some(r) => r.get(s),
        None => Some(v8::undefined(s).into()),
    };
    if !out.is_null() {
        *out = value.map_or(std::ptr::null_mut(), |v| to_napi(v));
    }
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_delete_reference(env: napi_env, r: napi_ref) -> napi_status {
    // as Node (CHECK_ARG(env, ref)): a NULL reference is napi_invalid_arg
    if r.is_null() {
        return napi_invalid_arg;
    }
    let e = &mut *env;
    let id = r as usize;
    if id <= e.refs.len() {
        e.refs[id - 1] = None;
    }
    napi_ok
}
/// The live reference `r` of the env, as napi_reference_ref / napi_reference_unref take it: a
/// NULL, unknown or deleted reference is napi_invalid_arg (Node: CHECK_ARG, then undefined
/// behaviour).
unsafe fn live_ref<'a>(env: napi_env, r: napi_ref) -> Result<&'a mut Ref, napi_status> {
    let id = r as usize;
    let refs = &mut (*env).refs;
    refs.get_mut(id.wrapping_sub(1))
        .and_then(|slot| slot.as_mut())
        .ok_or(napi_invalid_arg)
}

#[no_mangle]
pub unsafe extern "C" fn napi_reference_ref(
    env: napi_env,
    r: napi_ref,
    result: *mut u32,
) -> napi_status {
    let s = scope(env);
    match live_ref(env, r) {
        Ok(r) => {
            let count = r.inc(s);
            if !result.is_null() {
                *result = count;
            }
            napi_ok
        }
        Err(status) => status,
    }
}

#[no_mangle]
pub unsafe extern "C" fn napi_reference_unref(
    env: napi_env,
    r: napi_ref,
    result: *mut u32,
) -> napi_status {
    let s = scope(env);
    match live_ref(env, r) {
        // as Node: unref of a count of 0 is napi_generic_failure
        Ok(r) if r.count == 0 => napi_generic_failure,
        Ok(r) => {
            let count = r.dec(s);
            if !result.is_null() {
                *result = count;
            }
            napi_ok
        }
        Err(status) => status,
    }
}

// ---- stubs (threadsafe fns / cleanup hooks — not exercised by sync parse) ---

#[no_mangle]
pub unsafe extern "C" fn napi_add_env_cleanup_hook(
    _env: napi_env,
    _fun: *mut c_void,
    _arg: *mut c_void,
) -> napi_status {
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_stub() -> napi_status {
    napi_generic_failure
}

// ---- classes and property descriptors -----------------------------------------

/// The key a descriptor names, as Node's V8NameFromPropertyDescriptor: `utf8name`, else the
/// `name` value, which must be a string or a symbol (napi_name_expected).
unsafe fn property_name(
    s: &mut v8::PinScope<'static, 'static>,
    p: &napi_property_descriptor,
) -> Result<v8::Local<'static, v8::Name>, napi_status> {
    if !p.utf8name.is_null() {
        let bytes = std::ffi::CStr::from_ptr(p.utf8name).to_bytes();
        let name = v8::String::new_from_utf8(s, bytes, v8::NewStringType::Internalized)
            .ok_or(napi_generic_failure)?;
        return Ok(name.into());
    }
    if p.name.is_null() {
        return Err(napi_name_expected);
    }
    to_local(p.name).try_into().map_err(|_| napi_name_expected)
}

/// The template attributes of a prototype member, as Node's V8PropertyAttributesFromDescriptor:
/// a data member without napi_writable is read-only; no napi_enumerable is DontEnum, no
/// napi_configurable DontDelete.
fn template_attributes(p: &napi_property_descriptor) -> v8::PropertyAttribute {
    let mut attr = v8::PropertyAttribute::NONE;
    if p.getter.is_none() && p.setter.is_none() && p.attributes & napi_writable == 0 {
        attr = attr | v8::PropertyAttribute::READ_ONLY;
    }
    if p.attributes & napi_enumerable == 0 {
        attr = attr | v8::PropertyAttribute::DONT_ENUM;
    }
    if p.attributes & napi_configurable == 0 {
        attr = attr | v8::PropertyAttribute::DONT_DELETE;
    }
    attr
}

/// The descriptors behind a `(count, properties)` argument pair: properties are required for a
/// count above 0 (napi_invalid_arg).
unsafe fn descriptors<'a>(
    count: usize,
    properties: *const napi_property_descriptor,
) -> Result<&'a [napi_property_descriptor], napi_status> {
    if count == 0 {
        return Ok(&[]);
    }
    if properties.is_null() {
        return Err(napi_invalid_arg);
    }
    Ok(std::slice::from_raw_parts(properties, count))
}

/// napi_define_class, as Node: a constructor function named `name` whose prototype carries the
/// instance members (methods with the class as their receiver signature, accessors, values) of
/// the descriptors; the napi_static ones are then defined on the constructor like
/// napi_define_properties (the class is handed back even when one of those fails).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn napi_define_class(
    env: napi_env,
    name: *const c_char,
    len: usize,
    ctor: Option<napi_callback>,
    data: *mut c_void,
    property_count: usize,
    properties: *const napi_property_descriptor,
    out: *mut napi_value,
) -> napi_status {
    js_call(env, |s| {
        if out.is_null() {
            return Err(napi_invalid_arg);
        }
        let ctor = ctor.ok_or(napi_invalid_arg)?;
        let props = descriptors(property_count, properties)?;
        // as Node's CHECK_NEW_FROM_UTF8_LEN: the class name is required
        if name.is_null() {
            return Err(napi_invalid_arg);
        }
        let tpl = callback_template(s, env, ctor, data, None);
        let class_name =
            v8::String::new_from_utf8(s, utf8_arg(name, len), v8::NewStringType::Internalized)
                .ok_or(napi_generic_failure)?;
        tpl.set_class_name(class_name);
        let proto = tpl.prototype_template(s);
        for p in props.iter().filter(|p| p.attributes & napi_static == 0) {
            let key = property_name(s, p)?;
            let attr = template_attributes(p);
            if p.getter.is_some() || p.setter.is_some() {
                let getter = p
                    .getter
                    .map(|cb| callback_template(s, env, cb, p.data, None));
                let setter = p
                    .setter
                    .map(|cb| callback_template(s, env, cb, p.data, None));
                proto.set_accessor_property(key, getter, setter, attr);
            } else if let Some(method) = p.method {
                let sig = v8::Signature::new(s, tpl);
                let t = callback_template(s, env, method, p.data, Some(sig));
                proto.set_with_attr(key, t.into(), attr);
            } else {
                // A template holds only primitives: V8 aborts the process on an object (Node
                // crashes there); refuse it instead.
                if p.value.is_null() || to_local(p.value).is_object() {
                    return Err(napi_invalid_arg);
                }
                proto.set_with_attr(key, to_local(p.value).into(), attr);
            }
        }
        let class = tpl.get_function(s).ok_or(napi_generic_failure)?;
        put(out, class.into());
        let statics: Vec<napi_property_descriptor> = props
            .iter()
            .filter(|p| p.attributes & napi_static != 0)
            .copied()
            .collect();
        define_properties(s, env, class.into(), &statics)
    })
}

#[no_mangle]
pub unsafe extern "C" fn napi_define_properties(
    env: napi_env,
    object: napi_value,
    property_count: usize,
    properties: *const napi_property_descriptor,
) -> napi_status {
    js_call(env, |s| {
        let props = descriptors(property_count, properties)?;
        if object.is_null() {
            return Err(napi_invalid_arg);
        }
        let obj = to_object(s, object)?;
        define_properties(s, env, obj, props)
    })
}

unsafe extern "C" {
    /// The binding behind rusty_v8's `PropertyDescriptor::new_from_get_set` (V8's
    /// `PropertyDescriptor(get, set)`), which takes either half as an empty (NULL) handle; the
    /// Rust wrapper only accepts two values.
    fn v8__PropertyDescriptor__CONSTRUCT__Get_Set(
        this: *mut v8::PropertyDescriptor,
        get: *const v8::Value,
        set: *const v8::Value,
    );
}

/// An accessor descriptor with the halves given: a `None` half is absent from the descriptor
/// (V8's empty handle), so DefineProperty leaves that half of an existing accessor alone.
fn accessor_descriptor(
    get: Option<v8::Local<v8::Value>>,
    set: Option<v8::Local<v8::Value>>,
) -> v8::PropertyDescriptor {
    let half = |h: Option<v8::Local<v8::Value>>| h.map_or(std::ptr::null(), |h| &*h as *const _);
    let mut desc = std::mem::MaybeUninit::<v8::PropertyDescriptor>::uninit();
    unsafe {
        v8__PropertyDescriptor__CONSTRUCT__Get_Set(desc.as_mut_ptr(), half(get), half(set));
        desc.assume_init()
    }
}

/// Define `props` on `obj`, as Node's napi_define_properties: an accessor pair or a method
/// becomes a new function per property (no signature, no name); a data property with all three
/// attributes takes the CreateDataProperty fast path. A definition V8 refuses is
/// napi_invalid_arg (napi_generic_failure for a method).
unsafe fn define_properties(
    s: &mut v8::PinScope<'static, 'static>,
    env: napi_env,
    obj: v8::Local<'static, v8::Object>,
    props: &[napi_property_descriptor],
) -> NapiResult {
    for p in props {
        let key = property_name(s, p)?;
        let enumerable = p.attributes & napi_enumerable != 0;
        let configurable = p.attributes & napi_configurable != 0;
        let writable = p.attributes & napi_writable != 0;
        let (mut desc, refused) = if p.getter.is_some() || p.setter.is_some() {
            // as Node: a missing half is an empty handle, left out of the definition (an existing
            // accessor keeps that half), not `undefined`
            let mut accessor = |cb: Option<napi_callback>| match cb {
                Some(cb) => new_function(s, env, cb, p.data).map(|f| Some(f.into())),
                None => Ok(None),
            };
            let (get, set) = (accessor(p.getter)?, accessor(p.setter)?);
            (accessor_descriptor(get, set), napi_invalid_arg)
        } else if let Some(method) = p.method {
            let f = new_function(s, env, method, p.data)?;
            let desc = v8::PropertyDescriptor::new_from_value_writable(f.into(), writable);
            (desc, napi_generic_failure)
        } else {
            if p.value.is_null() {
                return Err(napi_invalid_arg);
            }
            let value = to_local(p.value);
            if enumerable && writable && configurable {
                if obj.create_data_property(s, key, value) != Some(true) {
                    return Err(napi_invalid_arg);
                }
                continue;
            }
            let desc = v8::PropertyDescriptor::new_from_value_writable(value, writable);
            (desc, napi_invalid_arg)
        };
        desc.set_enumerable(enumerable);
        desc.set_configurable(configurable);
        if obj.define_property(s, key, &desc) != Some(true) {
            return Err(refused);
        }
    }
    Ok(())
}

// threadsafe-function family: napi-rs creates a GC-finalizer tsfn at module init, so
// create MUST succeed (hand back a dummy handle). The tsfn never actually fires for our
// synchronous parse use, so call/unref are no-ops.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn napi_create_threadsafe_function(
    _env: napi_env,
    _func: napi_value,
    _async_resource: napi_value,
    _async_resource_name: napi_value,
    _max_queue_size: usize,
    _initial_thread_count: usize,
    _thread_finalize_data: *mut c_void,
    _thread_finalize_cb: *mut c_void,
    _context: *mut c_void,
    _call_js_cb: *mut c_void,
    result: *mut *mut c_void,
) -> napi_status {
    if !result.is_null() {
        *result = 1usize as *mut c_void; // non-null dummy handle
    }
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_call_threadsafe_function(
    _func: *mut c_void,
    _data: *mut c_void,
    _mode: i32,
) -> napi_status {
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_unref_threadsafe_function(
    _env: napi_env,
    _func: *mut c_void,
) -> napi_status {
    napi_ok
}

/// The module init symbol exported by the addon.
type RegisterFn = unsafe extern "C" fn(napi_env, napi_value) -> napi_value;

/// Load a `.node` addon: dlopen, set up the env, call `napi_register_module_v1` with a
/// fresh exports object, and return that object. Must be called inside a live scope.
pub fn load_addon(
    scope_ref: &mut v8::PinScope,
    path: &Path,
) -> Result<v8::Global<v8::Value>, String> {
    unsafe {
        let lib = libloading::Library::new(path).map_err(|e| format!("dlopen {e}"))?;
        let register: libloading::Symbol<RegisterFn> = lib
            .get(b"napi_register_module_v1")
            .map_err(|e| format!("no napi_register_module_v1: {e}"))?;

        let isolate = scope_ref.as_raw_isolate_ptr();
        let context = v8::Global::new(scope_ref, scope_ref.get_current_context());
        let env_ptr: napi_env = Box::into_raw(Box::new(Env {
            isolate,
            scope: scope_ref as *mut v8::PinScope as *mut c_void,
            context,
            last_exception: None,
            fns: Vec::new(),
            refs: Vec::new(),
            finalizers: Default::default(),
            next_finalizer: 0,
            tearing_down: false,
        }));
        // The isolate owns the env from here on (freed with it, like the functions using it).
        match scope_ref.get_slot_mut::<AddonEnvs>() {
            Some(envs) => envs.0.push(env_ptr),
            None => {
                scope_ref.set_slot(AddonEnvs(vec![env_ptr]));
            }
        }

        let exports = v8::Object::new(scope_ref);
        // The addon's module init runs arbitrary native code. Guard it two ways:
        //  - A JS exception it leaves pending (napi_throw_*, an unsupported N-API call we route
        //    through throw_unsupported, a throwing JS call) surfaces as a clean require() failure.
        //  - call_addon turns a Rust panic crossing the `extern "C"` FFI boundary (which is UB
        //    and can abort/segfault, taking the whole run down with no diagnostic) into a
        //    recoverable Err -> a thrown JS error in the loader. The rest of the run survives.
        // call_addon also holds the addon lock, as for every other entry into addon code: two
        // workers loading the same library must not run its init at once (a require() from
        // inside a native callback or another addon's init is a same-thread re-entry).
        let exports_napi = to_napi(exports.into());
        let result = call_addon(|| register(env_ptr, exports_napi));
        let result = init_outcome(result, path)?;
        if let Some(exc) = (*env_ptr).last_exception.take() {
            let msg = v8::Local::new(scope_ref, &exc).to_rust_string_lossy(scope_ref);
            return Err(format!("native addon threw during module init: {msg}"));
        }
        let ret_local = if result.is_null() {
            exports.into()
        } else {
            to_local(result)
        };
        // keep the lib loaded for the process lifetime
        std::mem::forget(lib);
        Ok(v8::Global::new(scope_ref, ret_local))
    }
}

#[cfg(all(test, unix))]
#[path = "napi_host_tests.rs"]
mod tests;
