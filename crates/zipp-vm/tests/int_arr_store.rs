//! In-place Int stores into a pinned dense all-Int Array on the INTEGER tiers
//! (`arr[i] = v`). The kernels run hot. They store in bounds and out of
//! bounds, values past i32, over holes and `undefined`, into frozen, sealed,
//! read-only-index and accessor-index arrays, into an array that turns mixed
//! (a double element) and one whose length shrank. The expected lines are
//! Node's.

const SOURCE: &str = r##""use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));
const t = (f) => { try { return f(); } catch (e) { return e.constructor.name; } };
function fill(a, n, k) { for (let i = 0; i < n; i++) a[i & 1023] = (i * k) | 0; return a[5] + a[1023]; }
function bump(a, n) { for (let i = 0; i < n; i++) a[i % a.length] = a[i % a.length] + 1; return a[0]; }
function big(a, n) { for (let i = 0; i < n; i++) a[i & 15] = i * 1000003; return a[3]; }
function oob(a, n) { for (let i = 0; i < n; i++) a[i] = i; return a.length; }
function holes(a, n) { for (let i = 0; i < n; i++) a[(i * 7) & 31] = i; return a.join(","); }
const base = () => { const a = []; for (let j = 0; j < 1024; j++) a.push(j); return a; };
let h = 0;
for (let r = 0; r < 200; r++) h = (h + fill(base(), 5000, r)) | 0;
log("fill", h);
const b = base(); for (let r = 0; r < 300; r++) bump(b, 3000); log("bump", b.slice(0, 6).join(","), b[1023]);
log("big", big(base(), 400000), JSON.stringify(big(base(), 3000)));
log("oob", oob([1, 2, 3], 10), oob(base(), 2000));
const hl = []; for (let j = 0; j < 32; j++) hl.push(j); delete hl[3]; hl[20] = undefined;
for (let r = 0; r < 300; r++) holes(hl, 40); log("holes", holes(hl, 40));
const fz = Object.freeze(base()); log("frozen", t(() => fill(fz, 3000, 2)), fz[5]);
const sl = Object.seal(base()); log("sealed", fill(sl, 3000, 3), sl[7]);
const ro = base(); Object.defineProperty(ro, 9, { value: 99, writable: false }); log("readonly", t(() => fill(ro, 3000, 4)), ro[9], ro[10]);
const acc = base(); let sets = 0; Object.defineProperty(acc, 4, { get() { return 4; }, set(v) { sets++; } }); fill(acc, 3000, 5); log("accessor", sets, acc[4]);
const mix = base(); for (let r = 0; r < 200; r++) fill(mix, 3000, r); mix[7] = 1.5; log("mixed", fill(mix, 3000, 9), mix[7]);
const ga = base(); fill(ga, 3000, 1); ga.length = 100; log("shrunk", fill(ga, 3000, 6), ga.length);
console.log(out.join("\n"));
"##;

fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn int_arr_store_parity() {
    let got = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(got.error.is_none(), "unexpected runtime error: {:?}", got.error);
    let got: Vec<String> = got.output.iter().flat_map(|s| s.lines().map(str::to_owned)).collect();
    assert_eq!(got, node_output(SOURCE));
}

/// The corpus again with the store off, the xmm INT emitter, eager JIT, GC
/// stress and the interpreter, each in a fresh process.
#[test]
fn int_arr_store_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in [
        ("ZIPP_NO_INT_ARR_STORE", "1"),
        ("ZIPP_NO_GPR_HOMES", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
        ("ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["int_arr_store_parity", "--exact"])
            .env(mode.0, mode.1)
            .output()
            .expect("spawn test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!stdout.contains("running 0 tests"), "mode filter matched nothing: {stdout}");
    }
}