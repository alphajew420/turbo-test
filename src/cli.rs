//! The `turbo-test` CLI driver — the body of `src/bin/turbo_test.rs`, split into testable pieces.
//!
//! Runs test files across N worker threads, each with its own V8 isolate booted from the
//! single shared framework snapshot (spec §4). Work-stealing via an atomic cursor balances
//! load; files are ordered slowest-first from persisted historical durations (duration-aware
//! scheduling) so the longest files start first and workers drain evenly. Results are
//! returned in-process (no IPC serialization needed for the thread model).
//!
//! Layout: [`parse_args`] turns the launcher's effective argv into [`Options`] (no side effects),
//! [`execute_file`] runs one file with the panic guard + transient/fresh-isolate retries against an
//! injectable [`Engine`], [`run_files`] is the worker pool, and the `render_*` / [`footer`] /
//! [`summary`] functions are the pure reporters. [`run`] glues them together and returns the
//! process exit code; the binary is a thin `exit(main_with_args(argv))` wrapper.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use crate::coverage;
use crate::runner::{self, TestCase, TestReport};

/// What running one test file produced: a report, or a load error message.
pub type FileOutcome = Result<TestReport, String>;

/// Printed (to stderr, exit 2) when no test files remain after argument parsing.
pub const USAGE: &str =
    "usage: turbo-test [--jobs N] [--shard i/n] [--reporter json] <file.test.ts> [more...]";

/// One test file's outcome, as the reporters consume it.
#[derive(Debug, Clone)]
pub struct FileResult {
    /// Index into the (sharded) file list.
    pub idx: usize,
    /// The default reporter's `PASS`/`FAIL`/`ERROR` line (with `✗ failure` continuation lines).
    pub line: String,
    pub passed: u32,
    pub failed: u32,
    pub load_error: bool,
    /// Load-error message (for the synthetic JUnit/TAP failing case); empty otherwise.
    pub error_msg: String,
    /// Per-test cases (name/status/duration/message) for the per-testcase reporters.
    pub tests: Vec<TestCase>,
    pub setup_us: f64,
    pub dur_ms: f64,
}

/// Which reporter to emit. Anything turbo-test does not implement is accepted-and-ignored and
/// falls back to `Default` (the text reporter) — never an error (vitest-compat requirement).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reporter {
    Default,
    Dot,
    Json,
    Junit,
    Tap,
    Verbose,
}

impl Reporter {
    /// The reporter named by a `--reporter` value (`None` = the flag had no value).
    pub fn parse(v: Option<&str>) -> Reporter {
        match v {
            Some("json") => Reporter::Json,
            Some("junit") => Reporter::Junit,
            Some("tap") => Reporter::Tap,
            Some("verbose") => Reporter::Verbose,
            Some("dot") => Reporter::Dot,
            _ => Reporter::Default,
        }
    }

    /// `json/junit/tap` are machine artifacts — stdout carries only the artifact (the human
    /// summary line goes to stderr). `default/dot/verbose` are human reporters (stdout).
    pub fn is_artifact(self) -> bool {
        matches!(self, Reporter::Json | Reporter::Junit | Reporter::Tap)
    }
}

/// A coverage-configuration call requested by a `--coverage*` flag, applied in argv order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoverageOp {
    Enable,
    OutDir(String),
    Thresholds(String),
    PerFile,
    Reporters(String),
    Include(String),
    Exclude(String),
}

impl CoverageOp {
    /// Apply this op to the process-global coverage configuration.
    pub fn apply(&self) {
        match self {
            CoverageOp::Enable => coverage::enable(),
            CoverageOp::OutDir(d) => coverage::set_out_dir(d),
            CoverageOp::Thresholds(v) => coverage::set_thresholds(v),
            CoverageOp::PerFile => coverage::set_per_file(true),
            CoverageOp::Reporters(v) => coverage::set_reporters(v),
            CoverageOp::Include(v) => coverage::add_include(v),
            CoverageOp::Exclude(v) => coverage::add_exclude(v),
        }
    }
}

