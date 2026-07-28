//! The **language-sensitive** case mappings of Unicode's `SpecialCasing.txt`.
//!
//! `String.prototype.toLocale{Lower,Upper}Case` differ from their
//! locale-independent siblings in exactly one way: for three languages the UCD
//! defines conditional mappings that override the default ones. ECMA-402
//! §String.prototype.toLocaleLowerCase step 6 defines `availableLocales` here as
//! "the languages for which the Unicode Character Database contains language
//! sensitive case mappings" — that is the closed set `az`, `lt`, `tr`, and this
//! module is the whole of it.
//!
//! Source: `SpecialCasing.txt` §"Language-Sensitive Mappings" (Unicode 16.0.0,
//! <https://www.unicode.org/Public/16.0.0/ucd/SpecialCasing.txt>), transcribed
//! row for row:
//!
//! ```text
//! # Lithuanian
//! 0307; 0307;      ;      ; lt After_Soft_Dotted;   # COMBINING DOT ABOVE
//! 0049; 0069 0307; 0049;  0049; lt More_Above;      # LATIN CAPITAL LETTER I
//! 004A; 006A 0307; 004A;  004A; lt More_Above;      # LATIN CAPITAL LETTER J
//! 012E; 012F 0307; 012E;  012E; lt More_Above;      # LATIN CAPITAL LETTER I WITH OGONEK
//! 00CC; 0069 0307 0300; 00CC; 00CC; lt;             # LATIN CAPITAL LETTER I WITH GRAVE
//! 00CD; 0069 0307 0301; 00CD; 00CD; lt;             # LATIN CAPITAL LETTER I WITH ACUTE
//! 0128; 0069 0307 0303; 0128; 0128; lt;             # LATIN CAPITAL LETTER I WITH TILDE
//! # Turkish and Azeri
//! 0130; 0069; 0130; 0130; tr;                       # LATIN CAPITAL LETTER I WITH DOT ABOVE
//! 0130; 0069; 0130; 0130; az;                       # (same)
//! 0307;     ; 0307; 0307; tr After_I;               # COMBINING DOT ABOVE
//! 0307;     ; 0307; 0307; az After_I;               # (same)
//! 0049; 0131; 0049; 0049; tr Not_Before_Dot;        # LATIN CAPITAL LETTER I
//! 0049; 0131; 0049; 0049; az Not_Before_Dot;        # (same)
//! 0069; 0069; 0130; 0130; tr;                       # LATIN SMALL LETTER I
//! 0069; 0069; 0130; 0130; az;                       # (same)
//! ```
//!
//! The four conditions are the definitions from the same file's header, and all
//! of them are stated in terms of the Canonical_Combining_Class of the
//! surrounding characters, which `unicode_normalization` already provides — no
//! new table is needed for them. `After_Soft_Dotted` additionally needs the
//! `Soft_Dotted` property, which is [`SOFT_DOTTED`] below.

use unicode_normalization::char::canonical_combining_class as ccc;

/// The `Soft_Dotted` code points, from `PropList.txt` (Unicode 16.0.0):
/// 34 ranges, 50 code points. Only `After_Soft_Dotted` (Lithuanian uppercasing)
/// reads this.
const SOFT_DOTTED: [(u32, u32); 34] = [
    (0x0069, 0x006A),
    (0x012F, 0x012F),
    (0x0249, 0x0249),
    (0x0268, 0x0268),
    (0x029D, 0x029D),
    (0x02B2, 0x02B2),
    (0x03F3, 0x03F3),
    (0x0456, 0x0456),
    (0x0458, 0x0458),
    (0x1D62, 0x1D62),
    (0x1D96, 0x1D96),
    (0x1DA4, 0x1DA4),
    (0x1DA8, 0x1DA8),
    (0x1E2D, 0x1E2D),
    (0x1ECB, 0x1ECB),
    (0x2071, 0x2071),
    (0x2148, 0x2149),
    (0x2C7C, 0x2C7C),
    (0x1D422, 0x1D423),
    (0x1D456, 0x1D457),
    (0x1D48A, 0x1D48B),
    (0x1D4BE, 0x1D4BF),
    (0x1D4F2, 0x1D4F3),
    (0x1D526, 0x1D527),
    (0x1D55A, 0x1D55B),
    (0x1D58E, 0x1D58F),
    (0x1D5C2, 0x1D5C3),
    (0x1D5F6, 0x1D5F7),
    (0x1D62A, 0x1D62B),
    (0x1D65E, 0x1D65F),
    (0x1D692, 0x1D693),
    (0x1DF1A, 0x1DF1A),
    (0x1E04C, 0x1E04D),
    (0x1E068, 0x1E068),
];

fn is_soft_dotted(c: char) -> bool {
    let u = c as u32;
    SOFT_DOTTED.iter().any(|&(a, b)| u >= a && u <= b)
}

/// BestAvailableLocale over the UCD's language-sensitive set. `None` means the
/// default (locale-independent) mapping applies — ECMA-402's "und".
pub(crate) fn special_casing_language(tag: &str) -> Option<&'static str> {
    // The tag is already canonical, so the language subtag is the prefix up to
    // the first "-" and is lowercase.
    let lang = tag.split('-').next().unwrap_or("");
    match lang {
        "tr" => Some("tr"),
        "az" => Some("az"),
        "lt" => Some("lt"),
        _ => None,
    }
}

