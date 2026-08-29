//! Tier-C entry-base mirror coverage.
//!
//! Whole-function code pins globals, heap versions and JIT IC storage in
//! callee-saved registers. Entry loads those bases from explicit VM mirrors;
//! allocation and later compilation exercise both movable backing vectors
//! before the original function is entered again. The historical three-helper
//! prologue remains available through `ZIPP_NO_TIERC_DIRECT_BASES=1`.

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
use std::process::Output;

fn source() -> String {
    let mut src = String::from(
        r#"
"use strict";
var bias = 11;
function probe(object) { return (object.x + bias) | 0; }
var stable = { x: 7 };
var sum = 0;
for (var warm = 0; warm < 80; warm++) sum = (sum + probe(stable)) | 0;
"#,
    );

    // Every function owns a distinct GetProp site. Compiling them after
    // `probe` forces repeated `ic_table` growth/reallocation.
    for i in 0..96 {
        src.push_str(&format!(
            "function site{i}(object) {{ return (object.x + {i}) | 0; }}\n"
        ));
    }
    for i in 0..96 {
        src.push_str(&format!(
            "for (var call{i} = 0; call{i} < 16; call{i}++) sum = (sum + site{i}(stable)) | 0;\n"
        ));
    }

    let retained = if std::env::var_os("ZIPP_GC_STRESS").is_some() {
        192
    } else {
        8_000
    };
    src.push_str(&format!(
        r#"
var retained = [];
for (var alloc = 0; alloc < {retained}; alloc++) retained.push({{ x: alloc, y: alloc + 1 }});
for (var again = 0; again < 256; again++) sum = (sum + probe(stable)) | 0;
console.log(sum + ":" + retained.length + ":" + probe(stable));
"#
    ));
    src
}

fn run_ok() -> Vec<String> {
    let out = zipp_vm::run(&source()).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn node_output() -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source())
        .output()
        .expect("node on PATH");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn tierc_direct_entry_bases_parity_after_growth() {
    assert_eq!(run_ok(), node_output());
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn child_log(disabled: bool) -> Output {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = std::process::Command::new(exe);
    cmd.args([
        "tierc_direct_entry_bases_parity_after_growth",
        "--exact",
        "--nocapture",
    ])
    .env("ZIPP_JITLOG", "1")
    .env("ZIPP_JIT_THRESHOLD", "8")
    .env_remove("ZIPP_NOJIT")
    .env_remove("ZIPP_NO_TIERC_DIRECT_BASES");
    if disabled {
        cmd.env("ZIPP_NO_TIERC_DIRECT_BASES", "1");
    }
    cmd.output().expect("spawn entry-base mechanism child")
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn assert_child(label: &str, out: &Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && !stdout.contains("running 0 tests"),
        "{label} child failed:\n{stdout}\n{stderr}"
    );
    stderr.into_owned()
}

#[test]
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn tierc_direct_entry_bases_mechanism_and_switch() {
    let direct = assert_child("direct", &child_log(false));
    assert!(
        direct.lines().any(|line| {
            line.contains("Tier-C entry-pins globals=1 version-ic=1")
                && line.contains("bases=direct")
        }),
        "direct mirror entry was not emitted:\n{direct}"
    );

    let helpers = assert_child("helpers", &child_log(true));
    assert!(
        helpers.lines().any(|line| {
            line.contains("Tier-C entry-pins globals=1 version-ic=1")
                && line.contains("bases=helpers")
        }),
        "off-switch did not restore helper entry:\n{helpers}"
    );
}

#[test]
fn tierc_direct_entry_bases_modes_remain_identical() {
    if std::env::var_os("ZIPP_TIERC_BASE_MODE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    for (label, env) in [
        ("helpers", Some(("ZIPP_NO_TIERC_DIRECT_BASES", "1"))),
        ("eager", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args([
            "tierc_direct_entry_bases_parity_after_growth",
            "--exact",
            "--nocapture",
        ])
        .env("ZIPP_TIERC_BASE_MODE_CHILD", "1")
        .env_remove("ZIPP_NO_TIERC_DIRECT_BASES")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_GC_STRESS")
        .env_remove("ZIPP_NOJIT");
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn entry-base mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && !stdout.contains("running 0 tests"),
            "{label} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