/// The runner-side options parsed from the launcher's effective argv.
#[derive(Debug, PartialEq)]
pub struct Options {
    pub files: Vec<PathBuf>,
    pub jobs: usize,
    pub shard: Option<(usize, usize)>,
    pub reporter: Reporter,
    pub output_file: Option<String>,
    /// `--bail N`: stop pulling new files once cumulative failed tests >= N (0 = disabled).
    pub bail: usize,
    /// `--no-allowOnly`: error if any `.only` test is collected (vitest CI default).
    pub allow_only: bool,
    /// Env vars the runtime reads (`TURBO_TEST_TIMEOUT`, …), set in this order before the run.
    pub env: Vec<(&'static str, String)>,
    /// Coverage configuration calls, applied in this order before the run.
    pub coverage: Vec<CoverageOp>,
    /// `ignoring unsupported flag` warnings, printed to stderr in this order.
    pub warnings: Vec<String>,
}

/// Default worker count = `TURBO_JOBS` when it parses, else host logical cores. (Reducing this
/// to leave headroom for V8's GC helper threads looked like a big win on a warm 40-file
/// microbench but REGRESSED the full suite badly: +75% on a 431-file run — with the whole cold
/// suite in flight, all cores stay productively busy and the GC helpers are mostly idle, so fewer
/// workers just leaves cores empty. Lesson: measure parallelism changes on the FULL suite, never a
/// subset.) An explicit `--jobs` flag still wins (see [`parse_args`]).
pub fn default_jobs(turbo_jobs: Option<&str>) -> usize {
    turbo_jobs.and_then(|v| v.parse().ok()).unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    })
}

