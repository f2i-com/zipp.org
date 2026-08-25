//! W20 BOXREF — the register tier learns to hold a BOXED HEAP VALUE and to run
//! an inline-cache probe, so `o = arr[i]; … o.p …` stops demoting the whole loop
//! to the memory tier.
//!
//! WHAT IS NEW, AND WHY IT NEEDS ITS OWN FILE. Two arms in `regalloc.rs` reach
//! places nothing on that tier had ever reached:
//!
//!   * the dense-Array `GetIndex` arm now admits an array of OBJECTS. Its dst is
//!     a `RegionPlan::box_regs` member — no xmm home at all; the element's Value
//!     bits go straight into the interpreter frame slot, which stays
//!     authoritative on every exit;
//!   * a `GetProp` arm probes the 8-way inline cache in the region BODY. That
//!     probe is bespoke (`emit_regalloc_ic_probe`), not the memory tier's
//!     `emit_ic_probe`, for one reason: the memory tier's clobbers r8..r11, and
//!     on this tier those four registers are `BOOL_GPRS`, the planner's file for
//!     `Bool` homes. Using it here would be the W14/W16 defect class again —
//!     three silent wrong answers so far. And on a MISS it CALLS
//!     `jit_get_prop_miss`, which clobbers the whole volatile file (rax/rcx/rdx,
//!     r8..r11, xmm0..xmm5): homes 2..5 and every bool live there, so they are
//!     spilled around the call.
//!
//! Both arms change SPEC-VISIBLE answers. A HOLE resolves through the PROTOTYPE
//! CHAIN rather than reading the dense Vec; an accessor frame-calls a getter,
//! which this tier cannot do; a dict-mode receiver has no shape; a
//! `setPrototypeOf` mid-loop changes the answer under a cached way. So every
//! expectation here comes from `node -e` — never from `ZIPP_NOJIT=1`, which
//! would pass an emitter bug that the interpreter shares.
//!
//! THE AXES.
//!   * [`boxref_parity_receivers`] — receiver counts across the 8-way cliff
//!     (1/2/8/9/64), each of the two receiver forms, own data and proto chain.
//!   * [`boxref_parity_bool_homes_on_a_probe_hit`] / `_across_the_miss_call` —
//!     BOXREF probe. This is the axis that catches a register-contract slip: the
//!     one to FOUR live JS booleans across a probe that HITS and across one
//!     that MISSES. The bool bump allocator hands out `BOOL_GPRS` in order, so k
//!     live bools occupy exactly r8..r(7+k); the miss row is what catches a
//!     dropped spill, because a probe miss runs a win64 call over all four.
//!   * [`boxref_receiver_version_guard_retires_a_stale_way`] — the guard a
//!     mutation run showed NOTHING else here catches; see its own doc.
//!   * [`boxref_a_discarded_read_still_runs_its_getter`] — the defect this
//!     mechanism actually shipped: dead-code elimination on a side-effecting op.
//!   * [`boxref_parity_semantics`] — holes, out-of-range/negative/fractional
//!     keys, accessors (own, inherited, side-effecting), Proxy, dict mode,
//!     frozen/sealed, `arguments`, an `arr_props` overlay, `setPrototypeOf` and
//!     shadowing mid-loop, a receiver reassigned mid-loop, and non-numeric
//!     property values (the dst tag guard).
//!   * [`boxref_all_modes_answer_identically`] re-runs the parity tests in child
//!     processes under each latch — the switches are memoized, so a mode IS a
//!     process.
//!   * [`boxref_mechanism_engages`] / [`boxref_mechanism_off_switch_declines`]
//!     read the tier back out of a child's `ZIPP_JITLOG`, so an admission change
//!     that quietly drops these kernels to the memory tier fails the suite
//!     instead of making it vacuous.

use std::process::Command;

// ── oracles ─────────────────────────────────────────────────────────────────

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
    let out = Command::new("node")
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
    assert_eq!(run_ok(src), node_output(src), "zipp != node for:\n{src}");
}

