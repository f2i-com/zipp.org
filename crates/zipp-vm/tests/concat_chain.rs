//! W11 (B124): n-ary string-concat chain fusion — `L1+L2+…+Ln` (n≥3, ≥1
//! syntactic string leaf) compiles to `acc = Add(L1,L2)` plus per-leaf
//! `StrConcatChain{acc,acc,leaf}` (in-place growth of the fresh accumulator)
//! instead of n-1 pairwise `Add` allocations. Semantics must equal `+`
//! exactly — leaf evaluation stays interleaved with the prior links'
//! coercions (E1 E2 C1 C2 | E3 C3 | …), numeric/BigInt prefixes fold with
//! real `+` rules, and every TypeError/throw lands at its exact step — so
//! each case asserts byte-identical output against `node -e`, and the final
//! test re-runs the set under `ZIPP_NO_CONCAT_FUSE=1` (the off-switch:
//! pairwise emission, bit-identical to pre-W11 bytecode), `ZIPP_NOJIT=1`,
//! `ZIPP_JIT_THRESHOLD=1` and `ZIPP_GC_STRESS=1` in child processes; a
//! `ZIPP_JITLOG` census asserts the gen-shaped region still compiles MEM
//! with the chain op admitted (the load-bearing admission-list risk).

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node v24 on PATH (expected values come from `node -e`)");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(|l| l.to_string())
        .collect()
}

fn assert_matches_node(src: &str) {
    let ours = run_ok(src);
    let node = node_output(src);
    assert_eq!(ours, node, "zipp != node for: {src}");
}

/// Whether a step CONCATENATES or ADDS is decided per pairwise step: a
/// numeric prefix folds with real `+` and only becomes a string builder at
/// the first string-producing link.
#[test]
fn ccf_parity_numeric_prefixes() {
    assert_matches_node(
        r#"
        "use strict";
        console.log(1 + 2 + 'x');
        console.log('x' + 1 + 2);
        console.log(1 + 2 + 3 + 'x' + 4 + 5);
        console.log(0.1 + 0.2 + 'f');
        console.log(1 + 2 + `t` + 3);
        console.log(true + true + 'b' + false);
        console.log(null + 1 + 'n' + undefined);
        console.log(-0 + '' + 0 + 'z');
        console.log(1/0 + 'i' + (-1/0) + (0/0));
        console.log(2147483647 + 'v' + (-2147483648));
        "#,
    );
}

/// valueOf/toString hooks mid-chain: coercion runs per pairwise step, so hook
/// side effects INTERLEAVE with later leaves' call effects in the spec's
/// order (E1 E2 C1 C2 | E3 C3 | …) — the reason an "evaluate all leaves
/// first" window op would be unsound.
#[test]
fn ccf_parity_valueof_tostring_order() {
    assert_matches_node(
        r#"
        "use strict";
        var log = [];
        var vo = { valueOf: function () { log.push('V'); return 7; } };
        var ts = { toString: function () { log.push('T'); return 't'; } };
        function f() { log.push('F'); return 'f'; }
        console.log(vo + 'a' + f() + vo + 'b' + ts, log.join(''));
        log = [];
        console.log(f() + vo + ts + 'x', log.join(''));
        log = [];
        console.log('p' + ts + f() + vo, log.join(''));
        "#,
    );
}

/// A throw mid-chain: earlier leaves' effects ran exactly once, later leaves
/// never evaluate, and the half-built accumulator is unobservable.
#[test]
fn ccf_parity_throw_mid_chain() {
    assert_matches_node(
        r#"
        "use strict";
        var ran = [];
        function g(t) { ran.push(t); return t; }
        function boom() { ran.push('X'); throw new Error('mid'); }
        try { void ('p' + g('a') + boom() + g('never')); }
        catch (e) { console.log('caught', e.message, ran.join('')); }
        ran = [];
        var thrower = { valueOf: function () { ran.push('V'); throw new Error('coerce'); } };
        try { void (g('a') + g('b') + thrower + g('never')); }
        catch (e) { console.log('caught2', e.message, ran.join('')); }
        "#,
    );
}

