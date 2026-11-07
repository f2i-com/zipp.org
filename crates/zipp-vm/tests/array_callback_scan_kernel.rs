//! `find`/`findIndex`/`some`/`every` run an eligible predicate in a fused
//! scan kernel (`compile_scan_kernel`; `ZIPP_NO_SCAN_KERNEL` calls it per
//! element) until it stops, and leave holes, non-Bool results and non-number
//! elements to the per-element loop: hot scans with captured values and an
//! index parameter, holes skipped or visited as undefined (and one the
//! prototype fills), truthiness, mixed elements, a `thisArg`, empty arrays.
//! The expected lines are Node's (v24), with the kernel, without it and in
//! the interpreter.

const SOURCE: &str = r##"
"use strict";
const out = [];
const N = []; for (let i = 0; i < 2000; i++) N.push((i * 37) % 2000 - 1000);
let a = 0, b = 0, c = 0, d = 0;
for (let r = 0; r < 400; r++) {
  const k = r % 50;
  if (N.some((x) => x === 999 - k)) a++;
  if (N.every((x) => x >= -1000 && x < 1000)) b++;
  const f = N.find((x) => x > 900 + k);
  c = (c + (f === undefined ? -1 : f)) | 0;
  d = (d + N.findIndex((x, i) => x + i > 2500 - k)) | 0;
}
out.push("hot " + a + " " + b + " " + c + " " + d);
// holes: some/every skip them, find visits them as undefined
const H = [1, 2, , 4];
out.push("holes " + H.some((x) => x === undefined) + " " + H.every((x) => x > 0) + " " + H.find((x) => x > 3) + " " + H.findIndex((x) => x === undefined));
// a hole that the prototype fills
Array.prototype[2] = 3;
out.push("protohole " + H.some((x) => x === 3) + " " + H.findIndex((x) => x === 3));
delete Array.prototype[2];
// non-Bool results use truthiness
out.push("truthy " + N.some((x) => x & 0) + " " + N.every((x) => x | 1) + " " + N.find((x) => x + 1000) + " " + [0, 0, 5].findIndex((x) => x));
// strings and mixed elements bail to the per-element loop
const S = [1, 2, "3", 4, 5.5];
out.push("mixed " + S.some((x) => x > 4) + " " + S.every((x) => x < 10) + " " + S.find((x) => x * 2 > 5) + " " + S.findIndex((x) => x > 2));
// a thisArg, empty arrays, an every that fails at the end
out.push("this " + [1, 2, 3].some(function (x) { return x === this.v; }, { v: 2 }) + " " + [].some((x) => true) + " " + [].every((x) => false) + " " + [1, 2, 3].every((x) => x < 3));
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "hot 400 400 377296 608128\nholes false true 4 2\nprotohole true 2\ntruthy false true -963 2\nmixed true true 3 2\nthis true false true false";

#[test]
fn array_scan_kernel_worker() {
    if std::env::var_os("ZIPP_SCAN_KERNEL_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn array_scan_kernel_matches_node() {
    if std::env::var_os("ZIPP_SCAN_KERNEL_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [("default", None), ("kernel-off", Some("ZIPP_NO_SCAN_KERNEL")), ("interpreter", Some("ZIPP_NOJIT"))] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["array_scan_kernel_worker", "--exact", "--nocapture"])
            .env("ZIPP_SCAN_KERNEL_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_SCAN_KERNEL");
        if let Some(v) = var {
            command.env(v, "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}