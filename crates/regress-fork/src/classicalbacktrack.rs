//! Classical backtracking execution engine

use crate::api::Match;
use crate::bytesearch;
use crate::cursor;
use crate::cursor::{Backward, Direction, Forward};
use crate::exec;
use crate::indexing;
use crate::indexing::{AsciiInput, ElementType, InputIndexer, Utf8Input};
use crate::insn::StartPredicate;
use crate::insn::{CompiledRegex, Insn, LoopFields};
use crate::matchers;
use crate::matchers::CharProperties;
use crate::position::PositionType;
use crate::scm;
use crate::scm::SingleCharMatcher;
use crate::types::{CaptureGroupID, GroupData, IP, LoopData, LoopID, MAX_CAPTURE_GROUPS};
use crate::util::DebugCheckIndex;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::ops::Range;

/// PATCH (see VENDORED.md): `ZIPP_RXSTATS=1` mechanism counters for the
/// possessify pass — how many match attempts ran, how many GreedyLoop1Char
/// backtrack entries were pushed vs elided as possessive, how many retry
/// iterations the pushed entries cost, and how many failed-run skips fired.
#[cfg(feature = "std")]
pub(crate) mod rxstats {
    use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

    pub static ATTEMPTS: AtomicU64 = AtomicU64::new(0);
    pub static GREEDY1_PUSHES: AtomicU64 = AtomicU64::new(0);
    pub static GREEDY1_RETRIES: AtomicU64 = AtomicU64::new(0);
    pub static POSSESSIVE_ELIDED: AtomicU64 = AtomicU64::new(0);
    pub static SKIPS: AtomicU64 = AtomicU64::new(0);

    /// Cached read of `ZIPP_RXSTATS`.
    #[inline(always)]
    pub fn enabled() -> bool {
        // 0 = unread; 1 = off; 2 = on.
        static CACHE: AtomicU8 = AtomicU8::new(0);
        match CACHE.load(Ordering::Relaxed) {
            0 => {
                let v = std::env::var_os("ZIPP_RXSTATS").is_some() as u8;
                CACHE.store(v + 1, Ordering::Relaxed);
                v == 1
            }
            v => v == 2,
        }
    }