/// A Symbol leaf throws the string-coercion TypeError at its exact step —
/// leaves before it evaluated, leaves after it did not.
#[test]
fn ccf_parity_symbol_typeerror() {
    assert_matches_node(
        r#"
        "use strict";
        var pos = [];
        function h(t) { pos.push(t); return t; }
        try { void ('a' + h('b') + Symbol('s') + h('never')); }
        catch (e) { console.log('sym', e instanceof TypeError, pos.join('')); }
        pos = [];
        try { void (Symbol('s') + 'a' + h('never')); }
        catch (e) { console.log('sym2', e instanceof TypeError, pos.join('')); }
        "#,
    );
}

/// BigInt chains: a bigint+bigint prefix folds as BigInt; BigInt+Number mixes
/// throw the spec's TypeError at their exact step; BigInt+string stringifies
/// (decimal, no trailing "n").
#[test]
fn ccf_parity_bigint_chains() {
    assert_matches_node(
        r#"
        "use strict";
        console.log(1n + 2n + 'x');
        console.log('x' + 1n + 2n);
        console.log(1n + 2n + 3n + 'z' + 4n);
        var ran = [];
        function w(t) { ran.push(t); return t; }
        try { void (1n + 2 + 'x' + w('never')); }
        catch (e) { console.log('mix1', e instanceof TypeError, ran.join('')); }
        ran = [];
        try { void (w('a') + 1 + 2n + 'x'); }
        catch (e) { console.log('mix2', e instanceof TypeError, ran.join('')); }
        console.log('big' + 123456789012345678901234567890n + 'end');
        "#,
    );
}

/// Template literals fuse through the same chain (q0 is always a string
/// leaf); `${}` substitutions ToString FIRST (string hint), so a valueOf-only
/// hook is bypassed exactly as unfused; a `${}` reading the DESTINATION var
/// mid-build sees its prior value.
#[test]
fn ccf_parity_template_literals() {
    assert_matches_node(
        r#"
        "use strict";
        var name = 'w';
        console.log(`a${name}b${1 + 2}c`);
        console.log(`${name}${name}${name}`);
        console.log(`x${1}${''}${2}y`);
        var hooked = {
            toString: function () { return 'TS'; },
            valueOf: function () { return 'VO'; }
        };
        console.log(`t:${hooked}!`);
        var acc = 'Z';
        acc = `${acc}|${acc}|${acc}`;
        console.log(acc);
        var n = 5;
        console.log(`#${n} [${n * 2}:${n * 3}] end`);
        "#,
    );
}

/// Chains over ropes: a huge leaf entering the chain must keep O(1) rope
/// links (never a per-statement flatten), and a loop-carried `x = x + a + b`
/// accumulator stays O(n) overall.
#[test]
fn ccf_parity_chains_over_ropes() {
    assert_matches_node(
        r#"
        "use strict";
        var huge = 'h'.repeat(5000);
        var s = 'a' + huge + 'b' + 'c';
        console.log(s.length, s[0], s[1], s[s.length - 2], s[s.length - 1]);
        var t = huge + '|' + huge + '|' + 'tail';
        console.log(t.length, t.charCodeAt(5000), t.slice(-4));
        var x = '';
        for (var i = 0; i < 2000; i++) x = x + 'a' + i;
        console.log(x.length, x.slice(0, 8), x.slice(-6));
        var y = 'seed';
        y = y + 'a' + y;
        console.log(y);
        "#,
    );
}

