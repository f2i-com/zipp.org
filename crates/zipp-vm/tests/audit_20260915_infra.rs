//! The Test262 host report (`ZIPP_REPORT_UNHANDLED=1`), 15 September 2026
//! review R256.
//!
//! A positive Test262 test whose assertion fails inside a promise reaction,
//! an async function body or a timer callback used to exit 0 with nothing on
//! stderr, so tools/run_test262.py scored it PASS. With the report on, the
//! program's error lines name every exception that ESCAPED A JOB and whose
//! promise never gained a handler; the runner fails a positive test whose
//! report names a Test262Error. A throw a timer callback does not catch needs
//! no report: it ends the program as the run's error, as it does in node.
//!
//! The narrowing to jobs that threw is load-bearing. An unhandled REJECTION is
//! not a Test262 failure, and the corpus relies on that: about sixteen
//! built-ins/Promise/{all,allSettled,race} tests reject with a `Test262Error`
//! on purpose and never handle it. Reporting every unhandled rejection scored
//! all of them FAIL and broke the gate.
//!
//! Its price is a known residual: a reaction that RETURNS a rejected promise
//! is ADOPTION, not a throw. `then_internal` marks the adopted promise handled
//! and rejects the dependent by pass-through, so neither is tagged and nothing
//! is reported. An `async` reaction callback whose body throws lands there too.
//! That is deliberate — the alternative fails the gate on the corpus tests
//! above — and it is why the report is a supplement to the runner's rules, not
//! a substitute for them.
//!
//! What is pinned: each lost-failure shape is reported, with the reason's
//! `Name: message`; a promise rejected by an ordinary `reject(value)` — the
//! corpus shape — is never reported however hostile its value; every handled
//! shape (late `catch`, `await`, combinators, `finally`, adoption of a pending
//! inner promise, async functions) reports nothing; a rejection handled by one
//! `.then` link reports only the unhandled leaf; and nothing is reported
//! without the switch. The reported
//! promises are GC roots (a `.then` dependent is unreachable once its reaction
//! ran), so each mode runs in a clean child process under the default,
//! interpreter, forced-JIT and GC-stress tiers. A throwing timer ends the run
//! in every mode, dropping the timers behind it.

const MARK: &str = "zipp: unhandled exception in promise job: ";

fn run(src: &str) -> (Vec<String>, Vec<String>) {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    (out.output, out.errput)
}

/// The same, for a program an uncaught throw ENDS: its error is the run's.
fn run_failing(src: &str) -> (Vec<String>, Vec<String>, Option<String>) {
    let out = zipp_vm::run(src).expect("source compiles");
    (out.output, out.errput, out.error)
}

const HARNESS: &str = "function Test262Error(message) { this.message = message || ''; }\n";

/// Each lost failure, alone, and the single line it must produce. Every one of
/// them is an exception thrown by the job that was running.
const LOST: &[(&str, &str)] = &[
    (
        "Promise.resolve().then(function () { throw new Test262Error('deferred assertion'); });",
        "zipp: unhandled exception in promise job: Test262Error: deferred assertion",
    ),
    (
        "(async function () { await 0; throw new Test262Error('async body'); })();",
        "zipp: unhandled exception in promise job: Test262Error: async body",
    ),
    (
        "Promise.resolve().finally(function () { throw new Test262Error('finally body'); });",
        "zipp: unhandled exception in promise job: Test262Error: finally body",
    ),
    (
        "Promise.resolve().then(function () { return { then: function () { throw new Test262Error('thenable job'); } }; });",
        "zipp: unhandled exception in promise job: Test262Error: thenable job",
    ),
    (
        "(async function* () { throw new Test262Error('async generator'); })().next();",
        "zipp: unhandled exception in promise job: Test262Error: async generator",
    ),
    (
        // An engine-raised error keeps the location the CLI would print.
        "Promise.resolve().then(function () { null.x; });",
        "zipp: unhandled exception in promise job: TypeError: Cannot read properties of null (reading 'x') (in <script>)",
    ),
];

/// Ordinary rejections: nothing threw, so Test262 does not score them and the
/// report must stay silent. The last three are the exact shapes of
/// built-ins/Promise/all/invoke-then-error-close.js and its siblings.
const NOT_A_FAILURE: &str = r#"
    Promise.reject(new Test262Error('never handled'));
    Promise.reject(new RangeError('leaf')).then(function () {});
    Promise.reject(3);
    new Promise(function (_, reject) { reject(new Test262Error('explicit reject')); });
    Promise.all([Promise.reject(new Test262Error('combinator'))]);
    var poisoned = {};
    poisoned[Symbol.iterator] = function () {
        return { next: function () { throw new Test262Error('iterator step'); },
                 return: function () { return {}; } };
    };
    Promise.all(poisoned);
"#;

