//! The Node-API host (src/napi_host.rs) end to end: C addons compiled at test time are
//! `require()`d from specs run by the real CLI, and the specs assert what every implemented
//! `napi_*` entrypoint produced (values, statuses, thrown errors, pending exceptions), plus the
//! module-init paths (exports vs returned value, throwing init, unsupported entrypoint, missing
//! init symbol, a file that fails to dlopen, a nested require() during init, init serialized
//! across workers), re-entrant callbacks, several addons loaded side by side, and napi_wrap with
//! its finalizers (on collection and at env teardown). The expected
//! values are Node-API's: the same specs pass against Node 26 with the same fixtures, except for
//! host-only behaviour (unsupported napi_new_instance etc., reference ids, cases where Node aborts or
//! deadlocks).
#![cfg(unix)]

mod common;
mod napi_support;

use common::{json_report, turbo_test, Project};
use napi_support::{compile_addon, spec};

/// A throwaway project with esbuild linked in (the module runner's CJS path is what gives specs
/// a `require()`, as in a real vitest project).
fn project(name: &str) -> Project {
    let p = Project::new(name);
    p.with_esbuild();
    p
}

/// Run the project's specs and assert every test passed (`expected` of them).
fn assert_all_pass(p: &Project, expected: u64) {
    let r = turbo_test(&p.dir, &["--reporter", "json"], &[]);
    let j = json_report(&r);
    assert_eq!(j["numFailedTests"], 0, "{}", r.all());
    assert_eq!(j["numPassedTests"], expected, "{}", r.all());
    assert_eq!(r.code, Some(0), "{}", r.all());
}

#[test]
fn every_implemented_entrypoint_behaves() {
    let p = project("napi-full");
    compile_addon(&p, "full");
    p.file("full.test.ts", spec("full.test.ts"));
    assert_all_pass(&p, 41);
}

#[test]
fn strings_over_v8_max_length_fail_cleanly() {
    let p = project("napi-huge");
    compile_addon(&p, "full");
    p.file("huge.test.ts", spec("huge.test.ts"));
    assert_all_pass(&p, 1);
}

#[test]
fn module_init_outcomes() {
    let p = project("napi-init");
    for stem in [
        "init_custom",
        "init_throw",
        "init_unsupported",
        "noreg",
        "init_nested",
        "second",
    ] {
        compile_addon(&p, stem);
    }
    p.file("garbage.node", "this is not a shared library\n");
    p.file("init.test.ts", spec("init.test.ts"));
    assert_all_pass(&p, 6);
}

/// Two workers require() the same addon at about the same time: its module init runs under the
/// addon lock like every other entry into addon code, one thread at a time.
#[test]
fn module_init_is_serialized_across_workers() {
    let p = project("napi-init-lock");
    compile_addon(&p, "init_lock");
    p.file("a.test.ts", spec("init_lock.test.ts"));
    p.file("b.test.ts", spec("init_lock.test.ts"));
    let r = turbo_test(&p.dir, &["--reporter", "json"], &[("TURBO_JOBS", "2")]);
    let j = json_report(&r);
    assert_eq!(j["numFailedTests"], 0, "{}", r.all());
    assert_eq!(j["numPassedTests"], 2, "{}", r.all());
}

#[test]
fn reentrant_callbacks_work() {
    let p = project("napi-reentry");
    compile_addon(&p, "full");
    compile_addon(&p, "second");
    p.file("reentry.test.ts", spec("reentry.test.ts"));
    assert_all_pass(&p, 3);
}

#[test]
fn each_addon_keeps_its_own_env() {
    let p = project("napi-addons");
    compile_addon(&p, "full");
    compile_addon(&p, "second");
    p.file("addons.test.ts", spec("addons.test.ts"));
    assert_all_pass(&p, 2);
}

#[test]
fn objects_wrap_native_pointers() {
    let p = project("napi-wrap");
    compile_addon(&p, "wrap");
    p.file("wrap.test.ts", spec("wrap.test.ts"));
    // --expose-gc: the spec collects wrapped objects to run their finalizers
    let r = turbo_test(
        &p.dir,
        &["--reporter", "json"],
        &[("TURBO_V8_FLAGS", "--expose-gc")],
    );
    let j = json_report(&r);
    assert_eq!(j["numFailedTests"], 0, "{}", r.all());
    assert_eq!(j["numPassedTests"], 5, "{}", r.all());
}

/// Objects still wrapped when the addon's env is torn down (the file's isolate is disposed) have
/// their finalizers run then, newest first, as Node does at exit: references can still be
/// deleted, JS cannot run (napi_pending_exception).
#[test]
fn wrap_finalizers_run_at_env_teardown() {
    let p = project("napi-wrap-teardown");
    compile_addon(&p, "wrap");
    p.file("wrap_teardown.test.ts", spec("wrap_teardown.test.ts"));
    let r = turbo_test(&p.dir, &["--reporter", "json"], &[]);
    let j = json_report(&r);
    assert_eq!(j["numPassedTests"], 1, "{}", r.all());
    let lines: Vec<&str> = r
        .stderr
        .lines()
        .filter(|l| l.starts_with("wrap teardown finalizer:"))
        .collect();
    assert_eq!(
        lines,
        [
            "wrap teardown finalizer: id=42 hint=142 delete_reference=0 call_function=10",
            "wrap teardown finalizer: id=41 hint=141 delete_reference=0 call_function=10",
        ],
        "{}",
        r.all()
    );
}
