//! Fixture addons for tests/napi_host.rs: C sources compiled with the system `cc` at test time
//! into a throwaway project, leaving every `napi_*` import undefined so it resolves against the
//! turbo-test binary's exported Node-API host (src/napi_host.rs) at dlopen time — exactly like
//! a prebuilt napi-rs `.node`.

use crate::common::Project;
use std::path::PathBuf;
use std::process::Command;

const NAPI_H: &str = include_str!("napi.h");

/// The C source of fixture `stem` (tests/napi_support/<stem>.c).
pub fn c_source(stem: &str) -> &'static str {
    match stem {
        "full" => include_str!("full.c"),
        "init_custom" => include_str!("init_custom.c"),
        "init_lock" => include_str!("init_lock.c"),
        "init_nested" => include_str!("init_nested.c"),
        "init_throw" => include_str!("init_throw.c"),
        "init_unsupported" => include_str!("init_unsupported.c"),
        "noreg" => include_str!("noreg.c"),
        "objectwrap" => include_str!("objectwrap.c"),
        "second" => include_str!("second.c"),
        "typetag" => include_str!("typetag.c"),
        "wrap" => include_str!("wrap.c"),
        _ => panic!("unknown napi fixture {stem}"),
    }
}

/// The spec file `name` (tests/napi_support/<name>).
pub fn spec(name: &str) -> &'static str {
    match name {
        "full.test.ts" => include_str!("full.test.ts"),
        "huge.test.ts" => include_str!("huge.test.ts"),
        "init.test.ts" => include_str!("init.test.ts"),
        "init_lock.test.ts" => include_str!("init_lock.test.ts"),
        "reentry.test.ts" => include_str!("reentry.test.ts"),
        "addons.test.ts" => include_str!("addons.test.ts"),
        "objectwrap.test.ts" => include_str!("objectwrap.test.ts"),
        "typetag.test.ts" => include_str!("typetag.test.ts"),
        "wrap.test.ts" => include_str!("wrap.test.ts"),
        "wrap_teardown.test.ts" => include_str!("wrap_teardown.test.ts"),
        _ => panic!("unknown napi spec {name}"),
    }
}

/// Compile fixture `stem` into `<project>/<stem>.node`. Fails loudly when `cc` is missing:
/// these tests must not silently skip on a runner without a C compiler.
pub fn compile_addon(p: &Project, stem: &str) -> PathBuf {
    p.file("csrc/napi.h", NAPI_H);
    let src = p.file(&format!("csrc/{stem}.c"), c_source(stem));
    let out = p.path(&format!("{stem}.node"));
    let mut cmd = Command::new("cc");
    cmd.arg("-shared");
    if cfg!(target_os = "macos") {
        // leave the napi_* imports to be bound against the host executable at load time
        cmd.args(["-undefined", "dynamic_lookup"]);
    } else {
        cmd.arg("-fPIC");
    }
    cmd.arg("-Wall").arg("-o").arg(&out).arg(&src);
    let o = cmd.output().unwrap_or_else(|e| {
        panic!("the napi host tests need a C compiler: could not run `cc` ({e})")
    });
    assert!(
        o.status.success(),
        "cc failed to build {stem}.node:\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    out
}
