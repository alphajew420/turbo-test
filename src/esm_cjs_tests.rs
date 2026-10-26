//! Unit tests for the native ESM→CJS emitter: every import/export form in app mode, bundler-mode
//! lowering through a `LowerCtx` resolver, the env-switch parsing, failure paths and the inline
//! source map appended under coverage.

use super::*;
use std::path::Path;

/// App-mode emit of a `.ts` file (no source map); panics if the emitter bails.
fn e(src: &str) -> String {
    emit(Path::new("t.ts"), src).expect("emit should succeed")
}

/// The part of an app-mode module after the `module.exports = …` line (requires + body).
fn after_exports(o: &str) -> &str {
    o.split("module.exports = __toCommonJS(__tt_exports);\n")
        .nth(1)
        .unwrap_or_else(|| panic!("no exports line in:\n{o}"))
}

/// Decode the inline `//# sourceMappingURL=data:…` map appended by `append_inline_map`.
fn inline_map(o: &str) -> serde_json::Value {
    use base64::Engine;
    let marker = "//# sourceMappingURL=data:application/json;base64,";
    let at = o
        .rfind(marker)
        .unwrap_or_else(|| panic!("no inline map in:\n{o}"));
    let b64 = o[at + marker.len()..].trim_end();
    let json = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap();
    serde_json::from_slice(&json).unwrap()
}

// ---- env switches ---------------------------------------------------------------------------

#[test]
fn default_on_switch_values() {
    assert!(default_on(None), "unset → on");
    assert!(!default_on(Some("")), "empty → off");
    assert!(!default_on(Some("0")), "0 → off");
    assert!(default_on(Some("1")));
    assert!(default_on(Some("false")), "anything but 0/empty is on");
    assert!(default_on(Some("00")));
}

#[test]
fn non_empty_switch_values() {
    assert!(!non_empty(None), "unset → off");
    assert!(!non_empty(Some("")), "empty → off");
    assert!(non_empty(Some("1")));
    assert!(
        non_empty(Some("0")),
        "strict is on for ANY non-empty value, even 0"
    );
}

#[test]
fn switch_wrappers_read_their_variables() {
    // Read-only: the process env is never mutated here (tests run on parallel threads).
    let var = |k: &str| std::env::var(k).ok();
    assert_eq!(enabled(), default_on(var("TURBO_NATIVE_CJS").as_deref()));
    assert_eq!(
        deps_enabled(),
        default_on(var("TURBO_NATIVE_DEPS").as_deref())
    );
    assert_eq!(
        strict(),
        non_empty(var("TURBO_NATIVE_CJS_STRICT").as_deref())
    );
}

// ---- helpers --------------------------------------------------------------------------------

#[test]
fn import_var_sanitizes_the_last_path_segment() {
    assert_eq!(import_var("react", 1), "import_react_1");
    assert_eq!(import_var("@scope/my-lib", 3), "import_my_lib_3");
    assert_eq!(import_var("./dir/", 2), "import_dir_2");
    assert_eq!(import_var("./a.b$c_d", 7), "import_a_b$c_d_7");
}

#[test]
fn quote_key_quotes_only_non_identifiers() {
    assert_eq!(quote_key("abc"), "abc");
    assert_eq!(quote_key("_$x9"), "_$x9");
    assert_eq!(quote_key(""), "\"\"");
    assert_eq!(quote_key("1a"), "\"1a\"");
    assert_eq!(quote_key("a-b"), "\"a-b\"");
    assert_eq!(quote_key("x\"y"), "\"x\\\"y\"");
}

#[test]
fn json_string_escapes_specials_and_controls() {
    assert_eq!(json_string("plain"), "\"plain\"");
    assert_eq!(
        json_string("q\"b\\n\nr\rt\tc\u{1}é"),
        "\"q\\\"b\\\\n\\nr\\rt\\tc\\u0001é\""
    );
    // round-trips through a real JSON parser
    let s = "a\"b\\c\nd\re\tf\u{1f}g";
    let back: String = serde_json::from_str(&json_string(s)).unwrap();
    assert_eq!(back, s);
}

