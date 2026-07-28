#[cfg(not(feature = "linear-ascii"))]
use regress::{Regex, RegexFallbackReason, RegexPlan};

#[cfg(not(feature = "linear-ascii"))]
#[test]
fn default_build_reports_the_linear_backend_unavailable() {
    let regex = Regex::new("a").unwrap();

    assert_eq!(regex.ascii_plan(), RegexPlan::Classical);
    assert_eq!(
        regex.ascii_fallback_reason(),
        Some(RegexFallbackReason::BackendUnavailable)
    );
    assert!(!regex.ascii_auto_eligible());

    let matches: Vec<_> = regex.find_from_ascii("ba", 0).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].range, 1..2);
}