/// A 40-leaf chain: per-link temp rollback keeps the register footprint flat
/// (Tier C's 64-reg cap must not be forfeited by long chains).
// Tier-gated: the safe-sandbox config (instrument, no JIT) caps source
// nesting below this case's chain length / cannot satisfy the JIT census.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn ccf_parity_forty_leaf_chain() {
    assert_matches_node(
        r#"
        "use strict";
        var v = 1;
        var big = 'a' + v + 'b' + v + 'c' + v + 'd' + v + 'e' + v +
                  'f' + v + 'g' + v + 'h' + v + 'i' + v + 'j' + v +
                  'k' + v + 'l' + v + 'm' + v + 'n' + v + 'o' + v +
                  'p' + v + 'q' + v + 'r' + v + 's' + v + 't' + v;
        console.log(big, big.length);
        "#,
    );
}

/// Interned single-char and empty-string leaves: the first link's fresh-alloc
/// discipline means an interned constant is never grown in place, and empty
/// leaves neither break the chain nor change the result.
#[test]
fn ccf_parity_interned_and_empty_leaves() {
    assert_matches_node(
        r#"
        "use strict";
        console.log('' + 'a' + '' + 'b' + '');
        console.log('x' + '' + 'y');
        console.log('' + '' + '' + 'q');
        console.log('a' + 'b' + 'c');
        var one = 'a';
        console.log(one + one + one + one, one);
        for (var i = 0; i < 5; i++) console.log('k' + '' + i + '');
        "#,
    );
}

/// Lone-surrogate WTF-8 seams: a high surrogate leaf followed by a low
/// surrogate leaf must canonicalize into ONE astral code point across the
/// in-place append seam, exactly as pairwise `+` does. (Asserted via code
/// units, not raw stdout, so terminal transcoding can't blur the diff.)
#[test]
fn ccf_parity_lone_surrogate_seam() {
    assert_matches_node(
        r#"
        "use strict";
        var hi = '\uD83D', lo = '\uDE00';
        var s = 'a' + hi + lo + 'b';
        var units = [];
        for (var i = 0; i < s.length; i++) units.push(s.charCodeAt(i));
        console.log(s.length, units.join(','));
        var t = 'x' + hi + 'y' + lo + 'z';
        units = [];
        for (i = 0; i < t.length; i++) units.push(t.charCodeAt(i));
        console.log(t.length, units.join(','));
        "#,
    );
}

/// The regex-log-scan gen shape run HOT, so the default mode executes the
/// fused links inside a compiled MEM region and Tier-C bodies (pad2/ri):
/// int-heavy leaves through call leaves that advance shared state — the
/// interleaving-sensitive shape the fusion must not reorder.
#[test]
fn ccf_parity_hot_gen_loop() {
    assert_matches_node(
        r#"
        "use strict";
        function pad2(n) { return n < 10 ? '0' + n : '' + n; }
        var seed = 12345;
        function rnd() { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed / 0x7fffffff; }
        function ri(n) { return (rnd() * n) | 0; }
        var lens = 0, sample = '';
        for (var i = 0; i < 30000; i++) {
            var line = '#' + ri(9000) + ' [' + pad2(ri(24)) + ':' + pad2(ri(60)) + ']' +
                       ' status=' + ri(600) + ' bytes=' + ri(100000) + ' ms=' + ri(2000);
            lens += line.length;
            if (i === 29999) sample = line;
        }
        console.log(lens, sample);
        "#,
    );
}

/// Chain result feeding heap ops in the same hot loop (`obj[key] = line`,
/// re-reads) — exercises the fused links alongside the barrier/IC machinery.
#[test]
fn ccf_parity_hot_chain_into_heap() {
    assert_matches_node(
        r#"
        "use strict";
        var lines = [];
        for (var i = 0; i < 20000; i++) {
            lines[i & 63] = 'r' + (i % 977) + ':' + (i % 31) + ';';
        }
        var h = 0;
        for (var j = 0; j < 64; j++) {
            var s = lines[j];
            for (var k = 0; k < s.length; k++) h = (h * 31 + s.charCodeAt(k)) | 0;
        }
        console.log(h);
        "#,
    );
}