#[test]
fn assemble_module_without_exports_has_no_export_block() {
    let out = assemble_module(
        &["var a = 1;".into()],
        &["__reExport(x);".into()],
        &[],
        "body();",
    );
    assert_eq!(
        out,
        "var __tt_exports = {};\nmodule.exports = __toCommonJS(__tt_exports);\n\
         var a = 1;\n__reExport(x);\nbody();"
    );
    let full = assemble(&[], &[], &[], "b");
    assert!(full.starts_with(PREAMBLE), "{full}");
    assert!(
        full.ends_with("module.exports = __toCommonJS(__tt_exports);\nb"),
        "{full}"
    );
}

// ---- app-mode imports -----------------------------------------------------------------------

#[test]
fn default_plus_named_import_shares_one_toesm_require() {
    let o = e("import D, { a } from \"s\"; D(a);");
    let rest = after_exports(&o);
    assert!(
        rest.starts_with("var import_s_1 = __toESM(require(\"s\"));\n"),
        "{o}"
    );
    assert!(rest.contains("import_s_1.default(import_s_1.a)"), "{o}");
}

#[test]
fn default_plus_namespace_import_binds_the_namespace_local() {
    let o = e("import D, * as ns from \"s\"; D(ns.k);");
    assert!(o.contains("var ns = __toESM(require(\"s\"));"), "{o}");
    assert!(o.contains("ns.default(ns.k)"), "{o}");
}

#[test]
fn string_named_import_specifier_reads_that_export() {
    let o = e("import { \"x\" as y } from \"s\"; y();");
    assert!(o.contains("import_s_1.x()"), "{o}");
}

#[test]
fn imports_are_numbered_in_statement_order() {
    let o = e("import a from \"one\"; import { b } from \"./two\"; import \"three\"; a(b);");
    let rest = after_exports(&o);
    assert!(
        rest.contains("var import_one_1 = __toESM(require(\"one\"));"),
        "{o}"
    );
    assert!(
        rest.contains("var import_two_2 = require(\"./two\");"),
        "{o}"
    );
    assert!(
        rest.contains("var import_three_3 = require(\"three\");"),
        "{o}"
    );
    assert!(rest.contains("import_one_1.default(import_two_2.b)"), "{o}");
}

#[test]
fn references_are_rewritten_scope_correctly() {
    let o = e("import { a } from \"s\";\n\
         function useIt() { return a + 1; }\n\
         function shadow(a) { return a; }\n\
         const b = 2; const o = { a, b };");
    assert!(
        o.contains("return import_s_1.a + 1"),
        "nested scope read is live: {o}"
    );
    assert!(
        o.contains("function shadow(a) {\n\treturn a;"),
        "a shadowing param is untouched: {o}"
    );
    assert!(
        o.contains("a: import_s_1.a"),
        "imported shorthand expands: {o}"
    );
    assert!(
        o.contains("\tb\n") || o.contains(" b }"),
        "a local shorthand stays shorthand: {o}"
    );
}

#[test]
fn dynamic_import_inside_dynamic_import_lowers_both() {
    let o = e("const m = import((await import(\"./a\")).name);");
    assert!(
        o.contains("Promise.resolve(__toESM(require((await Promise.resolve(__toESM(require(\"./a\")))).name)))"),
        "{o}"
    );
}

// ---- app-mode exports -----------------------------------------------------------------------

#[test]
fn export_class_and_let_are_registered_by_name() {
    let o = e("export class C {} export let n = 1;");
    assert!(o.contains("  C: () => C,\n"), "{o}");
    assert!(o.contains("  n: () => n,\n"), "{o}");
    assert!(after_exports(&o).contains("class C {}"), "{o}");
    assert!(after_exports(&o).contains("let n = 1"), "{o}");
}

#[test]
fn destructuring_exports_register_every_binding() {
    let o = e(
        "export const { a, b: [c, , ...d], ...e } = obj; export const [f = 1, { g }, ...h] = arr;",
    );
    for name in ["a", "c", "d", "e", "f", "g", "h"] {
        assert!(
            o.contains(&format!("  {name}: () => {name},\n")),
            "{name} missing: {o}"
        );
    }
    assert!(
        !o.contains("  b: () =>"),
        "b is a property key, not a binding: {o}"
    );
}