/// Enough back-edges to compile and then run compiled for a long while.
const N: u32 = 40_000;

// ── 1. receiver counts, both receiver forms ─────────────────────────────────

/// The element form (`o = arr[k]; o.p`) at 1, 2, 8, 9 and 64 receivers. Eight is
/// the last way; nine is the first count past it, where every access misses and
/// the arm has to fall back through the helper on every iteration without ever
/// answering wrong or evicting itself into the interpreter.
#[test]
fn boxref_parity_receivers() {
    for n in [1usize, 2, 8, 9, 64] {
        let src = format!(
            "var a=[];for(var i=0;i<{n};i++){{var o={{}};o.pad=i;o.p=i;a.push(o);}}\n\
             var s=0,k=0;for(var i=0;i<{N};i++){{s=(s+a[k].p)|0;k++;if(k==={n})k=0;}}\n\
             console.log(s);"
        );
        assert_matches_node(&src);
    }
}

/// The proto-chain form at every depth the way format can cache (1..=5 hops)
/// and one past it, where the miss helper answers uncached forever.
#[test]
fn boxref_parity_chain_depths() {
    for depth in 0usize..=7 {
        let src = format!(
            "var b={{q:7}};var c=b;for(var d=0;d<{depth};d++)c=Object.create(c);\n\
             var a=[Object.create(c)];a[0].own=1;\n\
             var s=0;for(var i=0;i<{N};i++){{s=(s+a[0].q)|0;}}console.log(s);"
        );
        assert_matches_node(&src);
    }
}

/// The second receiver form: a global the region never stores, read directly.
/// This is the arm `ZIPP_NO_REGALLOC_GETPROP=1` turns off.
#[test]
fn boxref_parity_global_receiver() {
    let src = format!(
        "var g={{v:6,w:2}};var s=0;for(var i=0;i<{N};i++){{s=(s+g.v+g.w)|0;}}console.log(s);"
    );
    assert_matches_node(&src);
    let src = format!(
        "function f(o){{var s=0;for(var i=0;i<{N};i++){{s=(s+o.v)|0;}}return s;}}\n\
         console.log(f({{v:3}}));"
    );
    assert_matches_node(&src);
}

// ── 2. the BOOL_GPRS sweep ──────────────────────────────────────────────────

