//! Execution engine bits.

#[cfg(feature = "bounded-backtracking")]
use crate::api::MatchLimitError;
use crate::api::{Match, MatchUsage};
use crate::insn::CompiledRegex;
use crate::position::PositionType;

/// A trait for finding the next match in a regex.
/// This is broken out from Executor to avoid needing to thread lifetimes
/// around.
pub trait MatchProducer: core::fmt::Debug {
    /// The position type of our indexer.
    type Position: PositionType;

    /// \return an initial position for the given start offset.
    fn initial_position(&self, offset: usize) -> Option<Self::Position>;

    /// Attempt to match at the given location.
    /// \return either the Match and the position to start looking for the next
    /// match, or None on failure.
    fn next_match(
        &mut self,
        pos: Self::Position,
        next_start: &mut Option<Self::Position>,
    ) -> Option<Match>;

    /// Work consumed and the first hard ceiling crossed by this producer.
    /// Unmetered backends use the zero-cost default.
    #[inline]
    fn match_usage(&self) -> MatchUsage {
        MatchUsage::UNMETERED
    }
}

/// A trait for executing a regex.
pub trait Executor<'r, 't>: MatchProducer {
    /// The ASCII variant.
    type AsAscii: Executor<'r, 't>;

    /// Construct a new Executor.
    fn new(re: &'r CompiledRegex, text: &'t str) -> Self;
}

/// A struct which enables iteration over matches.
#[derive(Debug)]
pub struct Matches<Producer: MatchProducer> {
    mp: Producer,
    position: Option<Producer::Position>,
}

impl<Producer: MatchProducer> Matches<Producer> {
    pub fn new(mp: Producer, start: usize) -> Self {
        let position = mp.initial_position(start);
        Matches { mp, position }
    }

    /// Return the producer's current meter state without consuming it.
    #[inline]
    pub fn match_usage(&self) -> MatchUsage {
        self.mp.match_usage()
    }

    /// Collect at most `max_items` while hard-capping the output `Vec` backing
    /// allocation. Per-match capture buffers remain charged to the producer's
    /// own memory ceiling. This is used by the sandbox before replacement
    /// processing retains every global match.
    #[cfg(feature = "bounded-backtracking")]
    pub fn try_collect_with_memory_limit(
        &mut self,
        max_items: usize,
        max_bytes: usize,
    ) -> Result<Vec<Match>, MatchLimitError> {
        let mut output = Vec::new();
        while output.len() < max_items {
            let Some(found) = self.next() else {
                break;
            };
            if output.len() == output.capacity() {
                let element_bytes = core::mem::size_of::<Match>().max(1);
                let current_bytes = output.capacity().saturating_mul(element_bytes);
                let available_entries = max_bytes.saturating_sub(current_bytes) / element_bytes;
                let grow_by = if output.capacity() == 0 {
                    4.min(available_entries)
                } else {
                    output.capacity().min(available_entries)
                };
                if grow_by == 0 || output.try_reserve_exact(grow_by).is_err() {
                    return Err(MatchLimitError::BacktrackMemory);
                }
                if output.capacity().saturating_mul(element_bytes) > max_bytes {
                    return Err(MatchLimitError::BacktrackMemory);
                }
            }
            output.push(found);
        }
        match self.match_usage().exhaustion {
            Some(error) => Err(error),
            None => Ok(output),
        }
    }
}

impl<Producer: MatchProducer> Iterator for Matches<Producer> {
    type Item = Match;
    fn next(&mut self) -> Option<Self::Item> {
        let pos = self.position?;
        self.mp.next_match(pos, &mut self.position)
    }
}
