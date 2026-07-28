//! Bounded linear-time executor for a conservative ASCII-compatible subset.
//!
//! The ECMAScript parser remains authoritative. This module is an execution
//! tier, not a second parser: patterns outside the deliberately small contract
//! retain the classical backtracker.

use crate::api::{Flags, Match, RegexFallbackReason, RegexPlan, clone_group_names};
use crate::classicalbacktrack::BacktrackExecutor;
use crate::exec;
use crate::indexing::AsciiInput;
use crate::insn::{CompiledRegex, StartPredicate};
use regex::bytes::{CaptureLocations, Regex as BytesRegex, RegexBuilder};
use std::sync::OnceLock;

const MAX_LINEAR_PATTERN_BYTES: usize = 16 * 1024;
const MAX_LINEAR_PROGRAM_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct LinearRegex {
    regex: BytesRegex,
    auto_eligible: bool,
}

pub(crate) type LinearPlan = OnceLock<Result<LinearRegex, RegexFallbackReason>>;

#[derive(Debug)]
pub struct AsciiMatches<'r, 't> {
    inner: AsciiMatchesInner<'r, 't>,
}

#[derive(Debug)]
enum AsciiMatchesInner<'r, 't> {
    Classical(exec::Matches<BacktrackExecutor<'r, AsciiInput<'t>>>),
    Linear(LinearMatches<'r, 't>),
}

#[derive(Debug)]
struct LinearMatches<'r, 't> {
    regex: &'r BytesRegex,
    input: &'t [u8],
    locations: Option<CaptureLocations>,
    next_start: Option<usize>,
    group_names: &'r [Box<str>],
}

