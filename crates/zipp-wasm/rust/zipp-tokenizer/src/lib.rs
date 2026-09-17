//! Byte-level BPE, as the vocabularies inside GGUF files define it.
//!
//! Drawn from the `tokenizer` crate of the `llm` repository, rebuilt to run
//! without `std` and with two things that one left open, both of which change
//! the token ids a prompt produces:
//!
//!   * **The pre-tokenizer is per-vocabulary.** Qwen2 splits digits one at a
//!     time; Llama-3 takes them three at a time; GPT-2 takes a run with its
//!     leading space. A GGUF file names which one it was trained with, in
//!     `tokenizer.ggml.pre`, and `pre.rs` scans that pattern rather than
//!     applying a single canonical one.
//!   * **Merges are ranked by position.** The GGUF merge list is in priority
//!     order and the lowest rank wins, which is what makes BPE deterministic.
//!
//! Getting either wrong produces token ids that are individually valid and
//! collectively wrong -- a model that reads as slightly stupid rather than
//! visibly broken. So `encode` is checked against known ids, not just against
//! its own `decode`.
//!
//! Why this is Rust rather than plugin Python: a vocabulary is 151,936 entries
//! and a merge table 151,387, and building those maps in the guest measured in
//! minutes. Here it is milliseconds, and the tables never cross a boundary.

#![cfg_attr(not(test), no_std)]
#![deny(rust_2018_idioms)]

extern crate alloc;

pub mod pre;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub use pre::Pattern;

/// The bytes that stand for themselves in GPT-2's byte alphabet.
fn is_kept(b: u8) -> bool {
    (b'!'..=b'~').contains(&b) || (0xA1..=0xAC).contains(&b) || (0xAE..=0xFF).contains(&b)
}

/// Byte to "visible" character, and back.
///
/// Byte-level BPE needs every byte to be a character that cannot be confused
/// with the whitespace the pre-tokenizer splits on, so the 68 bytes that are
/// not printable get moved to U+0100 and up. A space becomes U+0120, which is
/// why every vocabulary in this family is full of words beginning with 'G'.
struct Alphabet {
    to_char: [char; 256],
    to_byte: BTreeMap<char, u8>,
}

impl Alphabet {
    fn new() -> Self {
        let mut to_char = ['\0'; 256];
        let mut next = 0u32;
        for b in 0u8..=255 {
            to_char[b as usize] = if is_kept(b) {
                b as char
            } else {
                let c = char::from_u32(0x100 + next).expect("in range");
                next += 1;
                c
            };
        }
        debug_assert_eq!(next, 68);
        let mut to_byte = BTreeMap::new();
        for (b, &c) in to_char.iter().enumerate() {
            to_byte.insert(c, b as u8);
        }
        Self { to_char, to_byte }
    }

    fn visible(&self, bytes: &[u8]) -> String {
        bytes.iter().map(|&b| self.to_char[b as usize]).collect()
    }

    /// Unknown characters are dropped rather than panicking: a corrupt id
    /// should produce short output, not take the page down.
    fn bytes(&self, text: &str) -> Vec<u8> {
        text.chars().filter_map(|c| self.to_byte.get(&c).copied()).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenizerError {
    /// BPE reached a piece the vocabulary does not contain and there is no
    /// unknown token to stand in for it.
    OutOfVocabulary,
    /// `tokenizer.ggml.pre` named a pattern this build does not scan. Refused
    /// rather than guessed: the wrong pattern is silently wrong output.
    UnknownPattern,
}

impl core::fmt::Display for TokenizerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OutOfVocabulary => write!(f, "BPE produced a piece outside the vocabulary"),
            Self::UnknownPattern => write!(f, "unrecognised tokenizer.ggml.pre; refusing to guess a pre-tokenizer"),
        }
    }
}

pub struct Tokenizer {
    tokens: Vec<String>,
    ids: BTreeMap<String, u32>,
    /// "a b" (the form a GGUF merge list already uses) to its rank.
    merges: BTreeMap<String, u32>,
    /// Literal strings that must never be split, longest first.
    specials: Vec<(String, u32)>,
    alphabet: Alphabet,
    pattern: Pattern,
    unk: Option<u32>,
}

