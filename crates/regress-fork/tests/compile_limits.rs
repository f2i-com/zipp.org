//! How large a pattern may be, and what compiling it may cost.
//!
//! - Alternatives (issue #57). The checked profile (`prohibit-unsafe`) refused
//!   a disjunction of more than 64 alternatives, to keep a right spine away
//!   from the recursive IR walks; its alternation was already a balanced tree,
//!   so the limit protected nothing and keyword lists failed to load. The
//!   ordinary profile had no limit and built the right spine, which a 5,000
//!   word list made deep enough to overflow a 1 MiB stack, and whose
//!   early-fail pass was quadratic in its depth (682 ms for 20,000 words).

#![allow(clippy::uninlined_format_args)]

use regress::Regex;

fn words(n: usize) -> String {
    (0..n).map(|i| format!("w{i}")).collect::<Vec<_>>().join("|")
}

/// Run `f` on a thread with a 1 MiB stack, the smallest an embedder is likely
/// to give one, so an IR walk as deep as the alternation is long overflows.
fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("compiling on a 1 MiB stack must not overflow it")
}

#[test]
fn long_alternations_compile_and_keep_their_order() {
    for n in [64, 65, 200, 5000] {
        let pattern = words(n);
        let re = Regex::new(&pattern).unwrap_or_else(|e| panic!("{n} alternatives: {e}"));
        assert_eq!(re.find("w5").map(|m| m.range()), Some(0..2), "{n}");
        // The leftmost alternative that matches wins, not the longest: `w4`
        // comes before `w49`, whatever shape the tree has.
        let last = format!("w{}", n - 1);
        assert_eq!(re.find(&last).map(|m| m.range()), Some(0..2), "{n}");
        assert_eq!(re.find(&format!("x{last}")).map(|m| m.range()), Some(1..3), "{n}");
        assert!(re.find("x5").is_none(), "{n}");
        // Inside a group, and quantified.
        let grouped = Regex::new(&format!("^(?:{pattern})+$")).unwrap();
        assert!(grouped.find(&format!("w0w1{last}")).is_some(), "{n}");
        assert!(grouped.find("w0x").is_none(), "{n}");
    }
}

#[test]
fn capture_groups_across_many_alternatives_keep_their_numbers() {
    let n = 300;
    let pattern = (0..n).map(|i| format!("(x{i}y)")).collect::<Vec<_>>().join("|");
    let re = Regex::new(&pattern).unwrap();
    let m = re.find("--x250y").unwrap();
    assert_eq!(m.range(), 2..7);
    assert_eq!(m.captures.len(), n);
    for (index, capture) in m.captures.iter().enumerate() {
        let want = (index == 250).then_some(2..7);
        assert_eq!(*capture, want, "group {}", index + 1);
    }
}

#[test]
fn alternation_depth_is_bounded_on_a_small_stack() {
    on_small_stack(|| {
        let re = Regex::new(&words(50_000)).unwrap();
        assert_eq!(re.find("w49999").map(|m| m.range()), Some(0..2));
        assert_eq!(re.find("-w7").map(|m| m.range()), Some(1..3));
        // A property of strings is one atom and 3,953 alternatives.
        let emoji = Regex::with_flags(r"^\p{RGI_Emoji}$", "v").unwrap();
        assert!(emoji.find("\u{1F600}").is_some());
        assert!(emoji.find("x").is_none());
    });
}

#[test]
fn long_alternations_compile_in_linear_time() {
    // 20,000 alternatives took 682 ms to optimize in a release build when
    // the early-fail pass walked the subtree of every node; now ~10 ms.
    let pattern = words(20_000);
    let started = std::time::Instant::now();
    let re = Regex::new(&pattern).unwrap();
    let elapsed = started.elapsed();
    assert!(re.find("w19999").is_some());
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "20,000 alternatives took {elapsed:?}"
    );
}
