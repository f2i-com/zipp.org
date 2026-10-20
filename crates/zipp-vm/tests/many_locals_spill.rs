//! A function may declare more locals than the register file addresses.
//!
//! An instruction's register operand is 16 bits, and the register file stops
//! at 32,768, so a function with more locals failed to compile ("function
//! needs more than 32768 registers"). V8 has no such count: a function's
//! locals are slots of its stack frame, reached with wide operands, and only
//! the stack bounds them (65,536 fit Node's default stack, 524,288 a 7.8 MB
//! one). A function that declares more than 30,000 names now keeps its plain
//! locals past the first 16,384 registers in a spill vector reached with
//! 32-bit indices (`Instr::NewSpill` / `LoadSpill` / `StoreSpill`); every
//! other function compiles as it did.
//!
//! The probe runs 40,000 and 150,000 locals through reads, writes, updates,
//! destructuring, `const`, closures, a generator, try/catch/finally, objects
//! held only by spilled slots, and recursion. The expected lines are Node's
//! (v24, run with `--stack-size=7800`: 150,000 locals do not fit its default
//! stack). The probe also runs with the JIT off and, at 40,000 locals, under
//! `ZIPP_GC_STRESS=1`, where a lost write barrier would free the objects.

const PROBE: &str = r###"// Functions with more locals than zipp's register file addresses (32,768):
// reads, writes, updates, destructuring, consts, closures, a generator,
// try/catch/finally, recursion. Run Node with --stack-size=7800: V8 keeps
// locals on the machine stack, so 150,000 do not fit its default.
const out = [];
const check = (label, f) => {
  try {
    out.push(label + ": " + f());
  } catch (e) {
    out.push(label + ": threw " + e.name);
  }
};
const decls = (n, kind, prefix = "v") => {
  const lines = [];
  for (let i = 0; i < n; i++) lines.push(kind + " " + prefix + i + " = " + i + ";");
  return lines.join("\n");
};

for (const n of [40000, 150000]) {
  const a = n - 1, b = n - 2, c = n - 3, d = n - 4, e = n - 5, mid = n >> 1;
  for (const kind of ["var", "let"]) {
    const destructure = kind === "var"
      ? `var [v${d}, v${e}] = [7, 8];`
      : `[v${d}, v${e}] = [7, 8];`;
    check(`${kind} ${n} reads and writes`, () => new Function(decls(n, kind) + `
      var first = v${a};
      v${a}++;
      v${b} += 10;
      v${c} = v${c} * 2;
      ${destructure}
      var s = 0;
      for (let i = 0; i < 3; i++) { s += v${a} + i; }
      return [first, v${a}, v${b}, v${c}, v${d}, v${e}, v0, v${mid}, s, typeof v${a}, delete v${a}].join(",");`)());
  }
  check(`const ${n}`, () => new Function(decls(n, "const", "c") + `
    var r = c${n - 1} + c0;
    try { c${n - 1} = 5; } catch (e) { r = r + ":" + e.name; }
    return r + ":" + c${n - 1};`)());
  check(`closure ${n}`, () => new Function(decls(n, "let") + `
    const f = () => v0 + v${a};
    v${a} = 1000;
    v${b} = v${b} + 1;
    return f() + "," + v${b};`)());
  check(`generator ${n}`, () => {
    const g = new Function("return function* () {" + decls(n, "let") + `
      yield v${a};
      v${a} += 1;
      v${b} = v${a} * 2;
      yield v${a} + "," + v${b};
      return v${mid};
    }`)();
    const it = g();
    return [it.next().value, it.next().value, it.next().value].join("|");
  });
  check(`try ${n}`, () => new Function(decls(n, "var") + `
    try { v${b} = -2; throw v${a}; }
    catch (err) { v${c} = err + 1; }
    finally { v${d} = v${c} + v${b}; }
    return [v${b}, v${c}, v${d}].join(",");`)());
  // Objects held only by spilled slots across allocation churn: under
  // ZIPP_GC_STRESS every allocation collects, so a lost write barrier
  // would let a minor collection free them.
  check(`objects ${n}`, () => new Function(decls(n, "let") + `
    v${a} = { x: 41 };
    v${b} = [1, 2, 3];
    var junk;
    for (let i = 0; i < 2000; i++) junk = { i: i, s: "s" + i };
    v${c} = { y: v${a}.x + 1 };
    for (let i = 0; i < 2000; i++) junk = [i];
    return [v${a}.x, v${b}.length, v${c}.y, junk[0]].join(",");`)());
  check(`recursion ${n}`, () => {
    const r = new Function("return function r(depth) {" + decls(n, "var") + `
      v${a} = depth;
      if (depth > 0) { var below = r(depth - 1); return v${a} * 10 + below; }
      return v${a};
    }`)();
    return r(4);
  });
}
console.log(out.join("\n"));"###;

