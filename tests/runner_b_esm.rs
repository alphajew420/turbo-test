//! The runner's legacy ESM-graph loader (projects without esbuild, so the module-runner CJS path
//! is not available): static imports of JSON/CSS/ESM/CJS/builtins/`vitest`, dynamic `import()`,
//! entry `vi.mock` factories registered without a prepass, per-file load errors, and the
//! `__tt.run()` drive loop's contract checks.
#![cfg(unix)]

mod common;
use common::{turbo_test, Project};

/// The text-reporter line for `file` (`PASS  file  (...)`, `FAIL ...`, `ERROR file  (msg)`).
fn line_for<'a>(stdout: &'a str, file: &str) -> &'a str {
    stdout
        .lines()
        .find(|l| l.contains(&format!("/{file}  ")) || l.contains(&format!(" {file}  ")))
        .unwrap_or_else(|| panic!("no result line for {file} in:\n{stdout}"))
}

#[test]
fn static_and_dynamic_imports_of_every_module_kind() {
    let p = Project::new("rb-esm-kinds");
    p.file("package.json", r#"{"name":"rb-esm-kinds"}"#);
    p.file("dep.cjs", "exports.a = 1; exports.f = () => 'real';\n");
    p.file("data.json", r#"{"k": 5, "name": "x"}"#);
    p.file("s.module.css", ".c{}\n");
    p.file("esm.mjs", "export const E = 'esm'; export default 7;\n");
    p.file("bad.mjs", "export const = ;\n");
    p.file(
        "a.test.js",
        r#"import { describe, it, expect, vi } from 'vitest';
import { jest } from '@jest/globals';
import d from './data.json';
import s from './s.module.css';
import def, { E } from './esm.mjs';
import fs from 'node:fs';
import path from 'path';
import dep from './dep.cjs';
describe('g', () => {
  it('json', () => { expect(d.k).toBe(5); expect(d.name).toBe('x'); });
  it('css module proxy', () => { expect(s.foo).toBe('foo'); });
  it('esm', () => { expect(def).toBe(7); expect(E).toBe('esm'); });
  it('cjs', () => { expect(dep.a).toBe(1); expect(dep.f()).toBe('real'); });
  it('jest globals', () => { expect(typeof jest.fn).toBe('function'); expect(typeof vi.fn).toBe('function'); });
  it('import.meta.url', () => { expect(import.meta.url).toBe('file://' + globalThis.__cwd + '/a.test.js'); });
  it('dynamic', async () => {
    const m = await import('./esm.mjs'); expect(m.E).toBe('esm');
    const p = await import('node:path'); expect(typeof p.join).toBe('function');
    const v = await import('vitest'); expect(typeof v.vi.fn).toBe('function');
    await expect(import('./missing.mjs')).rejects.toThrow('dynamic import cannot resolve ./missing.mjs');
    await expect(import('./bad.mjs')).rejects.toThrow('dynamic import failed: ./bad.mjs');
  });
});
"#,
    );
    let r = turbo_test(&p.dir, &[], &[]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert!(
        line_for(&r.stdout, "a.test.js").contains("(7 passed, 0 failed)"),
        "{}",
        r.all()
    );
}

#[test]
fn node_fs_shim_is_backed_by_native_fs() {
    let p = Project::new("rb-esm-fs");
    p.file("package.json", r#"{"name":"rb-esm-fs"}"#);
    p.file("seed/one.txt", "1");
    p.file("seed/two.txt", "2");
    p.file(
        "fs.test.js",
        r#"import fs from 'node:fs';
import path from 'path';
test('fs natives', () => {
  const root = globalThis.__cwd;
  const dir = path.join(root, 'out');
  fs.mkdirSync(dir + '/sub/deeper', { recursive: true });
  expect(fs.existsSync(dir + '/sub/deeper')).toBe(true);
  fs.writeFileSync(dir + '/sub/f.txt', 'hello');
  fs.writeFileSync(dir + '/auto/created/g.txt', 'parents made');
  expect(fs.existsSync(dir + '/sub/f.txt')).toBe(true);
  expect(fs.readFileSync(dir + '/sub/f.txt', 'utf8')).toBe('hello');
  expect(fs.readFileSync(dir + '/auto/created/g.txt', 'utf8')).toBe('parents made');
  expect(fs.readdirSync(root + '/seed').sort()).toEqual(['one.txt', 'two.txt']);
  expect(fs.readdirSync(dir + '/nope')).toEqual([]);
  expect(() => fs.readFileSync(dir + '/nope.txt')).toThrow('readFileSync ' + dir + '/nope.txt');
  expect(() => fs.writeFileSync(dir + '/sub/f.txt/x', 'a')).toThrow('writeFileSync ' + dir + '/sub/f.txt/x');
  fs.rmSync(dir + '/sub/f.txt');
  expect(fs.existsSync(dir + '/sub/f.txt')).toBe(false);
  fs.rmSync(dir + '/missing-entry');
  fs.rmSync(dir);
  expect(fs.existsSync(dir)).toBe(false);
});
"#,
    );
    let r = turbo_test(&p.dir, &[], &[]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert!(!p.path("out").exists(), "rmSync(dir) removed the tree");
    assert!(p.path("seed/one.txt").exists());
}

#[test]
fn entry_mocks_register_without_an_esbuild_prepass() {
    let p = Project::new("rb-esm-mocks");
    p.file("package.json", r#"{"name":"rb-esm-mocks"}"#);
    for n in ["a", "b", "d"] {
        p.file(&format!("{n}.cjs"), &format!("exports.v = '{n}-real';\n"));
    }
    p.file("c.cjs", "exports.fn = () => 'c-real';\n");
    p.file(
        "m.test.js",
        r#"import a from './a.cjs';
import b from './b.cjs';
import c from './c.cjs';
import d from './d.cjs';
vi.mock('./a.cjs', () => ({ v: 'a-mock' }));
jest.mock('./b.cjs', () => ({ v: 'b-mock' }));
// a throwing factory must not break the registration of the others
vi.mock('./d.cjs', () => { throw new Error('factory boom'); });
vi.mock('./not-there.cjs', () => ({ v: 1 }));
test('mocks', () => {
  expect(a.v).toBe('a-mock');
  expect(b.v).toBe('b-mock');
  // no factory: the real implementation must not be reachable
  expect(c.fn?.()).toBe(undefined);
  expect(typeof d).toBe('object');
});
// factory-less form last: nothing may follow it (see the extract_mocks note)
vi.mock('./c.cjs');
"#,
    );
    for env in [&[][..], &[("TURBO_NO_MR", "1")][..]] {
        let r = turbo_test(&p.dir, &[], env);
        assert_eq!(r.code, Some(0), "{env:?}\n{}", r.all());
    }
}

#[test]
fn load_errors_name_the_failing_phase() {
    let p = Project::new("rb-esm-errors");
    p.file("package.json", r#"{"name":"rb-esm-errors"}"#);
    p.file("lib.mjs", "export const x = 1;\n");
    p.file("syn.mjs", "export const = ;\n");
    let t = "test('t', () => {});";
    p.file(
        "inst.test.js",
        &format!("import {{ nope }} from './lib.mjs'; {t}\n"),
    );
    p.file("depsyn.test.js", &format!("import './syn.mjs'; {t}\n"));
    p.file("miss.test.js", &format!("import './missing.mjs'; {t}\n"));
    p.file(
        "evalthrow.test.js",
        &format!("{t} throw new Error('top boom');\n"),
    );
    p.file(
        "tla.test.js",
        &format!("{t} await Promise.reject(new Error('tla boom'));\n"),
    );
    p.file("entrysyn.test.js", &format!("{t} )\n"));
    p.file("ok.test.js", &format!("{t}\n"));
    let r = turbo_test(&p.dir, &[], &[]);
    assert_eq!(r.code, Some(1), "{}", r.all());
    let o = &r.stdout;
    assert!(
        line_for(o, "inst.test.js").contains(
            "inst.test.js  (instantiate: SyntaxError: The requested module './lib.mjs' does not provide an export named 'nope')"
        ),
        "{o}"
    );
    assert!(line_for(o, "depsyn.test.js").contains("(graph load failed)"));
    assert!(line_for(o, "miss.test.js").contains("(graph load failed)"));
    assert!(line_for(o, "entrysyn.test.js").contains("(graph load failed)"));
    assert!(line_for(o, "evalthrow.test.js").contains("(module errored: Error: top boom)"));
    assert!(line_for(o, "tla.test.js").contains("(module errored: Error: tla boom)"));
    assert!(line_for(o, "ok.test.js").starts_with("PASS"));
    assert!(
        r.stderr.contains("cannot resolve ./missing.mjs from "),
        "{}",
        r.all()
    );
}

#[test]
fn drive_loop_rejects_a_broken_runtime_contract() {
    let p = Project::new("rb-esm-drive");
    p.file("package.json", r#"{"name":"rb-esm-drive"}"#);
    let cases = [
        (
            "nott",
            "Object.defineProperty(globalThis, '__tt', { get() { throw new Error('g'); } });",
            "(no __tt)",
        ),
        ("ttnum", "globalThis.__tt = 5;", "(__tt not object)"),
        ("runnum", "globalThis.__tt.run = 1;", "(run not fn)"),
        (
            "runget",
            "Object.defineProperty(globalThis.__tt, 'run', { get() { throw 1; } });",
            "(no run)",
        ),
        (
            "runval",
            "globalThis.__tt.run = () => 1;",
            "(run() not promise)",
        ),
        (
            "runthrow",
            "globalThis.__tt.run = () => { throw new Error('x'); };",
            "(run() threw)",
        ),
        (
            "runrej",
            "globalThis.__tt.run = () => Promise.reject(new Error('rj'));",
            "(test run promise did not fulfill (rejected: Error: rj))",
        ),
        (
            "runpend",
            "globalThis.__tt.run = () => new Promise(() => {});",
            "(test run promise did not fulfill (still pending))",
        ),
        (
            "runsum",
            "globalThis.__tt.run = async () => 5;",
            "(summary not object)",
        ),
    ];
    for (name, body, _) in &cases {
        p.file(
            &format!("{name}.test.js"),
            &format!("test('t', () => {{}}); {body}\n"),
        );
    }
    // A hand-built summary: counts, failure strings and per-test rows (null message -> empty,
    // non-object rows skipped) are read back field by field.
    p.file(
        "custom.test.js",
        "test('t', () => {}); globalThis.__tt.run = async () => ({ passed: 2, failed: 1, \
         failures: ['a: b'], tests: [{ name: 'n', status: 'passed', duration_ms: 1500, \
         message: null }, 7] });\n",
    );
    let r = turbo_test(&p.dir, &["--reporter", "verbose"], &[]);
    assert_eq!(r.code, Some(1), "{}", r.all());
    for (name, _, msg) in &cases {
        let l = line_for(&r.stdout, &format!("{name}.test.js"));
        assert!(
            l.starts_with("ERROR") && l.ends_with(msg),
            "{name}: {l}\n{}",
            r.all()
        );
    }
    let l = line_for(&r.stdout, "custom.test.js");
    assert!(
        l.starts_with("FAIL") && l.ends_with("custom.test.js  (2 passed, 1 failed)"),
        "{l}"
    );
    assert!(r.stdout.contains("✗ a: b"), "{}", r.all());
    assert!(r.stdout.contains("✓ n (1500ms)"), "{}", r.all());
}
