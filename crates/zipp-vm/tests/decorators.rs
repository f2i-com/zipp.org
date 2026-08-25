//! proposal-decorators: the ORDERING contract, pinned in the tree.
//!
//! Every assertion here is an ordering or routing rule that nothing else in the
//! repo guards: test262 ships only SYNTAX tests for decorators (34 executions,
//! all of which pass without a single one of these rules being right), so a
//! wrong answer in the decoration order is invisible to the suite.
//!
//! The expected strings are not guesses. Each was read off
//! ClassDefinitionEvaluation / ApplyDecoratorsToElementDefinition /
//! InitializeFieldOrAccessor in tc39/ecma262#2417 and then confirmed byte for
//! byte against BOTH implementations written to that text — Babel's
//! `@babel/plugin-proposal-decorators` at `version: "2023-11"` and TypeScript
//! 5.9's `__esDecorate`/`__runInitializers` emit. node/V8 implements no
//! decorators at any flag, so it cannot arbitrate any of this.

use zipp_vm::run;

fn out(src: &str) -> String {
    let o = run(src).expect("compile");
    assert!(
        o.error.is_none(),
        "unexpected throw: {:?}\nsrc:\n{src}",
        o.error
    );
    o.output.join("\n")
}

fn thrown(src: &str) -> String {
    let o = run(src).expect("compile");
    o.error
        .unwrap_or_else(|| format!("did not throw; output {:?}", o.output))
}

/// Decorator EXPRESSIONS evaluate in source order interleaved with computed
/// keys; a class's own list evaluates before the heritage, because
/// `ClassDeclaration : DecoratorList class BindingIdentifier ClassTail`
/// evaluates the list before ClassTail (where `extends` lives).
#[test]
fn decorator_expressions_evaluate_in_source_order_before_the_heritage() {
    let src = r#"
      const L = [];
      function d(t) { L.push("eval " + t); return (v, c) => v; }
      function k(t, key) { L.push("key " + t); return key; }
      @d("C1") @d("C2")
      class C extends (L.push("heritage"), Object) {
        @d("m") [k("mk", "m")]() {}
        @d("f") [k("fk", "f")] = 1;
      }
      console.log(L.join(","));
    "#;
    assert_eq!(
        out(src),
        "eval C1,eval C2,heritage,eval m,key mk,eval f,key fk"
    );
}

/// Elements are DECORATED in four groups — static non-fields, instance
/// non-fields, static fields, instance fields — document order within a group.
/// ClassDefinitionEvaluation runs four separate loops; a flat document-order
/// pass is a different observable sequence the moment a class mixes kinds.
#[test]
fn elements_decorate_in_four_groups_not_document_order() {
    let src = r#"
      const L = [];
      function d(t) { return (v, c) => { L.push(t); return v; }; }
      class C {
        @d("if1") if1 = 1;
        @d("m1") m1() {}
        @d("sf1") static sf1 = 1;
        @d("sm1") static sm1() {}
        @d("g1") get g1() { return 1; }
        @d("acc1") accessor acc1 = 1;
        @d("if2") if2 = 2;
        @d("sacc") static accessor sacc = 1;
        @d("s1") set s1(v) {}
        @d("sf2") static sf2 = 2;
      }
      console.log(L.join(","));
    "#;
    assert_eq!(out(src), "sm1,sacc,m1,g1,acc1,s1,sf1,sf2,if1,if2");
}

/// One element's own list applies INNERMOST FIRST: DecoratorListEvaluation
/// prepends, so `@a @b m(){}` calls `b` and hands its result to `a`.
#[test]
fn one_elements_decorators_apply_innermost_first() {
    let src = r#"
      const L = [];
      function d(t) { return (v, c) => { L.push(t); return v; }; }
      class C { @d("a") @d("b") @d("c") m() {} }
      console.log(L.join(","));
    "#;
    assert_eq!(out(src), "c,b,a");
}

/// A field decorator's returned initializer is PREPENDED to
/// `[[Initializers]]`, so the chain runs OUTERMOST first: `@a @b @c f = V`
/// yields `c(b(a(V)))`, not `a(b(c(V)))`. The two compose in opposite
/// directions and only one of them is the spec's.
#[test]
fn field_initializer_chains_run_outermost_first() {
    let src = r#"
      function w(t) { return (v, c) => (x) => t + "(" + x + ")"; }
      class C {
        @w("a") @w("b") @w("c") f = "V";
        @w("sa") @w("sb") static sf = "SV";
      }
      console.log(new C().f, C.sf);
    "#;
    assert_eq!(out(src), "c(b(a(V))) sb(sa(SV))");
}

/// An auto-accessor decorator's returned `init` joins the same chain, with the
/// same prepend rule.
#[test]
fn accessor_init_joins_the_same_chain() {
    let src = r#"
      function d(t) {
        return (v, c) => ({ get: v.get, set: v.set, init: (x) => t + "(" + x + ")" });
      }
      class C { @d("p") @d("q") accessor x = "0"; }
      console.log(new C().x);
    "#;
    assert_eq!(out(src), "q(p(0))");
}

