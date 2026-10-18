//! Optimizations on regex IR

use crate::insn::{MAX_BYTE_SET_LENGTH, MAX_CHAR_SET_LENGTH};
use crate::ir::*;
use crate::types::{BracketContents, MAX_LOOPS};
use crate::unicode;
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

/// When unrolling a loop, the largest minimum count we will unroll.
const LOOP_UNROLL_THRESHOLD: usize = 5;

/// Unrolling copies a loop's body `min` times, and an unrolled loop inside
/// another is copied again with each copy of the outer body. Unbounded, that
/// multiplied the IR by `min + 1` per level of nesting: 31 levels of
/// `(?:…|)+` never finished compiling (issue #58). The copies one regex may
/// make cost at most this many nodes plus `LOOP_UNROLL_THRESHOLD` times the
/// regex's own size, which covers every loop of a regex without nested
/// unrolled loops; a loop the budget cannot pay for stays a loop, which
/// matches the same way.
const UNROLL_BUDGET_BASE: usize = 1 << 10;

/// What unrolling may still add to one regex. Sizes count one per node plus
/// one per interval a bracket's set holds, as copying the set costs that.
struct UnrollBudget {
    /// Size the copies may still add.
    size: usize,

    /// Loops the copies may still add: the emitter gives each loop a 16-bit
    /// id, and the parser's `MAX_LOOPS` only counts the loops in the source.
    loops: usize,
}

impl UnrollBudget {
    fn new(r: &Regex) -> Self {
        let (mut size, mut loops) = (0usize, 0usize);
        walk(false, r.flags.unicode, &r.node, &mut |n: &Node, _: &mut Walk| {
            size = size.saturating_add(copy_size(n));
            loops += matches!(n, Node::Loop { .. }) as usize;
        });
        UnrollBudget {
            size: size
                .saturating_mul(LOOP_UNROLL_THRESHOLD)
                .saturating_add(UNROLL_BUDGET_BASE),
            loops: MAX_LOOPS.saturating_sub(loops),
        }
    }

    /// The (size, loops) that `copies` copies of `body` would add, or None if
    /// the budget cannot pay for them. Stops measuring once over budget, so a
    /// declined loop costs no more than the budget left.
    fn price(&self, body: &Node, copies: usize) -> Option<(usize, usize)> {
        let allowance = self.size / copies;
        let mut left = allowance;
        let mut loops = 0;
        if !measure_copy(body, &mut left, &mut loops) {
            return None;
        }
        let loops = loops * copies;
        (loops <= self.loops).then_some(((allowance - left) * copies, loops))
    }

    fn spend(&mut self, (size, loops): (usize, usize)) {
        self.size -= size;
        self.loops -= loops;
    }
}

/// The size of copying `n` itself, without its children.
fn copy_size(n: &Node) -> usize {
    match n {
        Node::Bracket(bc) => 1 + bc.cps.intervals().len(),
        _ => 1,
    }
}

/// Subtract the size of copying `n` from `left` and count its loops in
/// `loops`; false, part-way, if `left` runs out.
fn measure_copy(n: &Node, left: &mut usize, loops: &mut usize) -> bool {
    let Some(rest) = left.checked_sub(copy_size(n)) else {
        return false;
    };
    *left = rest;
    match n {
        Node::Cat(nodes) => nodes.iter().all(|nn| measure_copy(nn, left, loops)),
        Node::Alt(a, b) => measure_copy(a, left, loops) && measure_copy(b, left, loops),
        Node::Loop { loopee, .. } => {
            *loops += 1;
            measure_copy(loopee, left, loops)
        }
        Node::Loop1CharBody { loopee, .. } => measure_copy(loopee, left, loops),
        Node::CaptureGroup { contents, .. } | Node::LookaroundAssertion { contents, .. } => {
            measure_copy(contents, left, loops)
        }
        _ => true,
    }
}

/// Things that a Pass may do.
pub enum PassAction {
    // Do nothing to the given node.
    Keep,

    // Notes that we modified the node in-place.
    Modified,

    // Remove the given node outright, effectively replacing it with empty.
    Remove,

    /// Replace the given node with a new Node.
    Replace(Node),
}

#[derive(Debug)]
struct Pass<'a, F>
where
    F: FnMut(&mut Node, &Walk) -> PassAction,
{
    // The function.
    func: &'a mut F,

    // Whether this pass has changed anything.
    changed: bool,

    // If the regex is in unicode mode.
    unicode: bool,
}

