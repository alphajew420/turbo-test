//! Failed matchers throw what vitest 4.1.12 throws: chai's `AssertionError` with vitest's message
//! for every built-in matcher, positive and negated (spy listings and diffs included), snapshot
//! mismatches and custom (`expect.extend`) matchers. The corpus (`tests/data/matchers`) records
//! each case's error name and message; `vitest.json` is vitest 4.1.12's (NO_COLOR) record.
#![cfg(unix)]

mod common;
use common::{turbo_test, Project};
use serde_json::Value;

const FILES: [(&str, &str); 6] = [
    ("cases.js", include_str!("data/matchers/cases.js")),
    ("cases2.js", include_str!("data/matchers/cases2.js")),
    ("m.test.js", include_str!("data/matchers/m.test.js")),
    ("m2.test.js", include_str!("data/matchers/m2.test.js")),
    ("s.test.js", include_str!("data/matchers/s.test.js")),
    (
        "__snapshots__/s.test.js.snap",
        include_str!("data/matchers/s.test.js.snap"),
    ),
];

/// A record's (label, error name, message) — or (label, "passed", "").
fn rows(v: &Value) -> Vec<(String, String, String)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let s = |k: &str| r[k].as_str().unwrap_or_default().to_string();
            if r["passed"] == true {
                (s("label"), "passed".into(), String::new())
            } else {
                (s("label"), s("name"), s("message"))
            }
        })
        .collect()
}

#[test]
#[ignore = "turbo-test#71: fails until the fix"]
fn failed_matchers_throw_what_vitest_throws() {
    // turbo-test#71: a failed matcher threw a plain `Error` with turbo-test's own wording
    // (`expected 1 to be 2`, `expected {"a":1} to equal {"a":2}`, `expected spy called 3x, got 2`).
    let want: Value = serde_json::from_str(include_str!("data/matchers/vitest.json")).unwrap();
    for esbuild in [false, true] {
        let p = Project::new("rc-matchers");
        if esbuild {
            p.with_esbuild();
        }
        p.file("package.json", r#"{ "type": "module" }"#);
        for (name, src) in FILES {
            p.file(name, src);
        }
        let r = turbo_test(&p.dir, &["m.test.js", "m2.test.js", "s.test.js"], &[]);
        assert_eq!(r.code, Some(0), "{}", r.all());
        for (out, key) in [("out.json", "m"), ("out2.json", "m2"), ("out3.json", "s")] {
            let got: Value =
                serde_json::from_str(&std::fs::read_to_string(p.path(out)).unwrap()).unwrap();
            let (got, want) = (rows(&got), rows(&want[key]));
            assert_eq!(got.len(), want.len(), "esbuild={esbuild} {out}");
            for (g, w) in got.iter().zip(&want) {
                assert_eq!(g, w, "esbuild={esbuild} {out}");
            }
        }
    }
}
