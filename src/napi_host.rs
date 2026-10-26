//! Node-API (napi) host — load native `.node` addons in turbo-test's V8 embedding.
//!
//! Implements the subset of the napi C ABI that real addons call, backed by rusty_v8.
//! This is what Node/Deno/Bun each provide; here it is scoped to the surface turbo-dom's
//! parser addon needs (enumerated from `nm -u` on the .node):
//!   value/object/string/number, arrays, functions + cb_info, arraybuffer/typedarray,
//!   errors/exceptions, references, threadsafe-fn (stubbed).
//!
//! Bridge: `napi_value` IS a `v8::Local<Value>` (Local is `NonNull<T>` — pointer-sized),
//! so they transmute. Handles are created in the scope stored on the env for the current
//! native call (set by the function trampoline / loader before calling into the addon).

#![allow(non_camel_case_types, non_upper_case_globals, dead_code, clippy::missing_safety_doc)]

use std::ffi::c_void;
use std::os::raw::c_char;
use std::path::Path;
use std::ptr::NonNull;

pub type napi_status = i32;
pub const napi_ok: napi_status = 0;
pub const napi_invalid_arg: napi_status = 1;
pub const napi_string_expected: napi_status = 3;
pub const napi_number_expected: napi_status = 6;
pub const napi_boolean_expected: napi_status = 7;
pub const napi_generic_failure: napi_status = 9;
pub const napi_pending_exception: napi_status = 10;

pub type napi_value = *mut c_void;
pub type napi_env = *mut Env;
pub type napi_ref = *mut c_void;
pub type napi_callback_info = *mut CbInfo;
pub type napi_callback = unsafe extern "C" fn(napi_env, napi_callback_info) -> napi_value;
pub type napi_finalize = unsafe extern "C" fn(napi_env, *mut c_void, *mut c_void);

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
    pub isolate: *mut v8::Isolate,
    pub scope: *mut c_void, // *mut v8::PinScope<'static,'static>, valid only during a call
    pub context: Option<v8::Global<v8::Context>>,
    pub last_exception: Option<v8::Global<v8::Value>>,
    /// One record per napi_create_function; each function's data is an External pointing at
    /// its record, so a function always dispatches into the env (addon) that created it.
    pub fns: Vec<*mut Callback>,
    pub refs: Vec<Option<v8::Global<v8::Value>>>,
}

