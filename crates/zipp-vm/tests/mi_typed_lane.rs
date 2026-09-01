//! W19 (MI-LANE): the METHOD-inline typed lane.
//!
//! `MethodInlinePlan` used to be consumable by exactly one emitter — the boxed
//! one — so every intermediate of every inlined class-method body went through
//! a memory home with a NaN-box tag test. This gives the mi path the same
//! register-resident lane wave 13 gave leaf splices. The only additions to the
//! closed op set are `this.<field>` (a baked absolute load) and
//! `super.m()` / `super.v` (the existing guard block, then the super body
//! scheduled inline over registers shifted above the outer body's).
//!
//! The pins below are the mechanism's non-negotiable hazards:
//!
//! * REVERSED / IMMEDIATE OPERAND FORMS. Wave 13 shipped a miscompile because
//!   `IAddImmRev` (`imm - b`, Sub's form only) was reused for Add. The mi lane
//!   adds no new template, but it feeds the existing ones operand shapes they
//!   never saw from a leaf body — a field load or a super result on either
//!   side of a constant. The `lane_parity_imm_*` cases drive all four
//!   combinations (`K - this.x`, `K + this.x`, and both with a WIDE `K` that
//!   cannot be an ALU imm32) with values chosen so borrowing the wrong
//!   template gives a DIFFERENT answer: `K - x != x - K` for every `x != K`,
//!   and each case is byte-compared against `node -e`.
//! * EFFECT ORDERING. A lane guard bail re-runs the whole call through the
//!   helper, so a body that already committed a store would double-apply it.
//!   v1 must schedule NO store-bearing body: `lane_setter_never_schedules`
//!   greps the plan log, and `lane_parity_setter_roundtrip` pins the answers.
//! * FIELD REPRESENTATION. The lane bakes an Int-or-double tag guard chosen
//!   from the slot's live value. `lane_parity_double_field` /
//!   `lane_parity_field_retype_midrun` drive both, and the retype case forces
//!   the guard to start missing mid-run.
//! * THE SUPER GUARDS SURVIVE. The block is re-emitted with rax/rcx only (the
//!   boxed copy uses rdx and r10, which ARE lane value homes), so its three
//!   checks are re-proved here: holder reassignment, `setPrototypeOf` on the
//!   chain, and an own-property shadow.
//! * MAGNITUDE / ToInt32 BAILS still fail closed (`lane_parity_wrap_bail`,
//!   `lane_parity_beyond_2p53`).
//!
//! Every `lane_parity_` expectation is byte-compared against `node -e`, and the
//! final test re-runs the whole matrix under `ZIPP_NO_MI_LANE=1`,
//! `ZIPP_NO_METHOD_INLINE=1`, `ZIPP_NO_TYPED_SPLICE=1`, `ZIPP_NOJIT=1`,
//! `ZIPP_JIT_THRESHOLD=1` and `ZIPP_GC_STRESS=1` in child processes.

//! Pins x86-64 JIT mechanisms from the engine's logs and counters, which the interpreter-only profiles never emit; compiled only where that tier exists, like the other tier-pinning suites.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH (expected values come from `node -e`)");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(|l| l.to_string())
        .collect()
}

fn assert_matches_node(src: &str) {
    let ours = run_ok(src);
    let node = node_output(src);
    assert_eq!(ours, node, "zipp != node for: {src}");
}

/// Hot enough to reach the region + method-inline + lane tiers.
const HOT: usize = 80_000;

// ───────────────────────── the row's own shape ─────────────────────────

/// The `class-prototype-hot` P1 loop in miniature: four subclasses cycling
/// through one polymorphic `objs[i & 3].area()`, each body `super.area() * K`
/// plus a constant, one of them reading a second own field and one wrapping
/// with `| 0`. Every arm schedules a lane; a single wrong value in any of them
/// changes the accumulator.
#[test]
fn lane_parity_polymorphic_area_cycle() {
    let src = format!(
        r#""use strict";
        class Shape {{
          constructor(v) {{ this._v = v | 0; }}
          area() {{ return this._v + 1; }}
          get v() {{ return this._v; }}
        }}
        class Circle extends Shape {{ area() {{ return super.area() * 3 + 1; }} }}
        class Square extends Shape {{
          constructor(v) {{ super(v); this.side = v * 2; }}
          area() {{ return super.area() * 4 + this.side; }}
        }}
        class Tri extends Shape {{
          area() {{ return super.area() * 5 + 3; }}
          get v() {{ return super.v * 2; }}
        }}
        class Hex extends Shape {{ area() {{ return (super.area() * 6 + 5) | 0; }} }}
        var objs = [new Circle(11), new Square(22), new Tri(33), new Hex(44)];
        var acc = 0, gacc = 0;
        for (var i = 0; i < {HOT}; i++) {{
          acc = (acc + objs[i & 3].area()) | 0;
          gacc = (gacc + objs[i & 3].v) | 0;
        }}
        console.log("acc:" + acc + " gacc:" + gacc);
        console.log(objs[0].area() + "," + objs[1].area() + "," +
                    objs[2].area() + "," + objs[3].area());
        "#
    );
    assert_matches_node(&src);
}

