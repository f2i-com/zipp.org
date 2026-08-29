//! Safe-profile hard limits for work and transient storage inside one RegExp
//! instruction. These failures are host-terminal: guest `try`/`catch` cannot
//! resume execution after the regex engine reaches a ceiling.

#![cfg(feature = "safe-sandbox")]

use zipp_vm::embed::{self, HostValue, ScriptState};

fn slot(state: &ScriptState, name: &str) -> u32 {
    state
        .symbols()
        .into_iter()
        .find(|symbol| symbol.name == name)
        .unwrap_or_else(|| panic!("missing global function {name}"))
        .index
}

fn assert_regex_memory_failure(source: &str, function: &str, headroom: usize) {
    let mut state = embed::compile_script(source).expect("script compiles");
    state.run_init().expect("script initializes");
    state.set_limits(10_000_000, None);
    let baseline = state.heap_bytes();
    state.set_heap_limit(baseline + headroom);

    let message = state
        .call_slot(slot(&state, function), &[])
        .expect_err("regex materialization must respect heap headroom");
    assert!(
        message.contains("regular expression exceeded its backtrack memory budget"),
        "unexpected failure: {message}"
    );
    assert_eq!(state.resource_limit_error(), Some(message.as_str()));
}

#[test]
fn catastrophic_backtracking_is_a_sticky_host_failure() {
    let mut state = embed::compile_script(
        r#"
        function attack() {
            try {
                return /(a|aa)+$/.test("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa!");
            } catch (error) {
                // A normal JS RangeError could reach this and resume. A host
                // resource failure must stop before this assignment executes.
                globalThis.escaped = true;
                return "caught";
            }
        }
        "#,
    )
    .expect("script compiles");
    state.run_init().expect("script initializes");
    state.set_limits(512, None);

    let result = state.call_slot(slot(&state, "attack"), &[]);
    let message = result.expect_err("catastrophic match must stop");
    assert!(
        message.contains("regular expression exceeded its execution budget"),
        "unexpected failure: {message}"
    );
    assert_eq!(state.resource_limit_error(), Some(message.as_str()));
    assert!(state.steps_used() <= 512, "regex work overspent VM gas");
}

#[test]
fn transient_backtrack_stack_obeys_remaining_heap_headroom() {
    let mut state = embed::compile_script(
        r#"
        function matchOnce() {
            return /(a|aa)+$/.test("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa!");
        }
        "#,
    )
    .expect("script compiles");
    state.run_init().expect("script initializes");
    state.set_limits(100_000, None);
    let baseline = state.heap_bytes();
    // Leave enough room for the ordinary call frame but far less than this
    // failing search's explicit backtrack state.
    state.set_heap_limit(baseline + 8 * 1024);

    let result = state.call_slot(slot(&state, "matchOnce"), &[]);
    let message = result.expect_err("backtrack stack must respect heap headroom");
    assert!(
        message.contains("regular expression exceeded its backtrack memory budget"),
        "unexpected failure: {message}"
    );
    assert_eq!(state.resource_limit_error(), Some(message.as_str()));
}

#[test]
fn normal_ascii_and_utf16_matching_remain_available() {
    let mut state = embed::compile_script(
        r#"
        function normal() {
            return [/a+/g.test("baaa"), /\u{1F600}/u.test("x😀y")].join(",");
        }
        "#,
    )
    .expect("script compiles");
    state.run_init().expect("script initializes");
    state.set_limits(100_000, None);

    let value = state
        .call_slot(slot(&state, "normal"), &[])
        .expect("ordinary matches stay below the ceilings");
    assert_eq!(value, HostValue::String("true,true".into()));
    assert_eq!(state.resource_limit_error(), None);
}