/// `addInitializer` routing is per KIND, and it is the difference between
/// "before any field" and "right after this field":
///   * method/getter/setter -> the shared instance or static list,
///   * field/accessor       -> that ELEMENT's own list, run by
///     InitializeFieldOrAccessor once the element is defined.
/// A class decorator's callbacks run last of all.
#[test]
fn extra_initializers_are_per_kind_and_per_element() {
    let src = r#"
      const L = [];
      function ai(t) {
        return (v, c) => { c.addInitializer(function () { L.push(t + ":sf=" + this.sf + ":f2=" + this.f2); }); return v; };
      }
      function cls(v, c) { c.addInitializer(function () { L.push("class:sf2=" + this.sf2); }); return v; }
      @cls
      class C {
        @ai("instMethod") m() {}
        f = (L.push("field f"), 1);
        @ai("staticMethod") static sm() {}
        static sf = (L.push("field sf"), 2);
        @ai("instField") f2 = (L.push("field f2"), 3);
        @ai("staticField") static sf2 = (L.push("field sf2"), 4);
      }
      L.push("--new--");
      new C();
      console.log(L.join(" | "));
    "#;
    assert_eq!(
        out(src),
        "staticMethod:sf=undefined:f2=undefined | field sf | field sf2 \
         | staticField:sf=2:f2=undefined | class:sf2=4 | --new-- \
         | instMethod:sf=undefined:f2=undefined | field f | field f2 \
         | instField:sf=undefined:f2=3"
    );
}