/// Every rejection here is handled, some only after it settled.
const HANDLED: &str = r#"
    var p = Promise.reject(new Error('late'));
    Promise.resolve().then(function () { return p.catch(function () { console.log('late'); }); });
    Promise.reject(1).then(null, function () { console.log('then'); });
    Promise.resolve().then(function () { throw new Error('x'); }).catch(function () { console.log('catch'); });
    var settle;
    var inner = new Promise(function (_, reject) { settle = reject; });
    Promise.resolve().then(function () { return inner; }).catch(function () { console.log('adopted'); });
    Promise.resolve().then(function () { settle(new Error('inner')); });
    (async function () { try { await Promise.reject(new Error('aw')); } catch (e) { console.log('await'); } })();
    async function thrower() { throw new Error('af'); }
    thrower().catch(function () { console.log('async-fn'); });
    Promise.all([Promise.reject(new Error('all'))]).catch(function () { console.log('all'); });
    Promise.allSettled([Promise.reject(new Error('as'))]).then(function () { console.log('allSettled'); });
    Promise.any([Promise.reject(new Error('any'))]).catch(function () { console.log('any'); });
    Promise.race([Promise.reject(new Error('race'))]).catch(function () { console.log('race'); });
    Promise.reject(new Error('fin')).finally(function () {}).catch(function () { console.log('finally'); });
    new Promise(function (_, reject) { setTimeout(function () { reject(new Error('t')); }, 0); })
        .then(null, function () { console.log('timer-reject'); });
"#;

/// A warmed loop: every thrown job but the last is handled.
const HOT: &str = r#"
    function step(i, last) {
        var p = Promise.resolve().then(function () { throw new Test262Error('iteration ' + i); });
        if (!last) p.catch(function () {});
        return p;
    }
    for (var i = 0; i < 3000; i++) step(i, i === 2999);
"#;

#[test]
fn report_child() {
    if std::env::var_os("ZIPP_INFRA_REPORT_CHILD").is_none() {
        return;
    }
    for (body, line) in LOST {
        let (out, err) = run(&format!("{HARNESS}{body}"));
        assert!(out.is_empty(), "{body}: {out:?}");
        assert_eq!(err, [*line], "{body}");
    }
    // The corpus shapes: rejected, never handled, and correctly not a failure.
    let (out, err) = run(&format!("{HARNESS}{NOT_A_FAILURE}"));
    assert!(out.is_empty(), "{out:?}");
    assert!(err.is_empty(), "an ordinary rejection was reported: {err:?}");
    let (out, err) = run(HANDLED);
    assert!(err.is_empty(), "handled rejections were reported: {err:?}");
    let mut seen = out.clone();
    seen.sort();
    assert_eq!(
        seen,
        [
            "adopted", "all", "allSettled", "any", "async-fn", "await", "catch", "finally",
            "late", "race", "then", "timer-reject"
        ],
        "{out:?}"
    );
    let (out, err) = run(&format!("{HARNESS}{HOT}"));
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(err, [format!("{MARK}Test262Error: iteration 2999")]);
    // A throw a timer callback does not catch ENDS the program, as it does in
    // node: the queue is dropped, the later timer never runs, and the throw is
    // the run's error — which is already a Test262 failure without a report.
    let (out, err, error) = run_failing(
        "setTimeout(function () { throw new TypeError('in-timer'); }, 0);\n\
         setTimeout(function () { console.log('next timer'); }, 1);",
    );
    assert!(out.is_empty(), "{out:?}");
    assert!(err.is_empty(), "{err:?}");
    assert_eq!(error.as_deref(), Some("TypeError: in-timer"));
}

#[test]
fn quiet_child() {
    if std::env::var_os("ZIPP_INFRA_QUIET_CHILD").is_none() {
        return;
    }
    for (body, _) in LOST {
        let (_, err) = run(&format!("{HARNESS}{body}"));
        assert!(err.is_empty(), "reported without the switch: {err:?}");
    }
    let (out, err, error) =
        run_failing("setTimeout(function () { throw new TypeError('t'); }, 0);\n\
                     setTimeout(function () { console.log('next'); }, 1);");
    assert!(out.is_empty(), "{out:?}");
    assert!(err.is_empty(), "{err:?}");
    assert_eq!(error.as_deref(), Some("TypeError: t"));
}

#[test]
fn report_modes_match() {
    if std::env::var_os("ZIPP_INFRA_REPORT_CHILD").is_some()
        || std::env::var_os("ZIPP_INFRA_QUIET_CHILD").is_some()
    {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let mut runs: Vec<(&str, &str, Vec<(&str, &str)>)> = Vec::new();
    for (mode, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
    ] {
        runs.push((
            "report_child",
            mode,
            env.into_iter()
                .chain([("ZIPP_INFRA_REPORT_CHILD", "1"), ("ZIPP_REPORT_UNHANDLED", "1")])
                .collect(),
        ));
    }
    runs.push(("quiet_child", "switch-off", vec![("ZIPP_INFRA_QUIET_CHILD", "1")]));
    runs.push((
        "quiet_child",
        "switch-not-1",
        vec![("ZIPP_INFRA_QUIET_CHILD", "1"), ("ZIPP_REPORT_UNHANDLED", "0")],
    ));
    for (test, mode, env) in runs {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", test, "--nocapture"])
            // An inherited setting must not decide what a mode tests.
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_REPORT_UNHANDLED");
        for (key, value) in env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{test}/{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
