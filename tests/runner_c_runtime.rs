//! Runtime semantics that follow Node/vitest: the `URL` polyfill resolves relative references
//! like Node's WHATWG `URL`; an entry file's hoisted mock factory runs once.
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

/// The URL cases: expected values captured from Node 26's `URL`.
const URL_TEST: &str = r##"// expected hrefs: Node 26 `new URL(input, base)` (TypeError = throws)
const CASES = [
  ["./x.json", "file:///d/a.test.ts", "file:///d/x.json"],
  ["../y/z.js", "file:///d/e/a.ts", "file:///d/y/z.js"],
  ["x", "file:///d/", "file:///d/x"],
  ["../../../../up", "file:///a/b/c", "file:///up"],
  ["/abs", "https://h.example/p/q?x=1", "https://h.example/abs"],
  ["?k=v", "https://h.example/p/q", "https://h.example/p/q?k=v"],
  ["#frag", "https://h.example/p/q?x=1#old", "https://h.example/p/q?x=1#frag"],
  ["", "https://h.example/p/q?x=1#old", "https://h.example/p/q?x=1"],
  ["//other.example/r", "https://h.example/p", "https://other.example/r"],
  ["https://abs.example/a/./b/../c", undefined, "https://abs.example/a/c"],
  ["a/./b/../c/", "http://h/base/", "http://h/base/a/c/"],
  ["..", "http://h/a/b/c", "http://h/a/"],
  [".", "http://h/a/b/c", "http://h/a/b/"],
  ["sub/file.txt?q=1#h", "http://h:8080/dir/index.html", "http://h:8080/dir/sub/file.txt?q=1#h"],
  ["#x", "mailto:me@example.com", "mailto:me@example.com#x"],
  ["y", "mailto:me@example.com", "TypeError"],
  ["./data/./x.json", "file:///proj/src/a.test.ts", "file:///proj/src/data/x.json"],
  ["http://h", undefined, "http://h/"],
  ["not a url", undefined, "TypeError"],
];
test('URL resolves like Node', () => {
  for (const [u, b, want] of CASES) {
    let got;
    try { got = (b === undefined ? new URL(u) : new URL(u, b)).href; } catch (e) { got = e instanceof TypeError ? 'TypeError' : String(e); }
    expect(`${u} @ ${b} -> ${got}`).toBe(`${u} @ ${b} -> ${want}`);
  }
  expect(new URL('./x.json', 'file:///d/a.test.ts').pathname).toBe('/d/x.json');
  // the import.meta.url pattern
  expect(new URL('./data.json', import.meta.url).href).toBe(import.meta.url.replace(/[^/]*$/, 'data.json'));
});
"##;

#[test]
fn url_resolves_relative_references_like_node() {
    // turbo-test#38 item 1: a relative reference was appended to the whole base
    // (`new URL('./x.json', 'file:///d/a.test.ts').pathname` == "/d/a.test.ts/./x.json").
    for esbuild in [false, true] {
        let p = Project::new("rc-url");
        if esbuild {
            p.with_esbuild();
            p.file("package.json", r#"{"name":"r38"}"#);
        } else {
            p.file("package.json", r#"{"name":"r38","type":"module"}"#);
        }
        p.file("a.test.js", URL_TEST);
        assert_passes(&turbo_test(&p.dir, &["a.test.js"], &[]), 1);
    }
}

#[test]
fn an_entry_mock_factory_runs_once() {
    // turbo-test#38 item 2: the entry's hoisted factories ran in the mock prepass AND again in
    // the module body; vitest calls a factory once. Sync, async and jest.mock factories, on the
    // module-runner, legacy-bundle (TURBO_NO_MR) and ESM-graph (no esbuild) paths.
    for esbuild in [true, false] {
        let p = Project::new("rc-factory-once");
        if esbuild {
            p.with_esbuild();
            p.file("package.json", r#"{"name":"r38b"}"#);
            p.file(
                "node_modules/react/package.json",
                r#"{"name":"react","main":"index.js"}"#,
            );
            p.file(
                "node_modules/react/index.js",
                "exports.createElement = () => null;\n",
            );
        } else {
            p.file("package.json", r#"{"name":"r38b","type":"module"}"#);
        }
        p.file("m.js", "export const v = 'real';\n");
        p.file("n.js", "export const w = 'real-n';\n");
        p.file("j.js", "export const j = 'real-j';\n");
        // async factories without esbuild are turbo-test#38 item 5
        let n_factory = if esbuild { "async () =>" } else { "() =>" };
        p.file(
            "a.test.js",
            &format!(
                r#"import {{ v }} from './m.js';
import {{ w }} from './n.js';
import {{ j }} from './j.js';
vi.mock('./m.js', () => {{ globalThis.M = (globalThis.M || 0) + 1; return {{ v: 'mock' }}; }});
vi.mock('./n.js', {n_factory} {{ globalThis.N = (globalThis.N || 0) + 1; return {{ w: 'mock-n' }}; }});
jest.mock('./j.js', () => {{ globalThis.J = (globalThis.J || 0) + 1; return {{ j: 'mock-j' }}; }});
test('each factory ran once', () => {{
  expect([v, w, j]).toEqual(['mock', 'mock-n', 'mock-j']);
  expect([globalThis.M, globalThis.N, globalThis.J]).toEqual([1, 1, 1]);
}});
"#
            ),
        );
        let envs: &[&[(&str, &str)]] = if esbuild {
            &[&[], &[("TURBO_NO_MR", "1")]]
        } else {
            &[&[]]
        };
        for env in envs {
            let r = turbo_test(&p.dir, &["a.test.js"], env);
            assert_passes(&r, 1);
        }
    }
}
