//! Pre-tokenization: splitting text before BPE ever sees it.
//!
//! BPE on its own merges across word boundaries and produces a single token for
//! `Mr.Smith`. Every byte-level vocabulary is therefore trained against a
//! specific splitting pattern, and using the wrong one produces token ids that
//! are individually valid and collectively wrong -- the failure mode that looks
//! like a slightly stupid model rather than like a bug.
//!
//! The patterns are regular expressions upstream. They are hand-scanned here,
//! for two reasons: a regex engine with Unicode classes is a large dependency
//! to put in a browser for six alternatives, and `core` already knows which
//! characters are letters, digits and whitespace.
//!
//! The alternatives are ordered, and ordered alternation is what the scanner
//! reproduces: try each in turn at the current position, take the first that
//! matches. Where upstream relies on backtracking -- `\s+(?!\S)` -- the scanner
//! computes the same answer directly.

use alloc::vec::Vec;

/// Which pattern a vocabulary was trained with. GGUF records this as
/// `tokenizer.ggml.pre`; getting it wrong is not a crash, it is worse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    /// GPT-2's original. Letters and digits each take an optional leading
    /// space; no case-insensitive contractions.
    Gpt2,
    /// Qwen2 and Qwen3: digits are split **one at a time**, and a letter run
    /// may absorb one preceding non-letter that is not a line break.
    Qwen2,
    /// Llama-3, GPT-4: as Qwen2, but digits come in runs of up to three.
    Llama3,
}

impl Pattern {
    /// The `tokenizer.ggml.pre` value a GGUF file records.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "qwen2" => Some(Self::Qwen2),
            "llama3" | "llama-v3" | "llama-bpe" | "gpt-4" | "dbrx" => Some(Self::Llama3),
            "default" | "gpt-2" | "olmo" | "jais" => Some(Self::Gpt2),
            _ => None,
        }
    }

    fn max_digits(self) -> usize {
        match self {
            Self::Qwen2 => 1,
            Self::Llama3 => 3,
            Self::Gpt2 => usize::MAX,
        }
    }
}

fn letter(c: char) -> bool { c.is_alphabetic() }
fn digit(c: char) -> bool { c.is_numeric() }
fn space(c: char) -> bool { c.is_whitespace() }
fn newline(c: char) -> bool { c == '\r' || c == '\n' }

/// Split `text` into pre-tokens, in order. Their concatenation is `text`.
pub fn split(text: &str, pattern: Pattern) -> Vec<&str> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut pieces = Vec::new();
    let mut at = 0usize;
    while at < chars.len() {
        let taken = piece(&chars, at, pattern);
        // A pattern that consumed nothing would loop; every branch below
        // returns at least one, and this is the belt for that brace.
        let taken = if taken == 0 { 1 } else { taken };
        let start = chars[at].0;
        let end = chars.get(at + taken).map_or(text.len(), |&(offset, _)| offset);
        pieces.push(&text[start..end]);
        at += taken;
    }
    pieces
}

