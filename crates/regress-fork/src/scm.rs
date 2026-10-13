use crate::bytesearch::{ByteArraySet, ByteSet, SmallArraySet, charset_contains};
use crate::cursor;
use crate::cursor::Direction;
use crate::indexing::{ElementType, InputIndexer};
use crate::insn::{CompiledRegex, Insn, MAX_CHAR_SET_LENGTH};
use crate::matchers::CharProperties;
use crate::types::BracketContents;

/// A trait for things that match a single Element.
pub trait SingleCharMatcher<Input: InputIndexer, Dir: Direction> {
    /// \return whether we match the character at the given position, advancing
    /// the position if so. On a false return, the position is unspecified.
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position) -> bool;
}

/// Insn::Char
pub struct Char<Input: InputIndexer> {
    pub c: Input::Element,
}
impl<Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir> for Char<Input> {
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position) -> bool {
        match cursor::next(input, dir, pos) {
            Some(c2) => c2 == self.c,
            _ => false,
        }
    }
}

/// Insn::CharICase
pub struct CharICase<Input: InputIndexer> {
    pub c: Input::Element,
}
impl<Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir> for CharICase<Input> {
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position) -> bool {
        match cursor::next(input, dir, pos) {
            Some(c2) => c2 == self.c || input.fold(c2) == self.c,
            _ => false,
        }
    }
}

/// Insn::CharSet
pub struct CharSet<'a> {
    pub chars: &'a [u32; MAX_CHAR_SET_LENGTH],
}

impl<Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir> for CharSet<'_> {
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position) -> bool {
        match cursor::next(input, dir, pos) {
            Some(c) => charset_contains(self.chars, c.as_u32()),
            None => false,
        }
    }
}

/// Insn::Bracket
pub struct Bracket<'a> {
    pub bc: &'a BracketContents,
}

impl<Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir> for Bracket<'_> {
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position) -> bool {
        match cursor::next(input, dir, pos) {
            Some(c) => Input::CharProps::bracket(self.bc, c),
            _ => false,
        }
    }
}

/// Insn::MatchAny
pub struct MatchAny {}
impl MatchAny {
    pub fn new() -> Self {
        Self {}
    }
}
impl<Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir> for MatchAny {
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position) -> bool {
        // If there is a character, it counts as a match.
        cursor::next(input, dir, pos).is_some()
    }
}

/// Insn::MatchAnyExceptLineTerminator
pub struct MatchAnyExceptLineTerminator {}
impl MatchAnyExceptLineTerminator {
    pub fn new() -> Self {
        Self {}
    }
}
impl<Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir>
    for MatchAnyExceptLineTerminator
{
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position) -> bool {
        match cursor::next(input, dir, pos) {
            Some(c2) => !Input::CharProps::is_line_terminator(c2),
            _ => false,
        }
    }
}

/// Any ByteSet may match a single char.
pub struct MatchByteSet<'a, Bytes: ByteSet> {
    pub bytes: &'a Bytes,
}

impl<Input: InputIndexer, Dir: Direction, Bytes: ByteSet> SingleCharMatcher<Input, Dir>
    for MatchByteSet<'_, Bytes>
{
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position) -> bool {
        if Input::CODE_UNITS_ARE_BYTES {
            // Code units are bytes so we can skip decoding the full element.
            cursor::next_byte(input, dir, pos).is_some_and(|b| self.bytes.contains(b))
        } else {
            // Must decode the full element.
            cursor::next(input, dir, pos)
                .and_then(|c| c.as_u32().try_into().ok())
                .is_some_and(|c| self.bytes.contains(c))
        }
    }
}

/// Provide a variant for ByteArraySet where we hold it directly.
/// The arrays are small and so we don't want to indirect through a pointer.
pub struct MatchByteArraySet<ArraySet: SmallArraySet>(pub ByteArraySet<ArraySet>);

impl<Input: InputIndexer, Dir: Direction, ArraySet: SmallArraySet> SingleCharMatcher<Input, Dir>
    for MatchByteArraySet<ArraySet>
{
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position) -> bool {
        if let Some(b) = cursor::next_byte(input, dir, pos) {
            self.0.contains(b)
        } else {
            false
        }
    }
}

/// A ByteSeq of length <= 4 may match a single char.
pub struct MatchByteSeq<'a, const N: usize>(pub &'a [u8; N]);

impl<const N: usize, Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir>
    for MatchByteSeq<'_, N>
{
    #[inline(always)]
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position) -> bool {
        debug_assert!(N <= 4, "This looks like it could match more than one char");
        cursor::try_match_lit(input, dir, pos, self.0)
    }
}

/// Whichever one-character instruction `insn` is, decided per character: the
/// body of a one-character loop that runs right to left (in a lookbehind).
/// One matcher for every body keeps the loop machinery from being built
/// again, for that direction, for each matcher type above.
#[derive(Clone, Copy)]
pub struct Dyn<'a> {
    pub re: &'a CompiledRegex,
    pub insn: &'a Insn,
}

impl<Input: InputIndexer, Dir: Direction> SingleCharMatcher<Input, Dir> for Dyn<'_> {
    fn matches(&self, input: &Input, dir: Dir, pos: &mut Input::Position) -> bool {
        match self.insn {
            &Insn::Char(c) => match <Input::Element as ElementType>::try_from(c) {
                Some(c) => Char::<Input> { c }.matches(input, dir, pos),
                None => false,
            },
            &Insn::CharICase(c) => match <Input::Element as ElementType>::try_from(c) {
                Some(c) => CharICase::<Input> { c }.matches(input, dir, pos),
                None => false,
            },
            &Insn::Bracket(idx) => Bracket {
                bc: &self.re.brackets[idx],
            }
            .matches(input, dir, pos),
            Insn::AsciiBracket(bytes) => MatchByteSet { bytes }.matches(input, dir, pos),
            Insn::MatchAny => MatchAny::new().matches(input, dir, pos),
            Insn::MatchAnyExceptLineTerminator => {
                MatchAnyExceptLineTerminator::new().matches(input, dir, pos)
            }
            Insn::CharSet(chars) => CharSet { chars }.matches(input, dir, pos),
            &Insn::ByteSet2(bytes) => MatchByteArraySet(bytes).matches(input, dir, pos),
            &Insn::ByteSet3(bytes) => MatchByteArraySet(bytes).matches(input, dir, pos),
            &Insn::ByteSet4(bytes) => MatchByteArraySet(bytes).matches(input, dir, pos),
            Insn::ByteSeq1(bytes) => MatchByteSeq(bytes).matches(input, dir, pos),
            Insn::ByteSeq2(bytes) => MatchByteSeq(bytes).matches(input, dir, pos),
            Insn::ByteSeq3(bytes) => MatchByteSeq(bytes).matches(input, dir, pos),
            Insn::ByteSeq4(bytes) => MatchByteSeq(bytes).matches(input, dir, pos),
            Insn::ByteSeq5(bytes) => MatchByteSeq(bytes).matches(input, dir, pos),
            Insn::ByteSeq6(bytes) => MatchByteSeq(bytes).matches(input, dir, pos),
            _ => unreachable!("Missing SCM"),
        }
    }
}
