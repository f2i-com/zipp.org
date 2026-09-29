// Compare this probe against the previous and candidate regress libraries.
// It covers Unicode string properties, classes, captures, lookbehind and
// optimizer settings, including exact match budgets and compiled residency.
extern crate regress;
use regress::{Flags, MatchLimits, Regex};
use std::io::{self, Write};

fn main() {
    let properties = [
        "Basic_Emoji",
        "Emoji_Keycap_Sequence",
        "RGI_Emoji_Flag_Sequence",
        "RGI_Emoji_Modifier_Sequence",
        "RGI_Emoji_Tag_Sequence",
        "RGI_Emoji_ZWJ_Sequence",
        "RGI_Emoji",
    ];
    let texts = [
        "",
        "",
        "x",
        "😀",
        "1️⃣",
        "🇦🇺",
        "👍🏽",
        "👩‍💻",
        "👨‍👩‍👧",
        "x😀x",
        "😀😀",
        "©",
        "©️",
        "\u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}",
    ];
    let mut out = io::BufWriter::new(io::stdout().lock());
    for (p, property) in properties.iter().enumerate() {
        let atom = format!(r"\p{{{property}}}");
        let patterns = [
            atom.clone(),
            format!("[{atom}]"),
            format!("({atom}|x)"),
            format!("(?<={atom})."),
        ];
        for (shape, pattern) in patterns.iter().enumerate() {
            for flags in ["v", "iv"] {
                for no_opt in [false, true] {
                    // The baseline's no-opt path skips required case folding
                    // and hits its debug assertion for Basic_Emoji /iv.
                    if no_opt && flags == "iv" {
                        continue;
                    }
                    let mut options = Flags::from(flags);
                    options.no_opt = no_opt;
                    let re = Regex::with_flags(pattern, options).unwrap();
                    writeln!(
                        out,
                        "program {p}/{shape}/{flags}/{no_opt}|{}",
                        re.resident_bytes()
                    )
                    .unwrap();
                    for (t, text) in texts.iter().enumerate() {
                        let mut units: Vec<u16> = text.encode_utf16().collect();
                        // Include a lone surrogate without asking Rust strings
                        // to represent invalid Unicode.
                        if t == 0 {
                            units.push(0xd800);
                        }
                        for start in 0..=units.len().min(2) {
                            for max_steps in [0, 1, 2, 3, 8, 32, 128, 512, 4096] {
                                let limits = MatchLimits {
                                    max_steps,
                                    max_backtrack_bytes: 65536,
                                    max_memory_bytes: 65536,
                                };
                                let mut it = re.find_from_utf16_with_limits(&units, start, limits);
                                let matches: Vec<_> =
                                    it.by_ref().map(|m| (m.range(), m.captures)).collect();
                                writeln!(out, "{p}/{shape}/{flags}/{no_opt}/{t}/{start}/{max_steps}|{matches:?}|{:?}", it.match_usage()).unwrap();
                            }
                        }
                    }
                }
            }
        }
    }
}