/// Instance fields initialize in SOURCE order. Named and computed keys used to
/// be emitted as two back-to-back loops, so `[a] = 1; b = 2` ran `b` first —
/// which is wrong with or without decorators (node disagrees on plain ES too).
#[test]
fn instance_fields_initialize_in_source_order() {
    let src = r#"
      const L = []; const p = (s) => (L.push(s), s); const ka = "ka", kb = "kb";
      class A { [ka] = p("A.ka"); a = p("A.a"); [kb] = p("A.kb"); #x = p("A.#x"); b = p("A.b"); }
      class B extends Object { [ka] = p("B.ka"); a = p("B.a"); [kb] = p("B.kb"); constructor() { super(); } }
      class C { [ka] = p("C.ka"); accessor acc = p("C.acc"); [kb] = p("C.kb"); }
      new A(); new B(); new C();
      console.log(L.join(","));
    "#;
    assert_eq!(
        out(src),
        "A.ka,A.a,A.kb,A.#x,A.b,B.ka,B.a,B.kb,C.ka,C.acc,C.kb"
    );
}

/// A class decorator REPLACES the class, and `classEnv.InitializeBinding` binds
/// the class's own inner name to the replacement — before the static elements
/// run, so a static block sees it too.
#[test]
fn a_class_decorator_replacement_is_the_inner_binding_too() {
    let src = r#"
      function replace(v, c) { return class Replacement extends v {}; }
      @replace class R { static who() { return R.name; } static { this.tag = R.name; } }
      console.log(R.name, R.who(), R.tag);
    "#;
    assert_eq!(out(src), "Replacement Replacement Replacement");
}

/// `context.name` is a String or a Symbol, never anything else — including for
/// a computed key that CONSTANT-FOLDS at compile time (`["m"]`, `[1+1]`,
/// `[Symbol.iterator]`), which takes the static-name path through the compiler
/// and so never sees the key-recording op.
#[test]
fn context_name_of_a_folded_computed_key() {
    let src = r#"
      const L = [];
      function d(v, c) { L.push(c.kind + ":" + typeof c.name + ":" + String(c.name)); return v; }
      class C {
        @d ["litField"] = 1;
        @d ["litMethod"]() {}
        @d get ["litGet"]() { return 1; }
        @d accessor ["litAcc"] = 1;
        @d [1 + 1] = 3;
        @d [Symbol.iterator]() {}
      }
      console.log(L.join(" | "));
    "#;
    assert_eq!(
        out(src),
        "method:string:litMethod | getter:string:litGet | accessor:string:litAcc \
         | method:symbol:Symbol(Symbol.iterator) | field:string:litField | field:string:2"
    );
}

/// …and a decorator on such an element must actually REPLACE it. Marking the
/// element "computed" while emitting no key op made the runtime look the member
/// up under the literal key "undefined": the decorator ran, returned a
/// replacement, and the replacement went nowhere.
#[test]
fn a_folded_computed_key_element_is_really_replaced() {
    let src = r#"
      function rep(v, c) { return function () { return "REPLACED"; }; }
      class B { @rep ["litM"]() { return "orig"; } @rep [Symbol.iterator]() { return "orig"; } }
      console.log(new B().litM(), new B()[Symbol.iterator]());
    "#;
    assert_eq!(out(src), "REPLACED REPLACED");
}

/// `decorationState.[[Finished]]` is created fresh per decorator CALL and set
/// the instant that decorator returns — so a context object stashed by one
/// decorator is already closed when the next one runs, not merely when the
/// class is done.
#[test]
fn add_initializer_is_closed_per_decorator_call() {
    let src = r#"
      let ctx;
      function d1(v, c) { ctx = c; return v; }
      function d2(v, c) { ctx.addInitializer(() => {}); return v; }
      class X { @d1 m() {} @d2 n() {} }
    "#;
    assert!(thrown(src).contains("TypeError"), "{}", thrown(src));
    let after = r#"
      let ctx;
      function d(v, c) { ctx = c; return v; }
      class X { @d m() {} }
      ctx.addInitializer(() => {});
    "#;
    assert!(thrown(after).contains("TypeError"), "{}", thrown(after));
}

/// An accessor decorator's `{get, set, init}` are each "callable, or undefined,
/// or TypeError". Accepting a non-callable silently is the same class of bug as
/// a decorator that quietly does nothing.
#[test]
fn accessor_decorator_result_is_validated() {
    for bad in ["{ get: 5 }", "{ set: 5 }", "{ init: 5 }", "5"] {
        let src = format!("function d(v, c) {{ return {bad}; }} class X {{ @d accessor a = 1; }}");
        assert!(
            thrown(&src).contains("TypeError"),
            "{bad}: {}",
            thrown(&src)
        );
    }
    // An Object with none of the three is a no-op, not an error — and a
    // function IS an Object (`If newValue is an Object`), which is where both
    // transpilers diverge from the spec by testing `typeof x === "object"`.
    let ok = r#"
      function d(v, c) { return function () {}; }
      class X { @d accessor a = 7; }
      console.log(new X().a);
    "#;
    assert_eq!(out(ok), "7");
}

/// `@a.b` keeps the Reference it evaluated, so the decorator is called as a
/// METHOD of its base. `@(a.b)` covers a MemberExpression and behaves the same;
/// a bare `@a` and a `@a.b()` call have no reference and get `undefined`.
#[test]
fn a_member_expression_decorator_is_called_on_its_base() {
    let src = r#"
      "use strict";
      const L = [];
      const holder = { tag: "H", d(v, c) { L.push(this === undefined ? "undefined" : this.tag); return v; } };
      const plain = function (v, c) { "use strict"; L.push(this === undefined ? "undefined" : "?"); return v; };
      class C { @holder.d a() {} @(holder.d) b() {} @plain c() {} }
      console.log(L.join(","));
    "#;
    assert_eq!(out(src), "H,H,undefined");
}

/// Each EVALUATION of a `class` calls its decorators afresh, and an instance
/// must run its own evaluation's initializers — even when a later evaluation of
/// the same source has since replaced the compile-time class slot.
#[test]
fn decorator_state_is_per_class_evaluation() {
    let src = r#"
      function mk(n) {
        function add(v, c) { return (x) => x + n; }
        return class { @add f = 0; };
      }
      const A = mk(1), B = mk(100);
      console.log(new B().f, new A().f);
    "#;
    assert_eq!(out(src), "100 1");
}

/// A decorated instance METHOD forces a constructor into existence where the
/// class had none. A derived class's implicit constructor must still be
/// `constructor(...args) { super(...args) }`.
#[test]
fn forcing_a_constructor_keeps_implicit_derived_semantics() {
    let src = r#"
      function id(v, c) { return v; }
      class B { constructor(x) { this.x = x; } }
      class D extends B { @id m() {} }
      class E extends B { @id accessor a = 1; }
      console.log(new D(5).x, D.length, new E(7).x);
    "#;
    assert_eq!(out(src), "5 0 7");
}

/// The Decorator grammar is the restricted three-shape production, not
/// LeftHandSideExpression. Accepting more would turn a SyntaxError into a
/// runtime TypeError at best and a silently mis-parsed program at worst.
#[test]
fn the_decorator_grammar_stays_restricted() {
    for src in [
        "@a[b] class C {}",
        "@a(1).b class C {}",
        "@a(1)(2) class C {}",
        "@a`t` class C {}",
        "@a?.b class C {}",
        "class C { static @a m() {} }",
        "class C { @a constructor() {} }",
        "class C { @a static {} }",
        "@a function f() {}",
        "@a let x = 1;",
    ] {
        assert!(run(src).is_err(), "should not parse: {src}");
    }
    for src in [
        "@a class C {}",
        "@a.b.c class C {}",
        "@(a) class C {}",
        "@(a, b) class C {}",
        "@a.b(1, 2) class C {}",
        "class C { @a static accessor #p = 1; }",
        "class C { @a [k] = 1; }",
        "void (@a class {});",
    ] {
        assert!(run(src).is_ok(), "should parse: {src}");
    }
}