/// B253's exact hostile shape: B212 freezes `"item " + id`, a pinned colon
/// bridges that stable head, then the varying revision is the final int memo.
/// Held generations and effectful final coercions make accidental mutation or
/// skipped/reordered semantics observable while the loop drives memo hits.
#[test]
fn ccf_parity_frozen_head_ascii_suffix() {
    assert_matches_node(
        r#"
        "use strict";
        function item(id, revision, separator) {
            return "item " + id + separator + revision;
        }
        var held = [], hash = 0;
        for (var pass = 0; pass < 4200; pass++) {
            var revision = (pass / 8) | 0;
            for (var id = 0; id < 28; id++) {
                var text = item(id, revision, ':');
                if ((pass & 1023) === 0 && id < 2) held.push(text);
                hash = (Math.imul(hash, 33) + text.length +
                        text.charCodeAt(5) + text.charCodeAt(text.length - 1)) | 0;
            }
        }
        console.log(hash, held.length, held.join('|'));
        console.log(item(-2147483648, 0, ':'), item(2147483647, -1, '/'));
        var log = [];
        var effect = { valueOf: function () { log.push('V'); return 9; } };
        console.log(item(7, effect, ':'), log.join(''));
        var old = item(7, 3, ':'), next = item(7, 4, ':');
        console.log(old, next, old === 'item 7:3');
        "#,
    );
}

/// Self-aliasing accumulators (`a += a` shapes, chains whose leaves are the
/// destination var) and exotic primitive leaves (double/bool/null/undefined)
/// on a live builder: the single-dispatch fast link must route every one of
/// these through the generic pairwise `+` (fresh index -- never the raw
/// split-borrow arms) and still match node byte for byte, hot.
#[test]
fn ccf_parity_self_alias_and_exotic_leaves() {
    assert_matches_node(
        r#"
        "use strict";
        var a = 'ab';
        for (var i = 0; i < 4000; i++) { a += a; if (a.length > 4096) a = 'ab' + (i % 10); }
        console.log(a.length, a.slice(0, 8), a.slice(-8));
        var b = 'x';
        b = b + b + b;
        b = b + (b + b) + b + '' + b;
        console.log(b);
        for (var j = 0; j < 3000; j++) {
            var line = 'd=' + 1.5 + ' b=' + true + ' n=' + null + ' u=' + undefined + ' f=' + false + '#' + j;
            if (j === 2999) console.log(line);
        }
        console.log('q=' + 1e21 + ' s=' + 0.1 + ' neg=' + (-2.5) + ' nz=' + (-0) + ' end');
        "#,
    );
}

/// An object leaf with an effectful `toString` MID-chain, after earlier links
/// already committed into the builder in place: the coercion must run at its
/// exact pairwise step -- after link i's commit, before leaf i+1's evaluation
/// (per-link dispatch, never batched across links) -- and a throw at that
/// step must leave exactly the earlier side effects done.
#[test]
fn ccf_parity_effectful_leaf_after_committed_links() {
    assert_matches_node(
        r#"
        "use strict";
        var log = [];
        function e(tag, v) { log.push(tag); return v; }
        var obj = { n: 0, toString: function () { this.n++; log.push('T' + this.n); return '<' + this.n + '>'; } };
        for (var i = 0; i < 1500; i++) {
            var s = 'a' + e('L1', i) + 'b' + obj + 'c' + e('L2', i) + obj + 'z';
            if (i === 1499) console.log(s);
        }
        console.log(log.length, log.slice(0, 8).join(','), log.slice(-8).join(','));
        var thrown = '';
        try {
            var t = 'x' + e('P', 1) + { toString: function () { throw new Error('boom'); } } + e('NEVER', 2);
        } catch (err) { thrown = err.message; }
        console.log(thrown, log[log.length - 1]);
        "#,
    );
}

