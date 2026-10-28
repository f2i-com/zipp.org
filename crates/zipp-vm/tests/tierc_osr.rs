//! Hot loops in functions called too rarely to compile at a call, whose loops
//! no region serves, entered in the function's compiled body at the loop head
//! (`JitFn::run_at`): the values made before the loop (a double, an object whose
//! properties change mid-loop, an array that turns mixed), the frame's handlers,
//! a return from inside the loop, break, throws caught inside and outside, a
//! value turning double or string mid-loop, nested loops, closures over loop
//! state, and a generator's loop (never entered). Run interpreted, at the
//! default thresholds, with every function compiled at once, under GC stress and
//! with the entry off; the expected lines are Node 24's.

const EXPECTED: &str = "10790989
r0/2223
6954
RangeError at 1920
34920/7120
2400/7194
5198
840:5998r4r5r6r7
16788
7195,7695,8195,8695,9195,9695
15523.625|q3.5x|1.5,2,3,3.5,5,6,7,x
1,601,1201,1801,2401
10790991
r1/2224
6953
RangeError at 1921
35280/7194
2400/7194
5198
840:5998r4r5r6r7
28782
7195,7695,8195,8695,9195,9695
22322.625|q3.5x|1.5,2,3,3.5,5,6,7,x
1,601,1201,1801,2401
10790993
r2/2225
6953
RangeError at 1922
35640/7128
2400/7194
5198
840:5998r4r5r6r7
43176
7195,7695,8195,8695,9195,9695
29121.625|q3.5x|1.5,2,3,3.5,5,6,7,x
1,601,1201,1801,2401";

#[test]
fn tierc_osr_child() {
    if std::env::var_os("ZIPP_TIERC_OSR_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn loops_enter_compiled_bodies_at_the_head() {
    if std::env::var_os("ZIPP_TIERC_OSR_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("osr-off", Some(("ZIPP_NO_TIERC_OSR", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "tierc_osr_child", "--nocapture"])
            .env("ZIPP_TIERC_OSR_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// Hot loops in functions called too rarely to compile at a call, whose loops
// no region serves (a `continue` inside for-of routes through its finally):
// the function's compiled body is entered at the loop head. What they must
// keep: the values the interpreter made before the loop, the frame's
// handlers (for-of's iterator close, try/catch, try/finally), a `return`
// from inside the loop, `break`, a throw caught inside or outside, a value
// turning double or string mid-loop, a nested loop entered at either head,
// closures capturing loop state, values made before the loop and read in it
// (a double, an object whose properties change mid-loop, an array that turns
// mixed), and a generator's loop (never entered).
"use strict";
const rows = [];
for (let i = 0; i < 3000; i++) rows.push({ k: i % 7, v: i, s: "r" + (i % 11), skip: i % 5 === 0 });

function sumSkip(list, bias) {
  const pre = bias * 2;
  let t = pre;
  for (const r of list) {
    if (r.skip) continue;
    t += r.v * r.k;
  }
  return t;
}

function findFirst(list, want) {
  let seen = 0;
  for (const r of list) {
    seen++;
    if (r.skip) continue;
    if (r.v === want) return [r.s, seen];
  }
  return ["none", seen];
}

function breakAt(list, stop) {
  let n = 0;
  for (const r of list) {
    if (r.skip) continue;
    if (r.v >= stop) break;
    n += r.k;
  }
  return n;
}

function throwsAt(list, at) {
  let n = 0;
  for (const r of list) {
    if (r.skip) continue;
    if (r.v === at) throw new RangeError("at " + n);
    n++;
  }
  return n;
}

function catchesInside(list, every) {
  let caught = 0, n = 0;
  for (const r of list) {
    if (r.skip) continue;
    try {
      if (r.v % every === 0) throw r.v;
      n += r.k;
    } catch (e) {
      caught += e;
    }
  }
  return [caught, n];
}

function finallyCount(list) {
  let fin = 0, n = 0;
  for (const r of list) {
    if (r.skip) continue;
    try {
      n += r.k;
    } finally {
      fin++;
    }
  }
  return [fin, n];
}

function turnsDouble(list) {
  let t = 0;
  for (const r of list) {
    if (r.skip) continue;
    t += r.v > 2000 ? 0.5 : r.k;
  }
  return t;
}

function turnsString(list) {
  let t = 0;
  for (const r of list) {
    if (r.skip) continue;
    t = r.v > 2500 ? t + r.s : t + r.k;
  }
  return t.length + ":" + t.slice(0, 12);
}

function nested(list, reps) {
  let t = 0;
  for (let i = 0; i < reps; i++) {
    for (const r of list) {
      if (r.skip) continue;
      t += r.k + i;
    }
  }
  return t;
}

function closures(list) {
  const fs = [];
  let acc = 0;
  for (const r of list) {
    if (r.skip) continue;
    acc += r.k;
    if (r.v % 500 === 1) fs.push(() => r.v + acc);
  }
  return fs.map((f) => f()).join(",");
}

function invariants(list, bias, arr) {
  const k = bias * 0.5;
  const o = { m: bias + 1, n: [bias, 2], s: "q" };
  let t = 0, u = "";
  for (const r of list) {
    if (r.skip) continue;
    t += r.k * k + o.m + o.n[0] + arr[r.k];
    if (r.v === 1501) {
      o.m = 0.125;
      arr[3] = 3.5;
      arr[7] = "x";
    }
    if (r.v === 2501) {
      o.n = [7, 8];
      u += o.s + arr[3] + arr[7];
    }
  }
  return t + "|" + u + "|" + arr.join(",");
}

function* gen(list) {
  for (const r of list) {
    if (r.skip) continue;
    if (r.v % 600 === 1) yield r.v;
  }
}

const out = [];
for (let round = 0; round < 3; round++) {
  out.push(sumSkip(rows, round));
  out.push(findFirst(rows, 2222 + round).join("/"));
  out.push(breakAt(rows, 2900 - round));
  try {
    out.push(throwsAt(rows, 2401 + round));
  } catch (e) {
    out.push(e.name + " " + e.message);
  }
  out.push(catchesInside(rows, 97 + round).join("/"));
  out.push(finallyCount(rows).join("/"));
  out.push(turnsDouble(rows));
  out.push(turnsString(rows));
  out.push(nested(rows, 2 + round));
  out.push(closures(rows));
  out.push(invariants(rows, round + 0.25, [1.5, 2, 3, 4, 5, 6, 7]));
  out.push([...gen(rows)].join(","));
}
console.log(out.join("\n"));
"####;
