//! The Node-API host (src/napi_host.rs) end to end: C addons compiled at test time are
//! `require()`d from specs run by the real CLI, and the specs assert what every implemented
//! `napi_*` entrypoint produced (values, statuses, thrown errors), plus the module-init paths
//! (exports vs returned value, throwing init, unsupported entrypoint, missing init symbol,
//! a file that fails to dlopen).
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
    assert_all_pass(&p, 31);
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
    for stem in ["init_custom", "init_throw", "init_wrap", "noreg"] {
        compile_addon(&p, stem);
    }
    p.file("garbage.node", "this is not a shared library\n");
    p.file("init.test.ts", spec("init.test.ts"));
    assert_all_pass(&p, 5);
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
