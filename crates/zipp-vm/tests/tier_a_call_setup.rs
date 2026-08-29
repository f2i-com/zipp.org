//! Tier A's native self-call window intentionally implements only the entry
//! state needed by its integer subset. Shapes that can observe omitted
//! formals, an `arguments` object, or sloppy-call `this` must stay on a tier
//! with full OrdinaryCall setup.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const CHILD_ENV: &str = "ZIPP_TIER_A_CALL_SETUP_CHILD";
const MARKER: &str = "[tier-a-call-setup] ";
const EXPECTED: &str = "undefined|1|true";

const SRC: &str = r#"
function omitted(n, value) {
  "use strict";
  if (n <= 0) return value;
  if (n === 4) return omitted(n - 1, 7);
  if (n === 2) return omitted(n - 1, 7);
  return omitted(n - 1);
}

function argObject(n) {
  "use strict";
  if (n <= 0) return arguments;
  return argObject(n - 1);
}

function sloppyThis(n) {
  if (n <= 0) return this;
  return sloppyThis(n - 1);
}

var omittedResult;
var argumentsLength = 0;
var sloppyResult = false;
for (var i = 0; i < 64; i++) {
  omittedResult = omitted(4 + (i & 1), 9);
  argumentsLength = argObject(3, 7).length;
  sloppyResult = sloppyThis(3) === globalThis;
}
console.log(omittedResult + "|" + argumentsLength + "|" + sloppyResult);
"#;

fn child_run() {
    let outcome = zipp_vm::run(SRC).expect("call-setup source compiles");
    assert!(
        outcome.error.is_none(),
        "unexpected call-setup error: {:?}",
        outcome.error
    );
    assert_eq!(outcome.output, [EXPECTED]);
    eprintln!("{MARKER}{}", outcome.output[0]);
}

fn run_fresh(nojit: bool) -> std::process::Output {
    let mut command = std::process::Command::new(std::env::current_exe().expect("test exe"));
    command
        .arg("tier_a_incomplete_call_setup_stays_interpreted")
        .arg("--exact")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NOJIT");
    if nojit {
        command.env("ZIPP_NOJIT", "1");
    }
    command.output().expect("spawn call-setup child")
}

fn marker(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .find_map(|line| line.split_once(MARKER).map(|(_, value)| value.to_string()))
        .unwrap_or_else(|| {
            panic!(
                "missing {MARKER:?} in:\n{}",
                String::from_utf8_lossy(stderr)
            )
        })
}

#[test]
fn tier_a_incomplete_call_setup_stays_interpreted() {
    if std::env::var_os(CHILD_ENV).is_some() {
        child_run();
        return;
    }

    let enabled = run_fresh(false);
    assert!(
        enabled.status.success(),
        "JIT-enabled child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&enabled.stdout),
        String::from_utf8_lossy(&enabled.stderr)
    );
    let enabled_err = String::from_utf8_lossy(&enabled.stderr);
    assert_eq!(marker(&enabled.stderr), EXPECTED);
    assert!(
        enabled_err.contains("[jit] Tier C"),
        "unsafe shapes did not get hot enough to exercise tier selection:\n{enabled_err}"
    );
    assert!(
        !enabled_err.contains("[jit] Tier A"),
        "incomplete OrdinaryCall setup reached Tier A:\n{enabled_err}"
    );

    let nojit = run_fresh(true);
    assert!(
        nojit.status.success(),
        "interpreter child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&nojit.stdout),
        String::from_utf8_lossy(&nojit.stderr)
    );
    assert_eq!(marker(&nojit.stderr), EXPECTED);
    assert_eq!(marker(&enabled.stderr), marker(&nojit.stderr));
}
