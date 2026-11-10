//! A declared stack bounds native re-entry by the stack itself. Recursion that
//! nests the interpreter natively (a generator driven by `for...of` that calls
//! back into its driver, a callback that recurses through `forEach`) throws
//! the catchable RangeError once the next level would start below the floor
//! of the stack the host declared, not only at the re-entry depth cap. The Arm
//! release build's run loop takes more than `256 MiB / 4096` per level, so
//! `staging/sm/extensions/recursion.js` reached the cap's depth past the end
//! of the CLI's 256 MiB stack and aborted.
//!
//! The thread here has 64 MiB and declares 4: the floor must stop both
//! recursions well before the 4096-level cap, in every mode, with the error
//! still catchable and the program still running after it. An unoptimized
//! build is given 16 where 4 does not do: its interpreter's frame is several
//! times an optimized one's, and under 4 MiB its callback recursion stops at
//! or before the sixteenth level the program asks it to pass. Which of the
//! two a build needs is not something `cfg` says (the CI shards build with
//! debug assertions at opt-level 1, where 16 MiB lets the callback recursion
//! reach the cap first), so the program must give its output under one of
//! them.
//!
//! Native builds only: the sandbox profile's own caps (32 nested run loops,
//! 4,096 frames) stop both recursions long before any floor, and refuse the
//! 5,000-deep plain recursion the program ends with.
#![cfg(not(feature = "safe-sandbox"))]

const PROGRAM: &str = r#"var r, depth = 0, maxDepth = 0, err = "";
function* f() { r = arguments; test(); yield 170; }
function test() {
  depth++; if (depth > maxDepth) maxDepth = depth;
  try { for (var i of f()); } catch (e) { if (!err) err = e.constructor.name; }
  depth--;
}
test();
console.log("generator", err, maxDepth > 16, maxDepth < 4096, depth);
var d2 = 0, m2 = 0, e2 = "";
function viaCallback() { d2++; if (d2 > m2) m2 = d2; [1].forEach(viaCallback); d2--; }
try { viaCallback(); } catch (e) { e2 = e.constructor.name; }
console.log("callback", e2, m2 > 16, m2 < 4096);
function down(n) { return n === 0 ? 0 : 1 + down(n - 1); }
console.log("after", down(5000));
"#;

const EXPECTED: &str = "generator RangeError true true 0\ncallback RangeError true true\nafter 5000";

const THREAD_STACK: usize = 64 << 20;
/// The stacks declared, tried in turn: an optimized build's, then an
/// unoptimized one's.
const DECLARED: [usize; 2] = [4 << 20, 16 << 20];

#[test]
fn declared_stack_floor_child() {
    if std::env::var("ZIPP_STACK_FLOOR_CHILD").is_err() {
        return;
    }
    let mut got = String::new();
    for declared in DECLARED {
        let out = std::thread::Builder::new()
            .stack_size(THREAD_STACK)
            .spawn(move || {
                zipp_vm::set_native_stack_bytes(declared);
                zipp_vm::run(PROGRAM).expect("compiles")
            })
            .expect("thread")
            .join()
            .expect("no panic");
        assert_eq!(out.error, None, "{:?}", out.errput);
        got = out.output.join("\n");
        if got == EXPECTED {
            return;
        }
    }
    assert_eq!(got, EXPECTED, "under neither declared stack");
}

/// Each mode in a fresh process: an overflow aborts the process, which the
/// parent reports instead of dying with it.
#[test]
fn declared_stack_floor_all_modes() {
    let exe = std::env::current_exe().expect("test binary");
    for mode in [None, Some(("ZIPP_NOJIT", "1")), Some(("ZIPP_JIT_THRESHOLD", "1"))] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "declared_stack_floor_child", "--nocapture"])
            .env("ZIPP_STACK_FLOOR_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD");
        if let Some((k, v)) = mode {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{mode:?}: {:?}\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