/// How many characters the next piece takes, in ordered-alternation order.
fn piece(chars: &[(usize, char)], at: usize, pattern: Pattern) -> usize {
    let n = chars.len();
    let c = |i: usize| chars[i].1;

    // 1. Contractions. Qwen2 and Llama3 match them case-insensitively; GPT-2
    //    matches only the lowercase forms it was trained on.
    if c(at) == '\'' && at + 1 < n {
        let fold = |ch: char| if pattern == Pattern::Gpt2 { ch } else { ch.to_ascii_lowercase() };
        let one = fold(c(at + 1));
        if matches!(one, 's' | 't' | 'm' | 'd') { return 2; }
        if at + 2 < n {
            let two = (one, fold(c(at + 2)));
            if matches!(two, ('r', 'e') | ('v', 'e') | ('l', 'l')) { return 3; }
        }
    }

    // 2. A run of letters, optionally absorbing one character before it.
    //    GPT-2 absorbs only a space; the others absorb any non-letter,
    //    non-digit that is not a line break.
    {
        let mut i = at;
        let absorbs = match pattern {
            Pattern::Gpt2 => c(i) == ' ',
            _ => !letter(c(i)) && !digit(c(i)) && !newline(c(i)),
        };
        if absorbs { i += 1; }
        if i < n && letter(c(i)) {
            while i < n && letter(c(i)) { i += 1; }
            return i - at;
        }
    }

    // 3. Digits, in runs the pattern allows. GPT-2 takes a leading space with
    //    them; Qwen2 takes one digit at a time and no space.
    {
        let mut i = at;
        if pattern == Pattern::Gpt2 && c(i) == ' ' && i + 1 < n && digit(c(i + 1)) { i += 1; }
        if i < n && digit(c(i)) {
            let limit = pattern.max_digits();
            let start = i;
            while i < n && digit(c(i)) && i - start < limit { i += 1; }
            return i - at;
        }
    }

    // 4. Punctuation, optionally with a leading space and trailing line breaks.
    {
        let mut i = at;
        if c(i) == ' ' { i += 1; }
        let start = i;
        while i < n && !space(c(i)) && !letter(c(i)) && !digit(c(i)) { i += 1; }
        if i > start {
            if pattern != Pattern::Gpt2 {
                while i < n && newline(c(i)) { i += 1; }
            }
            return i - at;
        }
    }

    // 5. Whitespace ending in a line break: `\s*[\r\n]+`, which is the longest
    //    whitespace run whose last character is a break.
    if pattern != Pattern::Gpt2 {
        let mut i = at;
        while i < n && space(c(i)) { i += 1; }
        let mut end = i;
        while end > at && !newline(c(end - 1)) { end -= 1; }
        if end > at { return end - at; }
    }

    // 6. `\s+(?!\S)`: whitespace not followed by anything else. Upstream gets
    //    here by backtracking; the answer is the whole run at the end of the
    //    text, and one short of it otherwise -- the last space belongs to the
    //    word that follows, which rule 2 will take.
    {
        let mut i = at;
        while i < n && space(c(i)) { i += 1; }
        if i > at {
            if i == n { return i - at; }
            if i - at > 1 { return i - at - 1; }
        }
    }

    // 7. Whatever whitespace is left.
    {
        let mut i = at;
        while i < n && space(c(i)) { i += 1; }
        if i > at { return i - at; }
    }

    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    fn pieces(text: &str, pattern: Pattern) -> Vec<&str> { split(text, pattern) }

    #[test]
    fn every_split_is_lossless() {
        let samples = ["The capital of France is Paris.", "Mr.Smith said hi.",
                       "12345 and 0", "  leading", "trailing   ", "a\n\nb", "\r\n\r\n",
                       "emoji 👋 and accents café", "", " ", "'s 'RE won't"];
        for pattern in [Pattern::Gpt2, Pattern::Qwen2, Pattern::Llama3] {
            for text in samples {
                let joined: String = pieces(text, pattern).concat();
                assert_eq!(joined, text, "{pattern:?} lost bytes on {text:?}");
            }
        }
    }

    #[test]
    fn qwen2_splits_digits_one_at_a_time() {
        assert_eq!(pieces("12345", Pattern::Qwen2), ["1", "2", "3", "4", "5"]);
        // Llama3 takes them three at a time, which is the whole difference.
        assert_eq!(pieces("12345", Pattern::Llama3), ["123", "45"]);
    }

    #[test]
    fn a_word_keeps_its_leading_space() {
        assert_eq!(pieces("The capital of", Pattern::Qwen2), ["The", " capital", " of"]);
        assert_eq!(pieces("The capital of", Pattern::Gpt2), ["The", " capital", " of"]);
    }

    #[test]
    fn two_spaces_give_the_word_only_one() {
        assert_eq!(pieces("a  b", Pattern::Qwen2), ["a", " ", " b"]);
    }

    #[test]
    fn punctuation_separates_from_words() {
        assert_eq!(pieces("Mr.Smith", Pattern::Qwen2), ["Mr", ".Smith"]);
        assert_eq!(pieces("hi.", Pattern::Qwen2), ["hi", "."]);
    }

    #[test]
    fn contractions_fold_case_except_for_gpt2() {
        assert_eq!(pieces("won't", Pattern::Qwen2), ["won", "'t"]);
        assert_eq!(pieces("WON'T", Pattern::Qwen2), ["WON", "'T"]);
        // GPT-2 was trained only on the lowercase forms.
        assert_eq!(pieces("WON'T", Pattern::Gpt2), ["WON", "'", "T"]);
    }

    #[test]
    fn names_map_to_patterns() {
        assert_eq!(Pattern::from_name("qwen2"), Some(Pattern::Qwen2));
        assert_eq!(Pattern::from_name("llama-bpe"), Some(Pattern::Llama3));
        assert_eq!(Pattern::from_name("default"), Some(Pattern::Gpt2));
        assert_eq!(Pattern::from_name("something-else"), None);
    }
}
