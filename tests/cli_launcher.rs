//! The launcher front-end through the real binary: terminal exits and their messages, missing
//! file pruning, subcommand stripping, `--environment` / config environment, config aliases,
//! a forced `-c` config, `--root`, and `--changed` against a real git repository (plus the
//! git-unavailable fallback).
#![cfg(unix)]

mod common;
use common::{json_report, turbo_test, Project, Run};
use std::path::Path;
use std::process::Command;

fn tt(p: &Project, args: &[&str]) -> Run {
    turbo_test(&p.dir, args, &[])
}

const PASS: &str = "test('p', () => { expect(1).toBe(1) })\n";

#[test]
fn no_test_files_exit_codes() {
    let p = Project::new("launch-empty");
    p.file("src/util.ts", "export const x = 1\n");
    let r = tt(&p, &[]);
    assert_eq!(r.code, Some(1), "{}", r.all());
    assert_eq!(
        r.stderr,
        "turbo-test: no test files found (looked for *.test.* / *.spec.*).\n"
    );
    assert_eq!(r.stdout, "");
    let r = tt(&p, &["run", "--passWithNoTests"]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert_eq!(
        r.stderr,
        "turbo-test: no test files found — exiting 0 (--passWithNoTests).\n"
    );
}

#[test]
fn missing_files_are_pruned() {
    let p = Project::new("launch-missing");
    p.file("a.test.ts", PASS);
    let r = tt(&p, &["a.test.ts", "gone.test.ts", ""]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert!(
        r.stderr
            .starts_with("turbo-test: skipping 1 missing file(s): gone.test.ts\n"),
        "{}",
        r.all()
    );
    assert!(
        r.stdout.contains("PASS  a.test.ts  (1 passed, 0 failed)"),
        "{}",
        r.all()
    );
    let r = tt(&p, &["gone.test.ts", "also-gone.test.ts"]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert_eq!(
        r.stderr,
        "turbo-test: skipping 2 missing file(s): gone.test.ts, also-gone.test.ts\nturbo-test: no existing test files to run.\n"
    );
}

#[test]
fn subcommands_and_noop_flags_are_stripped() {
    let p = Project::new("launch-sub");
    p.file("a.test.ts", PASS);
    for sub in ["run", "watch", "dev"] {
        let r = tt(
            &p,
            &[
                sub,
                "--pool",
                "threads",
                "--sequence.seed",
                "1",
                "--globals",
                "--reporter",
                "json",
            ],
        );
        assert_eq!(r.code, Some(0), "{sub}: {}", r.all());
        assert_eq!(json_report(&r)["numTotalTestSuites"], 1);
        assert!(
            !r.stderr.contains("ignoring unsupported flag"),
            "{}",
            r.all()
        );
    }
}

#[test]
fn environment_flag_and_config() {
    let p = Project::new("launch-env");
    p.file(
        "dom.test.ts",
        "test('dom', () => { expect(typeof document).toBe('object') })\n",
    );
    assert_eq!(tt(&p, &[]).code, Some(0));
    assert_eq!(tt(&p, &["--environment", "node"]).code, Some(1));
    assert_eq!(tt(&p, &["--environment=jsdom"]).code, Some(0));
    p.file(
        "vitest.config.ts",
        "export default { test: { environment: 'node' } }\n",
    );
    assert_eq!(tt(&p, &[]).code, Some(1), "config environment applies");
    assert_eq!(
        tt(&p, &["--environment", "happy-dom"]).code,
        Some(0),
        "flag wins"
    );
}

#[test]
fn config_aliases_reach_the_resolver() {
    let p = Project::new("launch-alias");
    p.file("src/lib/value.ts", "export const value = 42\n");
    p.file("other/answer.ts", "export const answer = 'yes'\n");
    p.file(
        "vitest.config.ts",
        "import path from 'node:path'\n\
         export default {\n\
           resolve: { alias: { '@': path.resolve(__dirname, './src') } },\n\
           test: { alias: [ { find: '#other', replacement: './other' } ] },\n\
         }\n",
    );
    p.file(
        "a.test.ts",
        "import { value } from '@/lib/value'\nimport { answer } from '#other/answer'\n\
         test('alias', () => { expect(value).toBe(42); expect(answer).toBe('yes') })\n",
    );
    let r = tt(&p, &[]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert!(r.stdout.contains("(1 passed, 0 failed)"), "{}", r.all());
}

#[test]
fn forced_config_and_root() {
    let p = Project::new("launch-config");
    p.file(
        "vitest.config.ts",
        "export default { test: { include: ['none/**'] } }\n",
    );
    p.file(
        "conf/ci.config.ts",
        "export default { test: { include: ['**/*.check.test.ts'] } }\n",
    );
    p.file("conf/x.check.test.ts", PASS);
    p.file("conf/y.test.ts", PASS);
    assert_eq!(tt(&p, &[]).code, Some(1), "root config includes nothing");
    for args in [
        &["-c", "conf/ci.config.ts"][..],
        &["--config=conf/ci.config.ts"],
    ] {
        let r = tt(&p, &[args, &["--reporter", "json"][..]].concat());
        assert_eq!(r.code, Some(0), "{args:?}: {}", r.all());
        assert_eq!(json_report(&r)["numTotalTestSuites"], 1);
        assert!(json_report(&r)["testResults"][0]["name"]
            .as_str()
            .unwrap()
            .ends_with("conf/x.check.test.ts"));
    }
    // --root/--dir move discovery (and the config search) into a subdirectory
    p.file(
        "pkg/vite.config.ts",
        "export default { test: { include: ['**/*.test.ts'] } }\n",
    );
    p.file("pkg/a.test.ts", PASS);
    p.file("pkg/sub/b.test.ts", PASS);
    for args in [&["--root", "pkg"][..], &["--dir=pkg"]] {
        let r = tt(&p, &[args, &["--reporter", "json"][..]].concat());
        assert_eq!(
            json_report(&r)["numTotalTestSuites"],
            2,
            "{args:?}: {}",
            r.all()
        );
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@e",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn changed_runs_only_changed_test_files() {
    let p = Project::new("launch-changed");
    p.file(".gitignore", ".tt-tmp/\n.tt-cache/\n");
    p.file("a.test.ts", PASS);
    p.file("b.test.ts", PASS);
    git(&p.dir, &["init", "-q"]);
    git(&p.dir, &["add", "."]);
    git(&p.dir, &["commit", "-q", "-m", "init"]);

    let r = tt(&p, &["--changed"]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert_eq!(
        r.stderr,
        "turbo-test: --changed: no changed test files (of 2) — exiting 0.\n"
    );

    p.file("b.test.ts", "test('b2', () => { expect(2).toBe(2) })\n");
    p.file("c.test.ts", PASS);
    let r = tt(&p, &["--changed", "--reporter", "json"]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    let names: Vec<_> = json_report(&r)["testResults"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            t["name"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(names, ["b.test.ts", "c.test.ts"]);
    let r = tt(
        &p,
        &[
            "--changed=HEAD",
            "a.test.ts",
            "b.test.ts",
            "--reporter",
            "json",
        ],
    );
    assert_eq!(json_report(&r)["numTotalTestSuites"], 1, "{}", r.all());

    // git missing from PATH → warn and run everything
    let r = turbo_test(
        &p.dir,
        &["--changed", "--reporter", "json"],
        &[("PATH", "/nonexistent")],
    );
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert!(
        r.stderr.starts_with(
            "turbo-test: --changed: git unavailable / not a repo — running all discovered tests.\n"
        ),
        "{}",
        r.all()
    );
    assert_eq!(json_report(&r)["numTotalTestSuites"], 3);
}

#[test]
fn coverage_flags_and_config_thresholds() {
    let p = Project::new("launch-coverage");
    p.with_esbuild();
    p.file(
        "src/math.ts",
        "export function add(a: number, b: number) { return a + b }\nexport function unused(n: number) {\n  if (n > 1) { return n * 2 }\n  return n\n}\n",
    );
    p.file("src/skip.ts", "export const skipped = 1\n");
    p.file(
        "math.test.ts",
        "import { add } from './src/math'\nimport { skipped } from './src/skip'\ntest('add', () => { expect(add(1, 2) + skipped).toBe(4) })\n",
    );
    let summary = |dir: &str| -> serde_json::Value {
        let s = std::fs::read_to_string(p.path(&format!("{dir}/coverage-summary.json"))).unwrap();
        serde_json::from_str(&s).unwrap()
    };
    let files = |v: &serde_json::Value| -> Vec<String> {
        let mut k: Vec<String> = v
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| *k != "total")
            .map(|k| k.rsplit('/').next().unwrap().to_string())
            .collect();
        k.sort();
        k
    };

    // explicit flags: reporter, dir, include/exclude, per-file + a met threshold
    let r = tt(
        &p,
        &[
            "--coverage-reporter",
            "json-summary",
            "--coverage-dir",
            "cov1",
            "--coverage-include",
            "src/**",
            "--coverage-exclude",
            "src/skip.ts",
            "--coverage-per-file",
            "--coverage-thresholds",
            "lines=1",
        ],
    );
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert_eq!(files(&summary("cov1")), ["math.ts"]);

    // vitest config coverage block: thresholds/include injected when not passed
    p.file(
        "vitest.config.ts",
        "export default { test: { coverage: { include: ['src/**'], thresholds: { lines: 100 } } } }\n",
    );
    let r = tt(
        &p,
        &[
            "--coverage",
            "--coverage-reporters",
            "json-summary",
            "--coverage-dir",
            "cov2",
        ],
    );
    assert_eq!(
        r.code,
        Some(1),
        "unmet config threshold fails the run: {}",
        r.all()
    );
    assert_eq!(files(&summary("cov2")), ["math.ts", "skip.ts"]);
    // flags win over the config
    let r = tt(
        &p,
        &[
            "--coverage-threshold",
            "lines=1",
            "--coverage-reporter",
            "json-summary",
            "--coverage-dir",
            "cov3",
        ],
    );
    assert_eq!(r.code, Some(0), "{}", r.all());
}
