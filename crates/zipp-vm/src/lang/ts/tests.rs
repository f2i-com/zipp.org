//! TypeScript programs, run: each checks the output tsc's JavaScript would
//! print.

use crate::parse::ast::Goal;

/// Run TypeScript `src` (a script, or a module without imports) and return
/// its console output, one line per entry.
fn run(src: &str) -> Vec<String> {
    let ast = super::parse_auto(
        src,
        super::TsConfig {
            define_fields: true,
        },
    )
    .unwrap_or_else(|f| panic!("{}", f.message));
    let program = if ast.goal == Goal::Module {
        crate::compile::compile_main_module(&ast, src).expect("compiles")
    } else {
        crate::compile::compile_main_program(&ast, src).expect("compiles")
    };
    let mut vm = crate::vm::Vm::new(&program);
    let r = if ast.goal == Goal::Module {
        vm.run_module()
    } else {
        vm.run()
    };
    if let Err(e) = r {
        panic!("threw: {}\noutput so far: {:?}", e.0, vm.output);
    }
    std::mem::take(&mut vm.output)
}

fn out(src: &str) -> String {
    run(src).join("\n")
}

fn parse_err(src: &str) -> String {
    match super::parse_auto(
        src,
        super::TsConfig {
            define_fields: true,
        },
    ) {
        Ok(_) => panic!("expected a syntax error in {src:?}"),
        Err(f) => f.message,
    }
}

#[test]
fn annotations_are_dropped() {
    assert_eq!(
        out("let x: number = 1; const y: Array<Map<string, number[]>> = []; function f(a: string, b?: number, ...c: boolean[]): void { console.log(a, b, c.length) } f('a'); console.log(x, y.length)"),
        "a undefined 0\n1 0"
    );
}

#[test]
fn generic_calls_and_comparisons() {
    assert_eq!(
        out("function id<T>(x: T): T { return x } const a = 1, b = 2, c = 3; console.log(id<number>(5), a < b, a < b > c, id<string>`x`.length)"),
        "5 true false 1"
    );
    assert_eq!(out("const m = new Map<string, Array<Array<number>>>(); m.set('k', [[1]]); console.log(m.get('k')![0][0])"), "1");
}

#[test]
fn arrows_with_types() {
    assert_eq!(
        out("const f = (a: number, b?: string): string => `${a}${b ?? ''}`; const g = <T,>(x: T): T[] => [x]; const h = async (x: number): Promise<number> => x * 2; console.log(f(1), f(2, 'x'), g(3).length); h(4).then(v => console.log(v))"),
        "1 2x 1\n8"
    );
    assert_eq!(out("const t = true; const r = t ? (1) : 2; const k = t ? (x: number): number => x : null; console.log(r, k!(7))"), "1 7");
}

#[test]
fn assertions() {
    assert_eq!(
        out("const a: unknown = 'abc'; const n = (a as string).length; const m = <string>a; const o = { x: 1 } as const satisfies { x: number }; let v!: number; v = 2; console.log(n, m, o.x, v!)"),
        "3 abc 1 2"
    );
}

#[test]
fn erased_declarations() {
    assert_eq!(
        out("interface P { a: number; b?(): void; [k: string]: unknown } type U<T> = T extends string ? 'a' : 'b'; declare const g: number; declare function h(x: number): void; declare module 'm' { export const z: number } function f(a: string): string; function f(a: number): number; function f(a: any) { return a } console.log(f(1), typeof f)"),
        "1 function"
    );
}

#[test]
fn enums() {
    assert_eq!(
        out("enum C { R, G = 5, B, S = 'str', X = G << 2 } console.log(C.R, C.G, C.B, C[6], C.S, C.X, C[20])"),
        "0 5 6 B str 20 X"
    );
    assert_eq!(
        out("const enum D { A = 1, B = A * 3 } enum D2 { Q = D.B + 1 } console.log(D.B, D2.Q)"),
        "3 4"
    );
}

