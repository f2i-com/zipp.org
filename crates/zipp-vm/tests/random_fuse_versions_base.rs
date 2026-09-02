//! B205 random-scale fuse: the fused `Math.random() * k | 0` window (a user
//! seeded-xorshift `Math.random` override) must never read the heap versions
//! table through the pinned `r13`.
//!
//! The window is a `CallMethod` in bytecode, so nothing in Tier C's
//! `refetch_pinned` rule (GetProp/SetProp/method/leaf/MathOp) pins r13 for a
//! body whose only version reader is the fuse; entered through the cross3
//! native lane the body inherits the CALLER's r13, which is 0 when the caller
//! region pinned nothing. The release CLI segfaulted on the nanoid shape
//! below under `ZIPP_JIT_THRESHOLD=1` at the fuse's `cmp DWORD [r13 + Math*4]`
//! (the B207 ABA version guard). The guard now derives the base through the
//! VM-mirrored `versions_raw`, like every other probe in the window.
//!
//! Three programs, each compared byte-for-byte with the interpreter under
//! every execution mode and with node's output (hardcoded below, from
//! `node -e`): the exact crashing shape, `Math.random` as an accessor (the
//! plan builder declines an accessor slot, so the ordinary ops must run), and
//! `Math.random` re-assigned mid-loop (the baked `random` slot value misses,
//! then hits again). The interpreter child under JITLOG must also show the
//! fuse engaging, so a silent decline cannot pass this test for free.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const CHILD_ENV: &str = "ZIPP_RANDOM_FUSE_CHILD";
const MARK: &str = "RF|";

/// The exact crashing shape (bench/hostile nanoid-non-secure, reduced).
const REPRO: &str = r#"
let randomState = 0x9e3779b9;
Math.random = function () { randomState ^= randomState << 13; randomState ^= randomState >>> 17; randomState ^= randomState << 5; return (randomState >>> 0) / 4294967296; };
let urlAlphabet = "useandom-26T198340PX75pxJACKVERYMINDBUSHWOLF_GQZbfghjklqvwyzrict";
let nanoid = (size = 21) => { let id = ""; let i = size | 0; while (i-- > 0) { id += urlAlphabet[(Math.random() * 64) | 0] } return id };
let characters = 0; for (let i = 0; i < 2000; i++) { characters += nanoid(21).length; }
console.log("RF|repro chars", characters, "state", randomState, "next", nanoid(21));
"#;

/// `Math.random` installed as a getter returning the xorshift.
const GETTER: &str = r#"
let randomState = 0x9e3779b9;
let xs = function () { randomState ^= randomState << 13; randomState ^= randomState >>> 17; randomState ^= randomState << 5; return (randomState >>> 0) / 4294967296; };
let gets = 0;
Object.defineProperty(Math, "random", { get: function () { gets++; return xs; }, configurable: true });
let urlAlphabet = "useandom-26T198340PX75pxJACKVERYMINDBUSHWOLF_GQZbfghjklqvwyzrict";
let nanoid = (size = 21) => { let id = ""; let i = size | 0; while (i-- > 0) { id += urlAlphabet[(Math.random() * 64) | 0] } return id };
let characters = 0; for (let i = 0; i < 2000; i++) { characters += nanoid(21).length; }
console.log("RF|getter chars", characters, "state", randomState, "gets", gets, "next", nanoid(21));
"#;

