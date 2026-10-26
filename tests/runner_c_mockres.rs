//! Mock resolution that follows vitest: a `vi.doMock` applies to a later `import()` on every
//! loader path, and a mock whose specifier has a `?query` is a module id of its own.
#![cfg(unix)]

mod common;
use common::{turbo_test, Project};

/// Assert a clean pass with exactly `n` passing tests.
fn assert_passes(r: &common::Run, n: u32) {
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert!(
        r.stdout.contains(&format!("Tests  {n} passed ({n})")),
        "{}",
        r.all()
    );
}

/// A project without esbuild (the ESM-graph path).
fn esm_graph_project(name: &str) -> Project {
    let p = Project::new(name);
    p.file(
        "package.json",
        &format!(r#"{{"name":"{name}","type":"module"}}"#),
    );
    p
}

#[test]
fn do_mock_then_dynamic_import_sees_the_mock_on_every_path() {
    // turbo-test#39 item 1: on the ESM graph (and the TURBO_NO_MR bundle path) vi.doMock had no
    // effect on a later import() — also when the real module was imported before.
    for esbuild in [false, true] {
        let p = if esbuild {
            let p = Project::new("rc-domock-import");
            p.with_esbuild();
            p.file("package.json", r#"{"name":"r39"}"#);
            p
        } else {
            esm_graph_project("rc-domock-import-esm")
        };
        p.file("m.js", "export const v = 'real';\n");
        p.file("r.js", "export const x = 'real-x';\n");
        p.file(
            "a.test.js",
            r#"import { x } from './r.js';
test('doMock then import()', async () => {
  vi.doMock('./m.js', () => ({ v: 'mocked' }));
  const m = await import('./m.js');
  expect(m.v).toBe('mocked');
});
test('doMock of an already-imported module, then import()', async () => {
  expect(x).toBe('real-x');
  vi.doMock('./r.js', () => ({ x: 'mocked-x' }));
  const r = await import('./r.js');
  expect(r.x).toBe('mocked-x');
});
"#,
        );
        let envs: &[&[(&str, &str)]] = if esbuild {
            &[&[], &[("TURBO_NO_MR", "1")]]
        } else {
            &[&[]]
        };
        for env in envs {
            assert_passes(&turbo_test(&p.dir, &["a.test.js"], env), 2);
        }
    }
}

#[test]
fn a_query_suffixed_mock_is_a_module_of_its_own() {
    // turbo-test#39 item 2: resolve_spec strips `?query`, so `vi.mock('./rel.js?raw', ...)`
    // replaced the mock of `./rel.js`. vitest keys mocks by module id, query included.
    for esbuild in [true, false] {
        let p = if esbuild {
            let p = Project::new("rc-query-mock");
            p.with_esbuild();
            p.file("package.json", r#"{"name":"r39b"}"#);
            p
        } else {
            esm_graph_project("rc-query-mock-esm")
        };
        p.file("rel.js", "export const r = () => 'real';\n");
        p.file("other.js", "export const o = 'real-o';\n");
        p.file(
            "a.test.js",
            r#"import { r } from './rel.js';
import { o } from './other.js';
vi.mock('./rel.js', () => ({ r: () => 'mock-rel' }));
vi.mock('./rel.js?raw', () => ({ default: 'raw' }));
vi.mock('./other.js?inline', () => ({ default: 'inline' }));
test('query ids are separate modules', async () => {
  expect(r()).toBe('mock-rel');
  // only the query id is mocked: the plain module stays real
  expect(o).toBe('real-o');
  // the query id itself gets its own mock
  expect((await import('./rel.js?raw')).default).toBe('raw');
  expect((await import('./other.js?inline')).default).toBe('inline');
});
"#,
        );
        let envs: &[&[(&str, &str)]] = if esbuild {
            &[&[], &[("TURBO_NO_MR", "1")]]
        } else {
            &[&[]]
        };
        for env in envs {
            assert_passes(&turbo_test(&p.dir, &["a.test.js"], env), 1);
        }
    }
}
