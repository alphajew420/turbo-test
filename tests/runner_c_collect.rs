//! Test collection and run order as vitest 4.1.12 does them: tasks run (and are reported) in
//! declaration order. Expected values are vitest 4.1.12's on the same files.
#![cfg(unix)]

mod common;
use common::{turbo_test, Project, Run};

/// The TAP lines of a `--reporter tap` run (one `ok N - name` / `not ok N - name` per test).
fn tap_points(r: &Run) -> Vec<String> {
    r.stdout
        .lines()
        .filter(|l| l.starts_with("ok ") || l.starts_with("not ok "))
        .map(str::to_string)
        .collect()
}

#[test]
fn tasks_run_in_declaration_order() {
    // turbo-test#64: a block's own tests ran before its nested describes, so `last` ran before
    // `d > inner`. vitest 4.1.12 runs (and reports) first, d > inner, last.
    let p = Project::new("rc-collect-order");
    p.file(
        "o.test.js",
        "const log = []\n\
         test('first', () => { log.push('first') })\n\
         describe('d', () => {\n\
           test('inner', () => { log.push('inner') })\n\
           describe('deeper', () => { test('deep', () => { log.push('deep') }) })\n\
           test('inner2', () => { log.push('inner2') })\n\
         })\n\
         test('last', () => { log.push('last'); expect(log.join()).toBe('first,inner,deep,inner2,last') })\n",
    );
    let r = turbo_test(&p.dir, &["--reporter", "tap", "o.test.js"], &[]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert_eq!(
        tap_points(&r),
        [
            "ok 1 - first",
            "ok 2 - d > inner",
            "ok 3 - d > deeper > deep",
            "ok 4 - d > inner2",
            "ok 5 - last",
        ],
        "{}",
        r.all()
    );
}

#[test]
fn only_runs_what_vitest_runs() {
    // turbo-test#65: `describe.only` skipped its own tests (only tests marked `.only` themselves
    // ran). Expected = vitest 4.1.12 (`interpretTaskModes`): a task runs when it or an enclosing
    // suite is `.only`, but inside a suite holding `.only` tasks only those run.
    let p = Project::new("rc-collect-only");
    p.file(
        "o.test.js",
        "describe.only('d', () => {\n\
           test('a', () => {})\n\
         })\n\
         test('outside', () => {})\n\
         describe('e', () => {\n\
           test.only('eo', () => {})\n\
           test('en', () => {})\n\
         })\n\
         describe.only('f', () => {\n\
           test.only('fo', () => {})\n\
           test('fn', () => {})\n\
           describe('fd', () => { test('fdt', () => {}) })\n\
         })\n\
         describe('g', () => {\n\
           describe.only('gi', () => { test('gt', () => {}) })\n\
           test('gx', () => {})\n\
         })\n",
    );
    let r = turbo_test(&p.dir, &["--reporter", "tap", "o.test.js"], &[]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert_eq!(
        tap_points(&r),
        [
            "ok 1 - d > a",
            "ok 2 - outside # SKIP",
            "ok 3 - e > eo",
            "ok 4 - e > en # SKIP",
            "ok 5 - f > fo",
            "ok 6 - f > fn # SKIP",
            "ok 7 - f > fd > fdt # SKIP",
            "ok 8 - g > gi > gt",
            "ok 9 - g > gx # SKIP",
        ],
        "{}",
        r.all()
    );
}

/// The tests a `--reporter tap` run ran (its points without a `# SKIP` directive), names only.
fn ran(r: &Run) -> Vec<String> {
    tap_points(r)
        .into_iter()
        .filter(|l| !l.ends_with("# SKIP"))
        .map(|l| l.split_once(" - ").unwrap().1.to_string())
        .collect()
}

#[test]
#[ignore = "turbo-test#66: fails until the fix"]
fn name_pattern_matches_the_space_joined_full_name() {
    // turbo-test#66: -t was matched against `describe > test`. vitest 4.1.12 matches the full
    // name, the names joined with a space (`getTaskFullName`).
    let p = Project::new("rc-collect-tname");
    p.file(
        "a.test.js",
        "describe('outer', () => {\n\
           it('inner pass', () => {})\n\
           describe('deep', () => { it('leaf', () => {}) })\n\
         })\n\
         test('top', () => {})\n",
    );
    let cases: [(&str, &[&str]); 5] = [
        ("outer inner", &["outer > inner pass"]),
        ("outer > inner", &[]),
        ("^outer deep leaf$", &["outer > deep > leaf"]),
        ("deep", &["outer > deep > leaf"]),
        ("^top$", &["top"]),
    ];
    for (pat, want) in cases {
        let r = turbo_test(&p.dir, &["--reporter", "tap", "-t", pat, "a.test.js"], &[]);
        assert_eq!(r.code, Some(0), "{pat}\n{}", r.all());
        assert_eq!(ran(&r), want, "{pat}\n{}", r.all());
    }
}