#[test]
fn utf16_subject_copy_is_preflighted_for_every_regexp_entry_path() {
    let source = r#"
        let largeUtf16Subject = "é".repeat(257);
        for (let i = 0; i < 9; i++) {
            largeUtf16Subject = largeUtf16Subject + largeUtf16Subject;
        }
        const noMatch = /z/;

        function builtinExec() {
            return noMatch.test(largeUtf16Subject);
        }
        function intrinsicReplace() {
            return largeUtf16Subject.replace(noMatch, "");
        }
        function protocolMatch() {
            const receiver = {
                flags: "g",
                lastIndex: 0,
                exec: function () { return null; }
            };
            return RegExp.prototype[Symbol.match].call(receiver, largeUtf16Subject);
        }
        function protocolReplace() {
            const receiver = {
                flags: "",
                lastIndex: 0,
                exec: function () { return null; }
            };
            return RegExp.prototype[Symbol.replace].call(
                receiver,
                largeUtf16Subject,
                ""
            );
        }
        function protocolSplit() {
            return RegExp.prototype[Symbol.split].call(noMatch, largeUtf16Subject);
        }
    "#;

    for function in [
        "builtinExec",
        "intrinsicReplace",
        "protocolMatch",
        "protocolReplace",
        "protocolSplit",
    ] {
        assert_regex_memory_failure(source, function, 64 * 1024);
    }
}

#[test]
fn utf16_subject_reservation_releases_after_success_and_guest_throw() {
    let mut state = embed::compile_script(
        r#"
        let retainedUtf16Subject = "é".repeat(257);
        for (let i = 0; i < 7; i++) {
            retainedUtf16Subject = retainedUtf16Subject + retainedUtf16Subject;
        }
        const retainedNoMatch = /z/;

        function exerciseReservationLifetime() {
            retainedNoMatch.test(retainedUtf16Subject);
            retainedNoMatch.test(retainedUtf16Subject);

            const throwingReceiver = {
                flags: "g",
                lastIndex: 0,
                exec: function () { throw new Error("expected"); }
            };
            try {
                RegExp.prototype[Symbol.match].call(
                    throwingReceiver,
                    retainedUtf16Subject
                );
            } catch (error) {
                // The protocol helper's scoped UTF-16 reservation must unwind
                // before this catch resumes guest execution.
            }

            retainedNoMatch.test(retainedUtf16Subject);
            return "ok";
        }
        "#,
    )
    .expect("script compiles");
    state.run_init().expect("script initializes");
    state.set_limits(1_000_000, None);
    let baseline = state.heap_bytes();
    // One 32K-unit subject copy is 64 KiB. This admits one copy plus ordinary
    // call state, but not two leaked or double-charged copies.
    state.set_heap_limit(baseline + 112 * 1024);

    let value = state
        .call_slot(slot(&state, "exerciseReservationLifetime"), &[])
        .expect("sequential and unwound subject copies reuse their headroom");
    assert_eq!(value, HostValue::String("ok".into()));
    assert_eq!(state.resource_limit_error(), None);
}

#[test]
fn ascii_and_utf16_subject_protocol_results_stay_exact() {
    let mut state = embed::compile_script(
        r#"
        function subjectProtocolResults() {
            const astralRope = "a".repeat(256) + "\uD83D" + "\uDE00" + "b";
            const emptyUnicode = /(?:)/gu;
            emptyUnicode.lastIndex = 256;
            const emptyIterator = astralRope.matchAll(emptyUnicode);
            emptyIterator.next();
            const nullMatchReceiver = {
                flags: "g",
                lastIndex: 0,
                exec: function () { return null; }
            };
            const nullReplaceReceiver = {
                flags: "",
                lastIndex: 0,
                exec: function () { return null; }
            };
            return [
                /a+/.test("baaa"),
                /\u{1F600}/u.exec("x😀y").index,
                /\u{1F600}/u.exec(astralRope).index,
                emptyIterator.next().value.index,
                "café".replace(/é/, "E"),
                "A😀B".split(/😀/u).join("|"),
                RegExp.prototype[Symbol.match].call(nullMatchReceiver, "é") === null,
                RegExp.prototype[Symbol.replace].call(
                    nullReplaceReceiver,
                    "é",
                    "unused"
                )
            ].join(",");
        }
        "#,
    )
    .expect("script compiles");
    state.run_init().expect("script initializes");
    state.set_limits(1_000_000, None);

    let value = state
        .call_slot(slot(&state, "subjectProtocolResults"), &[])
        .expect("ordinary ASCII and UTF-16 protocols remain available");
    assert_eq!(
        value,
        HostValue::String("true,1,256,258,cafE,A|B,true,é".into())
    );
    assert_eq!(state.resource_limit_error(), None);
}

