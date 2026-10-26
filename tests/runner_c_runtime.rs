//! Runtime semantics that follow Node/vitest: the `URL` polyfill resolves relative references
//! like Node's WHATWG `URL`; an entry file's hoisted mock factory runs once; without esbuild the
//! entry prepass replays `vi.hoisted` values and awaits async factories; a setup file that throws
//! fails every test file using it; a passing matcher never formats its operands.
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
fn esm_graph_entry_mock_factories_see_vi_hoisted_values() {
    // turbo-test#38 item 4: without esbuild, the entry's factories were compiled on their own,
    // without the file's vi.hoisted declarations ("calls is not defined").
    let p = esm_graph_project("rc-esm-hoisted");
    p.file(
        "m.js",
        "export const v = 'real';\nexport const u = 'real-u';\n",
    );
    p.file(
        "a.test.js",
        r#"import { v, u } from './m.js';
const calls = vi.hoisted(() => ({ m: 0 }));
const { label } = vi.hoisted(() => ({ label: 'hoisted' }));
vi.mock('./m.js', () => { calls.m++; return { v: label, u: 'mock-u' }; });
test('factory closes over vi.hoisted values', () => {
  expect(v).toBe('hoisted');
  expect(u).toBe('mock-u');
  expect(calls.m).toBe(1);
});
"#,
    );
    assert_passes(&turbo_test(&p.dir, &["a.test.js"], &[]), 1);
}

#[test]
fn esm_graph_async_entry_mock_factories_are_awaited() {
    // turbo-test#38 item 5: without esbuild, an async factory's Promise was registered as the
    // module ("does not provide an export named 'w'").
    let p = esm_graph_project("rc-esm-async");
    p.file(
        "n.js",
        "export const w = 'real';\nexport default 'real-default';\n",
    );
    p.file(
        "o.js",
        "export const k = 'real-k';\nexport const keep = 'kept';\n",
    );
    p.file(
        "a.test.js",
        r#"import d, { w } from './n.js';
import { k, keep } from './o.js';
vi.mock('./n.js', async () => ({ w: 'mock', default: 'mock-default' }));
vi.mock('./o.js', async (importOriginal) => ({ ...(await importOriginal()), k: 'mock-k' }));
test('async factories are awaited', () => {
  expect([w, d]).toEqual(['mock', 'mock-default']);
  expect([k, keep]).toEqual(['mock-k', 'kept']);
});
"#,
    );
    assert_passes(&turbo_test(&p.dir, &["a.test.js"], &[]), 1);
}

#[test]
fn esm_graph_throwing_vi_hoisted_fails_the_file_and_mocks_still_register() {
    // turbo-test#38 item 4 edge: a vi.hoisted that throws fails the file with its error
    // (vitest); the prepass registers the file's mocks without the hoisted values meanwhile.
    let p = esm_graph_project("rc-esm-hoisted-throw");
    p.file("m.js", "export const v = 'real';\n");
    p.file(
        "a.test.js",
        r#"import { v } from './m.js';
const h = vi.hoisted(() => { throw new Error('hoisted boom'); });
vi.mock('./m.js', () => ({ v: 'mock' }));
test('never runs', () => { expect(v).toBe('mock'); });
"#,
    );
    let r = turbo_test(&p.dir, &["a.test.js"], &[]);
    assert_eq!(r.code, Some(1), "{}", r.all());
    assert!(
        r.stdout
            .lines()
            .any(|l| l.starts_with("ERROR") && l.contains("hoisted boom")),
        "{}",
        r.all()
    );
}