/// Parse the runner flags + test files (the launcher already consumed its own flags and resolved
/// the file set). Pure: env vars, coverage calls and warnings are returned, not applied.
pub fn parse_args<I: IntoIterator<Item = String>>(argv: I, default_jobs: usize) -> Options {
    let mut o = Options {
        files: Vec::new(),
        jobs: default_jobs,
        shard: None,
        reporter: Reporter::Default,
        output_file: None,
        bail: 0,
        allow_only: true,
        env: Vec::new(),
        coverage: Vec::new(),
        warnings: Vec::new(),
    };
    let mut args = argv.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            // `--maxWorkers N` is vitest's name for the worker count → maps to --jobs.
            "--jobs" | "-j" | "--maxWorkers" => {
                o.jobs = args.next().and_then(|v| v.parse().ok()).unwrap_or(o.jobs)
            }
            // turbo-test has no minimum-worker concept; accept-and-ignore the value (documented).
            "--minWorkers" => {
                let _ = args.next();
            }
            // `--testTimeout N` → default per-test timeout (ms); runtime reads __TT_DEFAULT_TIMEOUT.
            "--testTimeout" => {
                if let Some(v) = args.next() {
                    if v.parse::<u64>().is_ok() {
                        o.env.push(("TURBO_TEST_TIMEOUT", v));
                    }
                }
            }
            // `--retry N` → global default retry count; runtime reads __TT_DEFAULT_RETRY.
            "--retry" => {
                if let Some(v) = args.next() {
                    if v.parse::<u64>().is_ok() {
                        o.env.push(("TURBO_TEST_RETRY", v));
                    }
                }
            }
            // `--bail N` → stop the run after N failed tests total (cross-worker, file-granular).
            "--bail" => o.bail = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            // `--silent` → suppress test console.* output; runtime reads __TT_SILENT.
            "--silent" => o.env.push(("TURBO_TEST_SILENT", "1".to_string())),
            // `--allowOnly` / `--no-allowOnly` → permit (default) or forbid stray `.only` tests.
            "--allowOnly" => o.allow_only = true,
            "--no-allowOnly" => o.allow_only = false,
            "--shard" => {
                if let Some(v) = args.next() {
                    if let Some((i, n)) = v.split_once('/') {
                        if let (Ok(i), Ok(n)) = (i.parse(), n.parse()) {
                            o.shard = Some((i, n));
                        }
                    }
                }
            }
            // `--reporter <r>`: select an output reporter. Implemented: default, dot, json,
            // junit, tap, verbose. Any other value (html, tap-flat, hanging-process, …) is
            // accepted-and-ignored → falls back to the text reporter (never an error).
            "--reporter" | "--reporters" => o.reporter = Reporter::parse(args.next().as_deref()),
            // `--outputFile <path>`: write the active reporter's output to a file (vitest writes
            // the reporter artifact here — JUnit XML, TAP, JSON). Text/dot/verbose still print.
            "--outputFile" | "--output-file" => o.output_file = args.next(),
            // `-t <re>` / `--testNamePattern <re>` — run only tests whose full `describe > it`
            // name matches the regex (vitest semantics: unanchored, case-sensitive). Plumbed to
            // the runtime via env → `globalThis.__TT_NAME_PATTERN` (see runner bootstrap).
            "-t" | "--testNamePattern" => {
                if let Some(v) = args.next() {
                    o.env.push(("TURBO_TEST_NAME_PATTERN", v));
                }
            }
            // `-u` / `--update` — write missing/changed snapshots instead of failing (vitest
            // `--update`). Plumbed to the runtime via env → globalThis.__TT_UPDATE_SNAPSHOTS.
            "-u" | "--update" => o.env.push(("TURBO_UPDATE_SNAPSHOTS", "1".to_string())),
            "--coverage" => o.coverage.push(CoverageOp::Enable),
            // `--coverage-dir <path>` sets the lcov output dir (and implies --coverage).
            "--coverage-dir" => {
                o.coverage.push(CoverageOp::Enable);
                if let Some(d) = args.next() {
                    o.coverage.push(CoverageOp::OutDir(d));
                }
            }
            // `--coverage-thresholds lines=90,functions=80,branches=80` — gate (implies --coverage).
            "--coverage-thresholds" | "--coverage-threshold" => {
                o.coverage.push(CoverageOp::Enable);
                if let Some(v) = args.next() {
                    o.coverage.push(CoverageOp::Thresholds(v));
                }
            }
            // apply thresholds to EACH reported file, not just the total.
            "--coverage-per-file" => {
                o.coverage.push(CoverageOp::Enable);
                o.coverage.push(CoverageOp::PerFile);
            }
            // `--coverage-reporter lcov,json-summary,text,html` (repeatable / comma-list).
            "--coverage-reporter" | "--coverage-reporters" => {
                o.coverage.push(CoverageOp::Enable);
                if let Some(v) = args.next() {
                    o.coverage.push(CoverageOp::Reporters(v));
                }
            }
            // include/exclude globs (cwd-relative) — usually injected from vitest config by the
            // launcher.
            "--coverage-include" => {
                o.coverage.push(CoverageOp::Enable);
                if let Some(v) = args.next() {
                    o.coverage.push(CoverageOp::Include(v));
                }
            }
            "--coverage-exclude" => {
                o.coverage.push(CoverageOp::Enable);
                if let Some(v) = args.next() {
                    o.coverage.push(CoverageOp::Exclude(v));
                }
            }
            // `--no-file-parallelism` (vitest: run test files serially) → jobs = 1.
            "--no-file-parallelism" => o.jobs = 1,
            // Value-taking vitest flags with no effect on a native single-run runner. Consume the
            // following value token (the space form — the launcher forwards flag+value together)
            // so it isn't mistaken for a test-file path. The `--flag=value` inline form is one
            // token, absorbed by the `starts_with` catch-all below (nothing to consume).
            "--pool" | "--mode" | "--project" | "--exclude" | "--maxConcurrency" => {
                let _ = args.next();
            }
            // Boolean / no-effect vitest flags — silently accepted (NOT warned) so scripts and CI
            // passing them stay quiet.
            "--run"
            | "--watch"
            | "-w"
            | "--no-watch"
            | "--logHeapUsage"
            | "--hideSkippedTests"
            | "--no-color"
            | "--color"
            | "--dom"
            | "--segfaultRetry"
            | "--printConsoleTrace"
            | "--disableConsoleIntercept" => {}
            _ if a.starts_with("--pool=")
                || a.starts_with("--mode=")
                || a.starts_with("--project=")
                || a.starts_with("--exclude=")
                || a.starts_with("--maxConcurrency=")
                || a.starts_with("--sequence")
                || a.starts_with("--inspect")
                || a.starts_with("--browser") => {}
            // Unknown `-`/`--` token: a vitest flag turbo-test does not model. Warn + ignore —
            // NEVER treat it as a test-file path (that reached the runner as a hard load-error and
            // flipped the exit code). Test files and globs never start with `-`.
            other if other.starts_with('-') => o
                .warnings
                .push(format!("turbo-test: ignoring unsupported flag '{other}'")),
            _ => o.files.push(PathBuf::from(a)),
        }
    }
    o
}

/// Sharding: deterministic partition by index (spec §M6). An out-of-range `i/n` keeps every file.
pub fn apply_shard(files: Vec<PathBuf>, shard: Option<(usize, usize)>) -> Vec<PathBuf> {
    if let Some((i, n)) = shard {
        if n >= 1 && i >= 1 && i <= n {
            return files
                .into_iter()
                .enumerate()
                .filter(|(k, _)| k % n == i - 1)
                .map(|(_, f)| f)
                .collect();
        }
    }
    files
}

