//! Young objects stored into long-lived objects' fields and long-lived arrays'
//! elements by compiled code (shape and identity ways, the array mirror's
//! overwrite), whose barrier is called only for a young value
//! (`emit_heap_store_barrier`), then long-lived values moved between them and
//! primitives stored, with allocation churn between rounds so collections run;
//! every value is checked. Run interpreted, at the default thresholds, with
//! every function compiled at once, under GC stress, with the filter off and
//! with the mirror store off; the expected line is Node 24's.

const EXPECTED: &str = "10240,0";

#[test]
fn store_barrier_filter_child() {
    if std::env::var_os("ZIPP_STORE_BARRIER_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn compiled_stores_keep_young_values() {
    if std::env::var_os("ZIPP_STORE_BARRIER_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("filter-off", Some(("ZIPP_NO_BARRIER_FILTER", "1"))),
        ("mirror-store-off", Some(("ZIPP_NO_ARR_SET_MIRROR", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "store_barrier_filter_child", "--nocapture"])
            .env("ZIPP_STORE_BARRIER_CHILD", "1")
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

const PROGRAM: &str = r####"// Young objects stored into long-lived holders by compiled code (property
// stores through shape and identity ways, element overwrites through the
// array mirror), then allocation churn so collections run: every stored
// value must survive with its contents. Old values moved between old
// holders, and primitives, need no barrier and must stay too.
const out = [];
class Box { constructor(i) { this.i = i; this.v = null; this.w = 0; } }
const boxes = []; for (let i = 0; i < 256; i++) boxes.push(new Box(i));
const slots = new Array(256).fill(null);
const olds = []; for (let i = 0; i < 256; i++) olds.push({ old: i });
function churn(n) { let k = 0; for (let g = 0; g < n; g++) { const t = [g, { g }]; if (t[1].g === -1) k++; } return k; }
churn(300000);
function fillYoung(round) {
  for (let i = 0; i < 256; i++) {
    const b = boxes[i];
    b.v = { round, i, s: "r" + round + "i" + i };
    slots[i] = { round, i, tag: "s" + i };
    b.w = i * round;
  }
}
function fillOld(round) {
  for (let i = 0; i < 256; i++) { boxes[i].v = olds[(i + round) & 255]; slots[i] = olds[(i * 3 + round) & 255]; }
}
function check(round, young) {
  let ok = 0;
  for (let i = 0; i < 256; i++) {
    const b = boxes[i], s = slots[i];
    if (young) { if (b.v.round === round && b.v.i === i && b.v.s === "r" + round + "i" + i && s.round === round && s.tag === "s" + i && b.w === i * round) ok++; }
    else if (b.v.old === ((i + round) & 255) && s.old === ((i * 3 + round) & 255)) ok++;
  }
  return ok;
}
let total = 0;
for (let round = 1; round <= 40; round++) {
  if (round % 4 === 0) { fillOld(round); churn(60000); total += check(round, false); }
  else { fillYoung(round); churn(60000); total += check(round, true); }
}
out.push(total, churn(1000));
console.log(out.join(","));
"####;
