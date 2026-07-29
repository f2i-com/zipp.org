//! Differential tests for inlining a method body that uses `super`.
//!
//! The native region method-inliner (`build_method_shape` /
//! `build_accessor_shape` → `emit_mi_body`) expands a trivial `super.m()` /
//! `super.x` / `super.x = v` behind a BAKED plan: the class epoch, one version
//! guard per super-chain hop, and a re-read of the holder slot. The
//! `SuperBase` capture the compiler plants ahead of each of those ops has no
//! inlined consumer and is dropped.
//!
//! Every case here loops well past the region/Tier thresholds so the inline
//! actually installs, then MUTATES the thing one of those guards watches and
//! checks the observed value follows the mutation. A guard that failed to fire
//! would keep returning the pre-mutation answer.
//!
//! `super_ordering_*` pin the reason `SuperBase` exists at all: GetSuperBase
//! runs at MakeSuperPropertyReference time, BEFORE the argument list or the
//! assignment's RHS — so a prototype swap performed *by* an argument must not
//! be seen by the call it is an argument to.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Hot enough to reach the region + method-inline tiers.
const HOT: usize = 60_000;

fn hot(body: &str) -> String {
    format!("var HOT = {HOT};\n{body}")
}

#[test]
fn super_method_inline_matches_interpreter_and_follows_reassignment() {
    // `super.area()` inlines; then the PARENT's method is replaced on the
    // prototype. The holder-slot re-read must observe the new function.
    let out = run_ok(&hot(
        r#"
        class A { constructor(v) { this._v = v | 0; } area() { return this._v + 1; } }
        class B extends A { area() { return super.area() * 3 + 1; } }
        var o = new B(11);
        var acc = 0;
        for (var i = 0; i < HOT; i++) acc = (acc + o.area()) | 0;
        console.log("before", acc, o.area());
        A.prototype.area = function () { return this._v + 100; };
        var acc2 = 0;
        for (var i = 0; i < HOT; i++) acc2 = (acc2 + o.area()) | 0;
        console.log("after", o.area(), acc2);
        "#,
    ));
    // (11+1)*3+1 = 37 ; (11+100)*3+1 = 334. Both accumulators are HOT * the
    // per-call value — node agrees byte for byte.
    assert_eq!(out[0], format!("before {} 37", 37 * HOT));
    assert_eq!(out[1], format!("after 334 {}", 334 * HOT));
}

#[test]
fn super_method_inline_follows_set_prototype_of() {
    let out = run_ok(&hot(
        r#"
        class A { constructor(v) { this._v = v | 0; } area() { return this._v + 1; } }
        class B extends A { area() { return super.area() * 3 + 1; } }
        var o = new B(11);
        var acc = 0;
        for (var i = 0; i < HOT; i++) acc = (acc + o.area()) | 0;
        console.log("hot", o.area());
        // Re-target the super chain: B.prototype's [[Prototype]] is no longer
        // A.prototype, so `super.area` resolves somewhere else entirely.
        Object.setPrototypeOf(B.prototype, { area() { return this._v + 1000; } });
        console.log("retargeted", o.area());
        "#,
    ));
    assert_eq!(out[0], "hot 37");
    assert_eq!(out[1], "retargeted 3034"); // (11+1000)*3+1
}

#[test]
fn super_method_inline_declines_on_own_shadow() {
    // An own property shadowing the class method must win, both before the
    // inline installs and after.
    let out = run_ok(&hot(
        r#"
        class A { constructor(v) { this._v = v | 0; } area() { return this._v + 1; } }
        class B extends A { area() { return super.area() * 3 + 1; } }
        var o = new B(11);
        var acc = 0;
        for (var i = 0; i < HOT; i++) acc = (acc + o.area()) | 0;
        o.area = function () { return -7; };
        var seen = 0;
        for (var i = 0; i < HOT; i++) seen = o.area();
        console.log("shadowed", seen, acc);
        "#,
    ));
    assert_eq!(out[0], "shadowed -7 2220000");
}