/// Where the per-file duration history lives (`<tmp>/turbo-test-cache/durations.tsv`).
pub fn durations_path() -> PathBuf {
    std::env::temp_dir()
        .join("turbo-test-cache")
        .join("durations.tsv")
}

/// Load the `path\tms` duration history (missing file / malformed lines → ignored).
pub fn load_durations(path: &Path) -> HashMap<String, f64> {
    let mut m = HashMap::new();
    if let Ok(s) = std::fs::read_to_string(path) {
        for line in s.lines() {
            if let Some((p, d)) = line.split_once('\t') {
                if let Ok(ms) = d.parse::<f64>() {
                    m.insert(p.to_string(), ms);
                }
            }
        }
    }
    m
}

/// Persist the duration history (best effort: errors are ignored).
pub fn save_durations(path: &Path, m: &HashMap<String, f64>) {
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let mut out = String::new();
    for (p, d) in m {
        out.push_str(&format!("{p}\t{d}\n"));
    }
    let _ = std::fs::write(path, out);
}

/// Duration-aware order: slowest historical first (unknown = 0 ms; ties keep argv order —
/// work-stealing handles the rest). Returns indices into `files`.
pub fn schedule_order(files: &[PathBuf], hist: &HashMap<String, f64>) -> Vec<usize> {
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by(|&a, &b| {
        let da = hist
            .get(&files[a].to_string_lossy().to_string())
            .copied()
            .unwrap_or(0.0);
        let db = hist
            .get(&files[b].to_string_lossy().to_string())
            .copied()
            .unwrap_or(0.0);
        db.partial_cmp(&da).unwrap()
    });
    order
}

/// The history to persist after a run: the old one with every file that ran updated.
pub fn update_history(
    mut hist: HashMap<String, f64>,
    files: &[PathBuf],
    res: &[FileResult],
) -> HashMap<String, f64> {
    for r in res {
        hist.insert(files[r.idx].to_string_lossy().to_string(), r.dur_ms);
    }
    hist
}

/// The test-execution hooks [`execute_file`] / [`run_files`] drive. [`Engine::v8`] is the real
/// runner; tests substitute scripted closures.
pub struct Engine<'a> {
    /// Run one file (on the worker's reused isolate when reuse is on).
    pub run: &'a (dyn Fn(&Path) -> FileOutcome + Sync),
    /// Run one file on a FRESH isolate (panic-safe; restores the worker's reuse registry).
    pub run_fresh: &'a (dyn Fn(&Path) -> FileOutcome + Sync),
    /// Whether isolate reuse is active for this run.
    pub reuse_enabled: &'a (dyn Fn() -> bool + Sync),
    /// Panic recovery: leak the dead isolate's registry handles.
    pub forget_registry: &'a (dyn Fn() + Sync),
    /// End-of-worker teardown of the persistent isolate (no-op without reuse).
    pub end_worker: &'a (dyn Fn() + Sync),
}

impl Engine<'static> {
    /// The real V8 runner.
    pub fn v8() -> Self {
        Engine {
            run: &runner::run_test_file,
            run_fresh: &runner::run_test_file_fresh,
            reuse_enabled: &runner::is_reuse_enabled,
            forget_registry: &runner::forget_registry,
            end_worker: &runner::end_worker_reuse,
        }
    }
}

/// A caught panic payload as text (`&str` / `String` payloads; anything else → `"panic"`).
pub fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

/// The shared turbo-dom .node addon stores a process-global env that a concurrent worker's DOM
/// bootstrap can transiently clobber → "document is not defined" for a whole file. A whole file
/// failing (0 passed, ≥1 failed), an explicit DOM-not-ready error, or a load error is the
/// init-race signature worth re-running.
pub fn looks_transient(r: &FileOutcome) -> bool {
    match r {
        Ok(rep) => {
            (rep.passed == 0 && rep.failed > 0)
                || rep.failures.iter().any(|f| {
                    f.contains("is not defined")
                        && (f.contains("document") || f.contains("window") || f.contains("DOM"))
                })
        }
        Err(_) => true,
    }
}

/// Rank attempts: more passes first, then fewer failures; a load error is the worst.
pub fn score(r: &FileOutcome) -> i64 {
    match r {
        Ok(rep) => (rep.passed as i64) * 1000 - (rep.failed as i64),
        Err(_) => i64::MIN,
    }
}

/// Whether the outcome warrants the fresh-isolate retry (any failure, nothing passed, or error).
pub fn has_fail(r: &FileOutcome) -> bool {
    match r {
        Ok(rep) => rep.failed > 0 || (rep.passed == 0),
        Err(_) => true,
    }
}

