//! Fail-closed ASCII start prefilters derived from a required literal which is
//! more selective than the ordinary first-byte predicate.
//!
//! Planning happens on the parser IR before loop unrolling.  The owned plan is
//! attached only by `Regex::from_unicode_byteopt`, so UTF-8/UTF-16/UCS-2
//! programs cannot accidentally use byte-element reasoning.

use crate::bytesearch::ByteBitmap;
use crate::ir::{Node, Quantifier, Regex};
use memchr::memmem;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

/// Maximum literal size admitted by this narrow planner.  Larger literals
/// already have a strong ordinary start predicate and do not justify another
/// owned finder.
const MAX_LITERAL_LEN: usize = 64;

/// Maximum number of class bytes the executor will inspect behind a delimiter.
/// If an unbounded (or wider) run reaches this cap while another class byte is
/// still available, execution abandons the prefilter *without advancing* and
/// resumes the incumbent search from the original start.
pub(crate) const MAX_BACKSCAN: usize = 64;

#[derive(Debug, Clone)]
pub(crate) struct Literal {
    finder: Box<memmem::Finder<'static>>,
}

impl Literal {
    fn new(bytes: Vec<u8>) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > MAX_LITERAL_LEN || !bytes.is_ascii() {
            return None;
        }
        let finder = Box::new(memmem::Finder::new(&bytes).into_owned());
        Some(Self { finder })
    }

    #[inline(always)]
    pub(crate) fn finder(&self) -> &memmem::Finder<'static> {
        self.finder.as_ref()
    }

    #[cfg(test)]
    fn bytes(&self) -> &[u8] {
        self.finder.needle()
    }
}

/// An ASCII-byte-only start plan.
#[derive(Debug, Clone)]
pub(crate) enum Plan {
    /// A literal which every match begins with.  This is derived from a
    /// required literal P immediately followed by a one-char loop with min>=1;
    /// the finder searches for P+the loop character.
    RequiredPrefix { literal: Literal },

    /// A leading capture consisting solely of one ASCII class run, immediately
    /// followed by a nonempty literal whose first byte is outside the class.
    /// The literal starts at `d`; possible match starts are bounded behind `d`
    /// by `min..=max` class bytes.
    RunLiteral {
        literal: Literal,
        class: ByteBitmap,
        min: usize,
        max: Option<usize>,
    },
}

impl Plan {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::RequiredPrefix { .. } => "required-prefix",
            Self::RunLiteral { .. } => "run-literal",
        }
    }
}