impl Drop for Env {
    fn drop(&mut self) {
        for &f in &self.fns {
            drop(unsafe { Box::from_raw(f) });
        }
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
/// `v8impl::TryCatch`): while an exception is pending it returns `napi_pending_exception`
/// without running `f`; a JS exception thrown inside `f` is caught and parked on the env as
/// its pending exception. `napi_is_exception_pending` / `napi_get_and_clear_last_exception`
/// see it, and the trampoline (or the module loader) throws it to JS when the addon returns
/// to its caller. A termination is not caught: it keeps unwinding.
unsafe fn js_call(
    env: napi_env,
    f: impl FnOnce(&mut v8::PinScope<'static, 'static>) -> NapiResult,
) -> napi_status {
    if (*env).last_exception.is_some() {
        return napi_pending_exception;
    }
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

/// Callback info passed to a napi_callback during a function call.
pub struct CbInfo {
    pub args: Vec<napi_value>,
    pub this: napi_value,
    pub data: *mut c_void,
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
    let bytes = if len == usize::MAX {
        std::ffi::CStr::from_ptr(str_).to_bytes()
    } else {
        std::slice::from_raw_parts(str_ as *const u8, len)
    };
    // V8's String::NewFromUtf8 (what Node uses) replaces invalid sequences with U+FFFD.
    match v8::String::new_from_utf8(s, bytes, v8::NewStringType::Normal) {
        Some(v) => {
            put(out, v.into());
            napi_ok
        }
        None => napi_generic_failure,
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
    if buf.is_null() {
        if !result.is_null() {
            *result = str_val.utf8_length(s);
        }
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
        let str_ = to_local(v).to_string(s).ok_or(napi_generic_failure)?;
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
        let o: v8::Local<v8::Object> = to_local(obj).try_into().map_err(|_| napi_invalid_arg)?;
        o.set(s, k.into(), to_local(value));
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
        let o: v8::Local<v8::Object> = to_local(obj).try_into().map_err(|_| napi_invalid_arg)?;
        put(out, o.get(s, k.into()).ok_or(napi_generic_failure)?);
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
        let o: v8::Local<v8::Object> = to_local(obj).try_into().map_err(|_| napi_invalid_arg)?;
        o.set_index(s, index, to_local(value));
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
    let mut info = CbInfo {
        args: argv,
        this: unsafe { to_napi(args.this().into()) },
        data,
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
    let (reentrant, _guard) = enter_addon();
    // Calling the addon's native callback crosses the FFI boundary. A panic there is UB and
    // can crash the process; catch_unwind converts it into a thrown JS error instead.
    let ret = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        cb(env_ptr, &mut info as *mut CbInfo)
    }));
    if !reentrant {
        IN_ADDON.with(|f| f.set(false));
    }
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
    _name: *const c_char,
    _len: usize,
    cb: napi_callback,
    data: *mut c_void,
    out: *mut napi_value,
) -> napi_status {
    js_call(env, |s| {
        let record = Box::into_raw(Box::new(Callback { env, cb, data }));
        (*env).fns.push(record);
        let data_val = v8::External::new(s, record as *mut c_void);
        let tmpl = v8::FunctionTemplate::builder(napi_fn_trampoline)
            .data(data_val.into())
            .build(s);
        let f = tmpl.get_function(s).unwrap();
        put(out, f.into());
        Ok(())
    })
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
        let f: v8::Local<v8::Function> = to_local(func).try_into().map_err(|_| napi_invalid_arg)?;
        let args: Vec<v8::Local<v8::Value>> = (0..argc).map(|i| to_local(*argv.add(i))).collect();
        let recv_local = if recv.is_null() {
            v8::undefined(s).into()
        } else {
            to_local(recv)
        };
        put(
            out,
            f.call(s, recv_local, &args).ok_or(napi_pending_exception)?,
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
        // napi typedarray type codes -> matching v8 typed array. `length` is in ELEMENTS.
        let ta: v8::Local<v8::Value> = match type_ {
            0 => v8::Int8Array::new(s, ab, byte_offset, length)
                .unwrap()
                .into(),
            1 => v8::Uint8Array::new(s, ab, byte_offset, length)
                .unwrap()
                .into(),
            2 => v8::Uint8ClampedArray::new(s, ab, byte_offset, length)
                .unwrap()
                .into(),
            3 => v8::Int16Array::new(s, ab, byte_offset, length)
                .unwrap()
                .into(),
            4 => v8::Uint16Array::new(s, ab, byte_offset, length)
                .unwrap()
                .into(),
            5 => v8::Int32Array::new(s, ab, byte_offset, length)
                .unwrap()
                .into(),
            6 => v8::Uint32Array::new(s, ab, byte_offset, length)
                .unwrap()
                .into(),
            7 => v8::Float32Array::new(s, ab, byte_offset, length)
                .unwrap()
                .into(),
            8 => v8::Float64Array::new(s, ab, byte_offset, length)
                .unwrap()
                .into(),
            9 => v8::BigInt64Array::new(s, ab, byte_offset, length)
                .unwrap()
                .into(),
            10 => v8::BigUint64Array::new(s, ab, byte_offset, length)
                .unwrap()
                .into(),
            _ => v8::Uint8Array::new(s, ab, byte_offset, length)
                .unwrap()
                .into(),
        };
        put(out, ta);
        Ok(())
    })
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
    // Accept any ArrayBufferView (Uint8Array Buffer) or a raw ArrayBuffer.
    if let Ok(view) = TryInto::<v8::Local<v8::ArrayBufferView>>::try_into(val) {
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
        return napi_ok;
    }
    if let Ok(ab) = TryInto::<v8::Local<v8::ArrayBuffer>>::try_into(val) {
        if !length.is_null() {
            *length = ab.byte_length();
        }
        if !data.is_null() {
            *data = ab
                .get_backing_store()
                .data()
                .map(|p| p.as_ptr())
                .unwrap_or(std::ptr::null_mut());
        }
        return napi_ok;
    }
    napi_invalid_arg
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
        let arr: v8::Local<v8::Array> = to_local(v).try_into().map_err(|_| napi_invalid_arg)?;
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
        let o: v8::Local<v8::Object> = to_local(obj).try_into().map_err(|_| napi_invalid_arg)?;
        put(out, o.get_index(s, index).ok_or(napi_generic_failure)?);
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
        let o: v8::Local<v8::Object> = to_local(obj).try_into().map_err(|_| napi_invalid_arg)?;
        let names = o
            .get_own_property_names(s, v8::GetPropertyNamesArgs::default())
            .ok_or(napi_generic_failure)?;
        put(out, names.into());
        Ok(())
    })
}