#[test]
fn super_accessor_inline_getter_and_setter_round_trip() {
    // The `get v(){ return super.v * 2 }` / `set v(x){ super.v = x }` pair —
    // the SuperGet read and the SuperSet store, the latter being the inlined
    // body's only committing effect.
    let out = run_ok(&hot(
        r#"
        class A {
          constructor(v) { this._v = v | 0; }
          get v() { return this._v; }
          set v(x) { this._v = x | 0; }
        }
        class T extends A {
          get v() { return super.v * 2; }
          set v(x) { super.v = x; }
        }
        var o = new T(33);
        var g = 0;
        for (var i = 0; i < HOT; i++) { o.v = (i & 1023) - 7; g = (g + o.v) | 0; }
        console.log("roundtrip", g, o.v, o._v);
        "#,
    ));
    // Interpreted control: gacc accumulates 2*((i&1023)-7).
    let mut expect: i64 = 0;
    let mut last = 0i64;
    for i in 0..HOT as i64 {
        last = ((i & 1023) - 7) * 2;
        expect = ((expect + last) as i32) as i64;
    }
    assert_eq!(out[0], format!("roundtrip {expect} {last} {}", last / 2));
}

#[test]
fn super_setter_inline_follows_parent_setter_swap() {
    // `defineProperty` can swap the setter half in place with no version bump
    // and no realloc, so only the baked setter-value compare catches it.
    let out = run_ok(&hot(
        r#"
        class A {
          constructor(v) { this._v = v | 0; }
          get v() { return this._v; }
          set v(x) { this._v = x | 0; }
        }
        class T extends A { set v(x) { super.v = x; } get v() { return this._v; } }
        var o = new T(0);
        for (var i = 0; i < HOT; i++) o.v = 5;
        console.log("hot", o._v);
        Object.defineProperty(A.prototype, "v", {
          get() { return this._v; },
          set(x) { this._v = (x | 0) + 1000; },
          configurable: true,
        });
        o.v = 5;
        console.log("swapped", o._v);
        "#,
    ));
    assert_eq!(out[0], "hot 5");
    assert_eq!(out[1], "swapped 1005");
}

#[test]
fn super_method_inline_class_redefinition_is_tier_consistent() {
    // Re-executing a class declaration swaps `class_values[home_class_id]` to a
    // NEW class with a new prototype chain, without touching the prototypes the
    // hop guards watch. The baked plan's class-epoch guard is what catches that,
    // and this pins it: whatever the engine answers, the inlined tier must answer
    // the same as the interpreter.
    //
    // KNOWN DEVIATION (pre-existing, unrelated to inlining — `ZIPP_NOJIT=1` and
    // the pre-inline build give the same answer): zipp resolves `super` through
    // the ONE `class_values` slot a `class_id` owns, so re-running the
    // declaration retargets the FIRST instance's super chain too. This prints
    // "epoch 184 184"; V8 prints "epoch 37 184" because each evaluation of the
    // declaration makes a distinct class. Fixing it means giving `super`
    // resolution a per-closure home object rather than a per-class-id slot —
    // a semantics change, not a codegen one.
    let out = run_ok(&hot(
        r#"
        function make(k) {
          class A { constructor(v) { this._v = v | 0; } area() { return this._v + k; } }
          class B extends A { area() { return super.area() * 3 + 1; } }
          return new B(11);
        }
        var a = make(1), acc = 0;
        for (var i = 0; i < HOT; i++) acc = (acc + a.area()) | 0;
        var b = make(50), acc2 = 0;
        for (var i = 0; i < HOT; i++) acc2 = (acc2 + b.area()) | 0;
        console.log("epoch", a.area(), b.area());
        "#,
    ));
    assert_eq!(out[0], "epoch 184 184");
}

#[test]
fn super_method_inline_polymorphic_receivers() {
    // Four receivers cycling through one site — the shape the benchmark uses.
    // Two arms use super, two do not; every arm must give its own answer.
    let out = run_ok(&hot(
        r#"
        class A { constructor(v) { this._v = v | 0; } area() { return this._v + 1; } }
        class C extends A { area() { return super.area() * 3 + 1; } }
        class S extends A { constructor(v) { super(v); this.side = v * 2; }
                            area() { return super.area() * 4 + this.side; } }
        class P extends A { area() { return this._v + 7; } }
        class I extends A { }
        var objs = [new C(11), new S(22), new P(33), new I(44)];
        var acc = 0;
        for (var i = 0; i < HOT; i++) acc = (acc + objs[i & 3].area()) | 0;
        console.log("poly", acc, objs[0].area(), objs[1].area(), objs[2].area(), objs[3].area());
        "#,
    ));
    // C:(11+1)*3+1=37  S:(22+1)*4+44=136  P:33+7=40  I:44+1=45
    let per: i64 = 37 + 136 + 40 + 45;
    assert_eq!(out[0], format!("poly {} 37 136 40 45", per * (HOT as i64 / 4)));
}