#[cfg(feature = "std")]
#[inline]
fn switched_off(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

#[cfg(not(feature = "std"))]
#[inline]
fn switched_off(_name: &str) -> bool {
    false
}

fn top_nodes(node: &Node) -> &[Node] {
    match node {
        Node::Cat(nodes) if nodes.len() == 2 && matches!(nodes.last(), Some(Node::Goal)) => {
            top_nodes(&nodes[0])
        }
        Node::Cat(nodes) => nodes,
        _ => core::slice::from_ref(node),
    }
}

/// Append an exact, case-sensitive ASCII literal node.  Parser IR uses `Char`;
/// optimized byte IR uses `ByteSequence`, so accepting both keeps the proof
/// local to the semantic shape rather than an optimizer ordering accident.
fn append_literal(node: &Node, out: &mut Vec<u8>) -> bool {
    match node {
        &Node::Char { c, icase: false } if c < 0x80 => {
            out.push(c as u8);
            true
        }
        Node::ByteSequence(bytes) if !bytes.is_empty() && bytes.is_ascii() => {
            out.extend_from_slice(bytes);
            true
        }
        _ => false,
    }
}

fn one_literal_byte(node: &Node) -> Option<u8> {
    let mut bytes = Vec::new();
    append_literal(node, &mut bytes).then_some(())?;
    (bytes.len() == 1).then(|| bytes[0])
}

fn loop_one_literal(node: &Node) -> Option<(u8, Quantifier)> {
    match node {
        Node::Loop {
            loopee,
            quant,
            enclosed_groups,
        } if enclosed_groups.start == enclosed_groups.end => {
            Some((one_literal_byte(loopee)?, *quant))
        }
        Node::Loop1CharBody { loopee, quant } => Some((one_literal_byte(loopee)?, *quant)),
        _ => None,
    }
}

fn required_prefix(nodes: &[Node]) -> Option<Plan> {
    let mut prefix = Vec::new();
    let mut index = 0usize;
    while let Some(node) = nodes.get(index) {
        let before = prefix.len();
        if !append_literal(node, &mut prefix) {
            break;
        }
        if prefix.len() > MAX_LITERAL_LEN - 1 {
            return None;
        }
        debug_assert!(prefix.len() > before);
        index += 1;
    }
    if prefix.is_empty() {
        return None;
    }
    let (q, quant) = loop_one_literal(nodes.get(index)?)?;
    if quant.min == 0 {
        return None;
    }
    prefix.push(q);
    Some(Plan::RequiredPrefix {
        literal: Literal::new(prefix)?,
    })
}

fn class_bitmap(node: &Node) -> Option<ByteBitmap> {
    let mut result = ByteBitmap::default();
    match node {
        Node::Bracket(contents) if !contents.invert && !contents.cps.is_empty() => {
            for interval in contents.cps.intervals() {
                if interval.last >= 0x80 {
                    return None;
                }
                for cp in interval.first..=interval.last {
                    result.set(cp as u8);
                }
            }
        }
        Node::ByteSet(bytes) if !bytes.is_empty() && bytes.is_ascii() => {
            for &byte in bytes {
                result.set(byte);
            }
        }
        Node::CharSet(chars) if !chars.is_empty() && chars.iter().all(|&c| c < 0x80) => {
            for &c in chars {
                result.set(c as u8);
            }
        }
        &Node::Char { c, icase: false } if c < 0x80 => result.set(c as u8),
        Node::ByteSequence(bytes) if bytes.len() == 1 && bytes[0].is_ascii() => {
            result.set(bytes[0]);
        }
        _ => return None,
    }
    Some(result)
}

fn same_class(a: &ByteBitmap, b: &ByteBitmap) -> bool {
    a == b
}

/// Recognize exactly one identical-class run, tolerating the optimizer's
/// normalized form (mandatory atoms followed by a min=0 one-char loop).
fn class_run(node: &Node) -> Option<(ByteBitmap, usize, Option<usize>)> {
    if let Some(class) = class_bitmap(node) {
        return Some((class, 1, Some(1)));
    }
    match node {
        Node::Loop {
            loopee,
            quant,
            enclosed_groups,
        } if enclosed_groups.start == enclosed_groups.end => {
            let class = class_bitmap(loopee)?;
            (quant.min > 0).then_some((class, quant.min, quant.max))
        }
        Node::Loop1CharBody { loopee, quant } => {
            let class = class_bitmap(loopee)?;
            (quant.min > 0).then_some((class, quant.min, quant.max))
        }
        Node::Cat(parts) => {
            let mut class: Option<ByteBitmap> = None;
            let mut min = 0usize;
            let mut max = Some(0usize);
            let mut saw_loop = false;
            for part in parts {
                if let Some(atom) = class_bitmap(part) {
                    if saw_loop || class.as_ref().is_some_and(|c| !same_class(c, &atom)) {
                        return None;
                    }
                    class.get_or_insert(atom);
                    min = min.checked_add(1)?;
                    max = Some(max?.checked_add(1)?);
                    continue;
                }
                let (body, quant) = match part {
                    Node::Loop {
                        loopee,
                        quant,
                        enclosed_groups,
                    } if enclosed_groups.start == enclosed_groups.end => {
                        (class_bitmap(loopee)?, *quant)
                    }
                    Node::Loop1CharBody { loopee, quant } => (class_bitmap(loopee)?, *quant),
                    _ => return None,
                };
                if saw_loop || class.as_ref().is_some_and(|c| !same_class(c, &body)) {
                    return None;
                }
                class.get_or_insert(body);
                saw_loop = true;
                min = min.checked_add(quant.min)?;
                max = match (max, quant.max) {
                    (Some(a), Some(b)) => Some(a.checked_add(b)?),
                    _ => None,
                };
            }
            (min > 0 && max.is_none_or(|m| m >= min)).then_some((class?, min, max))
        }
        _ => None,
    }
}

/// Every accepted suffix atom consumes input independently of the leading
/// capture.  In particular, backreferences, alternatives, assertions,
/// anchors, generic captures-in-loops, and arbitrary-dot atoms all fail closed.
fn suffix_is_independent(node: &Node) -> bool {
    match node {
        Node::Empty | Node::Goal => true,
        Node::Char { c, icase: false } => *c < 0x80,
        Node::ByteSequence(bytes) => !bytes.is_empty() && bytes.is_ascii(),
        Node::ByteSet(bytes) => !bytes.is_empty() && bytes.is_ascii(),
        Node::CharSet(chars) => !chars.is_empty() && chars.iter().all(|&c| c < 0x80),
        Node::Bracket(_) => class_bitmap(node).is_some(),
        Node::Cat(nodes) => nodes.iter().all(suffix_is_independent),
        Node::CaptureGroup { contents, .. } => suffix_is_independent(contents),
        Node::Loop {
            loopee,
            quant,
            enclosed_groups,
        } => {
            enclosed_groups.start == enclosed_groups.end
                && quant.max.is_none_or(|max| max >= quant.min)
                && class_bitmap(loopee).is_some()
        }
        Node::Loop1CharBody { loopee, quant } => {
            quant.max.is_none_or(|max| max >= quant.min) && class_bitmap(loopee).is_some()
        }
        Node::Alt(..)
        | Node::Char { .. }
        | Node::MatchAny
        | Node::MatchAnyExceptLineTerminator
        | Node::Anchor { .. }
        | Node::WordBoundary { .. }
        | Node::BackRef { .. }
        | Node::NamedBackRef { .. }
        | Node::LookaroundAssertion { .. } => false,
    }
}

fn run_literal(nodes: &[Node]) -> Option<Plan> {
    let Node::CaptureGroup { contents, .. } = nodes.first()? else {
        return None;
    };
    let (class, min, max) = class_run(contents)?;
    if max.is_some_and(|m| m < min) {
        return None;
    }

    let mut literal = Vec::new();
    let mut index = 1usize;
    while let Some(node) = nodes.get(index) {
        if !append_literal(node, &mut literal) {
            break;
        }
        if literal.len() > MAX_LITERAL_LEN {
            return None;
        }
        index += 1;
    }
    if literal.is_empty() || class.contains(literal[0]) {
        return None;
    }

    // Validate the complete program, not merely the visible prefix.  This is
    // the proof that a failed attempt at the earliest start for delimiter d
    // permits resuming the delimiter search at d+1.
    if !nodes[index..].iter().all(suffix_is_independent) {
        return None;
    }

    Some(Plan::RunLiteral {
        literal: Literal::new(literal)?,
        class,
        min,
        max,
    })
}

/// Derive a plan for the byte-optimized ASCII twin.  Callers must not attach
/// this result to the ordinary Unicode program.
pub(crate) fn derive(re: &Regex) -> Option<Plan> {
    if switched_off("ZIPP_NO_RX_SUFFIX_START") || re.flags.icase || re.flags.unicode_mode() {
        return None;
    }
    let nodes = top_nodes(&re.node);
    if !switched_off("ZIPP_NO_RX_SUFFIX_REQUIRED_PREFIX") {
        if let Some(plan) = required_prefix(nodes) {
            return Some(plan);
        }
    }
    if !switched_off("ZIPP_NO_RX_SUFFIX_RUNLITERAL") {
        return run_literal(nodes);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Flags;

    fn plan(pattern: &str, flags: &str) -> Option<Plan> {
        let ir = crate::parse::try_parse(pattern.chars().map(u32::from), Flags::from(flags))
            .expect("valid test pattern");
        derive(&ir)
    }

    #[test]
    fn derives_required_loop_literal_before_unrolling() {
        let Plan::RequiredPrefix { literal } = plan(r"//+(\w+)", "").unwrap() else {
            panic!("wrong plan");
        };
        assert_eq!(literal.bytes(), b"//");
        assert!(plan(r"/\/+", "").is_some());
        assert!(plan(r"a*b", "").is_none());
    }

    #[test]
    fn derives_leading_class_run_and_literal() {
        let Plan::RunLiteral {
            literal,
            class,
            min,
            max,
        } = plan(r"([a-z]+)=(\d+)", "").unwrap()
        else {
            panic!("wrong plan");
        };
        assert_eq!(literal.bytes(), b"=");
        assert_eq!(min, 1);
        assert_eq!(max, None);
        assert!(class.contains(b'a'));
        assert!(class.contains(b'z'));
        assert!(!class.contains(b'A'));

        let Plan::RunLiteral { min, max, .. } = plan(r"([ab]{2,4}):x", "").unwrap() else {
            panic!("wrong plan");
        };
        assert_eq!((min, max), (2, Some(4)));
    }

    #[test]
    fn run_literal_rejects_every_uncertain_shape() {
        for pattern in [
            r"([ab]+)a",   // delimiter intersects C
            r"([ab]+)=\1", // backreference depends on the capture
            r"([ab]+)=(?:x|y)",
            r"([ab]+)=(?=x)",
            r"([ab]+)=x$",
            r"([ab]+)=.*",
            r"((?:a|b)+)=x", // the capture is not one class run
            r"()=x",         // empty leading capture
            r"([é]+)=x",     // non-ASCII class
            r"([ab]+)é",     // non-ASCII literal
        ] {
            assert!(plan(pattern, "").is_none(), "unexpected plan for {pattern}");
        }
        assert!(plan(r"([ab]+)=x", "i").is_none());
        assert!(plan(r"([ab]+)=x", "u").is_none());
    }
}