/// Run one file: panic-guarded, re-run up to 2 more times while the result looks transient
/// (keeping the best attempt), then — under isolate reuse, when it still fails — re-run on a fresh
/// isolate whose (best-of-3) result is authoritative.
pub fn execute_file(engine: &Engine, file: &Path) -> FileOutcome {
    // A panic in V8/loader must not abort the whole run: catch it, leak the dead isolate's
    // handles, and report this file as an error.
    let run_once =
        || match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (engine.run)(file))) {
            Ok(r) => r,
            Err(p) => {
                (engine.forget_registry)();
                Err(format!("panicked: {}", panic_message(&*p)))
            }
        };
    // Re-run (up to 3x) and KEEP THE BEST attempt — a flaky file lands a clean pass on some
    // attempt; a genuinely all-failing file stays the same (just slower). This keeps the suite
    // deterministic across runs.
    let mut r = run_once();
    let mut tries = 0;
    while looks_transient(&r) && tries < 2 {
        tries += 1;
        let r2 = run_once();
        if score(&r2) > score(&r) {
            r = r2;
        }
    }
    // Fresh-isolate retry (idea #1): under reuse, a file that still has failures may be a
    // cross-file leak ARTIFACT (it passes in a clean isolate). Re-run it on a fresh isolate — that
    // result is authoritative (fresh mode is 6189/0). This pins reuse correctness to fresh while
    // keeping reuse speed for the files that pass.
    if (engine.reuse_enabled)() && has_fail(&r) {
        let mut fr = (engine.run_fresh)(file);
        let mut ft = 0;
        while looks_transient(&fr) && ft < 2 {
            ft += 1;
            let f2 = (engine.run_fresh)(file);
            if score(&f2) > score(&fr) {
                fr = f2;
            }
        }
        // fresh is the source of truth — adopt it (even if it also fails: that's real).
        r = fr;
    }
    r
}

/// Turn one file's outcome into the reporters' [`FileResult`].
pub fn file_result(idx: usize, file: &Path, r: FileOutcome, dur_ms: f64) -> FileResult {
    match r {
        Ok(rep) => {
            let mark = if rep.ok() { "PASS" } else { "FAIL" };
            let mut line = format!(
                "{mark}  {}  ({} passed, {} failed)",
                file.display(),
                rep.passed,
                rep.failed
            );
            for f in &rep.failures {
                line.push_str(&format!("\n        ✗ {f}"));
            }
            FileResult {
                idx,
                line,
                passed: rep.passed,
                failed: rep.failed,
                load_error: false,
                error_msg: String::new(),
                tests: rep.tests,
                setup_us: rep.setup_us,
                dur_ms,
            }
        }
        Err(e) => FileResult {
            idx,
            line: format!("ERROR {}  ({e})", file.display()),
            passed: 0,
            failed: 0,
            load_error: true,
            error_msg: e,
            tests: Vec::new(),
            setup_us: 0.0,
            dur_ms,
        },
    }
}