#[test]
fn global_replace_cannot_accumulate_unbounded_match_captures() {
    // Most groups sit in an alternative that never participates, but every
    // Match would still carry a slot for each one. An empty alternative then
    // produces one global match per subject position: this used to multiply
    // unmetered native Vec allocations before the VM saw the result.
    // Include a named group: emitted matches share the compiled name table via
    // Arc rather than deep-cloning its pointer table and strings per hit.
    let groups = format!("(?<held>a){}", "()".repeat(255));
    let subject = "a".repeat(20_000);
    let source = format!(
        r#"
        function attackReplace() {{
            return "{}".replace(/(?:{}z|)/g, "x");
        }}
        "#,
        subject, groups
    );
    let mut state = embed::compile_script(&source).expect("script compiles");
    state.run_init().expect("script initializes");
    state.set_limits(10_000_000, None);
    let baseline = state.heap_bytes();
    state.set_heap_limit(baseline + 512 * 1024);

    let result = state.call_slot(slot(&state, "attackReplace"), &[]);
    let message = result.expect_err("retained match captures must be bounded");
    assert!(
        message.contains("regular expression exceeded its backtrack memory budget"),
        "unexpected failure: {message}"
    );
    assert_eq!(state.resource_limit_error(), Some(message.as_str()));
}

fn assert_recursive_functional_replace_is_bounded() {
    // A custom exec returns the SAME match object 12,000 times. Its guest heap
    // footprint is therefore constant while @@replace grows a native result
    // Vec to about 128 KiB and retains it across the first functional replacer
    // callback. One list fits this low heap headroom; a recursively stacked
    // second list does not. The depth cap is only a fail-safe for an unfixed
    // implementation, whose catch block would return "caught" instead.
    let source = r#"
        function descendReplace(depth) {
            const sharedMatch = { 0: "", length: 1, index: 0, groups: undefined };
            let remaining = 12000;
            const receiver = {
                flags: "g",
                lastIndex: 0,
                exec: function () {
                    if (remaining-- > 0) return sharedMatch;
                    return null;
                }
            };
            let recursed = false;
            return RegExp.prototype[Symbol.replace].call(
                receiver,
                "",
                function () {
                    if (!recursed && depth < 32) {
                        recursed = true;
                        descendReplace(depth + 1);
                    }
                    return "";
                }
            );
        }

        function attackRecursiveReplace() {
            try {
                return descendReplace(0);
            } catch (error) {
                globalThis.recursiveReplaceEscaped = true;
                return "caught";
            }
        }
    "#;
    let mut state = embed::compile_script(&source).expect("script compiles");
    state.run_init().expect("script initializes");
    state.set_limits(10_000_000, None);
    let baseline = state.heap_bytes();
    state.set_heap_limit(baseline + 220 * 1024);

    let result = state.call_slot(slot(&state, "attackRecursiveReplace"), &[]);
    let message = match result {
        Err(message) => message,
        Ok(value) => panic!(
            "recursive replace escaped its transient budget: {value:?}; resource={:?}",
            state.resource_limit_error()
        ),
    };
    assert!(
        message.contains("regular expression exceeded its backtrack memory budget"),
        "recursive replace failed for the wrong reason: {message}"
    );
    assert_eq!(state.resource_limit_error(), Some(message.as_str()));
}

#[test]
fn recursive_functional_replace_counts_retained_match_lists() {
    assert_recursive_functional_replace_is_bounded();
}

#[test]
fn completed_replace_releases_its_transient_reservation() {
    // Each collection fits independently but two simultaneous reservations do
    // not. Sequential replacements therefore prove the scoped charge is
    // released on the ordinary success path instead of accumulating per VM.
    let groups = "()".repeat(256);
    let source = format!(
        r#"
        const asciiSubject = "a".repeat(100);
        const utf16Subject = "é".repeat(100);
        const releasePattern = /(?:{}z|)/g;
        function replaceTwice() {{
            asciiSubject.replace(releasePattern, "");
            utf16Subject.replace(releasePattern, "");
            return "ok";
        }}
        "#,
        groups
    );
    let mut state = embed::compile_script(&source).expect("script compiles");
    state.run_init().expect("script initializes");
    state.set_limits(10_000_000, None);
    let baseline = state.heap_bytes();
    state.set_heap_limit(baseline + 1600 * 1024);

    let value = state
        .call_slot(slot(&state, "replaceTwice"), &[])
        .expect("sequential replacements must reuse transient headroom");
    assert_eq!(value, HostValue::String("ok".into()));
    assert_eq!(state.resource_limit_error(), None);
}