#[test]
fn anonymous_default_function_and_class_use_tt_default() {
    let f = e("export default function () { return 1; }");
    assert!(f.contains("default: () => __tt_default"), "{f}");
    assert!(f.contains("var __tt_default = function"), "{f}");
    assert!(!f.contains("export default"), "{f}");

    let c = e("export default class {}");
    assert!(c.contains("var __tt_default = class"), "{c}");
}

#[test]
fn named_default_class_keeps_its_binding() {
    let o = e("export default class Foo {}");
    assert!(o.contains("default: () => Foo"), "{o}");
    assert!(after_exports(&o).contains("class Foo {}"), "{o}");
    assert!(!o.contains("__tt_default"), "{o}");
}

#[test]
fn default_export_of_an_import_reads_it_live() {
    let o = e("import { a } from \"s\"; export default a;");
    assert!(o.contains("var __tt_default = import_s_1.a"), "{o}");
}

#[test]
fn local_reexport_of_an_import_points_at_the_require_var() {
    let o = e("import { a } from \"s\"; import D from \"d\"; const z = 1; export { a, D as dd, z as default };");
    assert!(o.contains("  a: () => import_s_1.a,\n"), "{o}");
    assert!(o.contains("  dd: () => import_d_2.default,\n"), "{o}");
    assert!(o.contains("  default: () => z,\n"), "{o}");
}

#[test]
fn string_export_names_are_quoted_keys() {
    let o = e("const a = 1; export { a as \"b-c\", a as 'x\"y' };");
    assert!(o.contains("  \"b-c\": () => a,\n"), "{o}");
    assert!(o.contains("  \"x\\\"y\": () => a,\n"), "{o}");
}

#[test]
fn export_star_as_namespace_is_a_named_toesm_export() {
    let o = e("export * as ns from \"s\";");
    assert!(
        o.contains("var import_s_1 = __toESM(require(\"s\"));"),
        "{o}"
    );
    assert!(o.contains("  ns: () => import_s_1,\n"), "{o}");
    assert!(!o.contains("__reExport(__tt_exports"), "{o}");
}

#[test]
fn reexport_from_with_string_names() {
    let o = e("export { \"a\" as \"b\", default as c } from \"s\";");
    assert!(o.contains("var import_s_1000001 = require(\"s\");"), "{o}");
    assert!(o.contains("  b: () => import_s_1000001.a,\n"), "{o}");
    assert!(o.contains("  c: () => import_s_1000001.default,\n"), "{o}");
}

#[test]
fn ts_only_exports_are_stripped_before_lowering() {
    let o = e("export interface I { a: number } export type T = string; export declare const d: number;\n\
         export function f(x: I): T { return String(x.a); }");
    assert!(o.contains("  f: () => f,\n"), "{o}");
    for gone in ["I:", "T:", "d:", "interface", ": number"] {
        assert!(
            !after_exports(&o).contains(gone) && !o.contains(&format!("  {gone}")),
            "{gone}: {o}"
        );
    }
}

#[test]
fn script_without_module_syntax_is_returned_unwrapped() {
    let src = "const x = require(\"y\");\nmodule.exports = x;\n";
    let o = emit(Path::new("plain.js"), src).unwrap();
    assert_eq!(
        o, src,
        "CJS input passes through (re-printed) with no preamble"
    );
    assert!(!o.contains("__toCommonJS"));
}

// ---- TS / JSX / decorators -----------------------------------------------------------------