#[test]
fn namespaces() {
    assert_eq!(
        out("namespace N { export const a = 1; export function f() { return a + 1 } export let c = 0; export function inc() { c++ } const hidden = 9; export namespace In { export const z = hidden } } N.inc(); N.inc(); console.log(N.a, N.f(), N.c, N.In.z, typeof (N as any).hidden)"),
        "1 2 2 9 undefined"
    );
    assert_eq!(out("namespace A.B.C { export const v = 'deep' } namespace A { export const w = 1 } console.log(A.B.C.v, A.w)"), "deep 1");
    assert_eq!(
        out("namespace T { export interface X { a: number } } console.log(typeof T)"),
        "undefined"
    );
}

#[test]
fn classes() {
    assert_eq!(
        out("abstract class B<T> { abstract get(): T; declare x: number; constructor(protected readonly n: number) {} describe(): string { return `n=${this.n}` } } interface I { m(): number } class D extends B<number> implements I { private static count: number = 0; public y?: string; z!: number; [key: string]: any; constructor(n: number, public extra = 'e') { super(n); D.count++ } get(): number { return this.n } m(): number { return D.count } } const d = new D(4); console.log(d.get(), d.describe(), d.extra, d.m(), 'x' in d, Object.keys(d).join())"),
        "4 n=4 e 1 false n,extra,y,z"
    );
}

#[test]
fn methods_and_overloads_in_classes() {
    assert_eq!(
        out("class K { f(a: string): string; f(a: number): number; f(a: any): any { return a } static s<T>(x: T): T { return x } async *g?(): AsyncGenerator<number> { yield 1 } } console.log(new K().f(3), K.s<number>(4), typeof new K().g)"),
        "3 4 function"
    );
}

#[test]
fn parameter_properties_follow_super() {
    assert_eq!(
        out("class A { constructor() { console.log('A') } } class B extends A { constructor(public v: number) { console.log('before'); super(); console.log(this.v) } } new B(5)"),
        "before\nA\n5"
    );
}

#[test]
fn this_parameter_and_predicates() {
    assert_eq!(
        out("function f(this: { k: number }, a: number) { return this.k + a } function isStr(x: unknown): x is string { return typeof x === 'string' } function assert(c: unknown): asserts c {} console.log(f.call({ k: 1 }, 2), isStr('a'))"),
        "3 true"
    );
}

#[test]
fn complex_types() {
    assert_eq!(
        out("type M<T> = { readonly [K in keyof T as `get${Capitalize<string & K>}`]-?: () => T[K] }; type Tup = [a: string, b?: number, ...rest: boolean[]]; type F = new (...args: any[]) => object; type R<T> = T extends (...args: any) => infer U ? U : never; type S<T> = T extends [infer H extends string, ...infer R] ? H : never; type E<X, Y> = (<V>() => V extends X ? 1 : 2) extends <V>() => V extends Y ? 1 : 2 ? true : false; type L = typeof import('./x'); let q: { a: 1 | -1; b: `x${number}`; c(): void; new (): F } | null = null; let r: unique symbol; console.log(q)"),
        "null"
    );
}

#[test]
fn modules_elide_type_imports() {
    let src = "import { A, type B } from './a'; import type { C } from './c'; import D from './d'; export interface E {} export type F = number; export { E, F }; const x: A = 1 as any; export { x }; console.log('ok')";
    let ast = super::parse_auto(
        src,
        super::TsConfig {
            define_fields: true,
        },
    )
    .unwrap_or_else(|f| panic!("{}", f.message));
    assert_eq!(ast.goal, Goal::Module);
    // Only the run-time statements remain: the import of `A` is gone (used
    // only as a type), so is `D`, and the export of the types.
    let imports = ast
        .body
        .iter()
        .filter(|s| matches!(s, crate::parse::ast::Stmt::Import(_)))
        .count();
    assert_eq!(imports, 0);
}

#[test]
fn ambiguities_resolve_as_tsc_does() {
    // `a < b > (c)` is a call with type arguments when a function is there.
    assert_eq!(
        out("const f = (x: number) => x + 1; console.log(f < number > (1))"),
        "2"
    );
    // A conditional's branch is no arrow with a return type.
    assert_eq!(
        out("const a = 1, b = 2, c = 3; const r = a ? (b) : c; console.log(r)"),
        "2"
    );
    assert_eq!(out("let x = 1; const y = x! == 1; console.log(y)"), "true");
}

