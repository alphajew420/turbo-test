//! Module-runner (esbuild project) semantics that follow vitest/Node: a module's own load error
//! is the reported cause (entry import, `require()`, dynamic `import()`).
#![cfg(unix)]

mod common;
use common::{turbo_test, Project};

fn esbuild_project(name: &str) -> Project {
    let p = Project::new(name);
    p.with_esbuild();
    p.file("package.json", &format!(r#"{{"name":"{name}"}}"#));
    p
}

/// Assert a clean pass with exactly `n` passing tests.
fn assert_passes(r: &common::Run, n: u32) {
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert!(
        r.stdout.contains(&format!("Tests  {n} passed ({n})")),
        "{}",
        r.all()
    );
}

/// The `ERROR <file>  (<msg>)` load-error line for `file`.
fn load_error_line<'a>(r: &'a common::Run, file: &str) -> &'a str {
    let suffix = format!("{file}  (");
    r.stdout
        .lines()
        .find(|l| l.starts_with("ERROR") && l.contains(&suffix))
        .unwrap_or_else(|| panic!("no load error for {file}\n{}", r.all()))
}

const FRESH_AND_REUSE: [&[(&str, &str)]; 2] = [&[], &[("TURBO_REUSE_ISOLATE", "1")]];

#[test]
fn module_runner_load_errors_keep_their_cause() {
    let p = esbuild_project("rc-load-cause");
    p.file("dep.js", "throw new Error('boom from dep');\n");
    p.file("a.test.js", "import './dep.js';\ntest('x', () => {});\n");
    p.file(
        "e.mjs",
        "export const a = 1;\nthrow new Error('esm body throws');\n",
    );
    p.file(
        "b.test.js",
        r#"test('require of a throwing ES module throws its error', () => {
  expect(() => require('./e.mjs')).toThrow('esm body throws');
});
"#,
    );
    for env in FRESH_AND_REUSE {
        let r = turbo_test(&p.dir, &["a.test.js"], env);
        assert_eq!(r.code, Some(1), "{env:?}\n{}", r.all());
        assert!(
            load_error_line(&r, "a.test.js").ends_with("(Error: boom from dep)"),
            "{env:?}\n{}",
            r.all()
        );
        assert_passes(&turbo_test(&p.dir, &["b.test.js"], env), 1);
    }
}

#[test]
fn esm_graph_dynamic_import_rejects_with_the_module_error() {
    // No esbuild: `import()` reaches the host callback, which loads the CJS module natively.
    let p = Project::new("rc-dyn-cause");
    p.file("package.json", r#"{"name":"rc-dyn-cause"}"#);
    p.file("throws.cjs", "throw new Error('cjs body boom');\n");
    p.file(
        "a.test.js",
        "test('rejects with the body error', async () => {\n  \
         await expect(import('./throws.cjs')).rejects.toThrow('cjs body boom');\n});\n",
    );
    assert_passes(&turbo_test(&p.dir, &[], &[]), 1);
}
