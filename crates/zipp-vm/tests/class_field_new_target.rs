//! `new.target` in a class field initializer is undefined: the initializer is
//! called, never constructed (an arrow inside one inherits that). A class with
//! no explicit constructor compiles its instance initializers into the one it
//! synthesizes, whose frame carries the `new` target, and a static
//! initializer compiles inline in the enclosing scope, where `new.target` was
//! a SyntaxError at script level and the enclosing function's inside one.
//! Expected outputs are node's.

fn run_ok(src: &str) -> String {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output.join("\n")
}

#[test]
fn field_initializers_see_no_new_target() {
    let src = r#"
        "use strict";
        const out = [];
        const name = (v) => v === undefined ? "undefined" : typeof v === "function" ? "fn:" + v.name : typeof v;
        class Base { f = name(new.target); arrow = (() => name(new.target))(); }
        class Derived extends Base { g = name(new.target); }
        class Explicit { h = name(new.target); constructor() { this.ctor = name(new.target); } }
        class ExplicitDerived extends Explicit { i = name(new.target); constructor() { super(); this.dctor = name(new.target); } }
        class WithFn { inner = new (function Made() { this.nt = name(new.target); })().nt; plain = (function () { return name(new.target); })(); }
        class Stat { static s = name(new.target); static { this.blk = name(new.target); } }
        const b = new Base(), d = new Derived(), e = new Explicit(), ed = new ExplicitDerived(), w = new WithFn();
        const r = Reflect.construct(Derived, [], Explicit);
        out.push([b.f, b.arrow, d.f, d.arrow, d.g].join(" "));
        out.push([e.h, e.ctor, ed.h, ed.i, ed.ctor, ed.dctor].join(" "));
        out.push([w.inner, w.plain, Stat.s, Stat.blk, r.f, r.g].join(" "));
        console.log(out.join("\n"));
    "#;
    assert_eq!(
        run_ok(src),
        "undefined undefined undefined undefined undefined\n\
         undefined fn:Explicit undefined undefined fn:ExplicitDerived fn:ExplicitDerived\n\
         fn:Made undefined undefined undefined undefined undefined"
    );
}

#[test]
fn static_initializers_allow_new_target_and_forbid_arguments() {
    assert_eq!(
        run_ok("class S { static s = new.target; static t = (() => new.target)(); } console.log(S.s, S.t);"),
        "undefined undefined"
    );
    assert_eq!(
        run_ok("function F() { class S { static s = new.target; } return S.s === undefined; } console.log(F(), new F() instanceof F);"),
        "true true"
    );
    let err = match zipp_vm::run("function F() { class S { static s = arguments; } }") {
        Err(e) => e.to_string(),
        Ok(out) => out.error.unwrap_or_default(),
    };
    assert!(err.contains("SyntaxError"), "static `arguments` must be an early error, got {err:?}");
}