impl Tokenizer {
    /// `merges` are "a b" in priority order. `specials` are ids whose token
    /// text is matched literally before BPE ever sees it -- chat markers like
    /// `<|im_start|>`, which byte-level BPE would otherwise shred into six
    /// tokens the model has never seen together.
    pub fn new(tokens: Vec<String>, merges: Vec<String>, pattern: Pattern,
               specials: Vec<u32>, unk: Option<u32>) -> Self {
        let mut ids = BTreeMap::new();
        for (id, token) in tokens.iter().enumerate() {
            // First wins: a duplicate later in the list is unreachable anyway.
            ids.entry(token.clone()).or_insert(id as u32);
        }
        let mut ranks = BTreeMap::new();
        for (rank, merge) in merges.into_iter().enumerate() {
            ranks.entry(merge).or_insert(rank as u32);
        }
        let mut literal: Vec<(String, u32)> = specials.into_iter()
            .filter_map(|id| tokens.get(id as usize).map(|t| (t.clone(), id)))
            .filter(|(t, _)| !t.is_empty())
            .collect();
        // Longest first, so `<|im_start|>` wins over any prefix of itself.
        literal.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.1.cmp(&b.1)));
        Self { tokens, ids, merges: ranks, specials: literal, alphabet: Alphabet::new(), pattern, unk }
    }

    pub fn vocab_size(&self) -> usize { self.tokens.len() }
    pub fn token(&self, id: u32) -> Option<&str> { self.tokens.get(id as usize).map(|s| s.as_str()) }
    pub fn id_of(&self, piece: &str) -> Option<u32> { self.ids.get(piece).copied() }
    pub fn pattern(&self) -> Pattern { self.pattern }

    /// Text to token ids.
    pub fn encode(&self, text: &str) -> Result<Vec<u32>, TokenizerError> {
        let mut out = Vec::new();
        let mut cursor = 0usize;
        while cursor < text.len() {
            // The earliest special token from here; ties go to the longest,
            // which `specials` being sorted longest-first already gives.
            let mut found: Option<(usize, usize, u32)> = None;
            for (literal, id) in &self.specials {
                if let Some(offset) = text[cursor..].find(literal.as_str()) {
                    let start = cursor + offset;
                    let better = match found {
                        None => true,
                        Some((best, best_end, _)) => start < best || (start == best && start + literal.len() > best_end),
                    };
                    if better { found = Some((start, start + literal.len(), *id)); }
                }
            }
            match found {
                Some((start, end, id)) => {
                    if start > cursor { self.encode_run(&text[cursor..start], &mut out)?; }
                    out.push(id);
                    cursor = end;
                }
                None => { self.encode_run(&text[cursor..], &mut out)?; break; }
            }
        }
        Ok(out)
    }

    /// One run of ordinary text: split it, then merge each piece.
    fn encode_run(&self, text: &str, out: &mut Vec<u32>) -> Result<(), TokenizerError> {
        for piece in pre::split(text, self.pattern) {
            let visible = self.alphabet.visible(piece.as_bytes());
            let mut parts: Vec<String> = visible.chars().map(|c| c.to_string()).collect();
            // Repeatedly apply the lowest-ranked merge that still applies. The
            // rank order is what makes this deterministic; taking the leftmost
            // applicable merge instead would give different, plausible ids.
            loop {
                let mut best: Option<(usize, u32)> = None;
                let mut key = String::new();
                for i in 0..parts.len().saturating_sub(1) {
                    key.clear();
                    key.push_str(&parts[i]);
                    key.push(' ');
                    key.push_str(&parts[i + 1]);
                    if let Some(&rank) = self.merges.get(&key) {
                        if best.is_none_or(|(_, b)| rank < b) { best = Some((i, rank)); }
                    }
                }
                let Some((i, _)) = best else { break };
                let merged = parts[i].clone() + &parts[i + 1];
                parts[i] = merged;
                parts.remove(i + 1);
            }
            for part in &parts {
                match self.ids.get(part).copied().or(self.unk) {
                    Some(id) => out.push(id),
                    None => return Err(TokenizerError::OutOfVocabulary),
                }
            }
        }
        Ok(())
    }

    /// Token ids back to text. Unknown ids contribute nothing.
    pub fn decode(&self, ids: &[u32]) -> String {
        let mut visible = String::new();
        for &id in ids {
            if let Some(token) = self.token(id) { visible.push_str(token); }
        }
        String::from_utf8_lossy(&self.alphabet.bytes(&visible)).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A miniature vocabulary: the byte alphabet plus a few merged pieces, in
    /// merge-priority order.
    fn toy() -> Tokenizer {
        let mut tokens: Vec<String> = Vec::new();
        for b in 0u8..=255 { tokens.push(Alphabet::new().to_char[b as usize].to_string()); }
        tokens.push("Ġw".to_string());      // 256
        tokens.push("or".to_string());      // 257
        tokens.push("Ġworld".to_string());  // 258
        tokens.push("<|end|>".to_string()); // 259
        let merges = alloc::vec!["Ġ w".to_string(), "o r".to_string()];
        Tokenizer::new(tokens, merges, Pattern::Qwen2, alloc::vec![259], None)
    }

    #[test]
    fn a_space_is_visible_and_reversible() {
        let alphabet = Alphabet::new();
        assert_eq!(alphabet.to_char[b' ' as usize], 'Ġ');
        let every: Vec<u8> = (0..=255u8).collect();
        assert_eq!(alphabet.bytes(&alphabet.visible(&every)), every);
    }

    #[test]
    fn merges_apply_in_rank_order_not_left_to_right() {
        let tok = toy();
        // " wor" offers both merges; rank 0 ("Ġ w") must go first.
        let ids = tok.encode(" wor").unwrap();
        assert_eq!(tok.decode(&ids), " wor");
        assert!(ids.contains(&256), "the rank-0 merge was not taken: {ids:?}");
    }

    #[test]
    fn a_special_token_is_never_split() {
        let tok = toy();
        let ids = tok.encode("or<|end|>or").unwrap();
        assert!(ids.contains(&259), "the special token was shredded: {ids:?}");
        assert_eq!(ids.iter().filter(|&&id| id == 259).count(), 1);
        assert_eq!(tok.decode(&ids), "or<|end|>or");
    }

    #[test]
    fn decode_round_trips_arbitrary_text() {
        let tok = toy();
        for text in ["hello world", "Mr.Smith said 12345", "caf\u{e9} \u{1f44b}", " leading", "trailing  "] {
            let ids = tok.encode(text).unwrap();
            assert_eq!(tok.decode(&ids), text, "round trip failed for {text:?}");
        }
    }

    #[test]
    fn an_unknown_piece_without_an_unk_token_is_refused() {
        // A vocabulary with no byte alphabet at all cannot spell anything.
        let tok = Tokenizer::new(alloc::vec!["only".to_string()], Vec::new(), Pattern::Qwen2, Vec::new(), None);
        assert_eq!(tok.encode("x"), Err(TokenizerError::OutOfVocabulary));
    }
}
