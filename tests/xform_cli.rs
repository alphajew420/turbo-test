//! The native ESM→CJS emitter (app files) and package bundler (node_modules) driven end-to-end
//! through the CLI's module runner, including the `TURBO_NATIVE_CJS` / `TURBO_NATIVE_DEPS`
//! opt-outs and `TURBO_NATIVE_CJS_STRICT` (no esbuild fallback).
#![cfg(unix)]

mod common;
use common::{json_report, turbo_test, Project, Run};

/// `file name → status` from a `--reporter json` run.
fn statuses(r: &Run) -> Vec<(String, String)> {
    let j = json_report(r);
    let mut v: Vec<(String, String)> = j["testResults"]
        .as_array()
        .unwrap_or_else(|| panic!("no testResults:\n{}", r.all()))
        .iter()
        .map(|t| {
            let name = t["name"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_string();
            (name, t["status"].as_str().unwrap().to_string())
        })
        .collect();
    v.sort();
    v
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

/// An esbuild project (so the module runner loads tests as CJS) with a stub `react/jsx-runtime`.
fn mr_project(name: &str) -> Project {
    let p = Project::new(name);
    p.with_esbuild();
    p.file(
        "node_modules/react/package.json",
        "{\"name\":\"react\",\"main\":\"index.js\"}",
    );
    p.file("node_modules/react/index.js", "module.exports = {};\n");
    p.file(
        "node_modules/react/jsx-runtime.js",
        "exports.jsx = (t) => t; exports.jsxs = exports.jsx; exports.Fragment = 'F';\n",
    );
    p
}

#[test]
fn native_emit_keeps_bindings_live_and_bundles_cyclic_deps() {
    let p = mr_project("xform-native");
    p.file("src/m.js", "export function f() { return 1; }\n");
    p.file(
        "src/user.ts",
        "import { f } from './m.js';\nexport const useIt = (): number => f();\n",
    );
    p.file(
        "node_modules/dep/package.json",
        "{\"name\":\"dep\",\"main\":\"index.js\"}",
    );
    p.file(
        "node_modules/dep/index.js",
        "import { b } from './b.js';\nexport const a = () => 'A' + b();\nexport * from './c.js';\n",
    );
    p.file(
        "node_modules/dep/b.js",
        "import { a } from './index.js';\nexport const b = () => 'B';\nexport const viaA = () => a();\n",
    );
    p.file("node_modules/dep/c.js", "export const c = 'C';\n");
    p.file(
        "native.test.ts",
        "import * as m from './src/m.js';\nimport { useIt } from './src/user';\n\
         import { a, c } from 'dep';\nimport { viaA } from 'dep/b.js';\n\
         test('live binding', () => { vi.spyOn(m, 'f').mockReturnValue(42); expect(useIt()).toBe(42); });\n\
         test('dep bundle', () => { expect(a()).toBe('AB'); expect(c).toBe('C'); expect(viaA()).toBe('AB'); });\n",
    );
    // native (default) and the esbuild baseline must agree
    for env in [
        &[][..],
        &[("TURBO_NATIVE_CJS", "0"), ("TURBO_NATIVE_DEPS", "0")][..],
    ] {
        let r = turbo_test(&p.dir, &["--reporter", "json"], env);
        assert_eq!(r.code, Some(0), "{env:?}: {}", r.all());
        let j = json_report(&r);
        assert_eq!(j["numPassedTests"], 2, "{env:?}: {}", r.all());
    }
}

/// App file + dep package the native path can't lower (JSX namespace names) but esbuild can.
fn namespace_jsx_project() -> Project {
    let p = mr_project("xform-strict");
    p.file("src/ns.jsx", "export const el = <a:b />;\n");
    p.file(
        "node_modules/nsdep/package.json",
        "{\"name\":\"nsdep\",\"main\":\"index.jsx\"}",
    );
    p.file(
        "node_modules/nsdep/index.jsx",
        "export const el = <c:d />;\n",
    );
    p.file(
        "app.test.js",
        "import { el } from './src/ns.jsx';\ntest('app', () => { expect(el).toBe('a:b'); });\n",
    );
    p.file(
        "dep.test.js",
        "import { el } from 'nsdep';\ntest('dep', () => { expect(el).toBe('c:d'); });\n",
    );
    p
}

#[test]
fn native_failures_fall_back_to_esbuild_unless_strict() {
    let p = namespace_jsx_project();
    let app_err = format!(
        "cjs read/transform failed: {}",
        p.path("src/ns.jsx").display()
    );
    let dep_err = format!(
        "cjs read/transform failed: {}",
        p.path("node_modules/nsdep/index.jsx").display()
    );

    // default: native fails, esbuild fallback succeeds — silently
    let r = turbo_test(&p.dir, &["--reporter", "json"], &[]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert_eq!(
        statuses(&r),
        vec![pair("app.test.js", "passed"), pair("dep.test.js", "passed")]
    );
    assert!(
        !r.stderr.contains("cjs read/transform failed"),
        "{}",
        r.all()
    );

    // strict: both native failures surface as load errors
    let r = turbo_test(
        &p.dir,
        &["--reporter", "json"],
        &[("TURBO_NATIVE_CJS_STRICT", "1")],
    );
    assert_eq!(r.code, Some(1), "{}", r.all());
    assert_eq!(
        statuses(&r),
        vec![pair("app.test.js", "error"), pair("dep.test.js", "error")]
    );
    assert!(r.stderr.contains(&app_err), "{}", r.all());
    assert!(r.stderr.contains(&dep_err), "{}", r.all());

    // an empty STRICT value is off
    let r = turbo_test(
        &p.dir,
        &["--reporter", "json"],
        &[("TURBO_NATIVE_CJS_STRICT", "")],
    );
    assert_eq!(r.code, Some(0), "{}", r.all());

    // strict + app emitter opted out (0 or empty): app goes straight to esbuild, dep still strict
    for off in ["0", ""] {
        let r = turbo_test(
            &p.dir,
            &["--reporter", "json"],
            &[("TURBO_NATIVE_CJS_STRICT", "1"), ("TURBO_NATIVE_CJS", off)],
        );
        assert_eq!(
            statuses(&r),
            vec![pair("app.test.js", "passed"), pair("dep.test.js", "error")],
            "{off:?}"
        );
        assert!(!r.stderr.contains(&app_err), "{}", r.all());
        assert!(r.stderr.contains(&dep_err), "{}", r.all());
    }

    // strict + dep bundler opted out: dep goes straight to esbuild, app still strict
    let r = turbo_test(
        &p.dir,
        &["--reporter", "json"],
        &[("TURBO_NATIVE_CJS_STRICT", "1"), ("TURBO_NATIVE_DEPS", "0")],
    );
    assert_eq!(
        statuses(&r),
        vec![pair("app.test.js", "error"), pair("dep.test.js", "passed")]
    );
    assert!(r.stderr.contains(&app_err), "{}", r.all());
    assert!(!r.stderr.contains(&dep_err), "{}", r.all());
}