impl<'a, F> Pass<'a, F>
where
    F: FnMut(&mut Node, &Walk) -> PassAction,
{
    fn new(func: &'a mut F, unicode: bool) -> Self {
        Pass {
            func,
            changed: false,
            unicode,
        }
    }

    fn run_postorder(&mut self, start: &mut Node) {
        walk_mut(
            true,
            self.unicode,
            start,
            &mut |n: &mut Node, walk: &mut Walk| match (self.func)(n, walk) {
                PassAction::Keep => {}
                PassAction::Modified => {
                    self.changed = true;
                }
                PassAction::Remove => {
                    *n = Node::Empty;
                    self.changed = true;
                }
                PassAction::Replace(newnode) => {
                    *n = newnode;
                    self.changed = true;
                }
            },
        )
    }

    fn run_to_fixpoint(&mut self, n: &mut Node) {
        debug_assert!(!self.changed, "Pass has already been run");
        loop {
            self.changed = false;
            self.run_postorder(n);
            if !self.changed {
                break;
            }
        }
    }
}

/// Run a "pass" on a regex, which is a function that takes a Node and maybe
/// returns a new node. \return true if something changed, false if nothing did.
fn run_pass<F>(r: &mut Regex, func: &mut F) -> bool
where
    F: FnMut(&mut Node, &Walk) -> PassAction,
{
    let mut p = Pass::new(func, r.flags.unicode_mode());
    p.run_to_fixpoint(&mut r.node);
    p.changed
}

// Here are some optimizations we support.

// Remove empty Nodes.
fn remove_empties(n: &mut Node, _w: &Walk) -> PassAction {
    match n {
        Node::Empty | Node::Goal | Node::Char { .. } => PassAction::Keep,
        Node::ByteSequence(v) => {
            if v.is_empty() {
                PassAction::Remove
            } else {
                PassAction::Keep
            }
        }

        // Note: do not remove empty sets. These always match against one character; an empty set
        // should just fail.
        Node::ByteSet(..) | Node::CharSet(..) => PassAction::Keep,
        Node::Cat(nodes) => {
            let blen = nodes.len();
            nodes.retain(|nn| !nn.is_empty());
            if nodes.len() == blen {
                // Nothing was removed.
                PassAction::Keep
            } else {
                match nodes.len() {
                    0 => PassAction::Remove,
                    1 => PassAction::Replace(nodes.pop().unwrap()),
                    _ => PassAction::Modified,
                }
            }
        }
        Node::Alt(left, right) => {
            // Empty alt may match the empty string.
            // Remove it only if both sides are empty.
            if left.is_empty() && right.is_empty() {
                PassAction::Remove
            } else {
                PassAction::Keep
            }
        }
        Node::MatchAny | Node::MatchAnyExceptLineTerminator | Node::Anchor { .. } => {
            PassAction::Keep
        }
        Node::Loop {
            quant,
            loopee,
            enclosed_groups,
        } => {
            // A loop is empty if it has an empty body, or 0 max iters.
            // But do not remove contained capture groups.
            if loopee.is_empty()
                || (quant.max == Some(0) && enclosed_groups.start == enclosed_groups.end)
            {
                PassAction::Remove
            } else {
                PassAction::Keep
            }
        }
        Node::Loop1CharBody { .. } => PassAction::Keep,
        Node::CaptureGroup { .. } => {
            // Capture groups could in principle be optimized if they only match empties.
            PassAction::Keep
        }
        Node::WordBoundary { .. }
        | Node::BackRef { .. }
        | Node::NamedBackRef { .. }
        | Node::Bracket { .. } => PassAction::Keep,
        Node::LookaroundAssertion {
            negate, contents, ..
        } => {
            // Negative arounds that match empties could in principle be optimized to always
            // fail. Here we only optimize positive ones.
            if !*negate && contents.is_empty() {
                PassAction::Remove
            } else {
                PassAction::Keep
            }
        }
    }
}

/// Check if a node contains any capture groups (direct or nested)
fn contains_capture_groups(node: &Node) -> bool {
    match node {
        Node::CaptureGroup { .. } => true,
        Node::Cat(nodes) => nodes.iter().any(contains_capture_groups),
        Node::Alt(left, right) => contains_capture_groups(left) || contains_capture_groups(right),
        Node::Loop { loopee, .. } => contains_capture_groups(loopee),
        Node::LookaroundAssertion { contents, .. } => contains_capture_groups(contents),
        _ => false,
    }
}

