//! Unit tests for the napi host paths an addon cannot drive through `require()`: the panic
//! guards (a C callback cannot unwind into Rust), stored pending exceptions, same-thread
//! re-entry of the addon lock, and the trampoline's missing-env / missing-slot fallbacks.
//! The end-to-end surface is covered by tests/napi_host.rs.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// Run `f` inside a fresh isolate + context.
fn with_scope(f: impl FnOnce(&mut v8::PinScope)) {
    crate::runner::init_v8();
    let isolate = &mut v8::Isolate::new(Default::default());
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    f(scope);
}

/// An env whose napi calls run in `scope`.
fn env_for(scope: &mut v8::PinScope) -> Box<Env> {
    Box::new(Env {
        isolate: std::ptr::null_mut(),
        scope: scope as *mut v8::PinScope as *mut c_void,
        context: None,
        last_exception: None,
        fns: Vec::new(),
        refs: Vec::new(),
    })
}

#[test]
fn init_outcome_maps_a_panic_to_a_load_error() {
    let path = Path::new("/addons/demo.node");
    let value = 0x40usize as napi_value;
    assert_eq!(init_outcome(Ok(value), path), Ok(value));
    assert_eq!(
        init_outcome(Err(Box::new("boom")), path),
        Err("native addon panicked during module init: /addons/demo.node".to_string())
    );
}

#[test]
fn callback_return_value_null_and_panic() {
    with_scope(|s| {
        let tc = std::pin::pin!(v8::TryCatch::new(s));
        let tc = &mut tc.init();
        let five = unsafe { to_napi(v8::Integer::new(tc, 5).into()) };
        assert_eq!(callback_return(tc, Ok(five)), Some(five));
        assert_eq!(callback_return(tc, Ok(std::ptr::null_mut())), None);
        assert!(!tc.has_caught());
        assert_eq!(callback_return(tc, Err(Box::new("boom"))), None);
        assert!(tc.has_caught());
        let msg = tc.exception().unwrap().to_rust_string_lossy(tc);
        assert_eq!(msg, "Error: turbo-test: native addon callback panicked");
    });
}

#[test]
fn stored_pending_exception_is_reported_then_cleared() {
    with_scope(|s| {
        let mut env = env_for(s);
        let stored: v8::Local<v8::Value> = v8::String::new(s, "stored").unwrap().into();
        env.last_exception = Some(v8::Global::new(s, stored));
        let e: napi_env = &mut *env;
        unsafe {
            let mut pending = false;
            assert_eq!(napi_is_exception_pending(e, &mut pending), napi_ok);
            assert!(pending);
            let mut out: napi_value = std::ptr::null_mut();
            assert_eq!(napi_get_and_clear_last_exception(e, &mut out), napi_ok);
            assert_eq!(to_local(out).to_rust_string_lossy(s), "stored");
            assert_eq!(napi_is_exception_pending(e, &mut pending), napi_ok);
            assert!(!pending);
            assert_eq!(napi_get_and_clear_last_exception(e, &mut out), napi_ok);
            assert!(to_local(out).is_undefined());
        }
    });
}

#[test]
fn addon_lock_is_reentrant_per_thread_and_exclusive_across_threads() {
    std::thread::spawn(|| {
        let (outer_reentrant, outer) = enter_addon();
        assert!(!outer_reentrant);
        assert!(outer.is_some());
        assert!(IN_ADDON.with(|f| f.get()));
        // another thread cannot enter while this one is inside the addon
        let blocked = std::thread::spawn(|| ADDON_LOCK.try_lock().is_err())
            .join()
            .unwrap();
        assert!(blocked);
        // same-thread re-entry takes no lock (no self-deadlock)
        let (inner_reentrant, inner) = enter_addon();
        assert!(inner_reentrant);
        assert!(inner.is_none());
        IN_ADDON.with(|f| f.set(false));
        drop(outer);
        let free = std::thread::spawn(|| ADDON_LOCK.try_lock().is_ok())
            .join()
            .unwrap();
        assert!(free);
    })
    .join()
    .unwrap();
}

static CALLED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn record_call(_env: napi_env, _info: napi_callback_info) -> napi_value {
    CALLED.store(true, Ordering::SeqCst);
    std::ptr::null_mut()
}

#[test]
fn trampoline_without_env_or_slot_returns_undefined() {
    with_scope(|s| {
        ENV.with(|e| *e.borrow_mut() = None);
        let mut env = env_for(s);
        let mut f: napi_value = std::ptr::null_mut();
        let status = unsafe {
            napi_create_function(
                &mut *env,
                std::ptr::null(),
                0,
                record_call,
                std::ptr::null_mut(),
                &mut f,
            )
        };
        assert_eq!(status, napi_ok);
        let func: v8::Local<v8::Function> = unsafe { to_local(f) }.try_into().unwrap();
        let recv: v8::Local<v8::Value> = v8::undefined(s).into();
        // no env installed on this thread
        let r = func.call(s, recv, &[]).unwrap();
        assert!(r.is_undefined());
        // an env without this function's slot
        ENV.with(|e| *e.borrow_mut() = Some(env_for(s)));
        let r = func.call(s, recv, &[]).unwrap();
        assert!(r.is_undefined());
        assert!(!CALLED.load(Ordering::SeqCst));
        ENV.with(|e| *e.borrow_mut() = None);
        // control: the sentinel does record a real invocation
        let ret = unsafe { record_call(&mut *env, std::ptr::null_mut()) };
        assert!(ret.is_null());
        assert!(CALLED.load(Ordering::SeqCst));
    });
}