/// The worker pool: `jobs` threads pull files in `order` from a shared cursor. Under `bail > 0`,
/// workers stop pulling NEW files once the cumulative failed-test count reaches `bail` (a worker
/// mid-file finishes that file — file-granular, see vitest.compat.md). Results sorted by index.
pub fn run_files(
    engine: &Engine,
    files: &[PathBuf],
    order: &[usize],
    jobs: usize,
    bail: usize,
) -> Vec<FileResult> {
    let cursor = AtomicUsize::new(0);
    let results = Mutex::new(Vec::<FileResult>::new());
    let failed_total = AtomicUsize::new(0);
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..jobs)
            .map(|_| {
                s.spawn(|| {
                    loop {
                        if bail > 0 && failed_total.load(Ordering::Relaxed) >= bail {
                            break;
                        }
                        let i = cursor.fetch_add(1, Ordering::Relaxed);
                        if i >= order.len() {
                            break;
                        }
                        let idx = order[i];
                        let file = &files[idx];
                        let t = Instant::now();
                        let r = execute_file(engine, file);
                        let fr = file_result(idx, file, r, t.elapsed().as_secs_f64() * 1000.0);
                        if bail > 0 {
                            failed_total.fetch_add(fr.failed as usize, Ordering::Relaxed);
                        }
                        results.lock().unwrap().push(fr);
                    }
                    // reuse path: tear down this worker's persistent isolate cleanly (no-op otherwise)
                    (engine.end_worker)();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    });
    let mut res = results.into_inner().unwrap();
    res.sort_by_key(|r| r.idx);
    res
}

/// Run-wide counts.
#[derive(Debug, Default, PartialEq)]
pub struct Totals {
    pub passed: u32,
    pub failed: u32,
    /// Files that failed to load.
    pub errors: u32,
    /// Sum / count of env-setup µs over the files that loaded.
    pub setup_sum: f64,
    pub setup_n: u32,
}

pub fn tally(res: &[FileResult]) -> Totals {
    let mut t = Totals::default();
    for r in res {
        t.passed += r.passed;
        t.failed += r.failed;
        if r.load_error {
            t.errors += 1;
        } else {
            t.setup_sum += r.setup_us;
            t.setup_n += 1;
        }
    }
    t
}

/// The human reporters' per-file stdout (default: PASS/FAIL lines; dot: one char per file + a
/// newline; verbose: PASS/FAIL line + one `✓/✗/- name (Nms)` line per test). Empty for artifacts.
pub fn render_human(reporter: Reporter, res: &[FileResult]) -> String {
    let mut out = String::new();
    for r in res {
        match reporter {
            Reporter::Default => {
                out.push_str(&r.line);
                out.push('\n');
            }
            Reporter::Dot => out.push_str(if r.load_error {
                "!"
            } else if r.failed == 0 {
                "."
            } else {
                "x"
            }),
            Reporter::Verbose => {
                out.push_str(&r.line);
                out.push('\n');
                for t in &r.tests {
                    let mark = match t.status.as_str() {
                        "passed" => "✓",
                        "failed" => "✗",
                        _ => "-",
                    };
                    out.push_str(&format!("   {mark} {} ({:.0}ms)\n", t.name, t.duration_ms));
                }
            }
            _ => {}
        }
    }
    if reporter == Reporter::Dot {
        out.push('\n');
    }
    out
}

/// XML-escape a string for JUnit attribute/text content.
pub fn xml_esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            _ => o.push(c),
        }
    }
    o
}

/// Vitest-compatible-ish JSON summary (numTotalTests/numPassedTests/...), per-file results.
pub fn render_json(files: &[PathBuf], res: &[FileResult], t: &Totals) -> String {
    let total: u32 = res.iter().map(|r| r.passed + r.failed).sum();
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut out = String::from("{");
    out.push_str(&format!("\"numTotalTestSuites\":{},", files.len()));
    out.push_str(&format!("\"numTotalTests\":{total},"));
    out.push_str(&format!("\"numPassedTests\":{},", t.passed));
    out.push_str(&format!("\"numFailedTests\":{},", t.failed));
    out.push_str(&format!("\"success\":{},", t.failed == 0 && t.errors == 0));
    out.push_str("\"testResults\":[");
    let items: Vec<String> = res
        .iter()
        .map(|r| {
            format!(
                "{{\"name\":\"{}\",\"status\":\"{}\",\"numPassingTests\":{},\"numFailingTests\":{}}}",
                esc(&files[r.idx].to_string_lossy()),
                if r.load_error {
                    "error"
                } else if r.failed == 0 {
                    "passed"
                } else {
                    "failed"
                },
                r.passed,
                r.failed
            )
        })
        .collect();
    out.push_str(&items.join(","));
    out.push_str("]}");
    out
}

/// JUnit XML: one `<testsuite>` per file, one `<testcase>` per non-skipped test.
pub fn render_junit(files: &[PathBuf], res: &[FileResult], t: &Totals) -> String {
    let total: u32 = res.iter().map(|r| r.passed + r.failed).sum();
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(&format!(
        "<testsuites name=\"turbo-test\" tests=\"{total}\" failures=\"{}\" errors=\"{}\">\n",
        t.failed, t.errors
    ));
    for r in res {
        let suite = files[r.idx].to_string_lossy().to_string();
        let secs = r.dur_ms / 1000.0;
        // A file that failed to LOAD has no per-test list: emit one synthetic failing
        // <testcase> carrying the load error, so the failure is never silently dropped.
        if r.load_error {
            out.push_str(&format!(
                "  <testsuite name=\"{}\" tests=\"1\" failures=\"1\" errors=\"1\" time=\"{:.3}\">\n",
                xml_esc(&suite),
                secs
            ));
            out.push_str(&format!(
                "    <testcase name=\"{}\" classname=\"{}\">\n      <error message=\"{}\"/>\n    </testcase>\n",
                xml_esc(&suite),
                xml_esc(&suite),
                xml_esc(&r.error_msg)
            ));
            out.push_str("  </testsuite>\n");
            continue;
        }
        out.push_str(&format!(
            "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"0\" time=\"{:.3}\">\n",
            xml_esc(&suite),
            r.passed + r.failed,
            r.failed,
            secs
        ));
        for tc in &r.tests {
            if tc.status == "skipped" {
                continue;
            }
            out.push_str(&format!(
                "    <testcase name=\"{}\" classname=\"{}\" time=\"{:.3}\"",
                xml_esc(&tc.name),
                xml_esc(&suite),
                tc.duration_ms / 1000.0
            ));
            if tc.status == "failed" {
                out.push_str(&format!(
                    ">\n      <failure message=\"{}\"/>\n    </testcase>\n",
                    xml_esc(&tc.message)
                ));
            } else {
                out.push_str("/>\n");
            }
        }
        out.push_str("  </testsuite>\n");
    }
    out.push_str("</testsuites>\n");
    out
}

