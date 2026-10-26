use super::*;

#[test]
fn bindings_are_the_top_level_names_the_factories_mention() {
    let src = "import { vi } from 'vitest'\n\
               import type { T } from './t'\n\
               import { U, type V, w } from './u'\n\
               import d, * as ns from './d'\n\
               let a = 1, { b, c: [e] } = o\n\
               var f\n\
               const g = 2\n\
               declare const h: number\n\
               function fn() {}\n\
               class K {}\n\
               enum E { X }\n\
               export const x = 1\n\
               export function xf() {}\n\
               export default function df() {}\n\
               export { a as aa }\n\
               interface I {}\n\
               const unused = 3\n\
               if (a) {}\n\
               const u: U = w\n\
               vi.mock('./m', () => ({ m: () => d + ns.y }))\n";
    let factories = "vi T U V w d ns a b e f g h fn K E x xf df I u";
    assert_eq!(
        bindings(Path::new("a.ts"), src, factories),
        [
            // `U` is only a type (no value reference); `vi` comes from the test API; `d` / `ns`
            // are read by the factory
            ("w".to_string(), false),
            ("d".to_string(), false),
            ("ns".to_string(), false),
            ("a".to_string(), true),
            ("b".to_string(), true),
            ("e".to_string(), true),
            ("f".to_string(), true),
            ("g".to_string(), false),
            ("fn".to_string(), true),
            ("K".to_string(), false),
            ("E".to_string(), false),
            ("x".to_string(), false),
            ("xf".to_string(), true),
            ("df".to_string(), true),
            ("u".to_string(), false),
        ]
    );
    // a default-exported class; anonymous defaults and expressions bind nothing
    let src = "export default class DK {}\n";
    assert_eq!(
        bindings(Path::new("a.js"), src, "DK"),
        [("DK".to_string(), false)]
    );
    for src in [
        "export default function () {}",
        "export default class {}",
        "export default 1",
    ] {
        assert!(bindings(Path::new("a.js"), src, "x").is_empty(), "{src}");
    }
}

#[test]
fn mentions_whole_identifiers_only() {
    assert!(mentions("() => a + b", "a") && mentions("x$y", "x$y"));
    assert!(!mentions("ab $a a_ a1", "a"));
    assert!(mentions("ab a", "a"));
}

#[test]
fn binder_and_scope_text() {
    let b = [("a".to_string(), true), ("K".to_string(), false)];
    assert_eq!(
        binder(&b),
        "globalThis.__ttScopeBind({ a: () => a, K: () => K }, { a: (v) => { a = v; } });"
    );
    assert_eq!(
        with_scope("f()", &b),
        "globalThis.__ttScopeNames([\"a\", \"K\"]);\nwith (globalThis.__ttScope) {\nf()\n}"
    );
}
