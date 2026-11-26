//! A class that extends a bound function.
//!
//! `super(...)` is `Construct(parent, args, newTarget)`. For a bound
//! function that is the bound function's own [[Construct]]: its target,
//! constructed with the bound arguments in front and this `new.target`,
//! the bound `this` ignored. `super(...)` used to CALL a bound parent with
//! the instance as `this`: a bound class threw "is not a function", and a
//! bound function ran on its bound `this` and left the instance untouched.
//!
//! The program extends a bound class, a bound ES5 constructor and a class
//! bound twice (whose bound argument makes the target return another
//! object), from hot code and through `Reflect.construct` with another
//! `new.target`, and checks that extending a bound arrow function is still
//! a TypeError. The expected lines are Node 24's.

const PROGRAM: &str = r#"
class Base { constructor(mode, extra) { this.kind = "base"; this.mode = mode; this.extra = extra; this.nt = new.target.name; if (mode === 1) return { kind: "override" }; } }
const Bound = Base.bind({ boundThis: true }, 0); Bound.prototype = Base.prototype;
class FromBound extends Bound { constructor(x) { super(x); this.b = 1; } }
function OldF(a, b) { this.a = a; this.b = b; this.viaNew = new.target !== undefined; }
const BoundOld = OldF.bind({ ignored: true }, 1); BoundOld.prototype = OldF.prototype;
class FromBoundOld extends BoundOld { constructor() { super(2); this.c = 3; } }
const Twice = Base.bind(null, 1).bind(null); Twice.prototype = Base.prototype;
class FromTwice extends Twice { constructor() { super("e"); this.after = true; } }
const NotCtor = (() => 1).bind(null);
const lines = [];
function run() {
  const o = new FromBound("x"), p = new FromBoundOld(), t = new FromTwice();
  return [o.kind, o.mode, o.extra, o.nt, o.b, o.boundThis, o instanceof FromBound, p.a, p.b, p.c, p.viaNew, p.ignored, p instanceof FromBoundOld, t.kind, t.after, t instanceof FromTwice].join(",");
}
let last; for (let i = 0; i < 3000; i++) last = run();
lines.push(last);
try { class Bad extends NotCtor {} lines.push("no error"); } catch (e) { lines.push(e.constructor.name); }
const r = Reflect.construct(FromBound, ["y"], FromBoundOld);
lines.push([r.kind, r.extra, r.nt, r.b, r instanceof FromBoundOld, r instanceof FromBound].join(","));
console.log(lines.join("\n"));
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output.iter().flat_map(|l| l.lines().map(str::to_string)).collect()
}

#[test]
fn super_bound_parent_answers_as_node() {
    assert_eq!(
        run_ok(PROGRAM),
        [
            "base,0,x,FromBound,1,,true,1,2,3,true,,true,override,true,false",
            "TypeError",
            "base,y,FromBoundOld,1,true,false",
        ]
    );
}

/// The same answers with the JIT compiling at once, with a collection at
/// every safe point, and with no native code.
#[test]
fn super_bound_parent_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("stress", "ZIPP_GC_STRESS", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("super_bound_parent_answers")
            .arg("--nocapture")
            .env(key, value)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the filter matched nothing in {name} mode"
        );
    }
}