#[test]
fn aggregate_capture_materialization_child() {
    if std::env::var_os("ZIPP_REGEX_CAPTURE_MATERIALIZATION_CHILD").is_none() {
        return;
    }

    // The result aliases one 8 KiB guest string in 63 capture slots. The guest
    // heap therefore stays small, but the observable @@replace protocol must
    // not clone roughly 0.5 MiB of Rust strings behind a 128 KiB heap limit.
    // Keeping this adversarial case in a subprocess makes regressions fail
    // deterministically without ever putting the test runner at OOM risk.
    let mut state = embed::compile_script(
        r#"
        const sharedCapture = "x".repeat(8192);
        const sharedResult = Array(64).fill(sharedCapture);
        sharedResult[0] = "";
        sharedResult.index = 0;
        sharedResult.groups = undefined;

        function materializeAliasedCaptures() {
            let once = true;
            const receiver = {
                flags: "",
                exec: function () {
                    if (!once) return null;
                    once = false;
                    return sharedResult;
                }
            };
            return RegExp.prototype[Symbol.replace].call(receiver, "", "");
        }
        "#,
    )
    .expect("script compiles");
    state.run_init().expect("script initializes");
    state.set_limits(1_000_000, None);
    let baseline = state.heap_bytes();
    state.set_heap_limit(baseline + 128 * 1024);

    let message = state
        .call_slot(slot(&state, "materializeAliasedCaptures"), &[])
        .expect_err("aggregate capture clones must respect heap headroom");
    assert!(
        message.contains("regular expression exceeded its backtrack memory budget"),
        "unexpected failure: {message}"
    );
    assert_eq!(state.resource_limit_error(), Some(message.as_str()));
}

#[test]
fn aliased_capture_materialization_is_bounded_in_a_subprocess() {
    let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .arg("--exact")
        .arg("aggregate_capture_materialization_child")
        .arg("--nocapture")
        .env("ZIPP_REGEX_CAPTURE_MATERIALIZATION_CHILD", "1")
        .status()
        .expect("capture-materialization child starts");
    assert!(
        status.success(),
        "capture-materialization child failed: {status}"
    );
}

#[test]
fn exec_result_and_intrinsic_replacement_capture_copies_are_bounded() {
    // Every lookahead capture aliases the same long subject range. The match
    // engine's compact ranges are cheap; materializing each range is not.
    let ascii_pattern = "(?=(x*))".repeat(64);
    let ascii_source = format!(
        r#"
        const captureSubject = "x".repeat(8192);
        const capturePattern = new RegExp("{ascii_pattern}");
        function materializeExecResult() {{
            return capturePattern.exec(captureSubject);
        }}
        function materializeAsciiReplacement() {{
            return captureSubject.replace(capturePattern, "");
        }}
        function materializeFunctionalReplacement() {{
            return captureSubject.replace(capturePattern, function () {{ return ""; }});
        }}
        "#,
    );
    for function in [
        "materializeExecResult",
        "materializeAsciiReplacement",
        "materializeFunctionalReplacement",
    ] {
        assert_regex_memory_failure(&ascii_source, function, 160 * 1024);
    }

    let utf16_pattern = "(?=(é*))".repeat(48);
    let utf16_source = format!(
        r#"
        const captureSubject = "é".repeat(4096);
        const capturePattern = new RegExp("{utf16_pattern}");
        function materializeUtf16Replacement() {{
            return captureSubject.replace(capturePattern, "");
        }}
        "#,
    );
    assert_regex_memory_failure(&utf16_source, "materializeUtf16Replacement", 192 * 1024);
}

#[test]
fn named_exec_result_maps_are_preflighted_before_allocation() {
    // Empty captures reuse the interned empty string, isolating the two named
    // ObjMaps (`groups` and `indices.groups`) and their key/index backing from
    // capture-string payload. Hundreds of unique names used to allocate those
    // maps after the result preflight had only counted its temporary Vec.
    let pattern = (0..512)
        .map(|index| format!("(?<named{index}>)"))
        .collect::<String>();
    let source = format!(
        r#"
        const namedPattern = new RegExp("{pattern}", "d");
        function materializeNamedResultMaps() {{
            return namedPattern.exec("");
        }}
        "#,
    );
    assert_regex_memory_failure(&source, "materializeNamedResultMaps", 96 * 1024);
}

