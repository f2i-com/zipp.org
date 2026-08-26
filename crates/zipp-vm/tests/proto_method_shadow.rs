//! B191: primitive-prototype method SHADOWS must be honored everywhere.
//!
//! `String.prototype.charCodeAt = f` (and friends) historically kept calling
//! the intrinsic: the name-dispatched builtin fast paths — interpreter AND
//! the JIT's dedicated helpers AND the INT-tier's raw string-pin loads —
//! resolved by receiver kind + name alone. The fix proves `live prototype
//! slot bits == the boot baseline` (`Vm::capture_proto_baselines`) before
//! serving an intrinsic, memoized in the B183 (version, slot, VALUE-bits)
//! form because an in-place overwrite bumps no version.
//!
//! Each case HEATS the intrinsic first (past every compile threshold), then
//! shadows, then re-runs — so the shadow lands on already-compiled code. The
//! expected outputs are node's, byte for byte; the harness's own use of
//! `.push` after the Array shadow is part of the oracle (node appends 6, not
//! 7, post values). Child processes re-run the probe under `ZIPP_NOJIT=1`
//! and `ZIPP_JIT_THRESHOLD=1` so every tier answers identically.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const PROBE: &str = r#"
"use strict";
function hotCCA(s, n){ var a=0; for (var i=0;i<n;i++) a=(a+s.charCodeAt(0))|0; return a; }
function hotIdx(s, n){ var a=0; for (var i=0;i<n;i++) a=(a+s.indexOf("b"))|0; return a; }
function hotSub(s, n){ var a=0; for (var i=0;i<n;i++) a=(a+s.substring(0,2).length)|0; return a; }
function hotSlice(s, n){ var a=0; for (var i=0;i<n;i++) a=(a+s.slice(0,2).length)|0; return a; }
function hotUp(s, n){ var a=0; for (var i=0;i<n;i++) a=(a+s.toUpperCase().length)|0; return a; }
function hotPush(n){ var a=[]; var c=0; for (var i=0;i<n;i++){ a.length=0; c=(c+a.push(1))|0; } return c; }
function hotGet(m, n){ var a=0; for (var i=0;i<n;i++) a=(a+m.get(1))|0; return a; }
var N = 60000;
var pre = [hotCCA("abc",N), hotIdx("abc",N), hotSub("abc",N), hotSlice("abc",N), hotUp("abc",N), hotPush(N)];
var m = new Map([[1,5]]);
pre.push(hotGet(m,N));
String.prototype.charCodeAt = function(){ return 7; };
String.prototype.indexOf = function(){ return 9; };
String.prototype.substring = function(){ return "xxxx"; };
String.prototype.slice = function(){ return "yyyyy"; };
String.prototype.toUpperCase = function(){ return "zzzzzz"; };
Array.prototype.push = function(){ return 42; };
Map.prototype.get = function(){ return 11; };
var post = [hotCCA("abc",N), hotIdx("abc",N), hotSub("abc",N), hotSlice("abc",N), hotUp("abc",N), hotPush(N)];
post.push(hotGet(m,N));
console.log(pre.join(","));
console.log(post.join(","));
"#;

/// Node's answers, captured from `node` on this exact probe. The post line has
/// SIX values: the shadowed `Array.prototype.push` means the harness's own
/// `post.push(...)` no longer appends.
const EXPECTED: [&str; 2] = [
    "5820000,60000,120000,120000,180000,60000,300000",
    "420000,540000,240000,300000,360000,2520000",
];

#[test]
fn prototype_method_shadows_are_honored_in_process() {
    let out = zipp_vm::run(PROBE).expect("probe compiles");
    assert!(out.error.is_none(), "probe error: {:?}", out.error);
    assert_eq!(out.output, EXPECTED);
}

/// The same probe re-run in CHILD PROCESSES under each tier mode (the
/// process-global latches make in-process env switching unreliable — the
/// concat_chain precedent). Each child re-executes the in-process test above
/// under the mode and must pass identically.
#[test]
fn prototype_method_shadows_are_honored_across_tiers() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("prototype_method_shadows_are_honored_in_process")
            .arg("--exact")
            .env(key, val)
            .output()
            .expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{key}={val} mode failed:
{stdout}
{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the filter matched nothing under {key}={val}:
{stdout}"
        );
    }
}