#[test]
fn errors_are_syntax_errors() {
    assert!(parse_err("let x: = 1").contains("SyntaxError"));
    assert!(parse_err("enum { A }").contains("SyntaxError"));
}

#[test]
fn module_detection() {
    assert!(super::looks_like_module("import x from 'y'"));
    assert!(super::looks_like_module(
        "const a = 1;\n  export const b = 2;"
    ));
    assert!(!super::looks_like_module(
        "const s = 'x'; important(); reexport = 1;"
    ));
    assert!(!super::looks_like_module("import('x')"));
}

/// [`run`] under tsc's default settings (no `tsconfig.json`): class fields
/// assigned in the constructor, not defined.
fn out_assigned(src: &str) -> String {
    let ast = super::parse_auto(src, super::TsConfig::default())
        .unwrap_or_else(|f| panic!("{}", f.message));
    let program = crate::compile::compile_main_program(&ast, src).expect("compiles");
    let mut vm = crate::vm::Vm::new(&program);
    if let Err(e) = vm.run() {
        panic!("threw: {}", e.0);
    }
    std::mem::take(&mut vm.output).join("\n")
}

#[test]
fn class_fields_follow_use_define_for_class_fields() {
    let src = "class A { x = 1; y?: number; #z = 2; static s = 3; constructor(public p: number) { console.log(Object.keys(this).join(), 'y' in this, this.#z) } } class B extends A { w = this.x + 10; } const b = new B(7); console.log(b.w, A.s)";
    // Defined (ES2022): every declared field exists, the parameter
    // property's first.
    assert_eq!(out(src), "p,x,y true 2\n11 3");
    // Assigned (tsc's default): parameter properties, then initializers, in
    // the constructor; `y` never exists; `B` gets a constructor.
    assert_eq!(out_assigned(src), "p,x false 2\n11 3");
}

#[test]
fn const_enums_are_inlined() {
    let src = "const enum K { A = 1, B = A * 4, S = 'x' } function f(K: any) { return K.A } const r = [K.A, K.B, K.S, f({ A: 9 })]; console.log(r.join())";
    assert_eq!(out(src), "1,4,x,9");
    let ast = super::parse_auto(src, super::TsConfig::default())
        .unwrap_or_else(|f| panic!("{}", f.message));
    // `K.A` in the array is the literal 1; the parameter `K` inside `f`
    // hides the enum, so `K.A` there stays a member read.
    let text = format!("{:?}", ast.body);
    assert!(text.contains("Num(4.0)"), "{text}");
}

#[test]
fn a_local_value_displaces_a_type_import() {
    // `Symbol` and `Point` are imported types; the module's own values of
    // those names are what the code means, and the imports go.
    let src = "import { Symbol, Point, used } from './types'; import './side-effect'; function Symbol(this: Symbol, n: number) { (this as any).n = n } let Point: new () => Point; namespace N { import Alias = Inner.V; export namespace Inner { export const V = 3 } export const w = Alias } console.log(used, N.w)";
    let ast = super::parse_auto(src, super::TsConfig::default()).unwrap_or_else(|f| panic!("{}", f.message));
    let imports: Vec<String> = ast
        .body
        .iter()
        .filter_map(|s| match s {
            crate::parse::ast::Stmt::Import(d) => Some(format!("{}:{}", d.source, d.specifiers.len())),
            _ => None,
        })
        .collect();
    assert_eq!(imports, ["./types:1", "./side-effect:0"]);
}

#[test]
fn json_imports_are_json_modules() {
    let src = "import data from './data.json'; import other from './x.json' with { type: 'json' }; console.log(data, other)";
    let ast = super::parse_auto(src, super::TsConfig::default()).unwrap_or_else(|f| panic!("{}", f.message));
    for s in &ast.body {
        if let crate::parse::ast::Stmt::Import(d) = s {
            assert_eq!(d.attributes.len(), 1, "{}", d.source);
            assert_eq!(d.attributes[0].value.to_lossy_string(), "json");
        }
    }
}
