//! `.length` of a dense Array read by compiled code through its array mirror
//! entry (`emit_arr_mirror_len`), through every way an array's length can
//! change between two reads: push and pop, `length` set larger (a virtual
//! length) and smaller, sparse writes past the dense end, `new Array(n)`,
//! splice, reverse, fill, copyWithin, shift and unshift, a frozen array, a
//! non-writable `length`, a mapped `arguments` object, an Array subclass,
//! a loop that shrinks its own array, and the non-Array receivers the same
//! site sees. Each read follows a compiled element read, which is what
//! settles an entry. Run interpreted, at the default thresholds, with every
//! function compiled at once and with the read switched off; the expected
//! line is Node 24's.

const EXPECTED: &str = "4,1,5,3,100,,100,2,2,1001,7,1001,5000001,50,50,1,6,5,4,7,7,7,7,8,2,0,3/9,0/9,3,4,100,11,0,1,4,x,3,2,4,4,4";

#[test]
fn array_len_mirror_child() {
    if std::env::var_os("ZIPP_ARRAY_LEN_MIRROR_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn array_lengths_read_through_the_mirror_stay_exact() {
    if std::env::var_os("ZIPP_ARRAY_LEN_MIRROR_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("mirror-read-off", Some(("ZIPP_NO_ARR_LEN_MIRROR", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "array_len_mirror_child", "--nocapture"])
            .env("ZIPP_ARRAY_LEN_MIRROR_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD");
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

const PROGRAM: &str = r####"// `.length` read by compiled code through the array mirror, through every way
// an array's length can change between two reads.
const out = [];
function at(a, i) { return a[i]; }
function rawlen(a) { return a.length; }
function len(a) { at(a, 0); return rawlen(a); }
function hot(a, n) { let s = 0; for (let i = 0; i < n; i++) s += len(a); return s; }
const warm = [1, 2, 3];
for (let r = 0; r < 3000; r++) { len(warm); hot(warm, 3); at(warm, r % 3); }
const a = [1, 2, 3, 4];
out.push(len(a), a[0]);
a.push(5); out.push(len(a));
a.pop(); a.pop(); out.push(len(a));
a.length = 100; out.push(len(a), a[50], len(a));
a.length = 2; out.push(len(a), a[1]);
a[1000] = 7; out.push(len(a), a[1000], len(a));
a[5000000] = 1; out.push(len(a));
const b = new Array(50); out.push(len(b)); b[0] = 1; out.push(len(b), b[0]);
const c = Array.from({ length: 6 }, (_, i) => i); out.push(len(c), c[5]);
c.splice(1, 2); out.push(len(c)); c.splice(0, 0, 9, 9, 9); out.push(len(c));
c.reverse(); out.push(len(c)); c.fill(0); out.push(len(c)); c.copyWithin(0, 1); out.push(len(c));
c.shift(); c.unshift(1, 2); out.push(len(c));
const f = Object.freeze([1, 2]); out.push(len(f));
const g = []; Object.defineProperty(g, "length", { writable: false }); out.push(len(g));
function args() { const r = len(arguments); arguments.length = 9; return r + "/" + len(arguments); }
out.push(args(1, 2, 3), args());
class Sub extends Array {} const s = Sub.of(1, 2, 3); out.push(len(s)); s.push(4); out.push(len(s));
const e = []; for (let i = 0; i < 100; i++) { e.push(i); if (len(e) !== i + 1) out.push("bad" + i); }
out.push(len(e));
const big = []; big[10] = 1; out.push(len(big)); big.length = 0; out.push(len(big)); big.push(1); out.push(len(big));
out.push(len("abcd"), len({ length: "x" }), len(new Uint8Array(3)), len(function (p, q) {}));
const holes = [1, , 3, , ]; out.push(len(holes));
const shrinkLoop = [1, 2, 3, 4, 5, 6];
let sl = 0; for (let i = 0; i < len(shrinkLoop); i++) { if (i === 2) shrinkLoop.length = 4; sl++; }
out.push(sl, len(shrinkLoop));
console.log(out.join(","));
"####;
