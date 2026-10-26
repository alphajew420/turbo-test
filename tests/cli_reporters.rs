//! Every reporter (default/dot/verbose/json/junit/tap + an unknown one) and `--outputFile`
//! against one project holding a partially failing file (with a skipped test and XML-special
//! names), a passing file, and a file that fails to load.
#![cfg(unix)]

mod common;
use common::{json_report, turbo_test, Project, Run};

fn fixture(name: &str) -> Project {
    let p = Project::new(name);
    p.file(
        "a.test.ts",
        "test('adds <&>', () => { expect(1 + 1).toBe(2) })\n\
         test.skip('later', () => {})\n\
         test('breaks', () => { expect('x').toBe('y') })\n",
    );
    p.file(
        "b.test.ts",
        "test('ok', () => { expect(true).toBe(true) })\n",
    );
    p.file("c.test.ts", "throw new Error('top <boom>')\n");
    p
}

fn path(p: &Project, rel: &str) -> String {
    p.path(rel).display().to_string()
}

fn run(p: &Project, args: &[&str]) -> Run {
    let r = turbo_test(&p.dir, args, &[]);
    assert_eq!(r.code, Some(1), "{}", r.all());
    r
}

const SUMMARY: &str = "3 files | 2 passed | 1 failed | 1 load-errors | 2 jobs | wall ";
const FOOTER: &str =
    "\n Test Files  2 failed | 1 passed (3)\n      Tests  1 failed | 2 passed (3)\n   Duration  ";

#[test]
fn default_text_reporter_and_unknown_fallback() {
    let p = fixture("rep-default");
    let lines = format!(
        "FAIL  {}  (1 passed, 1 failed)\n        ✗ breaks: expected \"x\" to be \"y\"\nPASS  {}  (1 passed, 0 failed)\nERROR {}  (module errored: Error: top <boom>)\n",
        path(&p, "a.test.ts"),
        path(&p, "b.test.ts"),
        path(&p, "c.test.ts")
    );
    for args in [
        &[][..],
        &["--reporter", "default"],
        &["--reporter", "html"],
        &["--reporters", "hanging-process"],
    ] {
        let r = run(&p, args);
        assert!(r.stdout.starts_with(&lines), "{args:?}\n{}", r.all());
        assert!(r.stdout.contains(FOOTER), "{}", r.all());
        assert!(r.stdout.contains(SUMMARY), "{}", r.all());
        assert!(!r.stderr.contains(SUMMARY), "{}", r.all());
    }
}

#[test]
fn dot_and_verbose_reporters() {
    let p = fixture("rep-dot");
    let r = run(&p, &["--reporter", "dot"]);
    assert!(r.stdout.starts_with("x.!\n"), "{}", r.all());
    assert!(
        r.stdout.contains(FOOTER) && r.stdout.contains(SUMMARY),
        "{}",
        r.all()
    );

    let r = run(&p, &["--reporter", "verbose"]);
    let out = &r.stdout;
    let a = out
        .find(&format!("FAIL  {}", path(&p, "a.test.ts")))
        .expect(out);
    let adds = out.find("\n   ✓ adds <&> (").expect(out);
    let later = out.find("\n   - later (").expect(out);
    let breaks = out.find("\n   ✗ breaks (").expect(out);
    let b = out
        .find(&format!("PASS  {}", path(&p, "b.test.ts")))
        .expect(out);
    let ok = out.find("\n   ✓ ok (").expect(out);
    let c = out
        .find(&format!("ERROR {}", path(&p, "c.test.ts")))
        .expect(out);
    assert!(
        a < adds && adds < later && later < breaks && breaks < b && b < ok && ok < c,
        "{out}"
    );
    assert!(out.contains(FOOTER), "{out}");
}