/// Non-ASCII leaves (Latin-1, BMP, astral) through a HOT chain: the in-place
/// str arm's byte append must keep WTF-8 content and UTF-16 unit accounting
/// exact (lengths, charCodeAt) -- asserted via code units so terminal
/// transcoding can't blur the diff.
#[test]
fn ccf_parity_nonascii_hot_chain() {
    assert_matches_node(
        r#"
        "use strict";
        var em = '\u00e9', snow = '\u2603', astral = '\uD83D\uDE00';
        var total = 0, last = '';
        for (var i = 0; i < 20000; i++) {
            var s = em + i + snow + (i % 7) + astral + 'k' + em + snow;
            total += s.length;
            if (i === 19999) last = s;
        }
        var units = [];
        for (var k = 0; k < last.length; k++) units.push(last.charCodeAt(k));
        console.log(total, last.length, units.join(','));
        "#,
    );
}

/// A chain whose builder CROSSES the 256-unit small-flat threshold mid-run
/// (in-place growth continues regardless), and one whose link-1 `Add` already
/// exceeds it -- a rope accumulator, so EVERY link must fall through to the
/// generic path (O(1) rope links preserved), hot in both directions.
// Tier-gated: the safe-sandbox config (instrument, no JIT) caps source
// nesting below this case's chain length / cannot satisfy the JIT census.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn ccf_parity_flat_threshold_crossing() {
    assert_matches_node(
        r#"
        "use strict";
        var piece40 = '0123456789012345678901234567890123456789';
        var last = '';
        for (var i = 0; i < 3000; i++) {
            var s = '<' + i + piece40 + i + piece40 + i + piece40 + i + piece40 +
                    i + piece40 + i + piece40 + i + piece40 + i + piece40 + '>';
            if (i === 2999) last = s;
        }
        console.log(last.length, last.slice(0, 50), last.slice(-50));
        var big = piece40 + piece40 + piece40 + piece40 + piece40 + piece40 + piece40 + piece40;
        for (var j = 0; j < 3000; j++) {
            var t = big + j + 'x' + j + big + '!';
            if (j === 2999) last = t;
        }
        console.log(last.length, last.slice(0, 30), last.slice(-30));
        "#,
    );
}

/// The load-bearing admission risk, asserted directly: the gen-shaped hot
/// loop's region must still compile on the MEM tier with the fused chain ops
/// admitted, and no tier may decline/reject `StrConcatChain` anywhere.
/// (`region_admit.rs` omission would DECLINE the gen loop region;
/// `proto_mem.rs` omission would un-compile Tier-C builder bodies — both are
/// silent perf cliffs no output-parity test catches.)
// Tier-gated: the safe-sandbox config (instrument, no JIT) caps source
// nesting below this case's chain length / cannot satisfy the JIT census.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn jitlog_census_gen_region_compiles_mem() {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(&exe)
        .arg("ccf_parity_hot_gen_loop")
        .arg("--exact")
        .arg("--nocapture")
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JITDUMP", "1")
        .output()
        .expect("spawn the test binary");
    assert!(
        out.status.success(),
        "census child failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("MEM region"),
        "the gen-shaped loop compiled no MEM region — the fused chain \
         demoted it to the interpreter:\n{stderr}"
    );
    // `[dump]` bytecode listings legitimately name the op — only the decline
    // markers signal an admission regression.
    assert!(
        !stderr.contains("[decline] StrConcatChain") && !stderr.contains("op StrConcatChain"),
        "a tier declined/rejected the chain op (region_admit/proto_mem \
         admission regression):\n{stderr}"
    );
}

/// Re-run every `ccf_parity_` case with the fusion off, the JIT off, the JIT
/// forced hot, and a collection at every safe point, each in a child process.
// Tier-gated: the safe-sandbox config (instrument, no JIT) caps source
// nesting below this case's chain length / cannot satisfy the JIT census.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_CONCAT_FUSE", "1"),
        ("ZIPP_NO_CHAIN_FAST", "1"),
        ("ZIPP_NO_CONCAT_SUFFIX_MEMO", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("ccf_parity_")
            .env(key, val)
            .output()
            .expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{key}={val} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the ccf_parity_ filter matched nothing under {key}={val}:\n{stdout}"
        );
    }
}