#[test]
fn enums_lower_like_tsc_in_both_decorator_modes() {
    // Member lines are tsc 5.9.3's output for this source (`tsc --target es2022`): string
    // members get no reverse mapping, member references are folded to constants. Runs in a
    // debug build, so it would also trip oxc's `with_enum_eval` debug_assert.
    let src = "enum S { A = 'a', B = A }\n\
               enum N { A, B = A + 2, C }\n\
               const enum CE { P = 1 }\n\
               export enum X { One = 1, Two = One + 1 }\n\
               export const v = [S.B, N.C, CE.P];\n";
    for legacy in [false, true] {
        let o = emit_with(Path::new("e.ts"), src, legacy).unwrap();
        for line in [
            "S[\"A\"] = \"a\";",
            "S[\"B\"] = \"a\";",
            "N[N[\"A\"] = 0] = \"A\";",
            "N[N[\"B\"] = 2] = \"B\";",
            "N[N[\"C\"] = 3] = \"C\";",
            "X[X[\"One\"] = 1] = \"One\";",
            "X[X[\"Two\"] = 2] = \"Two\";",
        ] {
            assert!(o.contains(line), "legacy={legacy}: missing {line}:\n{o}");
        }
        assert!(!o.contains("S[S["), "legacy={legacy}: {o}");
        assert!(o.contains("  X: () => X,\n"), "legacy={legacy}: {o}");
        assert!(o.contains("  v: () => v,\n"), "legacy={legacy}: {o}");
        assert!(
            o.contains("CE[CE[\"P\"] = 1] = \"P\";"),
            "legacy={legacy}: {o}"
        );
    }
}

#[test]
fn tsx_lowers_jsx_to_the_automatic_runtime() {
    let o = emit(
        Path::new("c.tsx"),
        "export const C = (p: { n: number }) => <b>{p.n}</b>;",
    )
    .unwrap();
    assert!(o.contains("require(\"react/jsx-runtime\")"), "{o}");
    assert!(o.contains(".jsx(\"b\", { children: p.n })"), "{o}");
    assert!(o.contains("  C: () => C,\n"), "{o}");
}

#[test]
fn legacy_decorators_lower_with_metadata_only_when_asked() {
    let src = "function dec(..._a: any[]): any {}\n\
               export class A { @dec m(@dec p: string): void {} }";
    let legacy = emit_with(Path::new("d.ts"), src, true).unwrap();
    assert!(legacy.contains("babelHelpers.decorate("), "{legacy}");
    assert!(
        legacy.contains("babelHelpers.decorateParam(0, dec)"),
        "{legacy}"
    );
    assert!(
        legacy.contains("\"design:paramtypes\", [String]"),
        "{legacy}"
    );
    assert!(legacy.contains("  A: () => A,\n"), "{legacy}");

    let standard = emit_with(Path::new("d.ts"), src, false).unwrap();
    assert!(!standard.contains("babelHelpers"), "{standard}");
}

// ---- failure paths --------------------------------------------------------------------------

#[test]
fn emit_bails_on_unsupported_extension_parse_transform_and_semantic_errors() {
    assert!(
        emit(Path::new("notes.txt"), "x").is_none(),
        "unknown extension"
    );
    assert!(
        emit(Path::new("t.ts"), "const = ;").is_none(),
        "parse error"
    );
    assert!(
        emit(Path::new("t.ts"), "const a = <div/>;").is_none(),
        "JSX in .ts is a parse error"
    );
    assert!(
        emit(Path::new("t.jsx"), "const a = <a:b />;").is_none(),
        "transform error (JSX namespace)"
    );
    assert!(
        emit(Path::new("t.js"), "let a; let a;").is_none(),
        "semantic error (redeclaration)"
    );
    assert!(
        emit(Path::new("t.ts"), "let a = 1; let a = 2;").is_none(),
        "semantic error after TS strip"
    );
}

// ---- inline source map (coverage) -----------------------------------------------------------

#[test]
fn module_map_is_offset_past_the_preamble_and_prefix() {
    let o = emit_inner(
        Path::new("dir/mod.ts"),
        "import { a } from \"s\";\nexport const x: number = a;\n",
        false,
        true,
    )
    .unwrap();
    let map = inline_map(&o);
    assert_eq!(map["version"], 3);
    assert_eq!(map["sources"], serde_json::json!(["mod.ts"]));
    assert_eq!(map["sourcesContent"], serde_json::json!([null]));
    let mappings = map["mappings"].as_str().unwrap();
    let body_line = o
        .lines()
        .position(|l| l.starts_with("const x = import_s_1.a"))
        .unwrap();
    let leading = mappings.chars().take_while(|&c| c == ';').count();
    assert_eq!(
        leading, body_line,
        "first mapped line must be the body's first line\n{o}"
    );
    // without coverage, no map
    let plain = emit_inner(Path::new("dir/mod.ts"), "export const x = 1;", false, false).unwrap();
    assert!(!plain.contains("sourceMappingURL"), "{plain}");
}