// If a node can never match, replace it with an always fails node.
fn propagate_early_fails(n: &mut Node, _w: &Walk) -> PassAction {
    // Don't optimize nodes containing capture groups to preserve user-visible
    // group numbers. Each arm asks only once it has found something to
    // replace: asking first walked the whole subtree of every node, which made
    // the pass quadratic in the depth of the tree.
    match n {
        Node::Cat(nodes) => {
            // If any child is an early fail, we are an early fail.
            // Note this assumes that there is no node after a Goal node.
            if nodes.iter().any(|nn| nn.match_always_fails()) && !contains_capture_groups(n) {
                PassAction::Replace(Node::make_always_fails())
            } else {
                PassAction::Keep
            }
        }
        Node::Alt(left, right) => {
            // If both sides are early fails, we are an early fail.
            let left_fails = left.match_always_fails();
            let right_fails = right.match_always_fails();
            if (left_fails || right_fails)
                && (contains_capture_groups(left) || contains_capture_groups(right))
            {
                return PassAction::Keep;
            }
            match (left_fails, right_fails) {
                (true, true) => PassAction::Replace(Node::make_always_fails()),
                (false, false) => PassAction::Keep,
                (true, false) | (false, true) => {
                    // Here either our left or right node always fails.
                    // "Steal" the other and return it, replacing us.
                    let mut new_node = Node::Empty;
                    core::mem::swap(
                        &mut new_node,
                        if left_fails { &mut *right } else { &mut *left },
                    );
                    PassAction::Replace(new_node)
                }
            }
        }
        Node::Loop {
            loopee,
            quant,
            enclosed_groups,
        } => {
            if enclosed_groups.start < enclosed_groups.end {
                return PassAction::Keep;
            }
            // If the loop body always fails, we always fail.
            if quant.min > 0 && loopee.match_always_fails() {
                PassAction::Replace(Node::make_always_fails())
            } else {
                PassAction::Keep
            }
        }
        _ => PassAction::Keep,
    }
}

// Remove excess cats.
fn decat(n: &mut Node, _w: &Walk) -> PassAction {
    match n {
        Node::Cat(nodes) => {
            if nodes.is_empty() {
                PassAction::Remove
            } else if nodes.len() == 1 {
                PassAction::Replace(nodes.pop().unwrap())
            } else if nodes.iter().any(|nn| nn.is_cat()) {
                // Flatmap child cats.
                // Unfortunately we can't use flatmap() because there's no single iterator type
                // we can return.
                // Avoid copying nodes by switching them into owned vec.
                let mut catted = Vec::new();
                core::mem::swap(nodes, &mut catted);

                // Decat them.
                let mut decatted = Vec::new();
                for nn in catted {
                    match nn {
                        Node::Cat(mut nnodes) => {
                            decatted.append(&mut nnodes);
                        }
                        _ => decatted.push(nn),
                    }
                }
                PassAction::Replace(Node::Cat(decatted))
            } else {
                PassAction::Keep
            }
        }
        _ => PassAction::Keep,
    }
}

/// Unfold icase chars.
/// That means for case-insensitive characters, figure out everything that they
/// could match.
/// TODO: evaluate unfolding performance and consider a cache within the optimizer.
fn unfold_icase_chars(n: &mut Node, w: &Walk) -> PassAction {
    match *n {
        Node::Char { c, icase } if icase && !w.unicode => {
            let unfolded = unicode::unfold_uppercase_char(c);
            debug_assert!(
                unfolded.contains(&c),
                "Char should always unfold to at least itself"
            );
            match unfolded.len() {
                0 => panic!("Char should always unfold to at least itself"),
                1 => {
                    // Character does not fold or unfold at all.
                    PassAction::Replace(Node::Char { c, icase: false })
                }
                2..=MAX_BYTE_SET_LENGTH => {
                    // We unfolded to 2+ characters.
                    PassAction::Replace(Node::CharSet(unfolded))
                }
                _ => panic!("Unfolded to more characters than we believed possible"),
            }
        }
        Node::Char { c, icase } if icase => {
            let unfolded = unicode::unfold_char(c);
            debug_assert!(
                unfolded.contains(&c),
                "Char should always unfold to at least itself"
            );
            match unfolded.len() {
                0 => panic!("Char should always unfold to at least itself"),
                1 => {
                    // Character does not fold or unfold at all.
                    PassAction::Replace(Node::Char { c, icase: false })
                }
                2..=MAX_BYTE_SET_LENGTH => {
                    // We unfolded to 2+ characters.
                    PassAction::Replace(Node::CharSet(unfolded))
                }
                _ => panic!("Unfolded to more characters than we believed possible"),
            }
        }
        _ => PassAction::Keep,
    }
}