const EXPECTED: &str = r###"var 40000 reads and writes: 39999,40000,40008,79994,7,8,0,20000,120003,number,false
let 40000 reads and writes: 39999,40000,40008,79994,7,8,0,20000,120003,number,false
const 40000: 39999:TypeError:39999
closure 40000: 1000,39999
generator 40000: 39999|40000,80000|20000
try 40000: -2,40000,39998
objects 40000: 41,3,42,1999
recursion 40000: 100
var 150000 reads and writes: 149999,150000,150008,299994,7,8,0,75000,450003,number,false
let 150000 reads and writes: 149999,150000,150008,299994,7,8,0,75000,450003,number,false
const 150000: 149999:TypeError:149999
closure 150000: 1000,149999
generator 150000: 149999|150000,300000|75000
try 150000: -2,150000,149998
objects 150000: 41,3,42,1999
recursion 150000: 100"###;

/// Run src on a 256 MiB stack, as the CLI does, and return its output.
fn run(src: String) -> String {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let out = zipp_vm::run(&src).expect("the probe compiles");
            assert!(out.error.is_none(), "runtime error: {:?}", out.error);
            out.output.join("\n")
        })
        .expect("spawn the probe thread")
        .join()
        .expect("the probe thread finishes")
}

#[test]
fn locals_past_the_register_file_match_node() {
    assert_eq!(run(PROBE.to_string()), EXPECTED);
}

/// The child half of the mode test: runs only when the parent sets the
/// variable, with the mode's environment.
#[test]
fn mode_child() {
    let Some(sizes) = std::env::var_os("ZIPP_MANY_LOCALS_CHILD") else {
        return;
    };
    let sizes = sizes.to_string_lossy().into_owned();
    let src = PROBE.replace("[40000, 150000]", &sizes);
    let lines = if sizes == "[40000]" { 8 } else { usize::MAX };
    let expected: Vec<&str> = EXPECTED.lines().take(lines).collect();
    assert_eq!(run(src), expected.join("\n"));
}

/// A spill vector is heap memory: under a ceiling it counts like any other.
/// 200,000 locals keep 16,384 in registers (about 128 KiB of frame, which
/// the ceiling below leaves room for) and spill the rest (about 1.5 MB, which
/// it does not).
#[cfg(feature = "safe-sandbox")]
#[test]
fn a_spill_vector_counts_against_the_heap_ceiling() {
    use zipp_vm::embed::{compile_script, JsValue};
    let n = 200_000;
    let mut src = String::from("function big() {\n");
    for i in 0..n {
        src.push_str(&format!("let v{i} = {i};\n"));
    }
    src.push_str(&format!("return v{};\n}}\n", n - 1));
    let mut roomy = compile_script(&src).expect("source compiles");
    roomy.run_init().expect("script runs");
    roomy.set_limits(u64::MAX, None);
    roomy.set_heap_limit(roomy.heap_bytes() + (64 << 20));
    assert_eq!(roomy.call_global("big", &[]), Ok(JsValue::Number((n - 1) as f64)));

    let mut tight = compile_script(&src).expect("source compiles");
    tight.run_init().expect("script runs");
    tight.set_limits(u64::MAX, None);
    tight.set_heap_limit(tight.heap_bytes() + (768 << 10));
    let refused = tight.call_global("big", &[]);
    assert!(
        refused.as_ref().is_err_and(|e| e.contains("memory budget")),
        "the spill vector should meet the ceiling, got {refused:?}"
    );
}

#[test]
fn locals_past_the_register_file_without_the_jit_and_under_gc_stress() {
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, sizes, env) in [
        ("nojit", "[40000, 150000]", ("ZIPP_NOJIT", "1")),
        ("gc-stress", "[40000]", ("ZIPP_GC_STRESS", "1")),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["mode_child", "--exact", "--nocapture"])
            .env("ZIPP_MANY_LOCALS_CHILD", sizes)
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS")
            .env(env.0, env.1)
            .output()
            .expect("spawn the mode child");
        assert!(
            out.status.success()
                && !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
            "{mode} child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}