//! A function whose first instruction is a loop's head (`while (n > 0) n--;`
//! as the start of its body), with Tier O on. Tier O builds a function's
//! entry block from its parameters alone, so a jump back to the first
//! instruction found the values the loop carries reset to the parameters on
//! every iteration: a debug build's assertion failed (`the entry has no
//! predecessor`, issue #93) and a release build never left the loop. Tier O
//! declines such a function now, and Tier C keeps it.
//!
//! Each mode runs in a process of its own, which the parent stops after a
//! minute: the failure this guards against is a loop that does not end.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = "w:0 fo:0 dw:-375 wb:0 wr:0 carry:4625744 two:55388 nest:78750 f:0";

const PROGRAM: &str = r####"// Loops whose head is the function's first instruction: while, for,
// do-while, while (true) with a break and with a return, a loop that carries
// a second value, one with two back-edges, a nest, and the issue's own.
function w(n) { while (n > 0) n--; return n; }
function fo(n) { for (; n > 0; n--) {} return n; }
function dw(n) { do { n--; } while (n > 0); return n; }
function wb(n) { while (true) { if (n <= 0) break; n--; } return n; }
function wr(n) { while (true) { if (n <= 0) return n; n--; } }
function carry(n, a) { while (n > 0) { a = (a + n) | 0; n--; } return a; }
function two(n, m) { while (n > 0) { n--; if (n & 1) continue; m += n; } return m; }
function nest(n) { var t = 0; while (n > 0) { var k = n; while (k > 0) { t += k; k--; } n--; } return t; }
function f(n) {
  while (n > 0) n--;
  return n;
}
var out = [];
for (var name of ["w", "fo", "dw", "wb", "wr"]) {
  var fn = { w, fo, dw, wb, wr }[name];
  var s = 0;
  for (var i = 0; i < 3000; i++) s += fn(i & 7);
  out.push(name + ":" + s);
}
var c = 0;
for (var i = 0; i < 3000; i++) c = (c + carry(i & 15, i)) | 0;
out.push("carry:" + c);
var t = 0;
for (var i = 0; i < 3000; i++) t += two(i & 15, 1);
out.push("two:" + t);
var q = 0;
for (var i = 0; i < 3000; i++) q += nest(i & 7);
out.push("nest:" + q);
let sum = 0;
for (let i = 0; i < 1600; i++) sum += f(3);
out.push("f:" + sum);
console.log(out.join(" "));
"####;

/// Every switch a mode below may set, cleared before each run.
const SWITCHES: [&str; 4] = ["ZIPP_OPT", "ZIPP_NO_OPT_TIER_UP", "ZIPP_NOJIT", "ZIPP_JIT_THRESHOLD"];

#[test]
fn entry_loop_worker() {
    if std::env::var_os("ZIPP_ENTRY_LOOP_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn entry_loops_end_in_every_mode() {
    let exe = std::env::current_exe().expect("test binary");
    let modes: [(&str, &[(&str, &str)]); 5] = [
        ("tier O at once", &[("ZIPP_OPT", "1"), ("ZIPP_NO_OPT_TIER_UP", "1")]),
        ("tier O after tier C", &[("ZIPP_OPT", "1")]),
        ("tier O, every function compiled at once", &[("ZIPP_OPT", "1"), ("ZIPP_NO_OPT_TIER_UP", "1"), ("ZIPP_JIT_THRESHOLD", "1")]),
        ("default", &[]),
        ("interpreter", &[("ZIPP_NOJIT", "1")]),
    ];
    for (mode, env) in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "entry_loop_worker", "--nocapture"])
            .env("ZIPP_ENTRY_LOOP_WORKER", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for s in SWITCHES {
            cmd.env_remove(s);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn the test binary");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().expect("wait") {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{mode}: the program did not end within a minute");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let out = child.wait_with_output().expect("output");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            status.success() && stdout.contains("1 passed"),
            "{mode}: {status:?}\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