#[test]
fn script_map_starts_at_line_zero_and_escapes_the_file_name() {
    let name = "we\"ird\\na\tme.ts";
    let o = emit_inner(
        Path::new(name),
        "const y: number = 1;\nmodule.exports = y;\n",
        false,
        true,
    )
    .unwrap();
    assert!(o.starts_with("const y = 1;\n"), "{o}");
    let map = inline_map(&o);
    assert_eq!(map["sources"][0], name);
    let mappings = map["mappings"].as_str().unwrap();
    assert!(
        !mappings.is_empty() && !mappings.starts_with(';'),
        "{mappings}"
    );
}

// ---- bundler mode (transform_to_parts) ------------------------------------------------------

/// `./esm*` → bundled ESM `e` (or `q` for `./esm2`), `./cjs` → bundled CJS `c`, else external.
fn bundled(spec: &str) -> SourceRef {
    match spec {
        "./esm" => SourceRef::BundledEsm { id: "e".into() },
        "./esm2" => SourceRef::BundledEsm { id: "q".into() },
        "./cjs" => SourceRef::BundledCjs { id: "c".into() },
        _ => SourceRef::External,
    }
}

fn parts(js: &str) -> ModuleParts {
    let resolve = bundled;
    let ctx = LowerCtx {
        exports_obj: "m9_exports".into(),
        resolve: Some(&resolve),
    };
    transform_to_parts(js, &ctx).expect("transform_to_parts should succeed")
}

#[test]
fn bundled_side_effect_imports() {
    let p = parts("import \"./esm\"; import \"./cjs\"; import \"bare\";");
    assert_eq!(
        p.requires,
        vec![
            "init_e();",
            "require_c();",
            "var import_bare_3 = require(\"bare\");"
        ]
    );
    assert!(p.has_module_syntax);
}

#[test]
fn bundled_esm_imports_read_the_exports_object() {
    let p = parts(
        "import E, { e1 as one } from \"./esm\"; import * as ns from \"./esm2\"; E(one, ns.k);",
    );
    assert_eq!(
        p.requires,
        vec!["init_e();", "var ns = (init_q(), q_exports);"]
    );
    assert!(
        p.body_code
            .contains("e_exports.default(e_exports.e1, ns.k)"),
        "{}",
        p.body_code
    );
}

#[test]
fn bundled_cjs_imports_use_require_id() {
    let p = parts(
        "import C, { c1 } from \"./cjs\"; import * as cns from \"./cjs\"; import { c2 } from \"./cjs\";\n\
         C(c1, cns.k, c2);",
    );
    assert_eq!(
        p.requires,
        vec![
            "var import_cjs_1 = __toESM(require_c());",
            "var cns = __toESM(require_c());",
            "var import_cjs_3 = require_c();",
        ]
    );
    assert!(
        p.body_code
            .contains("import_cjs_1.default(import_cjs_1.c1, cns.k, import_cjs_3.c2)"),
        "{}",
        p.body_code
    );
}

#[test]
fn bundled_export_star_forms() {
    let p = parts(
        "export * from \"./esm\"; export * from \"./cjs\"; export * from \"ext\";\n\
         export * as eAll from \"./esm\"; export * as cAll from \"./cjs\"; export { r1 } from \"./cjs\";",
    );
    assert_eq!(
        p.re_exports,
        vec![
            "__reExport(m9_exports, (init_e(), e_exports), module.exports);",
            "__reExport(m9_exports, require_c(), module.exports);",
            "__reExport(m9_exports, require(\"ext\"), module.exports);",
        ]
    );
    assert_eq!(
        p.requires,
        vec![
            "var import_esm_4 = (init_e(), e_exports);",
            "var import_cjs_5 = __toESM(require_c());",
            "var import_cjs_1000006 = require_c();",
        ]
    );
    let names: Vec<(&str, &str)> = p
        .exports
        .iter()
        .map(|x| (x.name.as_str(), x.value_js.as_str()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("eAll", "import_esm_4"),
            ("cAll", "import_cjs_5"),
            ("r1", "import_cjs_1000006.r1")
        ]
    );
}