// ---- wrap / new_instance: not supported; surface as a CATCHABLE JS throw ----
// Wrapping a native pointer on a JS object (napi_wrap/unwrap) and constructing a
// native class instance (napi_new_instance) need lifecycle/finalizer machinery we
// don't model. Rather than leave these UNEXPORTED (null pointer -> SIGSEGV when the
// addon calls them), export them and throw a clear, catchable JS error so the
// require() fails as a normal load-error and the rest of the run survives.
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
pub unsafe extern "C" fn napi_wrap(
    env: napi_env,
    _js_object: napi_value,
    _native_object: *mut c_void,
    _finalize_cb: *mut c_void,
    _finalize_hint: *mut c_void,
    _result: *mut napi_ref,
) -> napi_status {
    throw_unsupported(env, "napi_wrap")
}
#[no_mangle]
pub unsafe extern "C" fn napi_unwrap(
    env: napi_env,
    _js_object: napi_value,
    _result: *mut *mut c_void,
) -> napi_status {
    throw_unsupported(env, "napi_unwrap")
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
    _code: *const c_char,
    msg: *const c_char,
) -> napi_status {
    js_call(env, |s| {
        let m = std::ffi::CStr::from_ptr(msg).to_string_lossy();
        let ms = v8::String::new(s, &m).unwrap();
        let exc = v8::Exception::error(s, ms);
        s.throw_exception(exc);
        Ok(())
    })
}
#[no_mangle]
pub unsafe extern "C" fn napi_create_error(
    env: napi_env,
    _code: napi_value,
    msg: napi_value,
    out: *mut napi_value,
) -> napi_status {
    let s = scope(env);
    let ms: v8::Local<v8::String> = match to_local(msg).try_into() {
        Ok(m) => m,
        Err(_) => return napi_invalid_arg,
    };
    let exc = v8::Exception::error(s, ms);
    put(out, exc);
    napi_ok
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

// ---- references (minimal: hold a Global) ----------------------------------

#[no_mangle]
pub unsafe extern "C" fn napi_create_reference(
    env: napi_env,
    v: napi_value,
    _initial_refcount: u32,
    out: *mut napi_ref,
) -> napi_status {
    let s = scope(env);
    let g = v8::Global::new(s, to_local(v));
    let e = &mut *env;
    e.refs.push(Some(g));
    let id = e.refs.len();
    if !out.is_null() {
        *out = id as napi_ref;
    }
    napi_ok
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
    match e.refs[id - 1].as_ref() {
        Some(g) => put(out, v8::Local::new(s, g)),
        None => put(out, v8::undefined(s).into()),
    }
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_delete_reference(env: napi_env, r: napi_ref) -> napi_status {
    let e = &mut *env;
    let id = r as usize;
    if id >= 1 && id <= e.refs.len() {
        e.refs[id - 1] = None;
    }
    napi_ok
}
#[no_mangle]
pub unsafe extern "C" fn napi_reference_unref(
    _env: napi_env,
    _r: napi_ref,
    _result: *mut u32,
) -> napi_status {
    napi_ok
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

/// define_class → treat the constructor as a plain function (prototype methods skipped;
/// sufficient for addons whose exercised exports are functions, e.g. the parser).
#[no_mangle]
pub unsafe extern "C" fn napi_define_class(
    env: napi_env,
    name: *const c_char,
    len: usize,
    ctor: napi_callback,
    data: *mut c_void,
    _property_count: usize,
    _properties: *const c_void,
    out: *mut napi_value,
) -> napi_status {
    napi_create_function(env, name, len, ctor, data, out)
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

        let isolate = std::ptr::null_mut();
        let context = v8::Global::new(scope_ref, scope_ref.get_current_context());
        let env_ptr: napi_env = Box::into_raw(Box::new(Env {
            isolate,
            scope: scope_ref as *mut v8::PinScope as *mut c_void,
            context: Some(context),
            last_exception: None,
            fns: Vec::new(),
            refs: Vec::new(),
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
        //  - catch_unwind turns a Rust panic crossing the `extern "C"` FFI boundary (which is UB
        //    and can abort/segfault, taking the whole run down with no diagnostic) into a
        //    recoverable Err -> a thrown JS error in the loader. The rest of the run survives.
        let exports_napi = to_napi(exports.into());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            register(env_ptr, exports_napi)
        }));
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