#[test]
fn super_method_inline_throwing_target_propagates_once() {
    // A super target that throws must throw exactly once and be catchable —
    // the inlined body must not commit-then-re-run it.
    let out = run_ok(&hot(
        r#"
        class A { constructor(v) { this._v = v | 0; } area() { return this._v + 1; } }
        class B extends A { area() { return super.area() * 3 + 1; } }
        var o = new B(11), acc = 0;
        for (var i = 0; i < HOT; i++) acc = (acc + o.area()) | 0;
        var calls = 0;
        A.prototype.area = function () { calls++; throw new Error("boom"); };
        var caught = 0;
        try { o.area(); } catch (e) { caught = e.message; }
        console.log("throw", caught, calls);
        "#,
    ));
    assert_eq!(out[0], "throw boom 1");
}

#[test]
fn super_ordering_argument_list_swap_is_tier_consistent() {
    // Why `SuperBase` exists: GetSuperBase runs at MakeSuperPropertyReference
    // time, BEFORE ArgumentListEvaluation. This pins that an argument which
    // re-targets the chain produces the SAME answer in the inlined tier as in the
    // interpreter — the point of the op being dropped rather than mis-emitted.
    //
    // KNOWN DEVIATION (pre-existing; `ZIPP_NOJIT=1` and the pre-inline build
    // agree, so inlining is not involved): this prints "ordered LATER2 LATER3"
    // where V8 prints "ordered A2 LATER3". Per 13.3.6.1 the reference's
    // GetValue — the `super.m` lookup itself, not just the base — happens before
    // the arguments run, so the swapped prototype should not be visible to the
    // call it was swapped by. zipp captures only the BASE up front and resolves
    // the method after the argument list, so it sees the swap. Closing it means
    // resolving the method at `SuperBase` time and carrying the callee, not the
    // base — a separate correctness change.
    let out = run_ok(&hot(
        r#"
        class A { m(x) { return "A" + x; } }
        class B extends A {
          hit(swap) { return super.m(swap()); }
        }
        var o = new B();
        var later = { m(x) { return "LATER" + x; } };
        var n = 0;
        for (var i = 0; i < HOT; i++) n = (n + super_len(o)) | 0;
        function super_len(t) { return t.hit(function () { return 1; }).length; }
        console.log("warm", o.hit(function () { return 1; }));
        var seen = o.hit(function () { Object.setPrototypeOf(B.prototype, later); return 2; });
        console.log("ordered", seen, o.hit(function () { return 3; }));
        "#,
    ));
    assert_eq!(out[0], "warm A1");
    assert_eq!(out[1], "ordered LATER2 LATER3");
}

#[test]
fn super_ordering_assignment_rhs_swap_matches_node() {
    let out = run_ok(&hot(
        r#"
        var log = [];
        class A { set p(v) { log.push("A:" + v); } }
        class B extends A { set q(v) { super.p = v; } }
        var o = new B();
        for (var i = 0; i < HOT; i++) o.q = 1;
        console.log("warm", log.length, log[0]);
        var later = { set p(v) { log.push("LATER:" + v); } };
        // The RHS re-targets the chain. PutValue does the property lookup AFTER
        // the RHS is evaluated, so this store DOES land on the new setter — the
        // inlined setter's baked hop-version guard has to miss and fall back.
        // Node agrees byte for byte.
        o.q = (Object.setPrototypeOf(B.prototype, later), 2);
        o.q = 3;
        console.log("ordered", log[log.length - 2], log[log.length - 1]);
        "#,
    ));
    assert_eq!(out[0], format!("warm {HOT} A:1"));
    assert_eq!(out[1], "ordered LATER:2 LATER:3");
}
