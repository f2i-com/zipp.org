//! B233: `JSON.parse` reads an escape-free member name straight out of the
//! source bytes instead of routing it through the general string parser (a
//! `Vec<u8>`, then a `JsStr`, then `to_lossy_string`'s `String` — three
//! allocations where one will do, on the hottest allocation site JSON parsing
//! has).
//!
//! The shortcut is only legal if a name read the short way is INDISTINGUISHABLE
//! from one read the long way, so the cases below attack that: names that must
//! refuse the shortcut (an escape of each kind), names that must not (plain
//! ASCII, multi-byte UTF-8, the empty name), and the object-model consequences
//! a subtly different key would show up in — duplicate replacement, `__proto__`
//! as an OWN property, canonical-index enumeration order, descriptors,
//! mutation after the build, `for-in`, spread, freeze, a reviver, and a hot
//! polymorphic read site.
//!
//! Every expectation is node-oracled (v24.12.0), and the same file is checked
//! against the same string with the wave latched off, so a divergence is
//! attributable rather than merely present.
//!
//! Not pinned here: a name holding a LONE SURROGATE. zipp folds it to U+FFFD
//! with this wave on or off — object keys are Rust `String`s and cannot hold
//! one — so it is a pre-existing representational gap, recorded in the roadmap,
//! and would only make this test assert a known-wrong value.

const SRC: &str = include_str!("json_plain_key.js");

const EXPECTED: &str = concat!(
    "A:xy12|xy34|xy56\n",
    "B:k=2|k=2\n",
    "C:pqr:{\"p\":1,\"q\":2,\"r\":3}|p:{\"p\":9}|pqr:{\"p\":1,\"q\":2,\"r\":3}\n",
    "D:1,2,b|1,2,b\n",
    "E:abc=1|abc=2|ab/c=3|ab/c=4\n",
    "F:__proto__,n/true/true|__proto__,n/true/true\n",
    "F2:{\"value\":{\"z\":9},\"writable\":true,\"enumerable\":true,\"configurable\":true}",
    "|{\"value\":{\"z\":8},\"writable\":true,\"enumerable\":true,\"configurable\":true}\n",
    "G:truetruetrue|truetruetrue\n",
    "H:[{\"m\":1,\"n\":0},{\"n\":4,\"extra\":7,\"m\":99}]\n",
    "I:f00,f11,f22,f33,f44,f55,f66,f77,f88,f99,f1010,f1111,f0100\n",
    "J:2{\"\u{e9}\":1,\"\":2}|2{\"\u{e9}\":3,\"\":4}\n",
    "K:{\"a\":{\"a\":{\"a\":{\"v\":1},\"v\":2},\"v\":3},\"v\":4}/1234\n",
    "L:01010[{},{\"t\":1},{},{\"t\":2},{}]\n",
    "M:[{\"r\":10,\"s\":20},{\"r\":30,\"s\":40}]\n",
    "N:34996\n",
    "O:[{\"b\":1,\"a\":2},{\"b\":3,\"a\":4}]\n",
    "P:1/true/false\n",
    "Q:cd{\"c\":1,\"d\":2}\n",
    "R:1:65,2:34,2:92,2:9=1234|1:65,2:34,2:92,2:9=5678\n",
    "S:3/1|3/2/true",
);

#[test]
fn plain_key_reads_agree_with_the_general_string_parser() {
    let out = zipp_vm::run(SRC).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output, vec![EXPECTED.to_string()]);
}