#[test]
fn functional_custom_exec_bounds_non_string_capture_values() {
    // Primitive captures need ToString heap Values for the replacer. They do
    // not have the large payload of the alias test, but thousands of tiny heap
    // strings/slots must still be reconciled before the next capture allocation.
    assert_regex_memory_failure(
        r#"
        const numericResult = Array(4096).fill(12345);
        numericResult[0] = "";
        numericResult.index = 0;
        numericResult.groups = undefined;
        function materializeNumericCaptures() {
            const receiver = { flags: "", exec: function () { return numericResult; } };
            return RegExp.prototype[Symbol.replace].call(
                receiver,
                "",
                function () { return ""; }
            );
        }
        "#,
        "materializeNumericCaptures",
        96 * 1024,
    );
}

#[test]
fn capture_materialization_reservation_releases_on_guest_throw() {
    let mut state = embed::compile_script(
        r#"
        const rollbackCapture = "x".repeat(8192);
        function throwDuringCaptureRead() {
            const result = {
                0: "",
                length: 3,
                index: 0,
                groups: undefined,
                get 1() { return rollbackCapture; },
                get 2() { throw new Error("capture getter"); }
            };
            const receiver = { flags: "", exec: function () { return result; } };
            try {
                RegExp.prototype[Symbol.replace].call(receiver, "", "");
            } catch (error) {
                // A guest exception is recoverable. The scoped native charge
                // from capture 1 must be gone before the next regex operation.
            }
            return "a".replace(/a/, "ok");
        }
        "#,
    )
    .expect("script compiles");
    state.run_init().expect("script initializes");
    state.set_limits(1_000_000, None);
    let baseline = state.heap_bytes();
    state.set_heap_limit(baseline + 64 * 1024);

    let value = state
        .call_slot(slot(&state, "throwDuringCaptureRead"), &[])
        .expect("guest throw releases the scoped capture reservation");
    assert_eq!(value, HostValue::String("ok".into()));
    assert_eq!(state.resource_limit_error(), None);
}

#[test]
fn left_deep_rope_scratch_is_bounded_and_released() {
    let source = r#"
        let leftDeepSubject = "é".repeat(257);
        for (let i = 0; i < 8192; i++) {
            leftDeepSubject = leftDeepSubject + "é";
        }
        const deepNoMatch = /z/;

        function scanLeftDeep() {
            return deepNoMatch.test(leftDeepSubject);
        }
        function scanLeftDeepTwice() {
            const first = deepNoMatch.test(leftDeepSubject);
            const second = deepNoMatch.test(leftDeepSubject);
            return leftDeepSubject.length + "," + first + "," + second;
        }
    "#;

    // The UTF-16 output copy alone fits. The iterative traversal's pending
    // stack must also be charged before geometric growth on this left spine.
    assert_regex_memory_failure(source, "scanLeftDeep", 32 * 1024);

    let mut state = embed::compile_script(source).expect("script compiles");
    state.run_init().expect("script initializes");
    state.set_limits(10_000_000, None);
    let baseline = state.heap_bytes();
    // One copy + traversal stack fits; two simultaneously retained copies do
    // not. Sequential scans prove both scoped reservations unwind on success.
    state.set_heap_limit(baseline + 88 * 1024);
    let value = state
        .call_slot(slot(&state, "scanLeftDeepTwice"), &[])
        .expect("left-deep traversal reservations are reused");
    assert_eq!(value, HostValue::String("8449,false,false".into()));
    assert_eq!(state.resource_limit_error(), None);
}

