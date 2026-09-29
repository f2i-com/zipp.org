// Differential probe: compile against regress with and without compact-utf16.
// Compare stdout byte-for-byte; see docs/validation/2026-09-29-engine-size-round4.md.
extern crate regress;
use regress::{Flags, MatchLimits, Regex};
fn main() {
    let patterns = [
        "a*",
        "a+",
        "a{0,3}",
        "a{2,4}",
        "a{2,4}?b",
        "a*?b",
        ".*Z",
        "[^b]+",
        "[a-z]*?b",
        "(a+)(b*)\\1",
        "(?<=a+)b",
        "(?=a*)a*",
        "(?:a|aa)+b",
        "(α+)(β*)\\1",
        "[αβ]*γ",
        "(😀+)β\\1",
        "(?<=😀*)β",
        "[^]*Z",
    ];
    let texts = [
        "",
        "a",
        "aaa",
        "aaabaaa",
        "baaaa!",
        "aaaaab",
        "ααβαα",
        "αβγ",
        "😀😀β😀😀",
        "\u{d7ff}x",
    ];
    for (p, pattern) in patterns.iter().enumerate() {
        for (f, flags) in ["", "u", "i", "iu"].iter().enumerate() {
            for no_opt in [false, true] {
                let mut options = Flags::from(*flags);
                options.no_opt = no_opt;
                let re = Regex::with_flags(pattern, options).unwrap();
                for (t, text) in texts.iter().enumerate() {
                    let units: Vec<u16> = text.encode_utf16().collect();
                    for max_steps in 0..80 {
                        let limits = MatchLimits {
                            max_steps,
                            max_backtrack_bytes: 4096,
                            max_memory_bytes: 4096,
                        };
                        for start in 0..=units.len().min(2) {
                            for unicode in [false, true] {
                                macro_rules! collect_matches {
                                    ($matches:expr) => {{
                                        let mut it = $matches;
                                        let found: Vec<_> =
                                            it.by_ref().map(|m| (m.range(), m.captures)).collect();
                                        (found, it.match_usage())
                                    }};
                                }
                                let (found, usage) = if unicode {
                                    collect_matches!(
                                        re.find_from_utf16_with_limits(&units, start, limits)
                                    )
                                } else {
                                    collect_matches!(
                                        re.find_from_ucs2_with_limits(&units, start, limits)
                                    )
                                };
                                println!(
                                    "{p}/{f}/{no_opt}/{t}/{max_steps}/{start}/{unicode}|{found:?}|{usage:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