/// `Math.random` re-assigned while the fused loop is hot: xorshift -> LCG ->
/// xorshift -> constant -> xorshift.
const REASSIGN: &str = r#"
let randomState = 0x9e3779b9;
let xs = function () { randomState ^= randomState << 13; randomState ^= randomState >>> 17; randomState ^= randomState << 5; return (randomState >>> 0) / 4294967296; };
let lcgCalls = 0;
let lcg = function () { lcgCalls++; randomState = (randomState * 1103515245 + 12345) | 0; return (randomState >>> 0) / 4294967296; };
Math.random = xs;
let urlAlphabet = "useandom-26T198340PX75pxJACKVERYMINDBUSHWOLF_GQZbfghjklqvwyzrict";
let nanoid = (size = 21) => { let id = ""; let i = size | 0; while (i-- > 0) { id += urlAlphabet[(Math.random() * 64) | 0] } return id };
let characters = 0; let mid = "";
for (let i = 0; i < 3000; i++) {
  if (i === 1000) { Math.random = lcg; }
  if (i === 1500) { mid = nanoid(21); Math.random = xs; }
  if (i === 2000) { Math.random = function () { return 0.5; }; }
  if (i === 2500) { Math.random = xs; }
  characters += nanoid(21).length;
}
console.log("RF|reassign chars", characters, "state", randomState, "lcg", lcgCalls, "mid", mid, "next", nanoid(21));
"#;

/// `node -e` (v24) output of the three programs, in order.
const EXPECTED: [&str; 3] = [
    "RF|repro chars 42000 state 412495058 next Y0EvmtVlY30jlf8bMSgkm",
    "RF|getter chars 42000 state 412495058 gets 42000 next Y0EvmtVlY30jlf8bMSgkm",
    "RF|reassign chars 63000 state 114684212 lcg 10521 mid d7g5lgAfmKWr0-5C_yGXg next OGhVLYlt2K-vwjZMsleBX",
];

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// The child: runs the three programs in ONE process (each `run` builds a
/// fresh VM, so the mode env applies to every one) and prints their marked
/// lines to stdout for the parent to compare.
#[test]
fn random_fuse_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    for src in [REPRO, GETTER, REASSIGN] {
        for line in run_ok(src) {
            println!("{line}");
        }
    }
}

const MODE_ENVS: [&str; 5] = [
    "ZIPP_NOJIT",
    "ZIPP_JIT_THRESHOLD",
    "ZIPP_NO_NURSERY",
    "ZIPP_NO_RANDOM_FUSE",
    "ZIPP_JITLOG",
];

fn spawn_child(env: &[(&str, &str)]) -> std::process::Output {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["--exact", "random_fuse_child", "--nocapture"])
        .env(CHILD_ENV, "1");
    for key in MODE_ENVS {
        cmd.env_remove(key);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("spawn mode child")
}

fn marked_lines(out: &std::process::Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with(MARK))
        .map(|l| l.to_string())
        .collect()
}

#[test]
fn random_fuse_matches_interpreter_in_all_modes() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let interp = spawn_child(&[("ZIPP_NOJIT", "1")]);
    assert!(
        interp.status.success(),
        "interpreter child failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&interp.stdout),
        String::from_utf8_lossy(&interp.stderr)
    );
    let baseline = marked_lines(&interp);
    assert_eq!(baseline, EXPECTED, "interpreter output differs from node");

    for (mode, env) in [
        ("default", &[][..]),
        ("forced-jit", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("no-nursery", &[("ZIPP_NO_NURSERY", "1")][..]),
        (
            "forced-jit+no-nursery",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_NO_NURSERY", "1")][..],
        ),
        (
            "forced-jit+fuse-off",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_NO_RANDOM_FUSE", "1")][..],
        ),
    ] {
        let out = spawn_child(env);
        assert!(
            out.status.success(),
            "{mode} child failed (status {:?}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            marked_lines(&out),
            baseline,
            "{mode} output differs from the interpreter"
        );
    }
}

/// The lane must actually engage on the repro under forced JIT, so the parity
/// test above exercises the fused window (and its cross3-entered body) rather
/// than a declined plan.
#[test]
fn random_fuse_engages_on_repro() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let out = spawn_child(&[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_JITLOG", "1")]);
    assert!(
        out.status.success(),
        "JITLOG child failed (status {:?}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(
        log.contains("RANDOM*64|0 fused"),
        "the random-scale fuse did not engage:\n{log}"
    );
    assert!(
        log.contains("CROSS3 fn") && log.contains("native-emitted lane"),
        "the cross3 native lane (the entry that inherits the caller's r13) did not engage:\n{log}"
    );
    assert_eq!(marked_lines(&out), EXPECTED);
}