#[test]
fn json_reporter_keeps_stdout_clean() {
    let p = fixture("rep-json");
    let r = run(&p, &["--reporter", "json"]);
    let j = json_report(&r);
    assert_eq!(j["numTotalTestSuites"], 3);
    assert_eq!(j["numTotalTests"], 3);
    assert_eq!(j["numPassedTests"], 2);
    assert_eq!(j["numFailedTests"], 1);
    assert_eq!(j["success"], false);
    let st: Vec<_> = j["testResults"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["status"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(st, ["failed", "passed", "error"]);
    assert_eq!(j["testResults"][2]["name"], path(&p, "c.test.ts"));
    assert!(r.stderr.contains(SUMMARY), "{}", r.all());
    assert!(!r.stdout.contains("Test Files"), "{}", r.all());
}

#[test]
fn junit_reporter() {
    let p = fixture("rep-junit");
    let r = run(&p, &["--reporter", "junit"]);
    let x = &r.stdout;
    let (a, b, c) = (
        path(&p, "a.test.ts"),
        path(&p, "b.test.ts"),
        path(&p, "c.test.ts"),
    );
    assert!(x.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites name=\"turbo-test\" tests=\"3\" failures=\"1\" errors=\"1\">\n"), "{x}");
    assert!(
        x.contains(&format!(
            "  <testsuite name=\"{a}\" tests=\"2\" failures=\"1\" errors=\"0\" time=\""
        )),
        "{x}"
    );
    assert!(
        x.contains(&format!(
            "    <testcase name=\"adds &lt;&amp;&gt;\" classname=\"{a}\" time=\""
        )),
        "{x}"
    );
    assert!(
        x.contains(&format!(
            "    <testcase name=\"breaks\" classname=\"{a}\" time=\""
        )),
        "{x}"
    );
    assert!(x.contains("\">\n      <failure message=\"expected &quot;x&quot; to be &quot;y&quot;\"/>\n    </testcase>\n"), "{x}");
    assert!(!x.contains("later"), "skipped tests are omitted: {x}");
    assert!(
        x.contains(&format!(
            "  <testsuite name=\"{b}\" tests=\"1\" failures=\"0\" errors=\"0\" time=\""
        )),
        "{x}"
    );
    assert!(
        x.contains(&format!(
            "  <testsuite name=\"{c}\" tests=\"1\" failures=\"1\" errors=\"1\" time=\""
        )),
        "{x}"
    );
    assert!(x.contains(&format!(
        "    <testcase name=\"{c}\" classname=\"{c}\">\n      <error message=\"module errored: Error: top &lt;boom&gt;\"/>\n    </testcase>\n  </testsuite>\n</testsuites>\n"
    )), "{x}");
    assert!(r.stderr.contains(SUMMARY), "{}", r.all());
}

#[test]
fn tap_reporter() {
    let p = fixture("rep-tap");
    let r = run(&p, &["--reporter", "tap"]);
    // the load-error file is a `not ok` point too (turbo-test#32 item 4), never dropped
    assert_eq!(
        r.stdout,
        format!(
            "TAP version 13\n1..5\nok 1 - adds <&>\nok 2 - later # SKIP\nnot ok 3 - breaks\n  ---\n  message: expected \"x\" to be \"y\"\n  ...\nok 4 - ok\nnot ok 5 - {}\n  ---\n  message: module errored: Error: top <boom>\n  ...\n\n",
            path(&p, "c.test.ts")
        ),
        "{}",
        r.all()
    );
    assert!(r.stderr.contains(SUMMARY), "{}", r.all());
}

#[test]
fn output_file_failures_and_missing_dirs() {
    // turbo-test#32 item 5: text reporter + --outputFile into a missing dir silently wrote
    // nothing (exit 0); an unwritable --outputFile never failed the run.
    let p = Project::new("rep-outfile-dirs");
    p.file("a.test.js", "test('a', () => {})\n");
    let target = path(&p, "no/such/dir/out.txt");
    let r = turbo_test(&p.dir, &["--outputFile", &target, "a.test.js"], &[]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "PASS  a.test.js  (1 passed, 0 failed)\n"
    );
    std::fs::create_dir_all(p.path("adir")).unwrap();
    for reporter in ["default", "json"] {
        let r = turbo_test(
            &p.dir,
            &["--reporter", reporter, "--outputFile", "adir", "a.test.js"],
            &[],
        );
        assert_eq!(r.code, Some(1), "{reporter}: {}", r.all());
        assert!(
            r.stderr
                .contains("turbo-test: failed to write --outputFile 'adir': "),
            "{reporter}: {}",
            r.all()
        );
    }
}

#[test]
fn output_file_artifacts() {
    let p = fixture("rep-outfile");
    // artifact → file (parent dirs created); stdout gets the human footer instead
    let r = run(
        &p,
        &["--reporter", "junit", "--outputFile", "out/deep/junit.xml"],
    );
    let xml = std::fs::read_to_string(p.path("out/deep/junit.xml")).unwrap();
    assert!(
        xml.starts_with("<?xml") && xml.ends_with("</testsuites>\n"),
        "{xml}"
    );
    assert!(!r.stdout.contains("<?xml"), "{}", r.all());
    assert!(
        r.stdout.contains(FOOTER) && r.stdout.contains(SUMMARY),
        "{}",
        r.all()
    );

    run(&p, &["--reporter", "tap", "--output-file", "r.tap"]);
    assert!(std::fs::read_to_string(p.path("r.tap"))
        .unwrap()
        .starts_with("TAP version 13\n1..5\n"));

    run(&p, &["--reporter", "json", "--outputFile", "r.json"]);
    let j: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.path("r.json")).unwrap()).unwrap();
    assert_eq!(j["numFailedTests"], 1);

    // human reporter + --outputFile → the PASS/FAIL lines are written, verbose still prints
    let r = run(&p, &["--reporter", "verbose", "--outputFile", "lines.txt"]);
    let txt = std::fs::read_to_string(p.path("lines.txt")).unwrap();
    assert!(
        txt.starts_with(&format!(
            "FAIL  {}  (1 passed, 1 failed)\n        ✗ breaks",
            path(&p, "a.test.ts")
        )),
        "{txt}"
    );
    assert!(
        txt.ends_with(&format!(
            "ERROR {}  (module errored: Error: top <boom>)\n",
            path(&p, "c.test.ts")
        )),
        "{txt}"
    );
    assert!(r.stdout.contains("   ✓ adds <&> ("), "{}", r.all());

    // unwritable target (an existing directory) → error on stderr, run still reported
    std::fs::create_dir_all(p.path("adir")).unwrap();
    let r = run(&p, &["--reporter", "json", "--outputFile", "adir"]);
    assert!(
        r.stderr
            .contains("turbo-test: failed to write --outputFile 'adir': "),
        "{}",
        r.all()
    );
    assert!(r.stdout.contains(FOOTER), "{}", r.all());
}