/// `After_Soft_Dotted`/`After_I`: is there a `pred`-matching character before
/// position `i`, with no intervening character of combining class 0 or 230?
fn after(chars: &[char], i: usize, pred: impl Fn(char) -> bool) -> bool {
    for &c in chars[..i].iter().rev() {
        if pred(c) {
            return true;
        }
        let k = ccc(c);
        if k == 0 || k == 230 {
            return false;
        }
    }
    false
}

/// `More_Above`: is `chars[i]` followed by a combining class 230 character with
/// no intervening character of combining class 0 or 230?
fn more_above(chars: &[char], i: usize) -> bool {
    for &c in &chars[i + 1..] {
        let k = ccc(c);
        if k == 230 {
            return true;
        }
        if k == 0 {
            return false;
        }
    }
    false
}

/// `Before_Dot`: is `chars[i]` followed by U+0307, with only characters of
/// combining class other than 0 and 230 in between?
fn before_dot(chars: &[char], i: usize) -> bool {
    for &c in &chars[i + 1..] {
        if c == '\u{307}' {
            return true;
        }
        let k = ccc(c);
        if k == 0 || k == 230 {
            return false;
        }
    }
    false
}

/// The language-sensitive lower/upper mapping of `s`, or `None` when `lang`
/// has none (so the caller keeps its default `to_lowercase`/`to_uppercase`).
///
/// Every character the conditional table does NOT name falls through to the
/// default mapping, so this is the full result, not a patch on top of one.
pub(crate) fn transform_case(s: &str, lang: &str, upper: bool) -> Option<String> {
    if !matches!(lang, "tr" | "az" | "lt") {
        return None;
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let turkic = lang == "tr" || lang == "az";
    for (i, &c) in chars.iter().enumerate() {
        let handled = if upper {
            match c {
                // tr/az: i uppercases to İ rather than I.
                'i' if turkic => {
                    out.push('\u{130}');
                    true
                }
                // lt: the explicit dot a lowercase i carries is dropped again
                // when the letter it sits on becomes a capital.
                '\u{307}' if lang == "lt" && after(&chars, i, is_soft_dotted) => true,
                _ => false,
            }
        } else {
            match c {
                // tr/az: İ lowercases to plain i (its dot is inherent), and a
                // bare I loses its dot entirely unless one follows explicitly.
                '\u{130}' if turkic => {
                    out.push('i');
                    true
                }
                '\u{307}' if turkic && after(&chars, i, |p| p == 'I') => true,
                'I' if turkic && !before_dot(&chars, i) => {
                    out.push('\u{131}');
                    true
                }
                // lt: a capital I/J/Į that carries further accents above keeps
                // an explicit dot when it becomes lowercase, so the accents do
                // not sit where the dot belongs.
                'I' | 'J' | '\u{12E}' if lang == "lt" && more_above(&chars, i) => {
                    out.push(match c {
                        'I' => 'i',
                        'J' => 'j',
                        _ => '\u{12F}',
                    });
                    out.push('\u{307}');
                    true
                }
                // The three precomposed Lithuanian capitals decompose the same
                // way unconditionally.
                '\u{CC}' | '\u{CD}' | '\u{128}' if lang == "lt" => {
                    out.push('i');
                    out.push('\u{307}');
                    out.push(match c {
                        '\u{CC}' => '\u{300}',
                        '\u{CD}' => '\u{301}',
                        _ => '\u{303}',
                    });
                    true
                }
                _ => false,
            }
        };
        if !handled {
            // Not named by the conditional table: the default full mapping.
            if upper {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turkish_dotted_and_dotless_i() {
        assert_eq!(transform_case("\u{130}", "tr", false).unwrap(), "i");
        assert_eq!(transform_case("I\u{307}", "tr", false).unwrap(), "i");
        // A class-220 mark may intervene between the I and its dot (After_I).
        assert_eq!(transform_case("I\u{323}\u{307}", "tr", false).unwrap(), "i\u{323}");
        // A class-230 mark breaks both After_I and Not_Before_Dot.
        assert_eq!(
            transform_case("I\u{300}\u{307}", "tr", false).unwrap(),
            "\u{131}\u{300}\u{307}"
        );
        assert_eq!(transform_case("I", "tr", false).unwrap(), "\u{131}");
        assert_eq!(transform_case("i", "tr", true).unwrap(), "\u{130}");
        assert_eq!(transform_case("\u{131}", "tr", true).unwrap(), "I");
    }

    #[test]
    fn lithuanian_explicit_dot() {
        assert_eq!(transform_case("I\u{300}", "lt", false).unwrap(), "i\u{307}\u{300}");
        assert_eq!(transform_case("\u{12E}\u{300}", "lt", false).unwrap(), "\u{12F}\u{307}\u{300}");
        // No accent above: the ordinary mapping, no explicit dot.
        assert_eq!(transform_case("I", "lt", false).unwrap(), "i");
        // Uppercasing strips the dot the lowercase letter carried.
        assert_eq!(transform_case("i\u{307}", "lt", true).unwrap(), "I");
        assert_eq!(transform_case("i\u{323}\u{307}", "lt", true).unwrap(), "I\u{323}");
        // Not after a Soft_Dotted letter: the dot stays.
        assert_eq!(transform_case("I\u{307}", "lt", true).unwrap(), "I\u{307}");
    }

    #[test]
    fn other_languages_are_untouched() {
        assert!(transform_case("I", "en", false).is_none());
        assert_eq!(special_casing_language("tr-TR"), Some("tr"));
        assert_eq!(special_casing_language("en-US"), None);
    }
}
