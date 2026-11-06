//! A suspended `await` of a primitive queues its activation's resume job
//! directly (`ZIPP_NO_PRIMITIVE_AWAIT_DIRECT=1` restores the fulfilled
//! promise PromiseResolve makes and the subscription to it). The job lands
//! where the subscription would have queued it, so continuations interleave
//! with promise reactions, thenables, BigInt/Symbol/string/boolean awaits and
//! awaits inside `try`/`catch`/`finally` exactly as before. The expected
//! line is Node's (v24), with the direct path, without it and in the
//! interpreter.

const SOURCE: &str = r##"
"use strict";
const log = [];
async function a(tag, n) { for (let i = 0; i < n; i++) { log.push(tag + i); await (i & 1 ? null : i); } return tag; }
async function b(tag) { log.push(tag + "<"); await Promise.resolve(1); log.push(tag + "p"); await "s"; log.push(tag + "s"); await undefined; log.push(tag + ">"); }
async function c(tag) { const t = { then(r) { log.push(tag + "then"); r(7); } }; log.push(tag + "<"); const v = await t; log.push(tag + v); await 0n; log.push(tag + "big"); }
async function d(tag) {
  let p; const q = new Promise(r => { p = r; });
  let h = Promise.resolve(); for (let i = 0; i < 5; i++) h = h.then(() => log.push(tag + "h" + i)); h.then(() => p(9));
  await null; log.push(tag + "n"); log.push(tag + (await q));
}
async function e(tag) { try { await null; throw 1; } catch (x) { log.push(tag + "c" + x); await false; log.push(tag + "f"); } finally { await -0; log.push(tag + "z"); } }
Promise.resolve().then(() => log.push("j1")).then(() => log.push("j2")).then(() => log.push("j3"));
a("A", 4); b("B"); c("C"); a("D", 3); d("E"); e("F");
Promise.resolve().then(() => log.push("q1"));
(async () => { await Symbol.iterator; log.push("sym"); })();
let fin = Promise.resolve(); for (let i = 0; i < 40; i++) fin = fin.then(() => {});
fin.then(() => console.log(log.join(" ")));
"##;

const EXPECTED: &str = "A0 B< C< D0 j1 A1 Bp Cthen D1 Eh0 En Fc1 q1 sym j2 A2 Bs C7 D2 Eh1 Ff j3 A3 B> Cbig Eh2 Fz Eh3 Eh4 E9";

#[test]
fn await_primitive_order_worker() {
    if std::env::var_os("ZIPP_AWAIT_PRIM_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn await_primitive_order_matches_node() {
    if std::env::var_os("ZIPP_AWAIT_PRIM_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [("default", None), ("direct-off", Some("ZIPP_NO_PRIMITIVE_AWAIT_DIRECT")), ("interpreter", Some("ZIPP_NOJIT"))] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["await_primitive_order_worker", "--exact", "--nocapture"])
            .env("ZIPP_AWAIT_PRIM_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_PRIMITIVE_AWAIT_DIRECT");
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