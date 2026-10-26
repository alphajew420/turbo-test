//! Config and mock-graph semantics that follow jest/vitest: a JSON jest config (package.json
//! `"jest"` block, `jest.config.json`) is read like `jest.config.js`; a factory-less `vi.mock`
//! stays factory-less whatever code follows it.
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

const SETUP_TEST: &str = "test('setup ran', () => { expect(globalThis.FROM_SETUP).toBe(1); });\n";

#[test]
fn json_jest_configs_are_read() {
    // package.json "jest" block (turbo-test#31 item 1 repro)
    let p = Project::new("rc-jest-pkg");
    p.file(
        "package.json",
        r#"{"name":"r1","jest":{"setupFiles":["<rootDir>/setup.js"]}}"#,
    );
    p.file("setup.js", "globalThis.FROM_SETUP = 1;\n");
    p.file("a.test.js", SETUP_TEST);
    assert_passes(&turbo_test(&p.dir, &["a.test.js"], &[]), 1);

    // jest.config.json, with a JSON rootDir and setupFilesAfterEnv
    let q = Project::new("rc-jest-json");
    q.file("package.json", r#"{"name":"r1b"}"#);
    q.file(
        "jest.config.json",
        "{\n  \"rootDir\": \"src\",\n  \"setupFilesAfterEnv\": [\"<rootDir>/setup.js\"]\n}\n",
    );
    q.file("src/setup.js", "globalThis.FROM_SETUP = 1;\n");
    q.file("src/a.test.js", SETUP_TEST);
    assert_passes(&turbo_test(&q.dir, &["src/a.test.js"], &[]), 1);

    // the jest block's keys only: a "setupFiles" key elsewhere in package.json is not jest's
    let r = Project::new("rc-jest-scoped");
    r.file(
        "package.json",
        r#"{"name":"r1c","other":{"setupFiles":["./wrong.js"]},"jest":{"testEnvironment":"node"}}"#,
    );
    r.file("wrong.js", "globalThis.FROM_SETUP = 'wrong';\n");
    r.file(
        "a.test.js",
        "test('no setup', () => { expect(globalThis.FROM_SETUP).toBe(undefined); });\n",
    );
    assert_passes(&turbo_test(&r.dir, &["a.test.js"], &[]), 1);
}

#[test]
fn a_factory_less_mock_followed_by_more_code_stays_factory_less() {
    // turbo-test#31 item 4 end to end: `vi.mock('./a')` took the next call's factory (and that
    // mock was dropped), and in turbo-test#31 item 5's exact one-line repro the test callback
    // after a factory-less vi.mock became its "factory".
    let p = Project::new("rc-mock-parse");
    p.file("package.json", r#"{"name":"r4","type":"module"}"#);
    p.file("m.js", "export function f() { return 1 }\n");
    p.file("a.js", "export function a() { return 'real-a' }\n");
    p.file("b.js", "export const v = 'real-b';\n");
    p.file(
        "one.test.js",
        "import { f } from './m.js'; vi.mock('./m.js'); test('automock', () => { expect(vi.isMockFunction(f)).toBe(true) });\n",
    );
    p.file(
        "two.test.js",
        r#"import { a } from './a.js';
import { v } from './b.js';
vi.mock('./a.js');
vi.mock('./b.js', () => ({ v: 'mock-b' }));
test('both mocks', () => {
  expect(vi.isMockFunction(a)).toBe(true);
  expect(a()).toBe(undefined);
  expect(v).toBe('mock-b');
});
"#,
    );
    let r = turbo_test(&p.dir, &["one.test.js", "two.test.js"], &[]);
    assert_passes(&r, 2);
    p.with_esbuild();
    let r = turbo_test(&p.dir, &["one.test.js", "two.test.js"], &[]);
    assert_passes(&r, 2);
}