// Perform simple unrolling of loops that have a minimum.
fn unroll_loops(n: &mut Node, _w: &Walk, budget: &mut UnrollBudget) -> PassAction {
    match n {
        Node::Loop {
            loopee,
            quant,
            enclosed_groups,
        } => {
            // Do not unroll loops with enclosed groups.
            if enclosed_groups.start < enclosed_groups.end {
                return PassAction::Keep;
            }
            // Do not unroll large loops, or loops which may execute zero times.
            if quant.min == 0 || quant.min > LOOP_UNROLL_THRESHOLD {
                return PassAction::Keep;
            }
            // Nor loops whose copies the budget cannot pay for. The pass is
            // postorder, so inner loops, the ones run most, are paid for first.
            let Some(price) = budget.price(loopee, quant.min) else {
                return PassAction::Keep;
            };

            // We made it through. Replace us with a cat.
            let mut unrolled = Vec::new();
            for _ in 0..quant.min {
                let Some(node) = loopee.try_duplicate(0) else {
                    return PassAction::Keep;
                };
                unrolled.push(node);
            }
            budget.spend(price);

            // We unrolled 'min' elements.
            // Maybe our loop is now empty.
            quant.max = quant.max.map(|v| v - quant.min);
            quant.min = 0;
            if quant.max != Some(0) {
                // Move the loop to the end of unrolled.
                let mut loop_node = Node::Empty;
                core::mem::swap(&mut loop_node, n);
                unrolled.push(loop_node);
            }
            *n = Node::Cat(unrolled);
            PassAction::Modified
        }
        _ => PassAction::Keep,
    }
}

/// Replace Loops with 1Char loops whenever possible.
fn promote_1char_loops(n: &mut Node, _w: &Walk) -> PassAction {
    match n {
        Node::Loop {
            loopee,
            quant,
            enclosed_groups,
        } => {
            // Must be 1Char.
            if !loopee.matches_exactly_one_char() {
                return PassAction::Keep;
            }

            // The above check should be sufficient to ensure we have no enclosed groups.
            assert!(
                enclosed_groups.start >= enclosed_groups.end,
                "Should have no enclosed groups"
            );

            // This feels hackish?
            let mut new_loopee = Box::new(Node::Empty);
            core::mem::swap(&mut new_loopee, loopee);

            *n = Node::Loop1CharBody {
                loopee: new_loopee,
                quant: *quant,
            };
            PassAction::Modified
        }
        _ => PassAction::Keep,
    }
}