/// TAP v13: a flat numbered plan over every test across all files.
pub fn render_tap(res: &[FileResult]) -> String {
    let total: usize = res.iter().map(|r| r.tests.len()).sum();
    let mut out = String::from("TAP version 13\n");
    out.push_str(&format!("1..{total}\n"));
    let mut n = 0usize;
    for r in res {
        for t in &r.tests {
            n += 1;
            let ok = if t.status == "failed" { "not ok" } else { "ok" };
            let dir = if t.status == "skipped" { " # SKIP" } else { "" };
            out.push_str(&format!("{ok} {n} - {}{dir}\n", t.name));
            if t.status == "failed" && !t.message.is_empty() {
                out.push_str("  ---\n");
                out.push_str(&format!("  message: {}\n", t.message.replace('\n', " ")));
                out.push_str("  ...\n");
            }
        }
    }
    out
}

/// The active artifact reporter's output (json/junit/tap); `None` for the human reporters.
pub fn render_artifact(
    reporter: Reporter,
    files: &[PathBuf],
    res: &[FileResult],
    t: &Totals,
) -> Option<String> {
    match reporter {
        Reporter::Json => Some(render_json(files, res, t)),
        Reporter::Junit => Some(render_junit(files, res, t)),
        Reporter::Tap => Some(render_tap(res)),
        _ => None,
    }
}