#[derive(Debug, Clone)]
pub(crate) enum LinearCandidate {
    Source(String),
    Fallback(RegexFallbackReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    Classical,
    Auto,
    Linear,
}

fn selected_tier() -> Tier {
    static TIER: OnceLock<Tier> = OnceLock::new();
    *TIER.get_or_init(|| {
        match std::env::var("ZIPP_REGEX_TIER")
            .unwrap_or_else(|_| "classical".to_owned())
            .to_ascii_lowercase()
            .as_str()
        {
            "auto" => Tier::Auto,
            "linear" => Tier::Linear,
            // `pike` is intentionally conservative until the existing Pike
            // executor supports optimized one-char loops and passes the same
            // differential matrix as this tier.
            "classical" | "pike" | _ => Tier::Classical,
        }
    })
}

pub(crate) fn collect_candidate<I>(pattern: I, flags: Flags) -> LinearCandidate
where
    I: Iterator<Item = u32>,
{
    if flags.unicode_sets {
        return LinearCandidate::Fallback(RegexFallbackReason::UnicodeSets);
    }
    let mut source = String::new();
    for codepoint in pattern {
        if codepoint > 0x7f {
            return LinearCandidate::Fallback(RegexFallbackReason::NonAsciiPattern);
        }
        if source.len() == MAX_LINEAR_PATTERN_BYTES {
            return LinearCandidate::Fallback(RegexFallbackReason::PatternTooLarge);
        }
        source.push(char::from_u32(codepoint).expect("ASCII is a valid scalar"));
    }
    match safe_subset_reason(&source, flags.multiline) {
        Some(reason) => LinearCandidate::Fallback(reason),
        None => LinearCandidate::Source(source),
    }
}

fn safe_subset_reason(pattern: &str, multiline: bool) -> Option<RegexFallbackReason> {
    struct GroupState {
        nested_captures: usize,
        capturing: bool,
    }

    let bytes = pattern.as_bytes();
    let mut index = 0;
    let mut in_class = false;
    let mut groups: Vec<GroupState> = Vec::new();
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                index += 1;
                if index == bytes.len() {
                    // The ECMAScript parser will report the syntax error. The
                    // tier merely declines independently.
                    return Some(RegexFallbackReason::UnsupportedEscape);
                }
                let escaped = bytes[index];
                if escaped.is_ascii_digit() || escaped == b'k' {
                    return Some(RegexFallbackReason::Backreference);
                }
                if in_class && matches!(escaped, b'b' | b'B') {
                    return Some(RegexFallbackReason::UnsupportedEscape);
                }
                if escaped == b'x'
                    && !(bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
                        && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit))
                {
                    return Some(RegexFallbackReason::UnsupportedEscape);
                }
                if escaped == b'b' && bytes.get(index + 1) == Some(&b'{') {
                    // rust-regex assigns special meanings to `\b{start}`,
                    // `\b{end}`, and their half-boundary variants. In legacy
                    // ECMAScript this is `\b` followed by ordinary source.
                    return Some(RegexFallbackReason::UnsupportedEscape);
                }
                if escaped.is_ascii_alphabetic()
                    && !matches!(
                        escaped,
                        b'b' | b'B'
                            | b'd'
                            | b'D'
                            | b's'
                            | b'S'
                            | b'w'
                            | b'W'
                            | b'f'
                            | b'n'
                            | b'r'
                            | b't'
                            | b'v'
                            | b'x'
                    )
                {
                    // ECMAScript legacy identity escapes overlap with
                    // rust-regex-only escapes such as \a, \A, \z and \U.
                    return Some(RegexFallbackReason::UnsupportedEscape);
                }
                if matches!(escaped, b'<' | b'>') {
                    // rust-regex treats these as word-boundary assertions,
                    // while legacy ECMAScript treats them as identity escapes.
                    return Some(RegexFallbackReason::UnsupportedEscape);
                }
            }
            b'[' if in_class => {
                // rust-regex supports nested and POSIX character classes.
                return Some(RegexFallbackReason::UnsupportedClassSyntax);
            }
            b'[' => in_class = true,
            b']' if in_class => in_class = false,
            b'&' | b'-' | b'~' if in_class && bytes.get(index + 1) == Some(&bytes[index]) => {
                // In a legacy ECMAScript class these are ordinary class
                // characters/range punctuation. rust-regex interprets them
                // as intersection, difference, or symmetric difference.
                return Some(RegexFallbackReason::UnsupportedClassSyntax);
            }
            b'^' | b'$' if !in_class && multiline => {
                // ECMAScript recognizes CR and LF independently, including
                // the position between CRLF. rust-regex can use LF semantics
                // or CRLF-pair semantics, neither of which is identical.
                return Some(RegexFallbackReason::MultilineAnchor);
            }
            b'(' if !in_class && bytes.get(index + 1) == Some(&b'?') => {
                match bytes.get(index + 2).copied() {
                    Some(b':') => groups.push(GroupState {
                        nested_captures: 0,
                        capturing: false,
                    }),
                    Some(b'=') | Some(b'!') => {
                        return Some(RegexFallbackReason::Lookaround);
                    }
                    Some(b'<') => {
                        if matches!(bytes.get(index + 3), Some(b'=') | Some(b'!')) {
                            return Some(RegexFallbackReason::Lookaround);
                        }
                        // A named capture is accepted only if the linear
                        // backend accepts the same name and capture layout.
                        for group in &mut groups {
                            group.nested_captures += 1;
                        }
                        groups.push(GroupState {
                            nested_captures: 0,
                            capturing: true,
                        });
                    }
                    // Scoped modifiers and every other `(?...)` extension are
                    // kept on the authoritative executor for now.
                    _ => return Some(RegexFallbackReason::UnsupportedGroup),
                }
            }
            b'(' if !in_class => {
                for group in &mut groups {
                    group.nested_captures += 1;
                }
                groups.push(GroupState {
                    nested_captures: 0,
                    capturing: true,
                });
            }
            b')' if !in_class => {
                if let Some(group) = groups.pop() {
                    let quantified = matches!(
                        bytes.get(index + 1),
                        Some(b'*') | Some(b'+') | Some(b'?') | Some(b'{')
                    );
                    if quantified && group.capturing {
                        // Empty iterations and the capture retained after the
                        // last iteration do not have the same semantics in
                        // ECMAScript and rust-regex.
                        return Some(RegexFallbackReason::CaptureRepetitionSemantics);
                    }
                    if quantified && group.nested_captures > 0 {
                        // ECMAScript clears nested captures at the start of
                        // every enclosing repetition. rust-regex retains the
                        // most recent participating value instead.
                        return Some(RegexFallbackReason::CaptureResetSemantics);
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

pub(crate) fn resolve_candidate<'a>(
    candidate: &LinearCandidate,
    compiled: &CompiledRegex,
    plan: &'a LinearPlan,
) -> Result<&'a LinearRegex, RegexFallbackReason> {
    let LinearCandidate::Source(source) = candidate else {
        let LinearCandidate::Fallback(reason) = candidate else {
            unreachable!()
        };
        return Err(*reason);
    };
    plan.get_or_init(|| {
        let mut builder = RegexBuilder::new(source);
        builder
            .unicode(false)
            .case_insensitive(compiled.flags.icase)
            .multi_line(compiled.flags.multiline)
            .dot_matches_new_line(compiled.flags.dot_all)
            // On ASCII subjects, ECMAScript `.` excludes both CR and LF. CRLF
            // mode gives rust-regex exactly that dot behavior. Multiline anchors,
            // whose CRLF semantics differ, are rejected by the classifier.
            .crlf(true)
            .size_limit(MAX_LINEAR_PROGRAM_BYTES)
            .dfa_size_limit(MAX_LINEAR_PROGRAM_BYTES);
        let regex = builder
            .build()
            .map_err(|_| RegexFallbackReason::BackendRejected)?;
        if regex.captures_len() != compiled.groups as usize + 1 {
            return Err(RegexFallbackReason::CaptureLayoutMismatch);
        }
        // A no-capture search uses `find_at` below, but measurements still show
        // the classical executor winning for literals, simple classes, and
        // short/unanchored searches. The linear tier pays off for sufficiently
        // substantial start-anchored programs and for two or more captures,
        // whose bookkeeping amortizes setup. One-capture searches remain on
        // the classical executor. `linear` still forces every semantically
        // eligible program for differential/performance work.
        let auto_eligible = compiled.groups >= 2
            || (compiled.groups == 0
                && matches!(compiled.start_pred, StartPredicate::StartAnchored)
                && source.len() >= 32);
        Ok(LinearRegex {
            regex,
            auto_eligible,
        })
    })
    .as_ref()
    .map_err(|reason| *reason)
}

pub(crate) fn auto_eligible(linear: &LinearRegex) -> bool {
    linear.auto_eligible
}

impl<'r, 't> AsciiMatches<'r, 't> {
    pub(crate) fn new(
        compiled: &'r CompiledRegex,
        candidate: &LinearCandidate,
        plan: &'r LinearPlan,
        text: &'t str,
        start: usize,
    ) -> Self {
        Self::new_with_tier(compiled, candidate, plan, text, start, selected_tier())
    }

    pub(crate) fn new_for_plan(
        compiled: &'r CompiledRegex,
        candidate: &LinearCandidate,
        plan: &'r LinearPlan,
        text: &'t str,
        start: usize,
        selected: RegexPlan,
    ) -> Self {
        let tier = match selected {
            RegexPlan::Classical => Tier::Classical,
            RegexPlan::LinearAscii => Tier::Linear,
        };
        Self::new_with_tier(compiled, candidate, plan, text, start, tier)
    }

    fn new_with_tier(
        compiled: &'r CompiledRegex,
        candidate: &LinearCandidate,
        plan: &'r LinearPlan,
        text: &'t str,
        start: usize,
        tier: Tier,
    ) -> Self {
        let linear = (tier != Tier::Classical)
            .then(|| resolve_candidate(candidate, compiled, plan).ok())
            .flatten();
        let use_linear = linear
            .is_some_and(|plan| tier == Tier::Linear || (tier == Tier::Auto && plan.auto_eligible));
        let inner = if use_linear {
            if let Some(linear) = linear {
                AsciiMatchesInner::Linear(LinearMatches {
                    regex: &linear.regex,
                    input: text.as_bytes(),
                    locations: (linear.regex.captures_len() > 1)
                        .then(|| linear.regex.capture_locations()),
                    next_start: (start <= text.len()).then_some(start),
                    group_names: &compiled.group_names,
                })
            } else {
                unreachable!("use_linear requires a compiled linear plan")
            }
        } else {
            AsciiMatchesInner::Classical(exec::Matches::new(
                <BacktrackExecutor<'r, AsciiInput<'t>> as exec::Executor<'r, 't>>::new(
                    compiled, text,
                ),
                start,
            ))
        };
        Self { inner }
    }
}

impl Iterator for AsciiMatches<'_, '_> {
    type Item = Match;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            AsciiMatchesInner::Classical(matches) => matches.next(),
            AsciiMatchesInner::Linear(matches) => matches.next(),
        }
    }
}

impl Iterator for LinearMatches<'_, '_> {
    type Item = Match;

    fn next(&mut self) -> Option<Self::Item> {
        let search_start = self.next_start?;
        let (start, end, captures) = if let Some(locations) = &mut self.locations {
            let found = self
                .regex
                .captures_read_at(locations, self.input, search_start)?;
            let captures = (1..self.regex.captures_len())
                .map(|index| locations.get(index).map(|(start, end)| start..end))
                .collect();
            (found.start(), found.end(), captures)
        } else {
            let found = self.regex.find_at(self.input, search_start)?;
            (found.start(), found.end(), Vec::new())
        };
        self.next_start = if end > start {
            Some(end)
        } else if end < self.input.len() {
            Some(end + 1)
        } else {
            None
        };
        Some(Match {
            range: start..end,
            captures,
            group_names: clone_group_names(self.group_names),
        })
    }
}