/// Replace Cat(Char) with ByteSeq.
/// Also replace chars with literal bytes.
/// PATCH (see VENDORED.md): no longer cfg'd out under `utf16` — instead it is
/// only RUN for programs that will match BYTE inputs (`optimize` without the
/// `utf16` feature, or `optimize_bytes`), because UTF-16 inputs must never
/// match against bytes.
/// TODO: this seems to do too much; consider breaking this up.
fn form_literal_bytes(n: &mut Node, walk: &Walk) -> PassAction {
    // Helper to return a mutable reference to the nodes of a literal bytes.
    fn get_literal_bytes(n: &mut Node) -> Option<&mut Vec<u8>> {
        match n {
            Node::ByteSequence(v) => Some(v),
            _ => None,
        }
    }
    match n {
        Node::Char { c, icase } if !*icase => {
            if let Some(c) = char::from_u32(*c) {
                let mut buff = [0; 4];
                PassAction::Replace(Node::ByteSequence(
                    c.encode_utf8(&mut buff).as_bytes().to_vec(),
                ))
            } else {
                PassAction::Keep
            }
        }
        Node::CharSet(chars) if chars.iter().all(|&c| c <= 0x7F) => {
            // All of our chars are ASCII; use a byte set instead.
            PassAction::Replace(Node::ByteSet(chars.iter().map(|&c| c as u8).collect()))
        }
        Node::Cat(nodes) => {
            // Find and merge adjacent ByteSeq.
            let mut modified = false;
            for idx in 1..nodes.len() {
                let (prev_slice, curr_slice) = nodes.split_at_mut(idx);
                match (
                    get_literal_bytes(prev_slice.last_mut().unwrap()),
                    get_literal_bytes(curr_slice.first_mut().unwrap()),
                ) {
                    (Some(prev_bytes), Some(curr_bytes))
                        if !prev_bytes.is_empty() && !curr_bytes.is_empty() =>
                    {
                        if walk.in_lookbehind {
                            // Our characters were already reversed; we need to reverse them again
                            // as literal bytes are always forwards. For
                            // example, if we have (<=ab), then we will get Cat(b, a) but we want
                            // literal bytes "ab".
                            curr_bytes.append(prev_bytes);
                        } else {
                            prev_bytes.append(curr_bytes);
                            core::mem::swap(prev_bytes, curr_bytes);
                        }
                        modified = true;
                    }
                    _ => (),
                }
            }
            if modified {
                PassAction::Modified
            } else {
                PassAction::Keep
            }
        }
        _ => PassAction::Keep,
    }
}

/// Try to reduce a bracket to something simpler.
fn try_reduce_bracket(bc: &BracketContents) -> Option<Node> {
    if bc.invert {
        // Give up.
        return None;
    }

    // Count the number of code points.
    let mut cps_count = 0;
    for iv in bc.cps.intervals() {
        cps_count += iv.count_codepoints();
    }
    if cps_count > MAX_CHAR_SET_LENGTH {
        // Too many code points.
        return None;
    }
    // Ok, we want to make a char set.
    // Note we cannot make a char out of surrogates; should char conversion fail we
    // just give up.
    let mut res = Vec::new();
    for iv in bc.cps.intervals() {
        for cp in iv.codepoints() {
            res.push(cp);
        }
    }
    debug_assert!(res.len() <= MAX_CHAR_SET_LENGTH, "Unexpectedly many chars");
    Some(Node::CharSet(res))
}

/// Optimize brackets like [a-b].
/// Optimize certain stupid brackets like `[a]` to a single char.
/// Invert a bracket if it would *reduce* the number of ranges.
/// Note we only run this once.
fn simplify_brackets(n: &mut Node, _walk: &Walk) -> PassAction {
    match n {
        Node::Bracket(bc) => {
            if let Some(new_node) = try_reduce_bracket(bc) {
                return PassAction::Replace(new_node);
            }

            // TODO: does this ever help anything?
            if bc.cps.intervals().len() > bc.cps.inverted_interval_count() {
                bc.cps = bc.cps.inverted();
                bc.invert = !bc.invert;
                PassAction::Modified
            } else {
                PassAction::Keep
            }
        }
        _ => PassAction::Keep,
    }
}

pub fn optimize(r: &mut Regex) {
    optimize_impl(r, cfg!(not(feature = "utf16")));
}

/// PATCH (see VENDORED.md): `optimize` INCLUDING the byte-literal passes.
/// The resulting program matches correctly ONLY against byte-element inputs
/// (`find_from` over `&str` / `find_from_ascii`) — never run it over
/// UTF-16/UCS-2 input.
pub fn optimize_bytes(r: &mut Regex) {
    optimize_impl(r, true);
}

fn optimize_impl(r: &mut Regex, byte_input: bool) {
    run_pass(r, &mut simplify_brackets);
    let mut unroll_budget = UnrollBudget::new(r);
    loop {
        let mut changed = false;
        changed |= run_pass(r, &mut decat);
        if r.flags.icase {
            changed |= run_pass(r, &mut unfold_icase_chars);
        }
        changed |= run_pass(r, &mut |n: &mut Node, w: &Walk| {
            unroll_loops(n, w, &mut unroll_budget)
        });
        changed |= run_pass(r, &mut promote_1char_loops);
        if byte_input {
            changed |= run_pass(r, &mut form_literal_bytes);
        }
        changed |= run_pass(r, &mut remove_empties);
        changed |= run_pass(r, &mut propagate_early_fails);
        if !changed {
            break;
        }
    }
}