#[test]
fn generic_replace_retains_template_across_exec_and_releases_on_throw() {
    let source = r#"
        const retainedTemplate = "x".repeat(32768);
        const nestedUtf16Subject = "é".repeat(65536);
        const releaseUtf16Subject = "é".repeat(49152);

        function templateAcrossCustomExec() {
            const receiver = {
                flags: "",
                exec: function () {
                    /z/.test(nestedUtf16Subject);
                    return null;
                }
            };
            return RegExp.prototype[Symbol.replace].call(
                receiver,
                "",
                retainedTemplate
            );
        }

        function templateThrowThenScan() {
            const receiver = {
                flags: "",
                exec: function () { throw new Error("expected"); }
            };
            try {
                RegExp.prototype[Symbol.replace].call(
                    receiver,
                    "",
                    retainedTemplate
                );
            } catch (error) {
                // A guest exception is recoverable; the retained template
                // reservation must unwind before this next regex call.
            }
            return /z/.test(releaseUtf16Subject);
        }
    "#;

    // Template construction itself fits. Its retained host buffer plus the
    // nested custom-exec subject copy does not.
    assert_regex_memory_failure(source, "templateAcrossCustomExec", 200 * 1024);

    let mut state = embed::compile_script(source).expect("script compiles");
    state.run_init().expect("script initializes");
    state.set_limits(10_000_000, None);
    let baseline = state.heap_bytes();
    state.set_heap_limit(baseline + 190 * 1024);
    let value = state
        .call_slot(slot(&state, "templateThrowThenScan"), &[])
        .expect("guest throw releases the retained template reservation");
    assert_eq!(value, HostValue::Bool(false));
    assert_eq!(state.resource_limit_error(), None);
}

#[test]
fn generic_replace_counts_accumulated_output_across_nested_replacer() {
    assert_regex_memory_failure(
        r#"
        const nestedReplacementChunk = "x".repeat(32768);

        function nestedAccumulatedReplace(depth) {
            const shared = { 0: "", length: 1, index: 0, groups: undefined };
            let remaining = 2;
            const receiver = {
                flags: "g",
                lastIndex: 0,
                exec: function () {
                    if (remaining-- > 0) return shared;
                    return null;
                }
            };
            let calls = 0;
            return RegExp.prototype[Symbol.replace].call(
                receiver,
                "",
                function () {
                    calls++;
                    if (calls === 2 && depth === 0) {
                        nestedAccumulatedReplace(1);
                    }
                    return nestedReplacementChunk;
                }
            );
        }

        function accumulatedAcrossNestedReplacer() {
            return nestedAccumulatedReplace(0);
        }
        "#,
        "accumulatedAcrossNestedReplacer",
        216 * 1024,
    );
}

#[test]
fn deferred_legacy_statics_preflight_aggregate_and_handoff_to_heap() {
    let left = "a".repeat(32768);
    let right = "b".repeat(32768);
    let source = format!(
        r#"
        const legacySubject = "{left}z{right}";
        const legacyPattern = /z/;
        function prepareLegacyStatics() {{
            return legacyPattern.test(legacySubject);
        }}
        function readLegacyStatics() {{
            return RegExp.leftContext.length + "," + RegExp.rightContext.length;
        }}
        "#
    );

    let mut rejected = embed::compile_script(&source).expect("script compiles");
    rejected.run_init().expect("script initializes");
    rejected.set_limits(10_000_000, None);
    rejected
        .call_slot(slot(&rejected, "prepareLegacyStatics"), &[])
        .expect("successful test leaves lazy statics");
    let baseline = rejected.heap_bytes();
    rejected.set_heap_limit(baseline + 48 * 1024);
    let message = rejected
        .call_slot(slot(&rejected, "readLegacyStatics"), &[])
        .expect_err("all deferred slices must be admitted atomically");
    assert!(
        message.contains("regular expression exceeded its backtrack memory budget"),
        "unexpected failure: {message}"
    );
    let sticky = rejected
        .resource_limit_error()
        .expect("regex memory exhaustion is sticky");
    assert!(
        message.contains(sticky),
        "call-site decoration must retain the sticky resource cause: {message}"
    );
    assert!(
        rejected.heap_bytes() <= baseline + 4 * 1024,
        "aggregate rejection allocated legacy slices before preflight"
    );

    let mut admitted = embed::compile_script(&source).expect("script compiles");
    admitted.run_init().expect("script initializes");
    admitted.set_limits(10_000_000, None);
    admitted
        .call_slot(slot(&admitted, "prepareLegacyStatics"), &[])
        .expect("successful test leaves lazy statics");
    let baseline = admitted.heap_bytes();
    admitted.set_heap_limit(baseline + 96 * 1024);
    for _ in 0..2 {
        let value = admitted
            .call_slot(slot(&admitted, "readLegacyStatics"), &[])
            .expect("aggregate reservation transfers to audited heap ownership");
        assert_eq!(value, HostValue::String("32768,32768".into()));
    }
    assert_eq!(admitted.resource_limit_error(), None);
}
