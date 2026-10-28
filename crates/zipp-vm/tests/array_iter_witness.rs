//! Array destructuring, the swap idiom, spreads and pattern-headed for-of in
//! compiled code while Array iteration changes every way the built-in check
//! (`ArrayIterWitness`) must see: @@iterator and `next` overwritten in place
//! and restored, a `return` added on each prototype of the chain and removed
//! (every close observed), @@iterator deleted and re-added, and
//! %ArrayIteratorPrototype%'s prototype replaced. Run interpreted, at the
//! default thresholds, with every function compiled at once, under GC stress
//! and with the witness off; the expected line is Node 24's.

const EXPECTED: &str = "plain=180000:60 iterator-overwritten=204000:68 iterator-restored=180000:60 next-overwritten=NaN:NaN next-restored=180000:60 return-on-array-iterator=180000:60 closes>0:true return-removed=180000:60 return-on-iterator-proto=180000:60 closes>0:true return-on-object-proto=180000:60 closes>0:true all-restored=180000:60 deleted:TypeError re-added=180000:60 reprotoed=180000:60 closes>0:true final=180000:60";

#[test]
fn array_iter_witness_child() {
    if std::env::var_os("ZIPP_ARRAY_ITER_WITNESS_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn array_iteration_changes_are_seen() {
    if std::env::var_os("ZIPP_ARRAY_ITER_WITNESS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("witness-off", Some(("ZIPP_NO_ARRAY_ITER_WITNESS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "array_iter_witness_child", "--nocapture"])
            .env("ZIPP_ARRAY_ITER_WITNESS_CHILD", "1")
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

const PROGRAM: &str = r####"// Array destructuring, the swap idiom, spreads and for-of with pattern
// heads in compiled code, while Array iteration is changed every way that
// must be seen: the default @@iterator and next overwritten in place,
// `return` added on each prototype of the chain, @@iterator deleted and
// restored, and %ArrayIteratorPrototype%'s prototype replaced.
const out = [];
function swap(h) { [h[0], h[1]] = [h[1], h[0]]; return h[0]; }
function pair(p) { const [a, b] = p; return a * 10 + b; }
function spread(a) { return [...a].length + Math.max(...a); }
function heads(ps) { let s = 0; for (const [k, v] of ps) s += k * v; return s; }
function all() { return swap([1, 2]) + pair([3, 4]) + spread([5, 6, 7]) + heads([[1, 2], [3, 4]]); }
function run(tag) { let t = 0; for (let r = 0; r < 3000; r++) t += all(); out.push(tag + "=" + t + ":" + all()); }
run("plain");
const AP = Array.prototype, AIP = Object.getPrototypeOf([][Symbol.iterator]()), IP = Object.getPrototypeOf(AIP);
const iter0 = AP[Symbol.iterator], next0 = AIP.next;
AP[Symbol.iterator] = function* () { for (let i = this.length - 1; i >= 0; i--) yield this[i]; };
run("iterator-overwritten");
AP[Symbol.iterator] = iter0;
run("iterator-restored");
AIP.next = function () { const r = next0.call(this); if (!r.done) r.value += 100; return r; };
run("next-overwritten");
AIP.next = next0;
run("next-restored");
let closes = 0;
AIP.return = function () { closes++; return {}; };
run("return-on-array-iterator");
out.push("closes>0:" + (closes > 0));
delete AIP.return;
run("return-removed");
IP.return = function () { closes++; return {}; };
closes = 0; run("return-on-iterator-proto"); out.push("closes>0:" + (closes > 0));
delete IP.return;
Object.prototype.return = function () { closes++; return {}; };
closes = 0; run("return-on-object-proto"); out.push("closes>0:" + (closes > 0));
delete Object.prototype.return;
run("all-restored");
delete AP[Symbol.iterator];
try { all(); out.push("no throw"); } catch (e) { out.push("deleted:" + e.constructor.name); }
AP[Symbol.iterator] = iter0;
run("re-added");
const proxyProto = { return() { closes++; return {}; } };
Object.setPrototypeOf(AIP, proxyProto);
closes = 0; run("reprotoed"); out.push("closes>0:" + (closes > 0));
Object.setPrototypeOf(AIP, IP);
run("final");
console.log(out.join(" "));
"####;
