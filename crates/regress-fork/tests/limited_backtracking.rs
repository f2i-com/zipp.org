use regress::{Flags, MatchLimitError, MatchLimits, Regex};

fn unoptimized(pattern: &str) -> Regex {
    Regex::with_flags(
        pattern,
        Flags {
            no_opt: true,
            ..Flags::default()
        },
    )
    .unwrap()
}

#[test]
fn catastrophic_failure_stops_at_the_work_ceiling() {
    let regex = unoptimized(r"(a|aa)+$");
    let mut matches = regex.find_from_ascii_with_limits(
        "aaaaaaaaaaaaaaaaaaaaaaaa!",
        0,
        MatchLimits {
            max_steps: 256,
            max_backtrack_bytes: 64 * 1024,
            max_memory_bytes: 64 * 1024,
        },
    );

    assert!(matches.next().is_none());
    let usage = matches.match_usage();
    assert_eq!(usage.steps, 256);
    assert_eq!(usage.exhaustion, Some(MatchLimitError::Steps));
    // Exhaustion is terminal for the iterator, rather than restarting a fresh
    // budget on the next `next()` call.
    assert!(matches.next().is_none());
    assert_eq!(matches.match_usage(), usage);
}

#[test]
fn backtrack_storage_ceiling_is_distinct_from_no_match() {
    let regex = unoptimized(r"(a|b)c");
    let mut matches = regex.find_from_ascii_with_limits(
        "ac",
        0,
        MatchLimits {
            max_steps: 10_000,
            // Too small even for the mandatory Exhausted sentinel.
            max_backtrack_bytes: 1,
            max_memory_bytes: 1,
        },
    );

    assert!(matches.next().is_none());
    assert_eq!(
        matches.match_usage().exhaustion,
        Some(MatchLimitError::BacktrackMemory)
    );
}

#[test]
fn capture_and_loop_state_is_inside_the_memory_ceiling() {
    let pattern = format!("{}z", "()?".repeat(128));
    let regex = unoptimized(&pattern);
    let mut matches = regex.find_from_ascii_with_limits(
        "z",
        0,
        MatchLimits {
            max_steps: 100_000,
            max_backtrack_bytes: 1024,
            max_memory_bytes: 1024,
        },
    );

    assert!(matches.next().is_none());
    assert_eq!(
        matches.match_usage().exhaustion,
        Some(MatchLimitError::BacktrackMemory)
    );
}

#[test]
fn optimized_single_character_loop_stops_during_the_scan() {
    let regex = unoptimized(r"a*Z");
    let input = "a".repeat(128 * 1024);
    let mut matches = regex.find_from_ascii_with_limits(
        &input,
        0,
        MatchLimits {
            max_steps: 32,
            max_backtrack_bytes: 64 * 1024,
            max_memory_bytes: 64 * 1024,
        },
    );

    assert!(matches.next().is_none());
    assert_eq!(matches.match_usage().steps, 32);
    assert_eq!(
        matches.match_usage().exhaustion,
        Some(MatchLimitError::Steps)
    );
}

#[test]
fn ordinary_global_scan_completes_under_the_sandbox_defaults() {
    let regex = Regex::new(r".").unwrap();
    let input = "x".repeat(64 * 1024);
    let mut matches = regex.find_from_ascii_with_limits(&input, 0, MatchLimits::SANDBOX);
    let count = matches.by_ref().count();

    assert_eq!(count, input.len());
    assert_eq!(matches.match_usage().exhaustion, None);
    assert!(matches.match_usage().steps < MatchLimits::SANDBOX.max_steps);
}

#[test]
fn global_iteration_shares_one_budget_instead_of_resetting_per_match() {
    let regex = Regex::new(r".").unwrap();
    let input = "x".repeat(100);
    let mut matches = regex.find_from_ascii_with_limits(
        &input,
        0,
        MatchLimits {
            max_steps: 20,
            max_backtrack_bytes: 16 * 1024,
            max_memory_bytes: 16 * 1024,
        },
    );
    let count = matches.by_ref().count();

    assert!(
        count > 0 && count < input.len(),
        "unexpected match count {count}"
    );
    assert_eq!(
        matches.match_usage().exhaustion,
        Some(MatchLimitError::Steps)
    );
}

#[test]
fn nested_lookaround_stacks_share_the_memory_ceiling() {
    let regex = unoptimized(r"(?=(?=(a|aa)+$))");
    let input = format!("{}!", "a".repeat(96));
    let mut matches = regex.find_from_ascii_with_limits(
        &input,
        0,
        MatchLimits {
            max_steps: 1_000_000,
            max_backtrack_bytes: 128,
            max_memory_bytes: 128,
        },
    );

    assert!(matches.next().is_none());
    assert_eq!(
        matches.match_usage().exhaustion,
        Some(MatchLimitError::BacktrackMemory)
    );
}

#[test]
fn bounded_generic_non_greedy_loop_restores_the_current_entry() {
    // Multi-instruction loop bodies use EnterNonGreedyLoop (not the specialized
    // one-character form). The first exit attempt fails at offset 2 and must
    // re-enter from that current offset, not the loop's original offset 0.
    let regex = unoptimized(r"(?:ab)+?ab$");
    let mut matches = regex.find_from_ascii_with_limits(
        "abab",
        0,
        MatchLimits {
            max_steps: 10_000,
            max_backtrack_bytes: 16 * 1024,
            max_memory_bytes: 16 * 1024,
        },
    );
    let found = matches.next().expect("non-greedy loop must backtrack once");
    assert_eq!(found.range(), 0..4);
    assert_eq!(matches.match_usage().exhaustion, None);
}

#[test]
fn drained_scan_reports_exhaustion_without_emitting_a_false_match() {
    let regex = unoptimized(r"(a|aa)+$");
    let mut emitted = 0;
    let result = regex.scan_ascii_with_limits(
        "aaaaaaaaaaaaaaaaaaaaaaaa!",
        0,
        usize::MAX,
        MatchLimits {
            max_steps: 128,
            max_backtrack_bytes: 64 * 1024,
            max_memory_bytes: 64 * 1024,
        },
        &mut |_, _| emitted += 1,
    );

    assert_eq!(emitted, 0);
    assert_eq!(result.completion, Err(MatchLimitError::Steps));
    assert_eq!(
        result.match_usage().exhaustion,
        Some(MatchLimitError::Steps)
    );

    // The other two states are distinct as well: hitting the caller's match
    // cap is normal incomplete progress, while reaching the end is completion.
    let regex = Regex::new(r".").unwrap();
    let mut emitted = 0;
    let capped =
        regex.scan_ascii_with_limits("xx", 0, 1, MatchLimits::SANDBOX, &mut |_, _| emitted += 1);
    assert_eq!(emitted, 1);
    assert_eq!(capped.completion, Ok(false));

    let complete =
        regex.scan_ascii_with_limits("x", 0, usize::MAX, MatchLimits::SANDBOX, &mut |_, _| {});
    assert_eq!(complete.completion, Ok(true));
}