#[test]
fn bundled_require_calls_are_rewritten_only_for_static_bundled_specs() {
    let p = parts(
        "const a = require(\"./esm\"); const b = require(\"./cjs\"); const c = require(\"bare\");\n\
         const d = require(dyn); const f = require(\"./cjs\", 2); const g = foo(\"./cjs\");\n\
         const h = obj.require(\"./cjs\");",
    );
    let b = &p.body_code;
    assert!(b.contains("const a = (init_e(), e_exports);"), "{b}");
    assert!(b.contains("const b = require_c();"), "{b}");
    assert!(b.contains("const c = require(\"bare\");"), "{b}");
    assert!(b.contains("const d = require(dyn);"), "{b}");
    assert!(b.contains("const f = require(\"./cjs\", 2);"), "{b}");
    assert!(b.contains("const g = foo(\"./cjs\");"), "{b}");
    assert!(b.contains("const h = obj.require(\"./cjs\");"), "{b}");
    assert!(!p.has_module_syntax, "require() alone is not ESM syntax");
}

#[test]
fn transform_to_parts_flags_default_rewrite() {
    let p = parts("export default 7;");
    assert!(p.default_needs_rewrite);
    assert!(
        p.body_code.starts_with("var __tt_default = 7"),
        "{}",
        p.body_code
    );
    assert!(p.has_module_syntax);
}

#[test]
fn transform_to_parts_in_app_mode_keeps_everything_external() {
    let ctx = LowerCtx {
        exports_obj: "__tt_exports".into(),
        resolve: None,
    };
    let p = transform_to_parts(
        "import x from \"./esm\"; const y = require(\"./cjs\"); x(y);",
        &ctx,
    )
    .unwrap();
    assert_eq!(
        p.requires,
        vec!["var import_esm_1 = __toESM(require(\"./esm\"));"]
    );
    assert!(
        p.body_code.contains("const y = require(\"./cjs\");"),
        "{}",
        p.body_code
    );
}

#[test]
fn transform_to_parts_bails_on_parse_and_semantic_errors() {
    let ctx = LowerCtx {
        exports_obj: "__tt_exports".into(),
        resolve: None,
    };
    assert!(transform_to_parts("x(", &ctx).is_none(), "parse error");
    assert!(
        transform_to_parts("let a; let a;", &ctx).is_none(),
        "semantic error"
    );
    assert!(
        transform_to_parts("const t: number = 1;", &ctx).is_none(),
        "TS is not accepted here"
    );
}

// ---- defensive arms of `lower` (unreachable through `emit`, which strips TS and bails on
// parse errors first) ----------------------------------------------------------------------------

/// Run `lower` (app mode) directly on `src` parsed as `stype`, parse errors tolerated.
fn lower_raw(src: &str, stype: SourceType) -> Option<Vec<String>> {
    let alloc = Allocator::default();
    let mut program = Parser::new(&alloc, src, stype).parse().program;
    let scoping = SemanticBuilder::new()
        .build(&program)
        .semantic
        .into_scoping();
    let ctx = LowerCtx {
        exports_obj: "__tt_exports".into(),
        resolve: None,
    };
    let (parts, _) = lower(AstBuilder::new(&alloc), scoping, &ctx, &mut program, None)?;
    Some(
        parts
            .exports
            .iter()
            .map(|e| format!("{}={}", e.name, e.value_js))
            .collect(),
    )
}

#[test]
fn lower_rejects_unstripped_typescript_exports() {
    let ts = SourceType::ts();
    assert_eq!(lower_raw("export interface I { a: number }", ts), None);
    assert_eq!(lower_raw("export type T = string;", ts), None);
    assert_eq!(
        lower_raw("export const x = 1;", ts),
        Some(vec!["x=x".to_string()])
    );
}

#[test]
fn lower_registers_a_string_local_by_name() {
    // `export { "a" as b }` without `from` is a syntax error the parser recovers from.
    let exports = lower_raw("const a = 1; export { \"a\" as b };", SourceType::mjs());
    assert_eq!(exports, Some(vec!["b=a".to_string()]));
}
