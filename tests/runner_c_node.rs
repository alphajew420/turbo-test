//! Node built-in module shims through the real CLI: the `fs` shim's append semantics under
//! parallel workers.
#![cfg(unix)]

mod common;
use common::{turbo_test, Project};

#[test]
fn append_file_sync_from_parallel_workers_keeps_every_line() {
    // 4 files appending 200 lines each to ONE log from 4 workers: a read-modify-write shim
    // loses most of them; a real O_APPEND append keeps all 800.
    let p = Project::new("rc-append");
    p.file("package.json", r#"{"name":"rc-append","type":"module"}"#);
    let log = p.path("log.txt");
    let mut files = Vec::new();
    for n in 1..=4 {
        let name = format!("f{n}.test.js");
        p.file(
            &name,
            &format!(
                "import fs from 'node:fs';\ntest('append {n}', () => {{\n  for (let i = 0; i < 200; i++) fs.appendFileSync({log:?}, 'file{n}-line' + i + '\\n');\n}});\n"
            ),
        );
        files.push(name);
    }
    let args: Vec<&str> = files.iter().map(String::as_str).collect();
    let r = turbo_test(&p.dir, &args, &[("TURBO_JOBS", "4")]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 800, "{text}");
    for n in 1..=4 {
        let prefix = format!("file{n}-line");
        let mine: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| l.starts_with(&prefix))
            .collect();
        let want: Vec<String> = (0..200).map(|i| format!("{prefix}{i}")).collect();
        assert_eq!(mine, want, "file{n}'s lines, in order");
    }
}

#[test]
fn append_file_variants_extend_and_report_errors() {
    let p = Project::new("rc-append-variants");
    p.file(
        "package.json",
        r#"{"name":"rc-append-variants","type":"module"}"#,
    );
    p.file(
        "a.test.js",
        r#"import fs from 'node:fs';
test('appendFileSync and promises.appendFile', async () => {
  const root = globalThis.__cwd;
  const f = root + '/log.txt';
  fs.appendFileSync(f, 'a');
  fs.appendFileSync(f, 'b');
  await fs.promises.appendFile(f, 'c\n');
  expect(fs.readFileSync(f, 'utf8')).toBe('abc\n');
  expect(() => fs.appendFileSync(root + '/package.json/x', 'a')).toThrow('appendFileSync ' + root + '/package.json/x');
  await expect(fs.promises.appendFile(root + '/package.json/x', 'a')).rejects.toThrow('appendFileSync ' + root + '/package.json/x');
});
"#,
    );
    let r = turbo_test(&p.dir, &["a.test.js"], &[]);
    assert_eq!(r.code, Some(0), "{}", r.all());
    assert_eq!(std::fs::read_to_string(p.path("log.txt")).unwrap(), "abc\n");
}