// ─────────────── the wave-13 immediate/reversed-operand hazard ───────────────

/// `K - this.x` is the ONE immediate-lhs template the lane owns
/// (`IAddImmRev` = `imm - b`). Borrowing it for Add — or computing `x - K` —
/// changes every line: the values are picked so `K - x`, `x - K`, `K + x` and
/// `x + K` are four distinct numbers.
#[test]
fn lane_parity_imm_lhs_sub_narrow() {
    let src = format!(
        r#""use strict";
        class A {{ constructor(x) {{ this.x = x | 0; }} }}
        class B extends A {{ f() {{ return 1000 - this.x; }} }}
        var o = new B(7), acc = 0;
        for (var i = 0; i < {HOT}; i++) acc = (acc + o.f()) | 0;
        console.log("sub:" + acc + " one:" + o.f());
        "#
    );
    assert_matches_node(&src);
}

/// The commutative direction with the SAME operand placement: `K + this.x`
/// must NOT reach `IAddImmRev`. Distinct answer from the Sub case above.
#[test]
fn lane_parity_imm_lhs_add_narrow() {
    let src = format!(
        r#""use strict";
        class A {{ constructor(x) {{ this.x = x | 0; }} }}
        class B extends A {{ f() {{ return 1000 + this.x; }} }}
        var o = new B(7), acc = 0;
        for (var i = 0; i < {HOT}; i++) acc = (acc + o.f()) | 0;
        console.log("add:" + acc + " one:" + o.f());
        "#
    );
    assert_matches_node(&src);
}

/// A WIDE immediate on the left — outside i32, so it cannot be spelled as an
/// ALU imm32 and must be materialized. Sub keeps `imm - b`; Add must go
/// reg-reg. Both directions, with a super result as the other operand so the
/// value reaching the template comes through the mi additions.
#[test]
fn lane_parity_imm_lhs_wide() {
    let src = format!(
        r#""use strict";
        class A {{ constructor(x) {{ this._v = x | 0; }} area() {{ return this._v + 1; }} }}
        class S extends A {{ sub() {{ return 5000000000 - super.area(); }} }}
        class D extends A {{ add() {{ return 5000000000 + super.area(); }} }}
        var s = new S(123456), d = new D(123456);
        var a = 0, b = 0;
        for (var i = 0; i < {HOT}; i++) {{ a += s.sub(); b += d.add(); }}
        console.log("wide:" + a + ":" + b + " one:" + s.sub() + ":" + d.add());
        "#
    );
    assert_matches_node(&src);
}

