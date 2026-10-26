//! Module-runner (esbuild project) semantics that follow vitest/Node: a module's own load error
//! is the reported cause (entry import, `require()`, dynamic `import()`); `import.meta`
//! (url/filename/dirname) and CSS-module class-name proxies.
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

#[test]
fn module_runner_import_meta_is_the_files_own() {
    let p = esbuild_project("rc-import-meta");
    // the same source in two directories: each module sees its own location
    let meta = "export const url = import.meta.url;\nexport const filename = import.meta.filename;\n\
                export const dirname = import.meta.dirname;\nexport const kind = typeof import.meta;\n";
    p.file("src/meta.ts", meta);
    p.file("src/sub/meta.ts", meta);
    let src = p.path("src/meta.ts");
    let sub = p.path("src/sub/meta.ts");
    let entry = p.path("a.test.ts");
    p.file(
        "a.test.ts",
        &format!(
            r#"import * as m from './src/meta';
import * as s from './src/sub/meta';
test('import.meta of an app module', () => {{
  expect(m.url).toBe('file://{src}');
  expect(m.filename).toBe('{src}');
  expect(m.dirname).toBe('{src_dir}');
  expect(m.kind).toBe('object');
  expect(s.url).toBe('file://{sub}');
  expect(s.dirname).toBe('{sub_dir}');
}});
test('import.meta of the test file', () => {{
  expect(import.meta.url).toBe('file://{entry}');
  expect(import.meta.dirname).toBe('{dir}');
}});
"#,
            src = src.display(),
            src_dir = src.parent().unwrap().display(),
            sub = sub.display(),
            sub_dir = sub.parent().unwrap().display(),
            entry = entry.display(),
            dir = p.dir.display(),
        ),
    );
    for env in [
        &[][..],
        &[("TURBO_NATIVE_CJS", "0")][..],
        &[("TURBO_REUSE_ISOLATE", "1")][..],
    ] {
        assert_passes(&turbo_test(&p.dir, &["a.test.ts"], env), 2);
    }
}

#[test]
fn esm_graph_import_meta_has_filename_and_dirname() {
    // No esbuild: the entry is a native ES module and import.meta comes from the host callback.
    let p = Project::new("rc-import-meta-esm");
    p.file("package.json", r#"{"name":"rc-import-meta-esm"}"#);
    let entry = p.path("a.test.js");
    p.file(
        "a.test.js",
        &format!(
            "test('import.meta', () => {{\n  expect(import.meta.url).toBe('file://{e}');\n  \
             expect(import.meta.filename).toBe('{e}');\n  expect(import.meta.dirname).toBe('{d}');\n}});\n",
            e = entry.display(),
            d = p.dir.display(),
        ),
    );
    assert_passes(&turbo_test(&p.dir, &[], &[]), 1);
}

#[test]
fn import_meta_keeps_coverage_line_mapping() {
    let p = esbuild_project("rc-import-meta-cov");
    p.file(
        "src/meta.ts",
        "export const url = import.meta.url;\nexport function hit() {\n  return 'hit';\n}\n\
         export function miss() {\n  return 'miss';\n}\n",
    );
    p.file(
        "a.test.ts",
        "import { url, hit } from './src/meta';\n\
         test('t', () => { expect(url.startsWith('file://')).toBe(true); expect(hit()).toBe('hit'); });\n",
    );
    let r = turbo_test(
        &p.dir,
        &[
            "--coverage",
            "--coverage-reporter",
            "lcov",
            "--coverage-dir",
            "cov",
        ],
        &[],
    );
    assert_eq!(r.code, Some(0), "{}", r.all());
    let lcov = std::fs::read_to_string(p.path("cov/lcov.info")).unwrap();
    for da in ["DA:1,1", "DA:3,1", "DA:6,0"] {
        assert!(lcov.lines().any(|l| l == da), "{da}\n{lcov}");
    }
}

#[test]
fn module_runner_css_modules_are_class_name_proxies() {
    let p = esbuild_project("rc-css-modules");
    p.file("s.module.css", ".foo { color: red }\n");
    p.file("plain.css", "body { margin: 0 }\n");
    p.file(
        "a.test.ts",
        r#"import s from './s.module.css';
import './plain.css';
test('css module default import maps class names to themselves', () => {
  expect(s.foo).toBe('foo');
  expect(s['btn-primary']).toBe('btn-primary');
  expect(`${s.a} ${s.b}`).toBe('a b');
});
test('plain css is a side-effect import', () => {
  expect(require('./plain.css')).toBeDefined();
});
"#,
    );
    for env in [
        &[][..],
        &[("TURBO_NATIVE_CJS", "0")][..],
        &[("TURBO_REUSE_ISOLATE", "1")][..],
    ] {
        assert_passes(&turbo_test(&p.dir, &["a.test.ts"], env), 2);
    }
}
