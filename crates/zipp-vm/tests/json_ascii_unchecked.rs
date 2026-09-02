//! `JSON.parse`/`JSON.stringify` build `&str` views of bytes the scanner has
//! already proved ASCII without a second `core::str::from_utf8` pass — an
//! escape-free member name whose scan saw no byte >= 0x80, a number token
//! (ASCII by grammar), and a flat heap string whose `JsStr::ascii` flag is set
//! — and a plain integer token of at most 15 digits is read directly instead
//! of through `str::parse::<f64>`.
//!
//! Both shortcuts must be INVISIBLE: the same bytes out of `stringify`, the
//! same values out of `parse`, the same errors. The JS file attacks exactly
//! the places the shortcut and the general path could part ways — non-ASCII
//! and escaped names, lone surrogates, controls, `-0`, the 15/16/17-digit
//! boundary, fractions and exponents, ropes, `toJSON`, revivers, grammar
//! errors — and every expectation is node-oracled (v24.12.0).
//!
//! `ZIPP_NO_JSON_ASCII_UNCHECKED=1` and `ZIPP_NO_JSON_INT_FAST=1` restore the
//! old paths; the child runs below check the same file under each latch and
//! under every execution mode, so a divergence is attributable.

const SRC: &str = include_str!("json_ascii_unchecked.js");

const EXPECTED: &str = concat!(
    "A:{\"alpha\":1,\"beta_2\":\"x-17\",\"g\":[true,false,null],\"\":\"\"}|true\n",
    "B:{\"\u{e9}\":\"caf\u{e9}\",\"\u{4e2d}\u{6587}\":\"\u{1f600}\",\"k\":\"a\u{e9}b\"}|\u{e9},\u{4e2d}\u{6587},k|2\n",
    "C:{\"n\\u0000l\":\"\\u0000\\n\\t\\\"\\\\/\",\"\\u0001\":\"\\u001f\\b\\f\\r\"}|3,1\n",
    "D:{\"s\":\"\\ud800x\\udc00\",\"k\":\"\\udbff\"}|3|55296|\"a\\ud800b\"|{\"q\":\"\\udc00\\ud83d\"}\n",
    "E:[0,0,7,-7,123456789012345,-999999999999999,1000000000000000,9007199254740992,",
    "1.2345678901234568e+22,1.5,-2.5,0.1,100000,0.01,1e+21,1e-7,-1250,0.000001,null,null]",
    "|-Infinity|true|numbernumbernumbernumbernumbernumbernumbernumbernumbernumber",
    "numbernumbernumbernumbernumbernumbernumbernumbernumbernumber\n",
    "F:{\"a\":1,\"b\":\"tj-5\",\"d\":\"1970-01-01T00:00:00.000Z\",\"s\":\"q\"}|{\"a\":10,\"b\":[20,30]}\n",
    "G:510|34.104.101.32.115.97.105.100.32.92.34.104.105.92.34.92.92.32.92.117.48.48.48.55.92.117.",
    "48.48.49.98.32.101.110.100.34|34.120.127.121.8232.122.8233.119.34|true|150\n",
    "H:2,10,b,,-1,01,a|{\"2\":3,\"10\":2,\"b\":1,\"\":4,\"-1\":5,\"01\":6,\"a\":\"\"}\n",
    "I:11943|true|432579531\n",
    "J:SyntaxError,0,SyntaxError,SyntaxError,SyntaxError,SyntaxError,SyntaxError,0,0,100,",
    "SyntaxError,\"\\ud800\",SyntaxError",
);

#[test]
fn json_round_trips_match_node() {
    let out = zipp_vm::run(SRC).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output, vec![EXPECTED.to_string()]);
}

/// The same program under each latch and each execution mode, in a child
/// process (the latches are read once per process).
#[test]
fn json_round_trips_match_in_every_mode_and_with_the_latches_off() {
    if std::env::var_os("ZIPP_JSON_ASCII_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    const LATCHES: [&str; 8] = [
        "ZIPP_NO_JSON_ASCII_UNCHECKED",
        "ZIPP_NO_JSON_INT_FAST",
        "ZIPP_NO_JSON_PLAIN_KEY",
        "ZIPP_NO_JSON_QUOTE_BULK",
        "ZIPP_NO_JSON_LEAF_FAST",
        "ZIPP_NOJIT",
        "ZIPP_JIT_THRESHOLD",
        "ZIPP_NO_NURSERY",
    ];
    for (mode, env) in [
        ("default", None),
        ("no-ascii-unchecked", Some(("ZIPP_NO_JSON_ASCII_UNCHECKED", "1"))),
        ("no-int-fast", Some(("ZIPP_NO_JSON_INT_FAST", "1"))),
        ("no-plain-key", Some(("ZIPP_NO_JSON_PLAIN_KEY", "1"))),
        ("no-quote-bulk", Some(("ZIPP_NO_JSON_QUOTE_BULK", "1"))),
        ("no-leaf-fast", Some(("ZIPP_NO_JSON_LEAF_FAST", "1"))),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("no-nursery", Some(("ZIPP_NO_NURSERY", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "json_round_trips_match_node", "--nocapture"])
            .env("ZIPP_JSON_ASCII_CHILD", "1");
        for l in LATCHES {
            cmd.env_remove(l);
        }
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