/// The EXACT-INTEGER wide-immediate frontier, in a method body.
///
/// A `5000000000` LITERAL is a double, so it routes through `fbin` and never
/// reaches the integer templates at all. The wide *`Av::ImmI`* only arises
/// from a FOLD of two in-range integers — which is precisely the shape wave
/// 13's escaped miscompile had — so every constant here is folded inside the
/// body, and each is then placed on both sides of both ops against a field
/// load. `c - this.x` is the one that reaches `IAddImmRev` (`imm - b`);
/// `c + this.x` must materialize and go reg-reg instead. Borrowing the
/// reversed template for Add flips the sign of every `a*` line.
fn mi_wide_imm_matrix(cases: &[(&str, &str)], iters: u32) -> String {
    let mut methods = String::new();
    let mut names: Vec<String> = Vec::new();
    for (name, prelude) in cases {
        for (suffix, body) in [
            ("al", "return c + this.x;"), // wide imm on the LEFT of Add
            ("ar", "return this.x + c;"), // wide imm on the RIGHT of Add
            ("sl", "return c - this.x;"), // wide imm on the LEFT of Sub (IAddImmRev)
            ("sr", "return this.x - c;"), // wide imm on the RIGHT of Sub
        ] {
            let f = format!("{name}_{suffix}");
            methods.push_str(&format!("          {f}() {{ {prelude} {body} }}\n"));
            names.push(f);
        }
    }
    let calls = names
        .iter()
        .map(|f| format!("o.{f}()"))
        .collect::<Vec<_>>()
        .join(" + ");
    let probes = names
        .iter()
        .map(|f| format!("probes.push(o.{f}());"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#""use strict";
        class K {{
{methods}        }}
        var o = new K();
        var acc = 0;
        for (var i = 0; i < {iters}; i++) {{ o.x = i & 1023; acc += {calls}; }}
        var probes = [];
        for (var j = 0; j < 3; j++) {{ o.x = j; {probes} }}
        console.log("acc:" + acc);
        console.log(probes.join(","));
        "#
    )
}

/// Wave 13's escaped shape, now against a field load: two in-range integers
/// folded to 4000000000 (past i32), on both sides of both ops.
#[test]
fn lane_parity_imm_wide_fold_matrix() {
    assert_matches_node(&mi_wide_imm_matrix(
        &[
            ("w4e9", "var k = 2000000000, m = 2000000000; var c = k + m;"),
            // 2^31-1: the largest immediate that still fits imm32.
            ("i32max", "var k = 2147483646, m = 1; var c = k + m;"),
            // 2^31: one past — the first wide positive.
            ("w2p31", "var k = 2147483647, m = 1; var c = k + m;"),
            // -2^31-1: one past the most negative imm32, the other sign.
            (
                "wm2p31",
                "var z = 0; var k = z - 2147483647, m = z - 2; var c = k + m;",
            ),
        ],
        60000,
    ));
}

/// Wide immediate on the RIGHT of both ops, for the same reason.
#[test]
fn lane_parity_imm_rhs_wide() {
    let src = format!(
        r#""use strict";
        class A {{ constructor(x) {{ this._v = x | 0; }} area() {{ return this._v + 1; }} }}
        class S extends A {{ sub() {{ return super.area() - 5000000000; }} }}
        class D extends A {{ add() {{ return super.area() + 5000000000; }} }}
        var s = new S(-98765), d = new D(-98765);
        var a = 0, b = 0;
        for (var i = 0; i < {HOT}; i++) {{ a += s.sub(); b += d.add(); }}
        console.log("wide:" + a + ":" + b + " one:" + s.sub() + ":" + d.add());
        "#
    );
    assert_matches_node(&src);
}

// ───────────────────────── field representation ─────────────────────────

/// The slot holds a DOUBLE, so the lane bakes the non-Int tag guard. A body
/// that guarded Int here would fall back on every call (correct but null); one
/// that skipped the guard would read a double's raw bits as an integer.
#[test]
fn lane_parity_double_field() {
    let src = format!(
        r#""use strict";
        class A {{ constructor(x) {{ this.x = x; }} base() {{ return this.x * 2; }} }}
        class B extends A {{ f() {{ return super.base() + 0.25; }} }}
        var o = new B(1.5), acc = 0;
        for (var i = 0; i < {HOT}; i++) acc += o.f();
        console.log("dbl:" + acc + " one:" + o.f());
        "#
    );
    assert_matches_node(&src);
}

/// The slot is re-typed mid-run: Int at compile time, a double afterwards, and
/// finally a string. The baked tag guard must start MISSING — a lane that kept
/// sign-extending would read a double's bits as an integer.
#[test]
fn lane_parity_field_retype_midrun() {
    let src = format!(
        r#""use strict";
        class A {{ constructor(x) {{ this.x = x; }} }}
        class B extends A {{ f() {{ return this.x * 3 + 1; }} }}
        var o = new B(7), acc = 0;
        for (var i = 0; i < {HOT}; i++) acc += o.f();
        console.log("int:" + acc);
        o.x = 7.5;
        var acc2 = 0;
        for (var i = 0; i < {HOT}; i++) acc2 += o.f();
        console.log("retyped:" + acc2 + " one:" + o.f());
        o.x = "nope";
        console.log("string:" + o.f());
        "#
    );
    assert_matches_node(&src);
}

// ───────────────────────── magnitude / ToInt32 bails ─────────────────────────

/// A product that leaves i32 must take the MODULAR ToInt32 wrap through the
/// fallback, never a silent in-lane truncation — the emit_misc.rs:331 comment's
/// own case, driven with `this._v` climbing past 2^31 / K.
#[test]
fn lane_parity_wrap_bail() {
    let src = format!(
        r#""use strict";
        class A {{ constructor(v) {{ this._v = v | 0; }} area() {{ return this._v + 1; }} }}
        class B extends A {{ area() {{ return (super.area() * 40503) | 0; }} }}
        var o = new B(1), acc = 0;
        for (var i = 0; i < {HOT}; i++) {{
          acc = (acc + o.area()) | 0;
          o._v = (o._v + 977) | 0;
        }}
        console.log("wrap:" + acc + " v:" + o._v + " one:" + o.area());
        "#
    );
    assert_matches_node(&src);
}

/// An unwrapped product that grows past 2^53: the exact-integer invariant must
/// not be assumed. The answer is the f64 one in every tier.
#[test]
fn lane_parity_beyond_2p53() {
    let src = format!(
        r#""use strict";
        class A {{ constructor(v) {{ this._v = v; }} area() {{ return this._v + 1; }} }}
        class B extends A {{ area() {{ return super.area() * 1048576 + 3; }} }}
        var o = new B(9007199254740000), acc = 0;
        for (var i = 0; i < {HOT}; i++) acc += o.area();
        console.log("big:" + acc + " one:" + o.area());
        "#
    );
    assert_matches_node(&src);
}

/// IEEE op order is preserved bytecode-op-for-op across the super boundary:
/// `(super.q() / 10) * n` double-rounds, and the fused form does not.
#[test]
fn lane_parity_ieee_order_across_super() {
    let src = format!(
        r#""use strict";
        class A {{ constructor(u) {{ this.u = u; }} q() {{ return this.u / 10; }} }}
        class B extends A {{ f() {{ return super.q() * 7; }} }}
        var acc = 0;
        var os = [new B(1), new B(3), new B(7), new B(9)];
        for (var i = 0; i < {HOT}; i++) acc += os[i & 3].f();
        console.log("ieee:" + acc + " one:" + os[0].f());
        "#
    );
    assert_matches_node(&src);
}

// ───────────────────────── the super guards survive ─────────────────────────

/// Holder-slot reassignment: `A.prototype.area = fn` overwrites the slot in
/// place WITHOUT bumping a version, so only the lane's holder re-read catches
/// it. A dropped re-read keeps returning the pre-mutation answer forever.
#[test]
fn lane_parity_super_holder_reassignment() {
    let src = format!(
        r#""use strict";
        class A {{ constructor(v) {{ this._v = v | 0; }} area() {{ return this._v + 1; }} }}
        class B extends A {{ area() {{ return super.area() * 3 + 1; }} }}
        var o = new B(11), acc = 0;
        for (var i = 0; i < {HOT}; i++) acc = (acc + o.area()) | 0;
        console.log("before:" + acc + " " + o.area());
        A.prototype.area = function () {{ return this._v + 100; }};
        var acc2 = 0;
        for (var i = 0; i < {HOT}; i++) acc2 = (acc2 + o.area()) | 0;
        console.log("after:" + o.area() + " " + acc2);
        "#
    );
    assert_matches_node(&src);
}

/// THE REGISTER-DISCIPLINE PIN, and the hazard unique to this mechanism.
///
/// `emit_mi_body`'s copy of the super guard block uses rdx and r10 as scratch.
/// Both are lane VALUE homes (`LANE_GPR_HOMES = [r8, r9, r10, r11, rdx]`), so
/// re-emitting that block verbatim would silently destroy any intermediate
/// still live across the guard — and the row's own bodies never expose it,
/// because in `super.area() * K + C` the super call is the FIRST value op and
/// nothing is live yet.
///
/// This body keeps THREE integers live across the super call, which the
/// allocator hands r8/r9/r10 in `LANE_GPR_HOMES` order — so a guard written
/// the boxed way destroys the r10 one. Pinned by mutation: re-introducing
/// `mov edx, [r13+…]` / `mov r10, fn_bits` into the lane's `SuperGuard` makes
/// exactly this test fail and leaves the rest of the file green.
///
/// Three is the DEEPEST this can go: `method_inline_body_ok` caps a body at 16
/// registers and the compiler spends ~4 per `var`, so this body sits at exactly
/// 16 and a fourth live value declines the inline outright. If that cap is ever
/// relaxed (the lane needs no scratch window, so it is a natural follow-on),
/// rdx — `LANE_GPR_HOMES[4]` — becomes reachable too, and this is the test to
/// extend.
#[test]
fn lane_parity_live_values_across_super_guard() {
    let src = format!(
        r#""use strict";
        class A {{
          constructor(v) {{
            this._v = v | 0;
            this.p = (v + 1) | 0; this.q = (v + 2) | 0; this.r = (v + 3) | 0;
          }}
          area() {{ return this._v + 1; }}
        }}
        class B extends A {{
          f() {{
            var a = this.p, b = this.q, c = this.r;
            return super.area() + a + b + c;
          }}
        }}
        var o = new B(11), acc = 0;
        for (var i = 0; i < {HOT}; i++) acc = (acc + o.f()) | 0;
        console.log("live:" + acc + " one:" + o.f());
        "#
    );
    assert_matches_node(&src);
}

/// `setPrototypeOf` on the super chain must be caught by the hop version
/// compares the lane re-emits (with rax/rcx instead of rdx/r10).
#[test]
fn lane_parity_super_set_prototype_of() {
    let src = format!(
        r#""use strict";
        class A {{ constructor(v) {{ this._v = v | 0; }} area() {{ return this._v + 1; }} }}
        class B extends A {{ area() {{ return super.area() * 3 + 1; }} }}
        var o = new B(11), acc = 0;
        for (var i = 0; i < {HOT}; i++) acc = (acc + o.area()) | 0;
        console.log("hot:" + acc + " " + o.area());
        Object.setPrototypeOf(B.prototype, {{ area() {{ return this._v + 1000; }} }});
        console.log("retargeted:" + o.area());
        "#
    );
    assert_matches_node(&src);
}

/// An own-property SHADOW of the method name bumps the receiver version, so
/// the arm's identity+version guard (emitted before the lane) must miss.
#[test]
fn lane_parity_own_shadow() {
    let src = format!(
        r#""use strict";
        class A {{ constructor(v) {{ this._v = v | 0; }} area() {{ return this._v + 1; }} }}
        class B extends A {{ area() {{ return super.area() * 3 + 1; }} }}
        var o = new B(11), acc = 0;
        for (var i = 0; i < {HOT}; i++) acc = (acc + o.area()) | 0;
        console.log("hot:" + acc);
        o.area = function () {{ return -5; }};
        console.log("shadowed:" + o.area());
        "#
    );
    assert_matches_node(&src);
}

/// A parameter reaches the lane from the CALLER's argument slot (`ParamLoad`),
/// not from a copied window slot. A non-int argument must miss the entry tag
/// guard and re-run through the helper.
#[test]
fn lane_parity_method_params() {
    let src = format!(
        r#""use strict";
        class A {{ constructor(v) {{ this._v = v | 0; }} scale(k) {{ return this._v * k + 1; }} }}
        var o = new A(11), acc = 0;
        for (var i = 0; i < {HOT}; i++) acc = (acc + o.scale(i & 7)) | 0;
        console.log("param:" + acc + " one:" + o.scale(3) + " dbl:" + o.scale(1.5) +
                    " str:" + o.scale("2") + " none:" + o.scale());
        "#
    );
    assert_matches_node(&src);
}

/// A setter round-trip: v1 schedules NO lane for a store-bearing body, so
/// these run the boxed path — but they must keep answering identically while
/// the getter beside them IS laned.
#[test]
fn lane_parity_setter_roundtrip() {
    let src = format!(
        r#""use strict";
        class Shape {{
          constructor(v) {{ this._v = v | 0; }}
          get v() {{ return this._v; }}
          set v(x) {{ this._v = x | 0; }}
        }}
        class Tri extends Shape {{
          get v() {{ return super.v * 2; }}
          set v(x) {{ super.v = x; }}
        }}
        var objs = [new Shape(1), new Tri(2)];
        var gacc = 0;
        for (var i = 0; i < {HOT}; i++) {{
          var o = objs[i & 1];
          o.v = (i & 1023) - 7;
          gacc = (gacc + o.v) | 0;
        }}
        console.log("rt:" + gacc + " " + objs[0].v + "," + objs[1].v);
        "#
    );
    assert_matches_node(&src);
}

// ───────────────────────── engagement + off-switch ─────────────────────────

fn jitlog_stderr(filter: &str, env: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(filter)
        .arg("--exact")
        .arg("--nocapture")
        .env("ZIPP_JITLOG", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn the test binary");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "{filter} child failed:\n{stderr}\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    stderr
}

/// The parity assertions above are only testing the LANE if the lane was
/// actually scheduled. Each of these shapes must produce an `mi ... LANE` line.
#[test]
fn lane_engages_on_the_hazard_shapes() {
    for filter in [
        "lane_parity_polymorphic_area_cycle",
        "lane_parity_imm_lhs_sub_narrow",
        "lane_parity_imm_lhs_add_narrow",
        "lane_parity_imm_lhs_wide",
        "lane_parity_imm_rhs_wide",
        "lane_parity_imm_wide_fold_matrix",
        "lane_parity_double_field",
        "lane_parity_method_params",
        "lane_parity_live_values_across_super_guard",
    ] {
        let stderr = jitlog_stderr(filter, &[]);
        assert!(
            stderr.contains("LANE (ops="),
            "{filter}: no arm scheduled a method-inline lane, so the parity \
             assertion is not testing this mechanism:\n{stderr}"
        );
    }
}

/// A bare pass-through getter (`return this._v`) has no boxed intermediate to
/// remove and no super window to skip, so a lane there is a tag guard, a
/// sign-extend and a re-box in place of one move — measured +1.2% [-2.2, +3.1]
/// on a 24M-iteration micro, and it newly punts to the IC probe whenever the
/// field oscillates Int/double. It must DECLINE, while the `super.v` override
/// beside it (whose sub-window bind and zero-fill the lane does delete,
/// measured -4.3%) must still schedule.
#[test]
fn lane_parity_pass_through_and_super_getters() {
    let src = format!(
        r#""use strict";
        class Shape {{ constructor(v) {{ this._v = v | 0; }} get v() {{ return this._v; }} }}
        class Hex extends Shape {{ get v() {{ return super.v; }} }}
        var objs = [new Shape(3), new Hex(4)];
        var g = 0;
        for (var i = 0; i < {HOT}; i++) g = (g + objs[i & 1].v) | 0;
        console.log("pt:" + g);
        "#
    );
    assert_matches_node(&src);
}

/// The gate itself: the bare getter declines, the `super.v` one beside it does
/// not. Spawned as a CHILD of the parity test above (never of itself — a
/// self-filtered `jitlog_stderr` would recurse forever).
#[test]
fn lane_declines_a_bare_pass_through_getter() {
    let stderr = jitlog_stderr("lane_parity_pass_through_and_super_getters", &[]);
    assert!(
        stderr.contains("lane=DECLINED(mi-nothing-to-unbox)"),
        "the bare pass-through getter still scheduled a lane:\n{stderr}"
    );
    assert!(
        stderr.contains("getter LANE (ops="),
        "the `super.v` getter stopped scheduling, so the gate is too wide:\n{stderr}"
    );
}

/// v1 EFFECT-FREE gate: a setter body ends in a store, and a lane guard bail
/// re-runs the whole call — so no setter arm may ever schedule one.
#[test]
fn lane_setter_never_schedules() {
    let stderr = jitlog_stderr("lane_parity_setter_roundtrip", &[]);
    assert!(
        stderr.contains("INLINE setter"),
        "the setter site did not inline at all, so this gate is vacuous:\n{stderr}"
    );
    assert!(
        !stderr.contains("setter LANE ("),
        "a SETTER arm scheduled a lane — a mid-lane bail would re-run the call \
         and double-apply its store:\n{stderr}"
    );
}

/// Off-switch: with `ZIPP_NO_MI_LANE=1` no arm may schedule a lane (the boxed
/// `emit_mi_body` emission is byte-identical to pre-wave builds), while the
/// method inline itself must still engage.
#[test]
fn lane_off_switch_never_plans() {
    let stderr = jitlog_stderr(
        "lane_parity_polymorphic_area_cycle",
        &[("ZIPP_NO_MI_LANE", "1"), ("ZIPP_JIT_THRESHOLD", "1")],
    );
    assert!(
        !stderr.contains("LANE ("),
        "off-switch still planned mi lanes:\n{stderr}"
    );
    assert!(
        stderr.contains("INLINE method"),
        "the off-switch also disabled the method inline itself:\n{stderr}"
    );
}

/// Re-run the whole `lane_parity_` matrix with the lane off, the mi inline off,
/// the leaf lane off, the JIT off, the JIT forced hot, and under GC stress —
/// each in its own child process (env latches are read once per process).
#[test]
fn lane_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_MI_LANE", "1"),
        ("ZIPP_NO_METHOD_INLINE", "1"),
        ("ZIPP_NO_TYPED_SPLICE", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("lane_parity_")
            .env(key, val)
            .output()
            .expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{key}={val} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the lane_parity_ filter matched nothing under {key}={val}:\n{stdout}"
        );
    }
}