#[test]
fn a_throwing_setup_file_fails_every_file_that_uses_it() {
    // turbo-test#38 item 3: a setup file that throws was ignored and the run passed. vitest fails
    // each test file with the setup file's error. Module-runner (also with isolate reuse, where
    // setup runs once per worker), TURBO_NO_MR (bundle path) and no esbuild.
    for esbuild in [true, false] {
        let p = if esbuild {
            let p = Project::new("rc-setup-throws");
            p.with_esbuild();
            p.file("package.json", r#"{"name":"r38c"}"#);
            p
        } else {
            esm_graph_project("rc-setup-throws-esm")
        };
        p.file(
            "vitest.config.js",
            "export default { test: { setupFiles: ['./setup.js'] } };\n",
        );
        p.file("setup.js", "throw new Error('setup boom');\n");
        p.file("a.test.js", "test('a', () => {});\n");
        p.file("b.test.js", "test('b', () => {});\n");
        let envs: &[&[(&str, &str)]] = if esbuild {
            &[
                &[],
                &[("TURBO_NO_MR", "1")],
                &[("TURBO_REUSE_ISOLATE", "1"), ("TURBO_JOBS", "1")],
            ]
        } else {
            &[&[], &[("TURBO_REUSE_ISOLATE", "1"), ("TURBO_JOBS", "1")]]
        };
        for env in envs {
            let r = turbo_test(&p.dir, &["a.test.js", "b.test.js"], env);
            assert_eq!(r.code, Some(1), "{env:?}\n{}", r.all());
            for f in ["a.test.js", "b.test.js"] {
                assert!(
                    r.stdout.lines().any(|l| l.starts_with("ERROR")
                        && l.contains(f)
                        && l.contains("setup.js")
                        && l.contains("setup boom")),
                    "{env:?} {f}\n{}",
                    r.all()
                );
            }
        }
    }
    // a setup file that works is unaffected
    let ok = Project::new("rc-setup-ok");
    ok.with_esbuild();
    ok.file("package.json", r#"{"name":"r38d"}"#);
    ok.file(
        "vitest.config.js",
        "export default { test: { setupFiles: ['./setup.js'] } };\n",
    );
    ok.file("setup.js", "globalThis.SETUP = 1;\n");
    ok.file(
        "a.test.js",
        "test('a', () => { expect(globalThis.SETUP).toBe(1); });\n",
    );
    for env in [&[][..], &[("TURBO_NO_MR", "1")][..]] {
        assert_passes(&turbo_test(&ok.dir, &["a.test.js"], env), 1);
    }
}

#[test]
fn a_setup_file_that_fails_to_link_fails_the_test_file() {
    // turbo-test#38 item 3, ESM graph: a setup file whose imports do not link (a missing named
    // export) fails the test file with the instantiate error.
    let p = esm_graph_project("rc-setup-link");
    p.file(
        "vitest.config.js",
        "export default { test: { setupFiles: ['./setup.js'] } };\n",
    );
    p.file("m.js", "export const v = 1;\n");
    p.file(
        "setup.js",
        "import { nope } from './m.js';\nglobalThis.N = nope;\n",
    );
    p.file("a.test.js", "test('a', () => {});\n");
    let r = turbo_test(&p.dir, &["a.test.js"], &[]);
    assert_eq!(r.code, Some(1), "{}", r.all());
    assert!(
        r.stdout.lines().any(|l| l.starts_with("ERROR")
            && l.contains("setup file")
            && l.contains("instantiate:")
            && l.contains("nope")),
        "{}",
        r.all()
    );
}

/// turbo-test#43 item 2: a passing matcher never formats its operands (vitest builds the
/// failure message lazily); an operand whose JSON/string conversion throws — like a native
/// class prototype whose members check their receiver — fails only a failing assertion. The
/// spec passes under vitest 4.1.12 (matchers that vitest itself formats or deep-compares on a
/// pass, such as toContain and toHaveBeenCalledWith, are left out).
const LAZY_MESSAGE_TEST: &str = r#"import { test, expect, vi } from 'vitest';
const bad = () => { throw new TypeError('Illegal invocation'); };
const h = { toString: bad, valueOf: bad, toJSON: bad, [Symbol.toPrimitive]: bad };
Object.defineProperty(h, 'x', { get: bad, enumerable: true });
const spy = vi.fn();
spy(h);
const cases = {
  toBe: () => expect(h).toBe(h),
  toBeTruthy: () => expect(h).toBeTruthy(),
  notToBeFalsy: () => expect(h).not.toBeFalsy(),
  notToBeNull: () => expect(h).not.toBeNull(),
  notToBeUndefined: () => expect(h).not.toBeUndefined(),
  toBeDefined: () => expect(h).toBeDefined(),
  toBeTypeOf: () => expect(h).toBeTypeOf('object'),
  toSatisfy: () => expect(h).toSatisfy(() => true),
  toBeInstanceOf: () => expect(h).toBeInstanceOf(Object),
  toHaveProperty: () => expect(h).toHaveProperty('toString'),
  toHaveBeenLastCalledWith: () => expect(spy).toHaveBeenLastCalledWith(h),
  toHaveBeenNthCalledWith: () => expect(spy).toHaveBeenNthCalledWith(1, h),
  toHaveReturnedWith: () => expect(spy).toHaveReturnedWith(undefined),
  toHaveBeenCalled: () => expect(spy).toHaveBeenCalled(),
  toEqual: () => expect(h).toEqual(h),
  toStrictEqual: () => expect(h).toStrictEqual(h),
  toMatchObject: () => expect(h).toMatchObject({}),
};
for (const [name, fn] of Object.entries(cases)) test(name, fn);
test('a failing matcher still formats its message', () => {
  expect(() => expect(1).toBe(2)).toThrow('expected 1 to be 2');
  expect(() => expect([1]).toHaveLength(2)).toThrow('expected');
  expect(() => expect(h).toBe(1)).toThrow('Illegal invocation');
});
"#;

#[test]
fn passing_matchers_do_not_format_their_operands() {
    let p = Project::new("rc-lazy-msg");
    p.file("lazy.test.ts", LAZY_MESSAGE_TEST);
    let r = turbo_test(&p.dir, &[], &[]);
    assert_passes(&r, 18);
}
