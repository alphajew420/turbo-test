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

/// A node-addon-api-style ObjectWrap class: napi_define_class builds the prototype (instance
/// methods with the class as their signature, accessors, values) and the static members from its
/// property descriptors, with Node's attributes; napi_get_new_target tells `new` from a call;
/// napi_define_properties defines the same descriptors on any object (an accessor missing one
/// half leaves that half out of the definition, turbo-test#43 item 4).
#[test]
fn define_class_builds_an_object_wrap_class() {
    let p = project("napi-objectwrap");
    compile_addon(&p, "objectwrap");
    p.file("objectwrap.test.ts", spec("objectwrap.test.ts"));
    assert_all_pass(&p, 10);
}

/// Type tags (napi_type_tag_object / napi_check_object_type_tag: one tag per object, compared
/// as two 64-bit words) and napi_add_finalizer (any number per object, run on collection, an
/// optional weak result reference), as Node; an addon importing them no longer crashes the run.
#[test]
fn objects_carry_type_tags_and_finalizers() {
    let p = project("napi-typetag");
    compile_addon(&p, "typetag");
    p.file("typetag.test.ts", spec("typetag.test.ts"));
    // --expose-gc: the spec collects objects to run their finalizers
    let r = turbo_test(
        &p.dir,
        &["--reporter", "json"],
        &[("TURBO_V8_FLAGS", "--expose-gc")],
    );
    let j = json_report(&r);
    assert_eq!(j["numFailedTests"], 0, "{}", r.all());
    assert_eq!(j["numPassedTests"], 7, "{}", r.all());
}

/// Objects still wrapped (or with a napi_add_finalizer finalizer) when the addon's env is torn
/// down (the file's isolate is disposed) have their finalizers run then, newest first and
/// interleaved, as Node does at exit: in a handle scope, so value calls work (a strong reference
/// still has its value, the record's own reference has none, values can be created), while JS
/// cannot run (napi_pending_exception). The expected lines are Node 26's.
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
    let line = |id: u32| {
        format!(
            "wrap teardown finalizer: id={id} hint={} own_ref=0:-1 strong_ref=0:6 \
             create_object=0:6 get_named_property=10 delete_reference=0 call_function=10",
            id + 100
        )
    };
    assert_eq!(lines, [line(42), line(43), line(41)], "{}", r.all());
}

/// Reference counts, as Node (turbo-test#43 item 3): napi_create_reference honours its initial
/// count, napi_reference_ref / napi_reference_unref move it up and down, a reference at 0 is
/// weak (its value goes when the object is collected; only objects, functions and symbols can be
/// referenced) and above 0 strong, including a wrap's own reference.
#[test]
fn references_count_up_and_down() {
    let p = project("napi-refs");
    compile_addon(&p, "refs");
    p.file("refs.test.ts", spec("refs.test.ts"));
    // --expose-gc: the spec collects referenced objects
    let r = turbo_test(
        &p.dir,
        &["--reporter", "json"],
        &[("TURBO_V8_FLAGS", "--expose-gc")],
    );
    let j = json_report(&r);
    assert_eq!(j["numFailedTests"], 0, "{}", r.all());
    assert_eq!(j["numPassedTests"], 4, "{}", r.all());
}

/// The Node-API functions beyond full.c (turbo-test#44), one section per family; the same spec
/// passes against Node 26: errors (napi_throw_* / napi_create_* of each kind,
/// napi_get_last_error_info, napi_fatal_exception), numbers, bigints, dates and a NULL env.
#[test]
fn every_node_api_family_behaves() {
    let p = project("napi-api");
    compile_addon(&p, "api");
    p.file("api.test.ts", spec("api.test.ts"));
    let r = turbo_test(&p.dir, &["--reporter", "json"], &[]);
    let j = json_report(&r);
    assert_eq!(j["numFailedTests"], 0, "{}", r.all());
    assert_eq!(j["numPassedTests"], 13, "{}", r.all());
    // napi_fatal_exception reported the error as uncaught (what a throwing timer callback gets)
    assert!(
        r.stderr.contains("uncaught reported as uncaught"),
        "{}",
        r.all()
    );
}

/// napi_fatal_error prints `FATAL ERROR: <location> <message>` and aborts the process, as
/// Node does.
#[test]
fn fatal_error_aborts_the_process() {
    let p = project("napi-fatal");
    compile_addon(&p, "fatal");
    p.file(
        "fatal.test.ts",
        "import { test } from 'vitest';\ntest('dies', () => { require('./fatal.node').die(); });\n",
    );
    let r = turbo_test(&p.dir, &[], &[]);
    assert_eq!(r.code, None, "killed by SIGABRT\n{}", r.all());
    assert!(
        r.stderr
            .contains("FATAL ERROR: tt_fatal_location the message\n"),
        "{}",
        r.all()
    );
}