    #[inline(always)]
    pub fn bump(c: &AtomicU64) {
        if enabled() {
            c.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// `ZIPP_RXSTATS=1` counters: (attempts, greedy 1-char backtrack pushes,
/// retry iterations, possessive pushes elided, failed-run skips).
#[cfg(feature = "std")]
pub fn rx_stats() -> (u64, u64, u64, u64, u64) {
    use core::sync::atomic::Ordering::Relaxed;
    (
        rxstats::ATTEMPTS.load(Relaxed),
        rxstats::GREEDY1_PUSHES.load(Relaxed),
        rxstats::GREEDY1_RETRIES.load(Relaxed),
        rxstats::POSSESSIVE_ELIDED.load(Relaxed),
        rxstats::SKIPS.load(Relaxed),
    )
}

/// No-op counter bump for no_std builds.
#[cfg(not(feature = "std"))]
macro_rules! rxstat {
    ($name:ident) => {};
}
#[cfg(feature = "std")]
macro_rules! rxstat {
    ($name:ident) => {
        rxstats::bump(&rxstats::$name)
    };
}

#[derive(Clone, Debug)]
enum BacktrackInsn<Input: InputIndexer> {
    /// Nothing more to backtrack.
    /// This "backstops" our stack.
    Exhausted,

    /// Restore the IP and position.
    SetPosition { ip: IP, pos: Input::Position },

    SetLoopData {
        id: LoopID,
        data: LoopData<Input::Position>,
    },

    SetCaptureGroup {
        id: CaptureGroupID,
        data: GroupData<Input::Position>,
    },

    EnterNonGreedyLoop {
        // The IP of the loop.
        // This is guaranteed to point to an EnterLoopInsn.
        ip: IP,
        // The input position of the loop before entering it.
        // This is used to set up backtracking that restores this position.
        orig_pos: Input::Position,
        data: LoopData<Input::Position>,
    },

    GreedyLoop1Char {
        continuation: IP,
        min: Input::Position,
        max: Input::Position,
    },

    NonGreedyLoop1Char {
        continuation: IP,
        min: Input::Position,
        max: Input::Position,
    },
}

#[derive(Debug, Default)]
struct State<Position: PositionType> {
    loops: Vec<LoopData<Position>>,
    groups: Vec<GroupData<Position>>,
}

#[derive(Debug)]
pub(crate) struct MatchAttempter<'a, Input: InputIndexer> {
    re: &'a CompiledRegex,
    bts: Vec<BacktrackInsn<Input>>,
    s: State<Input::Position>,
    // PATCH (see possessify.rs): end of the maximal run consumed by the
    // pattern's first-atom possessive unbounded loop during the current
    // attempt. When the attempt fails, no match starts inside that run, so
    // the search may resume here instead of at start+1.
    skip_hint: Option<Input::Position>,
}

impl<'a, Input: InputIndexer> MatchAttempter<'a, Input> {
    pub(crate) fn new(re: &'a CompiledRegex, entry: Input::Position) -> Self {
        Self {
            re,
            bts: vec![BacktrackInsn::Exhausted],
            s: State {
                loops: vec![LoopData::new(entry); re.loops as usize],
                groups: vec![GroupData::new(); re.groups as usize],
            },
            skip_hint: None,
        }
    }

    #[inline(always)]
    fn push_backtrack(&mut self, bt: BacktrackInsn<Input>) {
        self.bts.push(bt)
    }

    #[inline(always)]
    fn pop_backtrack(&mut self) {
        // Note we never pop the last instruction so this will never be empty.
        debug_assert!(!self.bts.is_empty());
        if cfg!(feature = "prohibit-unsafe") {
            self.bts.pop();
        } else {
            unsafe { self.bts.set_len(self.bts.len() - 1) }
        }
    }

    fn prepare_to_enter_loop(
        bts: &mut Vec<BacktrackInsn<Input>>,
        pos: Input::Position,
        loop_fields: &LoopFields,
        loop_data: &mut LoopData<Input::Position>,
    ) {
        bts.push(BacktrackInsn::SetLoopData {
            id: loop_fields.loop_id,
            data: *loop_data,
        });
        loop_data.iters += 1;
        loop_data.entry = pos;
    }

    fn run_loop(
        &mut self,
        loop_fields: &'a LoopFields,
        pos: Input::Position,
        ip: IP,
    ) -> Option<IP> {
        let loop_data = &mut self.s.loops[loop_fields.loop_id as usize];
        let iteration = loop_data.iters;

        let do_taken = iteration < loop_fields.max_iters;
        let do_not_taken = iteration >= loop_fields.min_iters;

        let loop_taken_ip = ip + 1;
        let loop_not_taken_ip = loop_fields.exit as IP;

        // If we have looped more than the minimum number of iterations, reject empty
        // matches. ES6 21.2.2.5.1 Note 4: "once the minimum number of
        // repetitions has been satisfied, any more expansions of Atom that match the
        // empty character sequence are not considered for further repetitions."
        if loop_data.entry == pos && iteration > loop_fields.min_iters {
            return None;
        }

        match (do_taken, do_not_taken) {
            (false, false) => {
                // No arms viable.
                None
            }
            (false, true) => {
                // Only skipping is viable.
                Some(loop_not_taken_ip)
            }
            (true, false) => {
                // Only entering is viable.
                MatchAttempter::prepare_to_enter_loop(&mut self.bts, pos, loop_fields, loop_data);
                Some(loop_taken_ip)
            }
            (true, true) if !loop_fields.greedy => {
                // Both arms are viable; backtrack into the loop.
                let orig_pos = loop_data.entry;
                loop_data.entry = pos;
                self.bts.push(BacktrackInsn::EnterNonGreedyLoop {
                    ip,
                    orig_pos,
                    data: *loop_data,
                });
                Some(loop_not_taken_ip)
            }
            (true, true) => {
                debug_assert!(loop_fields.greedy, "Should be greedy");
                // Both arms are viable; backtrack out of the loop.
                self.bts.push(BacktrackInsn::SetPosition {
                    ip: loop_not_taken_ip,
                    pos,
                });
                MatchAttempter::prepare_to_enter_loop(&mut self.bts, pos, loop_fields, loop_data);
                Some(loop_taken_ip)
            }
        }
    }

    // Drive the loop up to \p max times.
    // \return the position (min, max), or None on failure.
    #[inline(always)]
    fn run_scm_loop_impl<Dir: Direction, Scm: SingleCharMatcher<Input, Dir>>(
        input: &Input,
        mut pos: Input::Position,
        min: usize,
        max: usize,
        dir: Dir,
        matcher: Scm,
    ) -> Option<(Input::Position, Input::Position)> {
        debug_assert!(min <= max, "min should be <= max");
        // Drive the iteration min times.
        // That tells us the min position.
        for _ in 0..min {
            if !matcher.matches(input, dir, &mut pos) {
                return None;
            }
        }
        let min_pos = pos;

        // Drive it up to the max.
        for _ in 0..(max - min) {
            let saved = pos;
            if !matcher.matches(input, dir, &mut pos) {
                pos = saved;
                break;
            }
        }
        let max_pos = pos;
        Some((min_pos, max_pos))
    }

    // Compute the maximum position from a starting position, up to a limit.
    // This is used for lazy computation in non-greedy loops.
    fn compute_max_pos<Dir: Direction, Scm: SingleCharMatcher<Input, Dir>>(
        input: &Input,
        mut pos: Input::Position,
        limit: usize,
        dir: Dir,
        matcher: Scm,
    ) -> Input::Position {
        for _ in 0..limit {
            let saved = pos;
            if !matcher.matches(input, dir, &mut pos) {
                pos = saved;
                break;
            }
        }
        pos
    }

    // Helper function to extract the duplicated match blocks that handle different instruction types
    // with different matcher functions. This significantly reduces code duplication and compile times.
    fn with_scm_loop_impl<Dir: Direction>(
        re: &CompiledRegex,
        input: &Input,
        pos: Input::Position,
        min: usize,
        max: usize,
        dir: Dir,
        ip: IP,
    ) -> Option<(Input::Position, Input::Position)> {
        match re.insns.iat(ip + 1) {
            &Insn::Char(c) => {
                let c = <<Input as InputIndexer>::Element as ElementType>::try_from(c)?;
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::Char { c })
            }
            &Insn::CharICase(c) => {
                let c = <<Input as InputIndexer>::Element as ElementType>::try_from(c)?;
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::CharICase { c })
            }
            &Insn::Bracket(idx) => {
                let bc = &re.brackets[idx];
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::Bracket { bc })
            }
            Insn::AsciiBracket(bitmap) => Self::run_scm_loop_impl(
                input,
                pos,
                min,
                max,
                dir,
                scm::MatchByteSet { bytes: bitmap },
            ),
            Insn::MatchAny => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchAny::new())
            }
            Insn::MatchAnyExceptLineTerminator => Self::run_scm_loop_impl(
                input,
                pos,
                min,
                max,
                dir,
                scm::MatchAnyExceptLineTerminator::new(),
            ),
            Insn::CharSet(chars) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::CharSet { chars })
            }
            &Insn::ByteSet2(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteArraySet(bytes))
            }
            &Insn::ByteSet3(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteArraySet(bytes))
            }
            &Insn::ByteSet4(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteArraySet(bytes))
            }
            Insn::ByteSeq1(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq2(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq3(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq4(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq5(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq6(bytes) => {
                Self::run_scm_loop_impl(input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            _ => {
                unreachable!("Missing SCM: {:?}", re.insns.iat(ip + 1));
            }
        }
    }

    // Helper function for compute_max_pos to avoid duplication
    fn with_scm_compute_max<Dir: Direction>(
        re: &CompiledRegex,
        input: &Input,
        pos: Input::Position,
        limit: usize,
        dir: Dir,
        ip: IP,
    ) -> Option<Input::Position> {
        let result = match re.insns.iat(ip + 1) {
            &Insn::Char(c) => {
                let c = <<Input as InputIndexer>::Element as ElementType>::try_from(c)?;
                Self::compute_max_pos(input, pos, limit, dir, scm::Char { c })
            }
            &Insn::CharICase(c) => {
                let c = <<Input as InputIndexer>::Element as ElementType>::try_from(c)?;
                Self::compute_max_pos(input, pos, limit, dir, scm::CharICase { c })
            }
            &Insn::Bracket(idx) => {
                let bc = &re.brackets[idx];
                Self::compute_max_pos(input, pos, limit, dir, scm::Bracket { bc })
            }
            Insn::AsciiBracket(bitmap) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteSet { bytes: bitmap })
            }
            Insn::MatchAny => Self::compute_max_pos(input, pos, limit, dir, scm::MatchAny::new()),
            Insn::MatchAnyExceptLineTerminator => Self::compute_max_pos(
                input,
                pos,
                limit,
                dir,
                scm::MatchAnyExceptLineTerminator::new(),
            ),
            Insn::CharSet(chars) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::CharSet { chars })
            }
            &Insn::ByteSet2(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteArraySet(bytes))
            }
            &Insn::ByteSet3(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteArraySet(bytes))
            }
            &Insn::ByteSet4(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteArraySet(bytes))
            }
            Insn::ByteSeq1(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq2(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq3(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq4(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq5(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq6(bytes) => {
                Self::compute_max_pos(input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            _ => {
                unreachable!("Missing SCM: {:?}", re.insns.iat(ip + 1));
            }
        };
        Some(result)
    }

    // Given that ip points at a loop whose body matches exactly one character, run
    // a "single character loop". The big idea here is that we don't need to save
    // our position every iteration: we know that our loop body matches a single
    // character so we can backtrack by matching a character backwards.
    // \return the next IP, or None if the loop failed.
    #[allow(clippy::too_many_arguments)]
    fn run_scm_loop<Dir: Direction>(
        &mut self,
        input: &Input,
        dir: Dir,
        pos: &mut Input::Position,
        min: usize,
        max: usize,
        ip: IP,
        greedy: bool,
        possessive: bool,
    ) -> Option<IP> {
        // For non-greedy loops, we can avoid computing the maximum match eagerly.
        // We'll only compute it when we need to set up backtracking.
        let (min_pos, max_pos) = if greedy {
            // For greedy loops, compute both min and max positions
            Self::with_scm_loop_impl(self.re, input, *pos, min, max, dir, ip)?
        } else {
            // For non-greedy loops, initially only compute the minimum position
            let (min_pos, _) = Self::with_scm_loop_impl(self.re, input, *pos, min, min, dir, ip)?;

            // For non-greedy loops, we only compute the max position if we need to set up backtracking
            let max_pos = if min < max {
                // We need to compute the max for backtracking purposes
                Self::with_scm_compute_max(self.re, input, min_pos, max - min, dir, ip)?
            } else {
                min_pos
            };
            (min_pos, max_pos)
        };

        debug_assert!(
            if Dir::FORWARD {
                min_pos <= max_pos
            } else {
                min_pos >= max_pos
            },
            "min should be <= (>=) max if cursor is tracking forwards (backwards)"
        );

        // Oh no where is the continuation? It's one past the loop body, which is one
        // past the loop. Strap in!
        let continuation = ip + 2;
        // PATCH (see possessify.rs): a possessive greedy loop's backtrack
        // entry is provably dead — every retry re-tests a disjoint follow at
        // a position holding a loop-class character — so skip the push. If
        // this loop is also the pattern's first atom and unbounded, record
        // the run end: a failed attempt then proves no match starts inside
        // the run (the Phase 2 argument), letting the search skip it.
        if greedy && possessive {
            if min_pos != max_pos {
                rxstat!(POSSESSIVE_ELIDED);
            }
            if Dir::FORWARD && self.re.skip_hint_ip == Some(ip as u32) {
                self.skip_hint = Some(max_pos);
            }
            *pos = max_pos;
            return Some(continuation);
        }
        if min_pos != max_pos {
            // Backtracking is possible.
            let bti = if greedy {
                rxstat!(GREEDY1_PUSHES);
                BacktrackInsn::GreedyLoop1Char {
                    continuation,
                    min: min_pos,
                    max: max_pos,
                }
            } else {
                BacktrackInsn::NonGreedyLoop1Char {
                    continuation,
                    min: min_pos,
                    max: max_pos,
                }
            };
            self.bts.push(bti);
        }

        // Start at the max (min) if greedy (nongreedy).
        *pos = if greedy { max_pos } else { min_pos };
        Some(continuation)
    }

    // Run a lookaround instruction, which is either forwards or backwards
    // (according to Direction). The half-open range
    // start_group..end_group is the range of contained capture groups.
    // \return whether we matched and negate was false, or did not match but negate
    // is true.
    fn run_lookaround<Dir: Direction>(
        &mut self,
        input: &Input,
        ip: IP,
        pos: Input::Position,
        start_group: CaptureGroupID,
        end_group: CaptureGroupID,
        negate: bool,
    ) -> bool {
        // Copy capture groups, because if the match fails (or if we are inverted)
        // we need to restore these.
        let range = (start_group as usize)..(end_group as usize);
        // TODO: consider retaining storage here?
        // Temporarily defeat backtracking.
        let saved_groups = self.s.groups.iat(range.clone()).to_vec();

        // Start with an "empty" backtrack stack.
        // TODO: consider using a stack-allocated array.
        let mut saved_bts = vec![BacktrackInsn::Exhausted];
        core::mem::swap(&mut self.bts, &mut saved_bts);

        // Enter into the lookaround's instruction stream.
        let matched = self.try_at_pos(*input, ip, pos, Dir::new()).is_some();

        // Put back our bts.
        core::mem::swap(&mut self.bts, &mut saved_bts);

        // If we are a positive lookahead that successfully matched, retain the
        // capture groups (but we need to set up backtracking). Otherwise restore
        // them.
        if matched && !negate {
            for (idx, cg) in saved_groups.iter().enumerate() {
                debug_assert!(idx + (start_group as usize) < MAX_CAPTURE_GROUPS);
                self.push_backtrack(BacktrackInsn::SetCaptureGroup {
                    id: (idx as CaptureGroupID) + start_group,
                    data: *cg,
                });
            }
        } else {
            self.s.groups.splice(range, saved_groups);
        }
        matched != negate
    }

    /// Attempt to backtrack.
    /// \return true if we backtracked, false if we exhaust the backtrack stack.
    fn try_backtrack<Dir: Direction>(
        &mut self,
        input: &Input,
        ip: &mut IP,
        pos: &mut Input::Position,
        _dir: Dir,
    ) -> bool {
        loop {
            // We always have a single Exhausted instruction backstopping our stack,
            // so we do not need to check for empty bts.
            debug_assert!(!self.bts.is_empty(), "Backtrack stack should not be empty");
            let bt = match self.bts.last_mut() {
                Some(bt) => bt,
                None => rs_unreachable!("BT stack should never be empty"),
            };
            match bt {
                BacktrackInsn::Exhausted => return false,

                BacktrackInsn::SetPosition {
                    ip: saved_ip,
                    pos: saved_pos,
                } => {
                    *ip = *saved_ip;
                    *pos = *saved_pos;
                    self.pop_backtrack();
                    return true;
                }
                BacktrackInsn::SetLoopData { id, data } => {
                    *self.s.loops.mat(*id as usize) = *data;
                    self.pop_backtrack();
                }
                BacktrackInsn::SetCaptureGroup { id, data } => {
                    *self.s.groups.mat(*id as usize) = *data;
                    self.pop_backtrack();
                }

                &mut BacktrackInsn::EnterNonGreedyLoop {
                    ip: loop_ip,
                    orig_pos,
                    data,
                } => {
                    *ip = loop_ip + 1;
                    *pos = data.entry;
                    let loop_fields = match &self.re.insns.iat(loop_ip) {
                        Insn::EnterLoop(loop_fields) => loop_fields,
                        _ => rs_unreachable!("EnterNonGreedyLoop must point at a loop instruction"),
                    };
                    let loop_data = self.s.loops.mat(loop_fields.loop_id as usize);
                    // Need to restore the position should we backtrack out of the loop (#131).
                    *bt = BacktrackInsn::SetLoopData {
                        id: loop_fields.loop_id,
                        data: LoopData {
                            entry: orig_pos,
                            ..data
                        },
                    };
                    *loop_data = data;
                    MatchAttempter::prepare_to_enter_loop(
                        &mut self.bts,
                        *pos,
                        loop_fields,
                        loop_data,
                    );
                    return true;
                }

                BacktrackInsn::GreedyLoop1Char {
                    continuation,
                    min,
                    max,
                } => {
                    // The match failed at the max location.
                    debug_assert!(
                        if Dir::FORWARD { max >= min } else { max <= min },
                        "max should be >= min (or <= if tracking backwards)"
                    );
                    // If min is equal to max, there is no more backtracking to be done;
                    // otherwise move opposite the direction of the cursor.
                    if *max == *min {
                        // We have backtracked this loop as far as possible.
                        self.bts.pop();
                        continue;
                    }
                    let newmax = if Dir::FORWARD {
                        input.next_left_pos(*max)
                    } else {
                        input.next_right_pos(*max)
                    };
                    if let Some(newmax) = newmax {
                        *pos = newmax;
                        *max = newmax;
                    } else {
                        rs_unreachable!("Should always be able to advance since min != max")
                    }
                    *ip = *continuation;
                    rxstat!(GREEDY1_RETRIES);
                    return true;
                }

                BacktrackInsn::NonGreedyLoop1Char {
                    continuation,
                    min,
                    max,
                } => {
                    // The match failed at the min location.
                    debug_assert!(
                        if Dir::FORWARD { max >= min } else { max <= min },
                        "max should be >= min (or <= if tracking backwards)"
                    );
                    if *max == *min {
                        // We have backtracked this loop as far as possible.
                        self.bts.pop();
                        continue;
                    }
                    // Move in the direction of the cursor.
                    let newmin = if Dir::FORWARD {
                        input.next_right_pos(*min)
                    } else {
                        input.next_left_pos(*min)
                    };
                    if let Some(newmin) = newmin {
                        *pos = newmin;
                        *min = newmin;
                    } else {
                        rs_unreachable!("Should always be able to advance since min != max")
                    }
                    *ip = *continuation;
                    return true;
                }
            }
        }
    }

    /// Attempt to match at a given IP and position.
    fn try_at_pos<Dir: Direction>(
        &mut self,
        inp: Input,
        mut ip: IP,
        mut pos: Input::Position,
        dir: Dir,
    ) -> Option<Input::Position> {
        debug_assert!(
            self.bts.len() == 1,
            "Should be only initial exhausted backtrack insn"
        );
        // TODO: we are inconsistent about passing Input by reference or value.
        let input = &inp;
        let re = self.re;
        // These are not really loops, they are just labels that we effectively 'goto'
        // to.
        #[allow(clippy::never_loop)]
        'nextinsn: loop {
            'backtrack: loop {
                // Helper macro to either increment ip and go to the next insn, or backtrack.
                macro_rules! next_or_bt {
                    ($e:expr) => {
                        if $e {
                            ip += 1;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    };
                }

                match re.insns.iat(ip) {
                    &Insn::Char(c) => {
                        let m = match <<Input as InputIndexer>::Element as ElementType>::try_from(c)
                        {
                            Some(c) => scm::Char { c }.matches(input, dir, &mut pos),
                            None => false,
                        };
                        next_or_bt!(m);
                    }

                    Insn::CharSet(chars) => {
                        let m = scm::CharSet { chars }.matches(input, dir, &mut pos);
                        next_or_bt!(m);
                    }

                    &Insn::ByteSet2(bytes) => {
                        next_or_bt!(scm::MatchByteArraySet(bytes).matches(input, dir, &mut pos))
                    }
                    &Insn::ByteSet3(bytes) => {
                        next_or_bt!(scm::MatchByteArraySet(bytes).matches(input, dir, &mut pos))
                    }
                    &Insn::ByteSet4(bytes) => {
                        next_or_bt!(scm::MatchByteArraySet(bytes).matches(input, dir, &mut pos))
                    }

                    Insn::ByteSeq1(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq2(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq3(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq4(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq5(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq6(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq7(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq8(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq9(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq10(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq11(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq12(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq13(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq14(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq15(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq16(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }

                    &Insn::CharICase(c) => {
                        let m = match <<Input as indexing::InputIndexer>::Element as indexing::ElementType>::try_from(c) {
                            Some(c) => scm::CharICase { c }.matches(input, dir, &mut pos),
                            None => false,
                        };
                        next_or_bt!(m)
                    }

                    Insn::AsciiBracket(bitmap) => next_or_bt!(
                        scm::MatchByteSet { bytes: bitmap }.matches(input, dir, &mut pos)
                    ),

                    &Insn::Bracket(idx) => {
                        next_or_bt!(
                            scm::Bracket {
                                bc: &self.re.brackets[idx]
                            }
                            .matches(input, dir, &mut pos)
                        )
                    }

                    Insn::MatchAny => {
                        next_or_bt!(scm::MatchAny::new().matches(input, dir, &mut pos))
                    }

                    Insn::MatchAnyExceptLineTerminator => {
                        next_or_bt!(
                            scm::MatchAnyExceptLineTerminator::new().matches(input, dir, &mut pos)
                        )
                    }

                    &Insn::WordBoundary { invert } => {
                        // Copy the positions since these destructively move them.
                        let prev_wordchar = input
                            .peek_left(pos)
                            .is_some_and(Input::CharProps::is_word_char);
                        let curr_wordchar = input
                            .peek_right(pos)
                            .is_some_and(Input::CharProps::is_word_char);
                        let is_boundary = prev_wordchar != curr_wordchar;
                        next_or_bt!(is_boundary != invert)
                    }

                    &Insn::WordBoundaryUnicodeICase { invert } => {
                        let prev_wordchar = input
                            .peek_left(pos)
                            .is_some_and(Input::CharProps::is_word_char_unicode_icase);
                        let curr_wordchar = input
                            .peek_right(pos)
                            .is_some_and(Input::CharProps::is_word_char_unicode_icase);
                        let is_boundary = prev_wordchar != curr_wordchar;
                        next_or_bt!(is_boundary != invert)
                    }

                    Insn::StartOfLine { multiline } => {
                        let multiline = *multiline;
                        let matches = match input.peek_left(pos) {
                            None => true,
                            Some(c) if multiline && Input::CharProps::is_line_terminator(c) => true,
                            _ => false,
                        };
                        next_or_bt!(matches)
                    }
                    Insn::EndOfLine { multiline } => {
                        let multiline = *multiline;
                        let matches = match input.peek_right(pos) {
                            None => true, // we're at the right of the string
                            Some(c) if multiline && Input::CharProps::is_line_terminator(c) => true,
                            _ => false,
                        };
                        next_or_bt!(matches)
                    }

                    &Insn::Jump { target } => {
                        ip = target as usize;
                        continue 'nextinsn;
                    }

                    &Insn::BeginCaptureGroup(cg_idx) => {
                        let cg = self.s.groups.mat(cg_idx as usize);
                        self.bts.push(BacktrackInsn::SetCaptureGroup {
                            id: cg_idx,
                            data: *cg,
                        });
                        if Dir::FORWARD {
                            cg.start = Some(pos);
                            debug_assert!(
                                cg.end.is_none(),
                                "Should not have already exited capture group we are entering"
                            )
                        } else {
                            cg.end = Some(pos);
                            debug_assert!(
                                cg.start.is_none(),
                                "Should not have already exited capture group we are entering"
                            )
                        }
                        next_or_bt!(true)
                    }

                    &Insn::EndCaptureGroup(cg_idx) => {
                        let cg = self.s.groups.mat(cg_idx as usize);
                        if Dir::FORWARD {
                            debug_assert!(
                                cg.start_matched(),
                                "Capture group should have been entered"
                            );
                            cg.end = Some(pos);
                        } else {
                            debug_assert!(
                                cg.end_matched(),
                                "Capture group should have been entered"
                            );
                            cg.start = Some(pos)
                        }
                        next_or_bt!(true)
                    }

                    &Insn::ResetCaptureGroup(cg_idx) => {
                        let cg = self.s.groups.mat(cg_idx as usize);
                        self.bts.push(BacktrackInsn::SetCaptureGroup {
                            id: cg_idx,
                            data: *cg,
                        });
                        cg.reset();
                        next_or_bt!(true)
                    }

                    &Insn::BackRef { group: cg_idx, icase } => {
                        let cg = self.s.groups.mat(cg_idx as usize);
                        // Backreferences to a capture group that did not match always succeed (ES5
                        // 15.10.2.9).
                        // Note we may be in the capture group we are examining, e.g. /(abc\1)/.
                        let matched;
                        if let Some(orig_range) = cg.as_range() {
                            if icase {
                                matched = matchers::backref_icase(input, dir, orig_range, &mut pos);
                            } else {
                                matched = matchers::backref(input, dir, orig_range, &mut pos);
                            }
                        } else {
                            // This group has not been exited and so the match succeeds (ES6
                            // 21.2.2.9).
                            matched = true;
                        }
                        next_or_bt!(matched)
                    }

                    Insn::BackRefMulti { groups, icase } => {
                        // Backreference to a name shared by several groups (necessarily in
                        // distinct alternatives): at most one can have participated. Match
                        // against that one, or succeed with an empty match when none did.
                        let mut matched = true;
                        for &g in groups.iter() {
                            if let Some(orig_range) = self.s.groups.mat(g as usize).as_range() {
                                matched = if *icase {
                                    matchers::backref_icase(input, dir, orig_range, &mut pos)
                                } else {
                                    matchers::backref(input, dir, orig_range, &mut pos)
                                };
                                break;
                            }
                        }
                        next_or_bt!(matched)
                    }

                    &Insn::Lookahead {
                        negate,
                        start_group,
                        end_group,
                        continuation,
                    } => {
                        if self.run_lookaround::<Forward>(
                            input,
                            ip + 1,
                            pos,
                            start_group,
                            end_group,
                            negate,
                        ) {
                            ip = continuation as IP;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    }

                    &Insn::Lookbehind {
                        negate,
                        start_group,
                        end_group,
                        continuation,
                    } => {
                        if self.run_lookaround::<Backward>(
                            input,
                            ip + 1,
                            pos,
                            start_group,
                            end_group,
                            negate,
                        ) {
                            ip = continuation as IP;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    }

                    &Insn::Alt { secondary } => {
                        self.push_backtrack(BacktrackInsn::SetPosition {
                            ip: secondary as IP,
                            pos,
                        });
                        next_or_bt!(true);
                    }

                    Insn::EnterLoop(fields) => {
                        // Entering a loop, not re-entering it.
                        self.s.loops.mat(fields.loop_id as usize).iters = 0;
                        match self.run_loop(fields, pos, ip) {
                            Some(next_ip) => {
                                ip = next_ip;
                                continue 'nextinsn;
                            }
                            None => {
                                break 'backtrack;
                            }
                        }
                    }

                    &Insn::LoopAgain { begin } => {
                        let act = match re.insns.iat(begin as IP) {
                            Insn::EnterLoop(fields) => self.run_loop(fields, pos, begin as IP),
                            _ => rs_unreachable!("EnterLoop should always refer to loop field"),
                        };
                        match act {
                            Some(next_ip) => {
                                ip = next_ip;
                                continue 'nextinsn;
                            }
                            None => break 'backtrack,
                        }
                    }

                    &Insn::Loop1CharBody {
                        min_iters,
                        max_iters,
                        greedy,
                        possessive,
                    } => {
                        if let Some(next_ip) = self.run_scm_loop(
                            input, dir, &mut pos, min_iters, max_iters, ip, greedy, possessive,
                        ) {
                            ip = next_ip;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    }

                    Insn::Goal => {
                        // Keep all but the initial give-up bts.
                        self.bts.truncate(1);
                        return Some(pos);
                    }

                    Insn::JustFail => {
                        break 'backtrack;
                    }
                }
            }

            // This after the backtrack loop.
            // A break 'backtrack will jump here.
            if self.try_backtrack(input, &mut ip, &mut pos, dir) {
                continue 'nextinsn;
            } else {
                // We have exhausted the backtracking stack.
                debug_assert!(self.bts.len() == 1, "Should have exhausted backtrack stack");
                return None;
            }
        }

        // This is outside the nextinsn loop.
        // It is an error to get here.
        // Every instruction should either continue 'nextinsn, or break 'backtrack.
        {
            #![allow(unreachable_code)]
            rs_unreachable!("Should not fall to end of nextinsn loop")
        }
    }
}

#[derive(Debug)]
pub struct BacktrackExecutor<'r, Input: InputIndexer> {
    input: Input,
    matcher: MatchAttempter<'r, Input>,
}

#[cfg(feature = "utf16")]
impl<'r, Input: InputIndexer> BacktrackExecutor<'r, Input> {
    pub(crate) fn new(input: Input, matcher: MatchAttempter<'r, Input>) -> Self {
        Self { input, matcher }
    }
}

impl<Input: InputIndexer> BacktrackExecutor<'_, Input> {
    /// PATCH (see rxjit.rs): one top-level forward match attempt — native
    /// code when this regex has compiled and the input is byte-addressed,
    /// the interpreter otherwise. Both paths leave identical observable
    /// state: capture groups in `matcher.s.groups`, `matcher.skip_hint`,
    /// and the returned end position.
    #[inline(always)]
    fn attempt_at(&mut self, pos: Input::Position) -> Option<Input::Position> {
        let inp = self.input;
        #[cfg(all(feature = "rx-jit", target_arch = "x86_64"))]
        if let Some(bytes) = inp.rxjit_bytes() {
            let re: &CompiledRegex = self.matcher.re;
            if let Some(code) = re.rxjit.acquire(re) {
                let origin = inp.left_end();
                let groups = &mut self.matcher.s.groups;
                let outcome =
                    crate::rxjit::run_attempt(code, bytes, inp.pos_to_offset(pos), |g, s, e| {
                        let gd = groups.mat(g);
                        gd.start = (s != u64::MAX).then(|| origin + s as usize);
                        gd.end = (e != u64::MAX).then(|| origin + e as usize);
                    });
                let hint = |h: u64| (h != u64::MAX).then(|| origin + h as usize);
                match outcome {
                    crate::rxjit::Outcome::Match { end, skip_hint } => {
                        self.matcher.skip_hint = hint(skip_hint);
                        return Some(origin + end);
                    }
                    crate::rxjit::Outcome::NoMatch { skip_hint } => {
                        self.matcher.skip_hint = hint(skip_hint);
                        return None;
                    }
                    // Native gave up (backtrack buffer cap); rerun this
                    // attempt in the interpreter.
                    crate::rxjit::Outcome::Bail => {}
                }
            }
        }
        self.matcher.try_at_pos(inp, 0, pos, Forward::new())
    }

    fn successful_match(&mut self, start: Input::Position, end: Input::Position) -> Match {
        // We want to simultaneously map our groups to offsets, and clear the groups.
        // A for loop is the easiest way to do this while satisfying the borrow checker.
        // TODO: avoid allocating so much.
        let mut captures = Vec::new();
        captures.reserve_exact(self.matcher.s.groups.len());
        for gd in self.matcher.s.groups.iter_mut() {
            captures.push(match gd.as_range() {
                Some(r) => Some(Range {
                    start: self.input.pos_to_offset(r.start),
                    end: self.input.pos_to_offset(r.end),
                }),
                None => None,
            });
            gd.start = None;
            gd.end = None;
        }
        Match {
            range: self.input.pos_to_offset(start)..self.input.pos_to_offset(end),
            captures,
            group_names: self.matcher.re.group_names.clone(),
        }
    }

    /// \return the next match for an anchored regex that only matches at the start.
    /// This avoids any string searching and only tries matching at the given position.
    fn next_match_anchored(
        &mut self,
        pos: Input::Position,
        next_start: &mut Option<Input::Position>,
    ) -> Option<Match> {
        let inp = self.input;
        // For anchored regexes, only try matching at the current position
        rxstat!(ATTEMPTS);
        if let Some(end) = self.attempt_at(pos) {
            // If we matched the empty string, we have to increment.
            if end != pos {
                *next_start = Some(end)
            } else {
                *next_start = inp.next_right_pos(end);
            }
            Some(self.successful_match(pos, end))
        } else {
            // Anchored regex failed to match at this position, no more matches
            None
        }
    }

    /// \return the next match, searching the remaining bytes using the given
    /// prefix searcher to quickly find the first potential match location.
    fn next_match_with_prefix_search<PrefixSearch: bytesearch::ByteSearcher>(
        &mut self,
        mut pos: Input::Position,
        next_start: &mut Option<Input::Position>,
        prefix_search: &PrefixSearch,
    ) -> Option<Match> {
        let inp = self.input;
        // PATCH (see rxjit.rs): when this regex has native code and the input
        // is byte-addressed, run the whole advance loop inside one scan
        // session — the per-attempt TLS borrow and context build hoist out.
        // The loop body below is move-for-move the legacy loop's, with
        // `attempt_at` unrolled onto `Session::attempt`.
        #[cfg(all(feature = "rx-jit", target_arch = "x86_64"))]
        if crate::rxjit::session_enabled() {
            if let Some(bytes) = inp.rxjit_bytes() {
                let re: &CompiledRegex = self.matcher.re;
                if let Some(code) = re.rxjit.acquire(re) {
                    return crate::rxjit::with_session(code, bytes, |sess| {
                        loop {
                            if Input::CODE_UNITS_ARE_BYTES {
                                pos = inp.find_bytes(pos, prefix_search)?;
                            }
                            // PATCH (see possessify.rs): a fresh attempt must
                            // not observe a hint recorded by an earlier one.
                            self.matcher.skip_hint = None;
                            rxstat!(ATTEMPTS);
                            let origin = inp.left_end();
                            let groups = &mut self.matcher.s.groups;
                            let outcome = sess.attempt(inp.pos_to_offset(pos), |g, s, e| {
                                let gd = groups.mat(g);
                                gd.start = (s != u64::MAX).then(|| origin + s as usize);
                                gd.end = (e != u64::MAX).then(|| origin + e as usize);
                            });
                            let hint = |h: u64| (h != u64::MAX).then(|| origin + h as usize);
                            let end = match outcome {
                                crate::rxjit::Outcome::Match { end, skip_hint } => {
                                    self.matcher.skip_hint = hint(skip_hint);
                                    Some(origin + end)
                                }
                                crate::rxjit::Outcome::NoMatch { skip_hint } => {
                                    self.matcher.skip_hint = hint(skip_hint);
                                    None
                                }
                                // Native gave up (backtrack buffer cap);
                                // rerun this attempt in the interpreter.
                                crate::rxjit::Outcome::Bail => {
                                    self.matcher.try_at_pos(inp, 0, pos, Forward::new())
                                }
                            };
                            if let Some(end) = end {
                                // If we matched the empty string, we have to increment.
                                if end != pos {
                                    *next_start = Some(end)
                                } else {
                                    *next_start = inp.next_right_pos(end);
                                }
                                return Some(self.successful_match(pos, end));
                            }
                            // Didn't find it at this position, try the next one.
                            // PATCH (see possessify.rs): if the failed attempt proved a whole
                            // run matchless, resume after the run instead.
                            let hint = self.matcher.skip_hint.take();
                            pos = inp.next_right_pos(pos)?;
                            if let Some(h) = hint {
                                if h > pos {
                                    rxstat!(SKIPS);
                                    pos = h;
                                }
                            }
                        }
                    });
                }
            }
        }
        loop {
            // Find the next start location, or None if none.
            // Don't try this unless CODE_UNITS_ARE_BYTES - i.e. don't do byte searches
            // on UTF-16 or UCS2.
            if Input::CODE_UNITS_ARE_BYTES {
                pos = inp.find_bytes(pos, prefix_search)?;
            }
            // PATCH (see possessify.rs): a fresh attempt must not observe a
            // hint recorded by an earlier one.
            self.matcher.skip_hint = None;
            rxstat!(ATTEMPTS);
            if let Some(end) = self.attempt_at(pos) {
                // If we matched the empty string, we have to increment.
                if end != pos {
                    *next_start = Some(end)
                } else {
                    *next_start = inp.next_right_pos(end);
                }
                return Some(self.successful_match(pos, end));
            }
            // Didn't find it at this position, try the next one.
            // PATCH (see possessify.rs): if the failed attempt proved a whole
            // run matchless, resume after the run instead.
            let hint = self.matcher.skip_hint.take();
            pos = inp.next_right_pos(pos)?;
            if let Some(h) = hint {
                if h > pos {
                    rxstat!(SKIPS);
                    pos = h;
                }
            }
        }
    }

    /// PATCH (see VENDORED.md): the range-copy half of `successful_match` —
    /// map the winning attempt's group data into caller-owned offset ranges,
    /// resetting each `GroupData` exactly as `successful_match` does, without
    /// building a `Match` (no per-match captures Vec, no group-name clone).
    fn take_group_ranges(&mut self, out: &mut Vec<Option<Range<usize>>>) {
        out.clear();
        for gd in self.matcher.s.groups.iter_mut() {
            out.push(gd.as_range().map(|r| Range {
                start: self.input.pos_to_offset(r.start),
                end: self.input.pos_to_offset(r.end),
            }));
            gd.start = None;
            gd.end = None;
        }
    }

    /// PATCH (see VENDORED.md): drained multi-match scan (`Regex::scan_ascii`'s
    /// engine). Semantically `cap` iterations of `exec::Matches::next` over
    /// this executor — the identical attempt sequence and the identical
    /// advance (next position = the match end, or one past it for an empty
    /// match) — except each hit is handed to `sink` as raw offset ranges
    /// instead of an allocated `Match`. \return true when the subject is
    /// exhausted (no match exists past the last emitted one), false when the
    /// scan stopped at `cap` hits.
    pub(crate) fn scan_drain(
        &mut self,
        start: usize,
        cap: usize,
        sink: &mut dyn FnMut(Range<usize>, &[Option<Range<usize>>]),
    ) -> bool {
        let re: &CompiledRegex = self.matcher.re;
        match &re.start_pred {
            StartPredicate::Arbitrary => {
                self.scan_drain_with_prefix_search(start, cap, sink, &bytesearch::EmptyString {})
            }
            StartPredicate::StartAnchored => self.scan_drain_anchored(start, cap, sink),
            StartPredicate::ByteSet1(bytes) => {
                self.scan_drain_with_prefix_search(start, cap, sink, bytes)
            }
            StartPredicate::ByteSet2(bytes) => {
                self.scan_drain_with_prefix_search(start, cap, sink, bytes)
            }
            StartPredicate::ByteSet3(bytes) => {
                self.scan_drain_with_prefix_search(start, cap, sink, bytes)
            }
            StartPredicate::ByteSeq(bytes) => {
                self.scan_drain_with_prefix_search(start, cap, sink, bytes.as_ref())
            }
            StartPredicate::ByteBracket(bitmap) => {
                self.scan_drain_with_prefix_search(start, cap, sink, bitmap)
            }
        }
    }

    /// The drained form of `next_match_anchored`: per hit the attempt and the
    /// advance are move-for-move that loop's, `sink` replaces
    /// `successful_match`.
    fn scan_drain_anchored(
        &mut self,
        start: usize,
        cap: usize,
        sink: &mut dyn FnMut(Range<usize>, &[Option<Range<usize>>]),
    ) -> bool {
        let inp = self.input;
        let Some(mut pos) = inp.try_move_right(inp.left_end(), start) else {
            return true;
        };
        let mut caps_buf: Vec<Option<Range<usize>>> =
            Vec::with_capacity(self.matcher.re.groups as usize);
        let mut emitted = 0usize;
        while emitted < cap {
            rxstat!(ATTEMPTS);
            let Some(end) = self.attempt_at(pos) else {
                return true;
            };
            self.take_group_ranges(&mut caps_buf);
            sink(inp.pos_to_offset(pos)..inp.pos_to_offset(end), &caps_buf);
            emitted += 1;
            // If we matched the empty string, we have to increment.
            pos = if end != pos {
                end
            } else {
                match inp.next_right_pos(end) {
                    Some(p) => p,
                    None => return true,
                }
            };
        }
        false
    }

    /// The drained form of `next_match_with_prefix_search`: up to `cap` hits
    /// from ONE executor, with the whole multi-match advance loop inside one
    /// scan session when the regex has native code — the per-attempt TLS
    /// borrow and context build hoist out across MATCHES too, not just across
    /// the positions of one. Per position the attempt sequence, group writes,
    /// skip-hint handling and interpreter Bail fallback are move-for-move the
    /// one-match loop's.
    fn scan_drain_with_prefix_search<PrefixSearch: bytesearch::ByteSearcher>(
        &mut self,
        start: usize,
        cap: usize,
        sink: &mut dyn FnMut(Range<usize>, &[Option<Range<usize>>]),
        prefix_search: &PrefixSearch,
    ) -> bool {
        let inp = self.input;
        let Some(mut pos) = inp.try_move_right(inp.left_end(), start) else {
            return true;
        };
        let mut caps_buf: Vec<Option<Range<usize>>> =
            Vec::with_capacity(self.matcher.re.groups as usize);
        let mut emitted = 0usize;
        #[cfg(all(feature = "rx-jit", target_arch = "x86_64"))]
        if crate::rxjit::session_enabled() {
            if let Some(bytes) = inp.rxjit_bytes() {
                let re: &CompiledRegex = self.matcher.re;
                if let Some(code) = re.rxjit.acquire(re) {
                    return crate::rxjit::with_session(code, bytes, |sess| {
                        'hits: while emitted < cap {
                            loop {
                                if Input::CODE_UNITS_ARE_BYTES {
                                    pos = match inp.find_bytes(pos, prefix_search) {
                                        Some(p) => p,
                                        None => return true,
                                    };
                                }
                                // PATCH (see possessify.rs): a fresh attempt must
                                // not observe a hint recorded by an earlier one.
                                self.matcher.skip_hint = None;
                                rxstat!(ATTEMPTS);
                                let origin = inp.left_end();
                                let groups = &mut self.matcher.s.groups;
                                let outcome = sess.attempt(inp.pos_to_offset(pos), |g, s, e| {
                                    let gd = groups.mat(g);
                                    gd.start = (s != u64::MAX).then(|| origin + s as usize);
                                    gd.end = (e != u64::MAX).then(|| origin + e as usize);
                                });
                                let hint = |h: u64| (h != u64::MAX).then(|| origin + h as usize);
                                let end = match outcome {
                                    crate::rxjit::Outcome::Match { end, skip_hint } => {
                                        self.matcher.skip_hint = hint(skip_hint);
                                        Some(origin + end)
                                    }
                                    crate::rxjit::Outcome::NoMatch { skip_hint } => {
                                        self.matcher.skip_hint = hint(skip_hint);
                                        None
                                    }
                                    // Native gave up (backtrack buffer cap);
                                    // rerun this attempt in the interpreter.
                                    crate::rxjit::Outcome::Bail => {
                                        self.matcher.try_at_pos(inp, 0, pos, Forward::new())
                                    }
                                };
                                if let Some(end) = end {
                                    self.take_group_ranges(&mut caps_buf);
                                    sink(
                                        inp.pos_to_offset(pos)..inp.pos_to_offset(end),
                                        &caps_buf,
                                    );
                                    emitted += 1;
                                    // If we matched the empty string, we have to increment.
                                    pos = if end != pos {
                                        end
                                    } else {
                                        match inp.next_right_pos(end) {
                                            Some(p) => p,
                                            None => return true,
                                        }
                                    };
                                    continue 'hits;
                                }
                                // Didn't find it at this position, try the next one.
                                // PATCH (see possessify.rs): if the failed attempt
                                // proved a whole run matchless, resume after the
                                // run instead.
                                let hint = self.matcher.skip_hint.take();
                                pos = match inp.next_right_pos(pos) {
                                    Some(p) => p,
                                    None => return true,
                                };
                                if let Some(h) = hint {
                                    if h > pos {
                                        rxstat!(SKIPS);
                                        pos = h;
                                    }
                                }
                            }
                        }
                        false
                    });
                }
            }
        }
        'hits: while emitted < cap {
            loop {
                if Input::CODE_UNITS_ARE_BYTES {
                    pos = match inp.find_bytes(pos, prefix_search) {
                        Some(p) => p,
                        None => return true,
                    };
                }
                // PATCH (see possessify.rs): a fresh attempt must not observe a
                // hint recorded by an earlier one.
                self.matcher.skip_hint = None;
                rxstat!(ATTEMPTS);
                if let Some(end) = self.attempt_at(pos) {
                    self.take_group_ranges(&mut caps_buf);
                    sink(inp.pos_to_offset(pos)..inp.pos_to_offset(end), &caps_buf);
                    emitted += 1;
                    // If we matched the empty string, we have to increment.
                    pos = if end != pos {
                        end
                    } else {
                        match inp.next_right_pos(end) {
                            Some(p) => p,
                            None => return true,
                        }
                    };
                    continue 'hits;
                }
                // Didn't find it at this position, try the next one.
                // PATCH (see possessify.rs): if the failed attempt proved a whole
                // run matchless, resume after the run instead.
                let hint = self.matcher.skip_hint.take();
                pos = match inp.next_right_pos(pos) {
                    Some(p) => p,
                    None => return true,
                };
                if let Some(h) = hint {
                    if h > pos {
                        rxstat!(SKIPS);
                        pos = h;
                    }
                }
            }
        }
        false
    }
}

/// PATCH (see VENDORED.md): one drained ASCII scan — `Regex::scan_ascii`'s
/// entry into this module. One executor (and, with rx-jit, one scan session)
/// serves up to `cap` matches; per hit `sink` receives the raw offset ranges
/// and no `Match` is built. \return whether the subject was exhausted.
pub(crate) fn scan_ascii_drain(
    re: &CompiledRegex,
    text: &str,
    start: usize,
    cap: usize,
    sink: &mut dyn FnMut(Range<usize>, &[Option<Range<usize>>]),
) -> bool {
    let input = AsciiInput::new(text, re.flags.unicode_mode());
    let mut ex = BacktrackExecutor {
        input,
        matcher: MatchAttempter::new(re, input.left_end()),
    };
    ex.scan_drain(start, cap, sink)
}

impl<Input: InputIndexer> exec::MatchProducer for BacktrackExecutor<'_, Input> {
    type Position = Input::Position;

    fn initial_position(&self, offset: usize) -> Option<Self::Position> {
        self.input.try_move_right(self.input.left_end(), offset)
    }

    fn next_match(
        &mut self,
        pos: Input::Position,
        next_start: &mut Option<Input::Position>,
    ) -> Option<Match> {
        // PATCH (perf, see VENDORED.md): dispatch on the start predicate for
        // ALL input types, not only when the `utf16` feature is off. The byte
        // searchers only engage for byte-element inputs (the
        // `Input::CODE_UNITS_ARE_BYTES` check inside
        // `next_match_with_prefix_search` skips them for UTF-16/UCS-2 input),
        // and `StartAnchored` is encoding-independent, so enabling the
        // dispatch under `utf16` is purely an optimization: without it every
        // `find_from`/`find_from_ascii` degraded to a try-at-every-position
        // scan whenever the feature was compiled in.
        match &self.matcher.re.start_pred {
            StartPredicate::Arbitrary => {
                self.next_match_with_prefix_search(pos, next_start, &bytesearch::EmptyString {})
            }
            StartPredicate::StartAnchored => self.next_match_anchored(pos, next_start),
            StartPredicate::ByteSet1(bytes) => {
                self.next_match_with_prefix_search(pos, next_start, bytes)
            }
            StartPredicate::ByteSet2(bytes) => {
                self.next_match_with_prefix_search(pos, next_start, bytes)
            }
            StartPredicate::ByteSet3(bytes) => {
                self.next_match_with_prefix_search(pos, next_start, bytes)
            }
            StartPredicate::ByteSeq(bytes) => {
                self.next_match_with_prefix_search(pos, next_start, bytes.as_ref())
            }
            StartPredicate::ByteBracket(bitmap) => {
                self.next_match_with_prefix_search(pos, next_start, bitmap)
            }
        }
    }
}

impl<'r, 't> exec::Executor<'r, 't> for BacktrackExecutor<'r, Utf8Input<'t>> {
    type AsAscii = BacktrackExecutor<'r, AsciiInput<'t>>;

    fn new(re: &'r CompiledRegex, text: &'t str) -> Self {
        let input = Utf8Input::new(text, re.flags.unicode_mode());
        Self {
            input,
            matcher: MatchAttempter::new(re, input.left_end()),
        }
    }
}

impl<'r, 't> exec::Executor<'r, 't> for BacktrackExecutor<'r, AsciiInput<'t>> {
    type AsAscii = BacktrackExecutor<'r, AsciiInput<'t>>;

    fn new(re: &'r CompiledRegex, text: &'t str) -> Self {
        let input = AsciiInput::new(text, re.flags.unicode_mode());
        Self {
            input,
            matcher: MatchAttempter::new(re, input.left_end()),
        }
    }
}