/// Deliver the artifact: to `output_file` when given (parent dirs created), else to stdout. With
/// a human reporter + `--outputFile`, the plain PASS/FAIL lines are written there so the flag is
/// never silently ignored. Returns the `(stdout, stderr)` text to print.
pub fn emit_output(
    artifact: Option<&str>,
    reporter: Reporter,
    output_file: Option<&str>,
    res: &[FileResult],
) -> (String, String) {
    let (mut stdout, mut stderr) = (String::new(), String::new());
    if let Some(text) = artifact {
        match output_file {
            Some(path) => {
                if let Some(parent) = Path::new(path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(path, text) {
                    stderr = format!("turbo-test: failed to write --outputFile '{path}': {e}\n");
                }
            }
            None => stdout = format!("{text}\n"),
        }
    } else if let (Some(path), false) = (output_file, reporter.is_artifact()) {
        let text: String = res.iter().map(|r| format!("{}\n", r.line)).collect();
        let _ = std::fs::write(path, text);
    }
    (stdout, stderr)
}

/// Wall time as vitest prints it: `1.23s` from one second up, else `456ms`.
pub fn format_duration(wall_ms: f64) -> String {
    if wall_ms >= 1000.0 {
        format!("{:.2}s", wall_ms / 1000.0)
    } else {
        format!("{:.0}ms", wall_ms)
    }
}

/// `a failed | b passed (total)`, or just `b passed (total)` when nothing failed.
pub fn count_line(failed: usize, passed: usize, total: usize) -> String {
    if failed > 0 {
        format!("{failed} failed | {passed} passed ({total})")
    } else {
        format!("{passed} passed ({total})")
    }
}

/// Vitest-style summary footer (`Test Files` / `Tests` / `Duration`). A file counts as failed if
/// any test failed OR it failed to load. Counted from actual results, not `file_count` — under
/// `--bail` some files never run, and those must NOT be reported as passed; the total stays the
/// discovered file count.
pub fn footer(file_count: usize, res: &[FileResult], t: &Totals, wall_ms: f64) -> String {
    let failed_files = res.iter().filter(|r| r.load_error || r.failed > 0).count();
    let passed_files = res
        .iter()
        .filter(|r| !r.load_error && r.failed == 0)
        .count();
    format!(
        "\n Test Files  {}\n      Tests  {}\n   Duration  {}",
        count_line(failed_files, passed_files, file_count),
        count_line(
            t.failed as usize,
            t.passed as usize,
            (t.passed + t.failed) as usize
        ),
        format_duration(wall_ms),
    )
}

/// turbo-test's diagnostic summary line (files, counts, jobs, wall, env setup, cache hit-rate).
pub fn summary(
    file_count: usize,
    t: &Totals,
    jobs: usize,
    wall_ms: f64,
    cache: (u64, u64),
) -> String {
    let avg_setup = if t.setup_n > 0 {
        t.setup_sum / t.setup_n as f64
    } else {
        0.0
    };
    let (hits, misses) = cache;
    let hit_rate = if hits + misses > 0 {
        100.0 * hits as f64 / (hits + misses) as f64
    } else {
        0.0
    };
    format!(
        "\n{} files | {} passed | {} failed | {} load-errors | {} jobs | wall {:.0} ms | env setup {:.2} ms/file | cache {:.0}% hit",
        file_count,
        t.passed,
        t.failed,
        t.errors,
        jobs,
        wall_ms,
        avg_setup / 1000.0,
        hit_rate
    )
}

/// 0 when nothing failed, nothing failed to load and the coverage gate (if any) passed; else 1.
pub fn exit_code(t: &Totals, cov_ok: bool) -> i32 {
    if t.failed == 0 && t.errors == 0 && cov_ok {
        0
    } else {
        1
    }
}

/// The whole CLI: launcher front-end (subcommand stripping, config/discovery, `--changed`, …;
/// may exit for terminal cases), then [`parse_args`] + [`run`]. Returns the exit code.
pub fn main_with_args(raw: Vec<String>) -> i32 {
    let jobs = default_jobs(std::env::var("TURBO_JOBS").ok().as_deref());
    run(parse_args(crate::launcher::prepare(raw), jobs))
}

/// Apply `opts` (env, coverage config), run every file and print the reports. Returns the exit
/// code (2 = usage: no test files).
pub fn run(opts: Options) -> i32 {
    for w in &opts.warnings {
        eprintln!("{w}");
    }
    for (k, v) in &opts.env {
        std::env::set_var(k, v);
    }
    for op in &opts.coverage {
        op.apply();
    }
    if opts.files.is_empty() {
        eprintln!("{USAGE}");
        return 2;
    }
    // --no-allowOnly: plumb to the runtime (which fails any file that collected a `.only`).
    if !opts.allow_only {
        std::env::set_var("TURBO_TEST_FORBID_ONLY", "1");
    }
    let files = apply_shard(opts.files, opts.shard);

    runner::init_v8(); // builds the framework snapshot once, before workers spawn

    let hist_path = durations_path();
    let hist = load_durations(&hist_path);
    let order = schedule_order(&files, &hist);
    let jobs = opts.jobs.min(files.len()).max(1);

    let wall = Instant::now();
    let res = run_files(&Engine::v8(), &files, &order, jobs, opts.bail);
    let wall_ms = wall.elapsed().as_secs_f64() * 1000.0;

    let t = tally(&res);
    print!("{}", render_human(opts.reporter, &res));
    save_durations(&hist_path, &update_history(hist, &files, &res));

    let artifact = render_artifact(opts.reporter, &files, &res, &t);
    let (out, err) = emit_output(
        artifact.as_deref(),
        opts.reporter,
        opts.output_file.as_deref(),
        &res,
    );
    print!("{out}");
    eprint!("{err}");

    let summary = summary(
        files.len(),
        &t,
        jobs,
        wall_ms,
        runner::transform_cache_stats(),
    );
    // Keep stdout clean for machine artifacts (json/junit/tap) printed to stdout — send the human
    // summary to stderr so it doesn't corrupt the parsed output.
    if opts.reporter.is_artifact() && opts.output_file.is_none() {
        eprintln!("{summary}");
    } else {
        // Human reporters get the vitest-style footer; the turbo diagnostic line follows it.
        println!("{}", footer(files.len(), &res, &t, wall_ms));
        println!("{summary}");
    }
    let cov_ok = if coverage::enabled() {
        coverage::report()
    } else {
        true
    };
    exit_code(&t, cov_ok)
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