/// `k` live JS booleans spanning a BOXREF probe, in the shape
/// [`bool_home_clobber`] established: each bool is DEFINED by a comparison
/// before the body op and READ only AFTER the loop, so a clobber has to survive
/// the rest of the iteration and the backedge to be observable. `typeof` is
/// printed beside each because the two faces of this defect differ — a corrupted
/// home reads back as the wrong boolean in one and as a `Number` (NaN) in the
/// other.
///
/// `mask` picks the axis that matters here. With 7 (eight receivers) the probe
/// HITS and only its own register contract is under test. With 15 (sixteen
/// receivers, past the 8 ways) every access MISSES, so the win64 miss helper
/// runs — over all four `BOOL_GPRS` and both volatile xmm home halves — and this
/// is what fails if the spill around that call is wrong.
fn bool_sweep_kernel(k: usize, nrecv: usize, mask: usize) -> String {
    const DEFS: [&str; 4] = [
        "b0 = i >= 4;",
        "b1 = i < 4;",
        "b2 = t > 2.5;",
        "b3 = t < 2.5;",
    ];
    let init: String = (0..k).map(|j| format!("var b{j} = false;\n  ")).collect();
    let defs: String = DEFS[..k].iter().map(|d| format!("    {d}\n")).collect();
    let out: String = (0..k)
        .map(|j| format!(r#" + " " + (typeof b{j}) + ":" + b{j}"#))
        .collect();
    format!(
        r#"var A = [];
for (var q = 0; q < {nrecv}; q++) {{ var o = {{}}; o.pad = q; o.p = q * 3; A.push(o); }}
function kernel(n) {{
  var h = 1.5, i = 0, t = 0;
  {init}for (i = 0; i < n; i++) {{
{defs}    t = A[i & {mask}].p;
    h = h * 0.5 + t;
  }}
  return "" + h{out};
}}
var s = "";
for (var r = 0; r < 3; r++) s += "|" + kernel(900);
console.log(s);
"#
    )
}

/// The probe-HIT half of the sweep: eight receivers, so every way is filled.
#[test]
fn boxref_parity_bool_homes_on_a_probe_hit() {
    for k in 1..=4 {
        assert_matches_node(&bool_sweep_kernel(k, 8, 7));
    }
}

/// The probe-MISS half: sixteen receivers past eight ways, so the win64 miss
/// helper runs on every access, over every volatile home.
#[test]
fn boxref_parity_bool_homes_across_the_miss_call() {
    for k in 1..=4 {
        assert_matches_node(&bool_sweep_kernel(k, 16, 15));
    }
}

/// The same, on the GLOBAL-receiver arm — a region with no pin at all, hence a
/// different frame layout for the spill and probe scratch.
#[test]
fn boxref_parity_bool_homes_global_receiver() {
    for k in 1..=4 {
        const DEFS: [&str; 4] = [
            "b0 = i >= 4;",
            "b1 = i < 4;",
            "b2 = t > 2.5;",
            "b3 = t < 2.5;",
        ];
        let init: String = (0..k).map(|j| format!("var b{j} = false;\n  ")).collect();
        let defs: String = DEFS[..k].iter().map(|d| format!("    {d}\n")).collect();
        let out: String = (0..k)
            .map(|j| format!(r#" + " " + (typeof b{j}) + ":" + b{j}"#))
            .collect();
        let src = format!(
            r#"var G = {{ p: 5 }};
function kernel(n) {{
  var h = 1.5, i = 0, t = 0;
  {init}for (i = 0; i < n; i++) {{
{defs}    t = G.p;
    h = h * 0.5 + t;
  }}
  return "" + h{out};
}}
var s = "";
for (var r = 0; r < 3; r++) s += "|" + kernel(900);
console.log(s);
"#
        );
        assert_matches_node(&src);
    }
}

// ── 2b. the RECEIVER-VERSION guard ──────────────────────────────────────────

/// A way is `(obj_bits, version)` keyed. Identity alone is ABA-blind and says
/// nothing about LAYOUT, so the version compare is what makes a cached
/// `(vals_ptr, slot)` still mean what it meant.
///
/// Every case here mutates the receiver from OUTSIDE the hot region — an inner
/// loop is the compiled region, an outer loop does the mutation — because a
/// `SetProp` or a `Call` INSIDE the region declines it to the memory tier and
/// the case would be vacuous. That is the whole reason this test exists as its
/// own function: the obvious in-loop mutations do not reach the arm, and a
/// mutation run of the emitter (version guard deleted) proved the receiver-count
/// and semantics tests above do not catch it.
#[test]
fn boxref_receiver_version_guard_retires_a_stale_way() {
    const OUTER: u32 = 400;
    const INNER: u32 = 300;
    let half = OUTER / 2;
    let cases = [
        // SHADOWING a chain hit: the way points at the holder's slot, and only
        // the RECEIVER's version changes — a hop-only guard misses it.
        format!("var a=[Object.create({{v:3}})];var s=0;\n\
                 for(var o=0;o<{OUTER};o++){{for(var i=0;i<{INNER};i++)s=(s+a[0].v)|0;\n\
                 if(o==={half})a[0].v=7;}}console.log(s);"),
        // The holder's `vals` Vec REALLOCATES: a cached `vals_ptr` then dangles.
        format!("var a=[{{v:11}}];var s=0;\n\
                 for(var o=0;o<{OUTER};o++){{for(var i=0;i<{INNER};i++)s=(s+a[0].v)|0;\n\
                 if(o==={half}){{for(var j=0;j<64;j++)a[0]['x'+j]=j;}}}}console.log(s);"),
        // A DELETE ahead of the cached slot: dictionary mode, and the property
        // is no longer at the cached index.
        format!("var a=[{{pad0:1,pad1:2,v:9}}];var s=0;\n\
                 for(var o=0;o<{OUTER};o++){{for(var i=0;i<{INNER};i++)s=(s+a[0].v)|0;\n\
                 if(o==={half})delete a[0].pad0;}}console.log(s);"),
        // The cached DATA slot becomes an ACCESSOR: reading it directly would
        // hand back the getter FUNCTION as if it were the value.
        format!("var a=[{{v:4}}];var s=0;\n\
                 for(var o=0;o<{OUTER};o++){{for(var i=0;i<{INNER};i++)s=(s+a[0].v)|0;\n\
                 if(o==={half})Object.defineProperty(a[0],'v',{{get:function(){{return 40;}},configurable:true}});}}\n\
                 console.log(s);"),
        // `setPrototypeOf` under a chain way — only the receiver's link moves.
        format!("var a=[Object.create({{v:10}})];var s=0;\n\
                 for(var o=0;o<{OUTER};o++){{for(var i=0;i<{INNER};i++)s=(s+a[0].v)|0;\n\
                 if(o==={half})Object.setPrototypeOf(a[0],{{v:20}});}}console.log(s);"),
        // EIGHT receivers cycling, ONE of them mutated: the stale way is not the
        // one the probe reaches first, so the guard has to be per-way.
        format!("var a=[];for(var i=0;i<8;i++)a.push({{pad:i,v:i+1}});var s=0,k=0;\n\
                 for(var o=0;o<{OUTER};o++){{for(var i=0;i<{INNER};i++){{s=(s+a[k].v)|0;k++;if(k===8)k=0;}}\n\
                 if(o==={half}){{delete a[5].pad;a[5].v=500;}}}}console.log(s);"),
        // The same three on the GLOBAL-receiver arm.
        format!("var g=Object.create({{v:3}});var s=0;\n\
                 for(var o=0;o<{OUTER};o++){{for(var i=0;i<{INNER};i++)s=(s+g.v)|0;\n\
                 if(o==={half})g.v=7;}}console.log(s);"),
        format!("var g={{v:11}};var s=0;\n\
                 for(var o=0;o<{OUTER};o++){{for(var i=0;i<{INNER};i++)s=(s+g.v)|0;\n\
                 if(o==={half}){{for(var j=0;j<64;j++)g['y'+j]=j;}}}}console.log(s);"),
        format!("var g={{pad0:1,pad1:2,v:9}};var s=0;\n\
                 for(var o=0;o<{OUTER};o++){{for(var i=0;i<{INNER};i++)s=(s+g.v)|0;\n\
                 if(o==={half})delete g.pad0;}}console.log(s);"),
    ];
    for src in cases {
        assert_matches_node(&src);
    }
}

// ── 3. the spec-visible edges ───────────────────────────────────────────────

/// A HOLE is not the bits in the dense Vec: an absent index resolves through the
/// PROTOTYPE CHAIN. Dropping that compare is a wrong answer that only shows when
/// something actually lives at the index on `Array.prototype`.
#[test]
fn boxref_hole_resolves_through_the_prototype() {
    let src = format!(
        "var a=[{{v:1}},{{v:2}},{{v:3}}];delete a[1];\n\
         Object.getPrototypeOf(a)[1]={{v:999}};\n\
         var s=0,k=0;for(var i=0;i<{N};i++){{s=(s+a[k].v)|0;k++;if(k===3)k=0;}}\n\
         delete Object.getPrototypeOf(a)[1];console.log(s);"
    );
    assert_matches_node(&src);
}

/// Out-of-range, negative and fractional keys, and an EMPTY hole with nothing on
/// the chain (which must throw exactly where node throws).
#[test]
fn boxref_parity_key_edges() {
    let cases = [
        format!("var a=[{{v:1}},{{v:2}}];var t;for(var i=0;i<{N};i++){{t=a[99];}}console.log(typeof t);"),
        format!("var a=[{{v:1}},{{v:2}}];var t;for(var i=0;i<{N};i++){{t=a[-1];}}console.log(typeof t);"),
        format!("var a=[{{v:1}},{{v:2}}];var s=0;for(var i=0;i<{N};i++){{s=(s+(a[1.5]===undefined?1:0))|0;}}console.log(s);"),
        format!("var a=[{{v:1}},{{v:2}}];var s=0;for(var i=0;i<{N};i++){{s=(s+a[i&1].v)|0;}}a.length=1;console.log(s+' '+a.length);"),
    ];
    for src in cases {
        assert_matches_node(&src);
    }
}

/// Accessors — own, inherited, and one that SIDE-EFFECTS on every read. The tier
/// cannot frame-call user code, so each of these must reach the interpreter, and
/// the side-effecting one proves it ran exactly as many times as node ran it.
#[test]
fn boxref_parity_accessors() {
    let src = format!(
        "var hits=0;var a=[];for(var i=0;i<4;i++){{var o={{h:i}};\n\
         Object.defineProperty(o,'v',{{get:function(){{hits++;return this.h;}},configurable:true}});a.push(o);}}\n\
         var s=0,k=0;for(var i=0;i<{N};i++){{s=(s+a[k].v)|0;k++;if(k===4)k=0;}}\n\
         console.log(s+' '+hits);"
    );
    assert_matches_node(&src);
    let src = format!(
        "var p={{}};Object.defineProperty(p,'v',{{get:function(){{return this.h*2;}},configurable:true}});\n\
         var a=[];for(var i=0;i<3;i++){{var o=Object.create(p);o.h=i+1;a.push(o);}}\n\
         var s=0,k=0;for(var i=0;i<{N};i++){{s=(s+a[k].v)|0;k++;if(k===3)k=0;}}console.log(s);"
    );
    assert_matches_node(&src);
}

/// A getter that GROWS the pinned array under the loop. The pin's `base` is a
/// `Vec` pointer, so a growth reallocates it — the identity/bounds guards and the
/// deopt-on-accessor are what keep this from reading freed memory.
#[test]
fn boxref_accessor_that_grows_the_pinned_array() {
    let src = format!(
        "var a=[];var p={{}};Object.defineProperty(p,'v',{{get:function(){{\n\
         if(a.length<40)a.push(Object.create(p));return a.length;}},configurable:true}});\n\
         a.push(Object.create(p));\n\
         var s=0;for(var i=0;i<{N};i++){{s=(s+a[0].v)|0;}}console.log(s+' '+a.length);"
    );
    assert_matches_node(&src);
}

/// Dict mode (shape id 0 — the trap a shape-keyed guard falls into), frozen and
/// sealed receivers, a Proxy, `arguments`, and an `arr_props` overlay (whose
/// snapshot DECLINES, so the whole arm must fall back).
#[test]
fn boxref_parity_exotic_receivers() {
    let cases = [
        format!("var a=[];for(var i=0;i<4;i++){{var o={{doomed:1,v:i+1}};delete o.doomed;a.push(o);}}\n\
                 var s=0,k=0;for(var i=0;i<{N};i++){{s=(s+a[k].v)|0;k++;if(k===4)k=0;}}console.log(s);"),
        format!("var a=[Object.freeze({{v:5}}),Object.seal({{v:6}})];\n\
                 var s=0,k=0;for(var i=0;i<{N};i++){{s=(s+a[k].v)|0;k++;if(k===2)k=0;}}console.log(s);"),
        format!("var a=[new Proxy({{v:4}},{{get:function(t,k){{return k==='v'?41:t[k];}}}})];\n\
                 var s=0;for(var i=0;i<{N};i++){{s=(s+a[0].v)|0;}}console.log(s);"),
        format!("function f(){{var s=0;for(var i=0;i<{N};i++){{s=(s+arguments[0].v)|0;}}return s;}}\n\
                 console.log(f({{v:2}}));"),
        format!("var a=[{{v:1}},{{v:2}}];Object.defineProperty(a,'1',{{get:function(){{return {{v:77}};}},configurable:true}});\n\
                 var s=0,k=0;for(var i=0;i<{N};i++){{s=(s+a[k].v)|0;k++;if(k===2)k=0;}}console.log(s);"),
    ];
    for src in cases {
        assert_matches_node(&src);
    }
}

/// The answer CHANGES mid-loop: `setPrototypeOf` on the receiver, an own
/// property shadowing a chain hit, and the pinned array variable reassigned. The
/// first two are what the way's hop versions guard; the third is the pin's
/// identity compare.
#[test]
fn boxref_parity_mutation_mid_loop() {
    let cases = [
        format!("var a=[Object.create({{v:10}})];var s=0;\n\
                 for(var i=0;i<{N};i++){{if(i==={half})Object.setPrototypeOf(a[0],{{v:20}});s=(s+a[0].v)|0;}}console.log(s);",
                half = N / 2),
        format!("var a=[Object.create({{v:3}})];var s=0;\n\
                 for(var i=0;i<{N};i++){{if(i==={half})a[0].v=7;s=(s+a[0].v)|0;}}console.log(s);",
                half = N / 2),
        format!("var x=[{{v:1}}],y=[{{v:100}}];var live=x;var s=0;\n\
                 for(var i=0;i<{N};i++){{if(i==={half})live=y;s=(s+live[0].v)|0;}}console.log(s);",
                half = N / 2),
        format!("var g={{v:1}};var s=0;\n\
                 for(var i=0;i<{N};i++){{if(i==={half})g={{v:5}};s=(s+g.v)|0;}}console.log(s);",
                half = N / 2),
    ];
    for src in cases {
        assert_matches_node(&src);
    }
}

/// The dst tag guard: the probe answers a raw `Value`, and the destination is an
/// f64 home. A string, a bool, `null`, `undefined` and an object must DEOPT, not
/// be reinterpreted as a double.
#[test]
fn boxref_parity_non_numeric_values() {
    let cases = [
        format!("var a=[{{v:'a'}},{{v:'b'}}];var s='';var k=0;\n\
                 for(var i=0;i<{N};i++){{s=a[k].v;k++;if(k===2)k=0;}}console.log(s);"),
        format!("var a=[{{v:true}},{{v:null}},{{v:undefined}},{{v:0}}];var r=[],k=0;\n\
                 for(var i=0;i<{N};i++){{r[k]=a[k].v;k++;if(k===4)k=0;}}console.log(r.join(','));"),
        format!("var a=[{{v:{{n:1}}}}];var t;for(var i=0;i<{N};i++){{t=a[0].v;}}console.log(typeof t+' '+t.n);"),
        format!("var a=[{{v:1}},{{v:2.5}},{{v:3}}];var s=0,k=0;\n\
                 for(var i=0;i<{N};i++){{s=s+a[k].v;k++;if(k===3)k=0;}}console.log(s);"),
        // A value that turns non-numeric HALFWAY through the loop.
        format!("var a=[{{v:1}}];var s=0;for(var i=0;i<{N};i++){{if(i==={half})a[0].v='x';s=s+a[0].v;}}console.log(String(s).slice(0,24));",
                half = N / 2),
    ];
    for src in cases {
        assert_matches_node(&src);
    }
}

/// An array of objects whose elements turn into NUMBERS (and back) mid-loop: the
/// element's tag is not guarded on this arm — the bits go to a frame slot — so
/// what has to hold is that the following `GetProp` deopts on a non-heap
/// receiver rather than dereferencing a double.
#[test]
fn boxref_parity_element_type_flips() {
    let src = format!(
        "var a=[{{v:1}},{{v:2}}];var s=0,k=0;\n\
         for(var i=0;i<{N};i++){{if(i==={half})a[1]=7;s=(s+(a[k]&&a[k].v?a[k].v:0))|0;k++;if(k===2)k=0;}}\n\
         console.log(s);",
        half = N / 2
    );
    assert_matches_node(&src);
}

/// A property read whose VALUE IS DISCARDED, through a getter that has a SIDE
/// EFFECT. This is the defect this mechanism actually shipped and then fixed,
/// and it is the reason nothing else in this file was enough.
///
/// `RegionPlan::dead` skips a region op whose dst is never read, licensed by one
/// sentence: "every regalloc-region op is side-effect-free (heap ops decline the
/// region)". The BOXREF `GetProp` arm is the first thing that ever made that
/// sentence false. `o.p` as a STATEMENT has a dead dst, the emitter skipped the
/// op, and the getter never ran — with the value discarded, NOTHING is
/// observable except the side effect, so every receiver-count and semantics case
/// above passed while the engine silently stopped executing user code.
///
/// It surfaced through `zipp-vm`'s own
/// `super_getter_inline_preserves_values_and_effects`, whose counter read 8
/// instead of 200000. The fix keeps an admitted `GetProp` dst out of `dead`.
#[test]
fn boxref_a_discarded_read_still_runs_its_getter() {
    // The original: a `super` getter chain, where only the counter can tell.
    let src = format!(
        "var n=0;\n\
         class A{{constructor(x){{this._v=x}} get v(){{n++;return this._v}}}}\n\
         class B extends A{{get v(){{return super.v*2}}}}\n\
         var b=new B(1);\n\
         for(var i=0;i<{N};i++) b.v;\n\
         console.log(n);"
    );
    assert_matches_node(&src);
    // The same shape on the ELEMENT arm, and with a plain own accessor rather
    // than a class chain.
    let src = format!(
        "var n=0;var a=[];\n\
         for(var q=0;q<4;q++){{var o={{h:q}};\n\
         Object.defineProperty(o,'v',{{get:function(){{n++;return this.h;}},configurable:true}});a.push(o);}}\n\
         var k=0;for(var i=0;i<{N};i++){{a[k].v;k++;if(k===4)k=0;}}\n\
         console.log(n);"
    );
    assert_matches_node(&src);
    // A discarded read of a plain DATA property must still be allowed to
    // disappear — this is the control that says the fix did not just turn the
    // optimisation off wholesale.
    let src = format!(
        "var a=[{{v:1}},{{v:2}}];var k=0;\n\
         for(var i=0;i<{N};i++){{a[k].v;k++;if(k===2)k=0;}}console.log('ok');"
    );
    assert_matches_node(&src);
    // And a discarded read whose getter THROWS on a later iteration: the throw
    // has to happen, at the same iteration node throws at.
    let src = format!(
        "var n=0;var o={{}};\n\
         Object.defineProperty(o,'v',{{get:function(){{n++;if(n==={half})throw new Error('x');return 1;}},configurable:true}});\n\
         var a=[o];try{{for(var i=0;i<{N};i++) a[0].v;}}catch(e){{}}\n\
         console.log(n);",
        half = N / 2
    );
    assert_matches_node(&src);
}

// ── 4. modes ────────────────────────────────────────────────────────────────

/// Every parity test again in a child process per latch. The switches are
/// memoized `AtomicU8` latches read once per process, so a mode IS a process.
#[test]
fn boxref_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 7] = [
        &[("ZIPP_NO_BOX_HOME", "1")],
        &[("ZIPP_NO_REGALLOC_GETPROP", "1")],
        &[("ZIPP_NO_BOX_HOME", "1"), ("ZIPP_NO_REGALLOC_GETPROP", "1")],
        &[("ZIPP_BOXREF_MISS", "deopt")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = Command::new(&exe);
        cmd.arg("boxref_");
        cmd.arg("--skip").arg("boxref_all_modes");
        cmd.arg("--skip").arg("boxref_mechanism");
        for (key, val) in mode {
            cmd.env(key, val);
        }
        let out = cmd.output().expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the boxref_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}

// ── 5. mechanism ────────────────────────────────────────────────────────────

/// The element kernel this whole file is about, as one string, so the mechanism
/// tests and the parity tests are looking at the same program.
fn element_kernel() -> String {
    // FUNCTION-scoped on purpose. At SCRIPT scope the bytecode compiler RECYCLES
    // a register — the same `r17` is the element read's dst at one ip and a
    // `LoadInt` temp at another — and a `box_regs` member must have exactly ONE
    // def, so the region declines. That is a real admission gap (B94's
    // live-range splitting is the mechanism that would close it), not a property
    // of this kernel; the parity tests above are function-scoped for the same
    // reason, and `polymorphic-objects`' script-scope mega-read happens not to
    // recycle and does engage.
    format!(
        "function kernel(){{\n\
         var a=[];for(var i=0;i<8;i++){{var o={{}};o.pad=i;o.p=i;a.push(o);}}\n\
         var s=0,k=0;for(var i=0;i<{N};i++){{s=(s+a[k].p)|0;k++;if(k===8)k=0;}}\n\
         return s;}}\n\
         console.log(kernel());"
    )
}

/// ON, the kernel's region reaches the register tier and says so.
#[test]
fn boxref_mechanism_engages() {
    let log = jitlog_of(&element_kernel(), &[]);
    assert!(
        log.contains("BOXREF"),
        "the element kernel did not engage BOXREF — this file would be vacuous:\n{log}"
    );
    assert!(
        !log.contains("EVICTED"),
        "the BOXREF region evicted; a DOUBLE region that evicts leaves the loop \
         interpreted unless the boxref retry catches it:\n{log}"
    );
}

/// OFF, it declines to the memory tier with the pre-wave reason — the proof that
/// `ZIPP_NO_BOX_HOME=1` really is the off switch and not a no-op.
#[test]
fn boxref_mechanism_off_switch_declines() {
    let log = jitlog_of(&element_kernel(), &[("ZIPP_NO_BOX_HOME", "1")]);
    assert!(
        !log.contains("BOXREF"),
        "ZIPP_NO_BOX_HOME=1 still engaged the mechanism:\n{log}"
    );
    assert!(
        log.contains("MEM region"),
        "with the switch off the kernel should compile on the memory tier:\n{log}"
    );
}

/// The map's mechanism, measured rather than argued. `reserve_ic_sites` hands a
/// fresh compile eight ZEROED ways and `set_ic` is reachable only from the miss
/// helpers, so a probe that DEOPTS instead of calling one can never fill a way:
/// it misses on every access and the region evicts. The answer stays right —
/// that is what the parity run under this mode shows — but the tier is lost.
#[test]
fn boxref_mechanism_deopt_on_miss_cannot_warm_its_cache() {
    let log = jitlog_of(&element_kernel(), &[("ZIPP_BOXREF_MISS", "deopt")]);
    assert!(
        log.contains("BOXREF"),
        "the deopt variant should still COMPILE the region:\n{log}"
    );
    assert!(
        log.contains("EVICTED"),
        "a deopt-on-miss probe over zeroed ways should evict; if this ever stops \
         being true the ways found a filler and the default can be revisited:\n{log}"
    );
}

/// Run `src` in a child under `ZIPP_JITLOG=1` (plus `env`) and hand back stderr.
fn jitlog_of(src: &str, env: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = Command::new(&exe);
    cmd.arg("boxref_jitlog_child")
        .arg("--exact")
        .arg("--ignored")
        .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JITDECLINE", "1")
        .env("ZIPP_BOXREF_SRC", src);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn the test binary");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "jitlog child failed:\n{}\n{stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    stderr
}

/// The worker for [`jitlog_of`]. A no-op unless `ZIPP_BOXREF_SRC` is set,
/// because the JIT switches are memoized latches: a mode IS a process.
#[test]
#[ignore = "worker: spawned by jitlog_of with ZIPP_BOXREF_SRC set"]
fn boxref_jitlog_child() {
    let Some(src) = std::env::var_os("ZIPP_BOXREF_SRC") else {
        return;
    };
    let _ = run_ok(&src.to_string_lossy());
}
