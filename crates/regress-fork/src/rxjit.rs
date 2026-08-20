//! PATCH (see VENDORED.md): native x86-64 compilation of the classical
//! backtracker's bytecode.
//!
//! This is an execution tier, not a second regex engine: it compiles the
//! EXISTING `CompiledRegex` instruction stream (insn.rs) to machine code that
//! reproduces `classicalbacktrack.rs`'s `try_at_pos` exactly — the same
//! backtrack stack entries, the same capture-group save/restore discipline,
//! the same one-char-loop min/max scan, and the same possessive/skip-hint
//! behavior — over byte-addressed input (`AsciiInput`). Semantics therefore
//! carry by construction; anything outside the supported subset (general
//! `EnterLoop`/`LoopAgain`, backreferences, lookaround, multi-byte loop
//! bodies) declines at compile time and keeps the interpreter, as does every
//! non-x86-64 target and every non-byte input.
//!
//! Per-attempt contract (mirrors `try_at_pos` with `Forward`):
//!   - entry: capture groups all unset; a fresh backtrack stack.
//!   - success: returns the end offset; groups hold the match's captures.
//!   - failure: returns "no match"; groups are back to unset (the interpreter
//!     restores them while unwinding; native simply never publishes them —
//!     the wrapper reinitializes the scratch array per attempt).
//!
//! Every single-character matcher (Char, CharICase, CharSet, Bracket,
//! AsciiBracket, ByteSet2/3/4, MatchAny, dot) is lowered to one shared
//! primitive — "test the next byte against a 256-entry table" — where the
//! table is computed at JIT-compile time by evaluating the interpreter's own
//! matcher for each byte value 0..=255. That makes byte-level parity with the
//! `AsciiInput` interpreter a table-construction fact rather than a
//! re-implementation risk.
//!
//! Off-switch: `ZIPP_NO_RX_JIT=1`. Compile policy: a regex compiles on its
//! Nth forward byte attempt (`ZIPP_RX_JIT_THRESHOLD`, default 64), so cold
//! regexes never pay compilation. `ZIPP_RXSTATS=1` reports compiled/declined/
//! native/interpreted counts (see `stats`).

use crate::bytesearch::{charset_contains, ByteSet};
use crate::insn::{CompiledRegex, Insn};
use crate::matchers::{ASCIICharProperties, CharProperties};
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use dynasmrt::{dynasm, DynamicLabel, DynasmApi, DynasmLabelApi, ExecutableBuffer};
use std::cell::RefCell;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Counters (`ZIPP_RXSTATS=1`; compile-time events count unconditionally).
// ---------------------------------------------------------------------------

static COMPILED: AtomicU64 = AtomicU64::new(0);
static DECLINED_UNSUPPORTED: AtomicU64 = AtomicU64::new(0);
static DECLINED_LIMITS: AtomicU64 = AtomicU64::new(0);
static NATIVE_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static FALLBACK_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static BAILS: AtomicU64 = AtomicU64::new(0);
/// Scan sessions opened (`with_session`). Counts unconditionally — per call,
/// not per attempt — so the differential harness can assert the session path
/// engaged without env plumbing.
static SESSIONS: AtomicU64 = AtomicU64::new(0);

/// Scan sessions opened.
pub(crate) fn session_stats() -> u64 {
    SESSIONS.load(Ordering::Relaxed)
}

/// (compiled, declined: unsupported insn, declined: limits, native attempts,
/// interpreter attempts on byte inputs, native bails).
pub(crate) fn stats() -> (u64, u64, u64, u64, u64, u64) {
    let l = |c: &AtomicU64| c.load(Ordering::Relaxed);
    (
        l(&COMPILED),
        l(&DECLINED_UNSUPPORTED),
        l(&DECLINED_LIMITS),
        l(&NATIVE_ATTEMPTS),
        l(&FALLBACK_ATTEMPTS),
        l(&BAILS),
    )
}

// ---------------------------------------------------------------------------
// Gate: env off-switch, compile threshold, and the test-harness override.
// ---------------------------------------------------------------------------

/// Test override: 0 = normal policy, 1 = force off, 2 = force on (compile on
/// the first attempt). Set via `__rxjit_force` for the differential harness.
static FORCE: AtomicU8 = AtomicU8::new(0);

pub(crate) fn force(mode: Option<bool>) {
    FORCE.store(
        match mode {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        },
        Ordering::Relaxed,
    );
}

fn env_disabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var_os("ZIPP_NO_RX_JIT").is_some_and(|v| v != "0"))
}

fn threshold() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("ZIPP_RX_JIT_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(64)
    })
}

/// Test override for the scan session: 0 = env policy, 1 = force off, 2 =
/// force on. Set via `__rx_scansession_force` for the differential harness.
static SESSION_FORCE: AtomicU8 = AtomicU8::new(0);

pub(crate) fn session_force(mode: Option<bool>) {
    SESSION_FORCE.store(
        match mode {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        },
        Ordering::Relaxed,
    );
}

fn session_env_disabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var_os("ZIPP_NO_RX_SCANSESSION").is_some_and(|v| v != "0"))
}

/// Whether the advance loop may hoist the per-attempt wrapper into a session
/// (`ZIPP_NO_RX_SCANSESSION=1` keeps the legacy `run_attempt` path).
#[inline]
pub(crate) fn session_enabled() -> bool {
    match SESSION_FORCE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => !session_env_disabled(),
    }
}

// ---------------------------------------------------------------------------
// The per-regex slot: use counter + lazily compiled code.
// ---------------------------------------------------------------------------

/// Compilation state cached on the `CompiledRegex`. Cloning a regex clones it
/// COLD: the twin re-earns compilation through its own counter.
#[derive(Default)]
pub struct JitSlot {
    count: AtomicU32,
    code: OnceLock<Option<JitCode>>,
}

impl Clone for JitSlot {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl core::fmt::Debug for JitSlot {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("JitSlot")
            .field("compiled", &self.code.get().map(Option::is_some))
            .finish()
    }
}

impl JitSlot {
    /// The native code for this regex, compiling it if this attempt crosses
    /// the use threshold. `None` means "run the interpreter" (cold, declined,
    /// or switched off).
    #[inline]
    pub(crate) fn acquire(&self, re: &CompiledRegex) -> Option<&JitCode> {
        if let Some(code) = self.code.get() {
            let code = code.as_ref();
            if code.is_none() && crate::classicalbacktrack::rxstats::enabled() {
                FALLBACK_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            }
            return code;
        }
        match FORCE.load(Ordering::Relaxed) {
            1 => return None,
            2 => {}
            _ => {
                if env_disabled() {
                    return None;
                }
                let n = self.count.fetch_add(1, Ordering::Relaxed);
                if n < threshold() {
                    if crate::classicalbacktrack::rxstats::enabled() {
                        FALLBACK_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
                    }
                    return None;
                }
            }
        }
        self.code.get_or_init(|| compile(re)).as_ref()
    }
}

// ---------------------------------------------------------------------------
// Runtime context and the attempt wrapper.
// ---------------------------------------------------------------------------

/// The block the native code reads its world from. Field offsets are baked
/// into the emitted code — keep in sync with the prologue in `emit_code`.
#[repr(C)]
struct JitCtx {
    input: *const u8,   // 0x00
    len: u64,           // 0x08
    start: u64,         // 0x10
    groups: *mut u64,   // 0x18: 2 slots per group; u64::MAX = unset
    bt_base: *mut u64,  // 0x20: backtrack stack (32-byte entries)
    bt_limit: *mut u64, // 0x28: last allowed entry start
    skip_hint: u64,     // 0x30: out; u64::MAX = none
}

/// Compiled native code for one regex. The entry reproduces one forward
/// `try_at_pos` attempt; returns end offset, -1 (no match) or -2 (backtrack
/// buffer full — the caller grows it and reruns, or bails to the interpreter).
pub(crate) struct JitCode {
    /// Keeps the mapping alive; `entry` points into it.
    _buf: ExecutableBuffer,
    entry: unsafe extern "win64" fn(*mut JitCtx) -> i64,
    groups: usize,
}

// ExecutableBuffer is an immutable mapped region after finalize; the entry
// pointer is only ever called, never written through.
unsafe impl Send for JitCode {}
unsafe impl Sync for JitCode {}

pub(crate) enum Outcome {
    Match { end: usize, skip_hint: u64 },
    NoMatch { skip_hint: u64 },
    /// Native gave up (backtrack buffer cap); rerun the attempt interpreted.
    Bail,
}

struct Scratch {
    groups: Vec<u64>,
    bt: Vec<u64>,
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch {
        groups: Vec::new(),
        // 1024 entries of 4 u64s to start; grows on demand.
        bt: vec![0u64; 4096],
    });
}

/// Hard cap on the native backtrack buffer (u64s): 32 MB. Past this the
/// attempt reruns in the interpreter (whose stack is a plain growable Vec).
const MAX_BT_U64S: usize = 1 << 22;

/// Run one native attempt at byte offset `start`. On a match, `set_group` is
/// called for every capture group with (index, start, end) — u64::MAX = unset.
pub(crate) fn run_attempt(
    code: &JitCode,
    bytes: &[u8],
    start: usize,
    mut set_group: impl FnMut(usize, u64, u64),
) -> Outcome {
    SCRATCH.with(|s| {
        let s = &mut *s.borrow_mut();
        loop {
            s.groups.clear();
            s.groups.resize(code.groups * 2, u64::MAX);
            let mut ctx = JitCtx {
                input: bytes.as_ptr(),
                len: bytes.len() as u64,
                start: start as u64,
                groups: s.groups.as_mut_ptr(),
                bt_base: s.bt.as_mut_ptr(),
                // Entries are 4 u64s; the last allowed entry starts 4 short.
                bt_limit: unsafe { s.bt.as_mut_ptr().add(s.bt.len() - 4) },
                skip_hint: u64::MAX,
            };
            // SAFETY: the code only reads `bytes[0..len)`, reads/writes the
            // scratch arrays within the bounds passed here, and follows the
            // declared calling convention.
            let r = unsafe { (code.entry)(&mut ctx) };
            match r {
                -2 => {
                    let grown = s.bt.len() * 2;
                    if grown > MAX_BT_U64S {
                        BAILS.fetch_add(1, Ordering::Relaxed);
                        return Outcome::Bail;
                    }
                    s.bt.resize(grown, 0);
                    // Retry from scratch: an attempt is deterministic.
                }
                -1 => {
                    if crate::classicalbacktrack::rxstats::enabled() {
                        NATIVE_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
                    }
                    return Outcome::NoMatch { skip_hint: ctx.skip_hint };
                }
                end => {
                    if crate::classicalbacktrack::rxstats::enabled() {
                        NATIVE_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
                    }
                    for g in 0..code.groups {
                        set_group(g, s.groups[g * 2], s.groups[g * 2 + 1]);
                    }
                    return Outcome::Match { end: end as usize, skip_hint: ctx.skip_hint };
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Scan session: the per-call form of the attempt wrapper. One advance loop's
// worth of attempts shares a single TLS borrow, groups sizing, and context
// build; each attempt only refills the group slots and re-enters the code.
// ---------------------------------------------------------------------------

pub(crate) struct Session<'s> {
    code: &'s JitCode,
    ctx: JitCtx,
    groups: &'s mut Vec<u64>,
    bt: &'s mut Vec<u64>,
}

/// Open a session over `bytes` and run `f` inside it. The thread-local
/// scratch backs the session; if it is already borrowed (regress-internal
/// re-entry — none exists today) the session degrades to freshly allocated
/// buffers rather than panicking.
pub(crate) fn with_session<R>(
    code: &JitCode,
    bytes: &[u8],
    f: impl FnOnce(&mut Session) -> R,
) -> R {
    SESSIONS.fetch_add(1, Ordering::Relaxed);
    SCRATCH.with(|s| match s.try_borrow_mut() {
        Ok(mut s) => {
            let s = &mut *s;
            f(&mut Session::new(code, bytes, &mut s.groups, &mut s.bt))
        }
        Err(_) => {
            let mut groups = Vec::new();
            let mut bt = vec![0u64; 4096];
            f(&mut Session::new(code, bytes, &mut groups, &mut bt))
        }
    })
}

impl<'s> Session<'s> {
    fn new(
        code: &'s JitCode,
        bytes: &[u8],
        groups: &'s mut Vec<u64>,
        bt: &'s mut Vec<u64>,
    ) -> Self {
        groups.clear();
        groups.resize(code.groups * 2, u64::MAX);
        let ctx = JitCtx {
            input: bytes.as_ptr(),
            len: bytes.len() as u64,
            start: 0,
            groups: groups.as_mut_ptr(),
            bt_base: bt.as_mut_ptr(),
            // Entries are 4 u64s; the last allowed entry starts 4 short.
            bt_limit: unsafe { bt.as_mut_ptr().add(bt.len() - 4) },
            skip_hint: u64::MAX,
        };
        Self { code, ctx, groups, bt }
    }

    /// Run one native attempt at byte offset `start`; the per-attempt
    /// contract is `run_attempt`'s. On a match, `set_group` is called for
    /// every capture group with (index, start, end) — u64::MAX = unset.
    pub(crate) fn attempt(
        &mut self,
        start: usize,
        mut set_group: impl FnMut(usize, u64, u64),
    ) -> Outcome {
        debug_assert!(start as u64 <= self.ctx.len);
        loop {
            // A failed attempt leaves partial group writes behind — refill
            // before every entry (only the alloc/borrow was hoisted).
            self.groups.fill(u64::MAX);
            self.ctx.groups = self.groups.as_mut_ptr();
            self.ctx.start = start as u64;
            self.ctx.skip_hint = u64::MAX;
            // SAFETY: as in `run_attempt` — the code only reads the input
            // slice `with_session` was given (alive for the whole session)
            // and stays within the scratch bounds in `ctx`.
            let r = unsafe { (self.code.entry)(&mut self.ctx) };
            match r {
                -2 => {
                    let grown = self.bt.len() * 2;
                    if grown > MAX_BT_U64S {
                        BAILS.fetch_add(1, Ordering::Relaxed);
                        return Outcome::Bail;
                    }
                    self.bt.resize(grown, 0);
                    // The resize can move the buffer: recompute the context
                    // pointers before the retry.
                    self.ctx.bt_base = self.bt.as_mut_ptr();
                    self.ctx.bt_limit =
                        unsafe { self.bt.as_mut_ptr().add(self.bt.len() - 4) };
                }
                -1 => {
                    if crate::classicalbacktrack::rxstats::enabled() {
                        NATIVE_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
                    }
                    return Outcome::NoMatch { skip_hint: self.ctx.skip_hint };
                }
                end => {
                    if crate::classicalbacktrack::rxstats::enabled() {
                        NATIVE_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
                    }
                    for g in 0..self.code.groups {
                        set_group(g, self.groups[g * 2], self.groups[g * 2 + 1]);
                    }
                    return Outcome::Match { end: end as usize, skip_hint: self.ctx.skip_hint };
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Compile-time planning: classify the insn stream, or decline.
// ---------------------------------------------------------------------------

/// Caps chosen far above anything the hot row needs; past them the
/// interpreter is fine and the code-size risk is not worth it.
const MAX_INSNS: usize = 4096;
const MAX_GROUPS: usize = 1024;

enum Node {
    /// Consume one byte matching table `t`.
    Test(usize),
    /// Consume a literal byte sequence.
    Seq(Vec<u8>),
    /// One-char loop over table `t`; continuation is the node after the
    /// (skipped) body.
    Loop { t: usize, min: u64, max: u64, greedy: bool, possessive: bool, hint: bool },
    /// The consumed body of a preceding Loop; nothing may jump here.
    Skip,
    Jump(usize),
    Alt(usize),
    Begin(u32),
    End(u32),
    Reset(u32),
    Sol { multiline: bool, lt: usize },
    Eol { multiline: bool, lt: usize },
    Wb { invert: bool, w: usize },
    Goal,
    JustFail,
}

struct Plan {
    nodes: Vec<Node>,
    tables: Vec<[u8; 256]>,
}

fn intern(tables: &mut Vec<[u8; 256]>, t: [u8; 256]) -> usize {
    if let Some(i) = tables.iter().position(|x| x == &t) {
        return i;
    }
    tables.push(t);
    tables.len() - 1
}

fn build_table(tables: &mut Vec<[u8; 256]>, f: impl Fn(u8) -> bool) -> usize {
    let mut t = [0u8; 256];
    for b in 0..=255u8 {
        t[b as usize] = f(b) as u8;
    }
    intern(tables, t)
}

/// The 256-entry table for a single-character matcher insn, built by
/// evaluating the same predicates the interpreter's `scm` matchers use for
/// `AsciiInput` (Element = u8) on every byte value. `None` if the insn is not
/// a supported one-byte matcher.
fn table_for(re: &CompiledRegex, insn: &Insn, tables: &mut Vec<[u8; 256]>) -> Option<usize> {
    let unicode = re.flags.unicode_mode();
    Some(match insn {
        // scm::Char — element compare after u8 narrowing (wide chars never
        // match byte input).
        &Insn::Char(c) => match u8::try_from(c) {
            Ok(cb) => build_table(tables, |b| b == cb),
            Err(_) => build_table(tables, |_| false),
        },
        // scm::CharICase — `c2 == c || fold(c2) == c`.
        &Insn::CharICase(c) => match u8::try_from(c) {
            Ok(cb) => build_table(tables, |b| {
                b == cb || ASCIICharProperties::fold(b, unicode) == cb
            }),
            Err(_) => build_table(tables, |_| false),
        },
        // scm::CharSet.
        Insn::CharSet(chars) => build_table(tables, |b| charset_contains(chars, b as u32)),
        // scm::Bracket via ASCIICharProperties::bracket.
        &Insn::Bracket(idx) => {
            let bc = &re.brackets[idx];
            build_table(tables, |b| ASCIICharProperties::bracket(bc, b))
        }
        // scm::MatchByteSet over the ASCII bitmap.
        Insn::AsciiBracket(bitmap) => build_table(tables, |b| bitmap.contains(b)),
        // scm::MatchByteArraySet.
        &Insn::ByteSet2(set) => build_table(tables, |b| set.contains(b)),
        &Insn::ByteSet3(set) => build_table(tables, |b| set.contains(b)),
        &Insn::ByteSet4(set) => build_table(tables, |b| set.contains(b)),
        // scm::MatchAny — any byte counts.
        Insn::MatchAny => build_table(tables, |_| true),
        // scm::MatchAnyExceptLineTerminator.
        Insn::MatchAnyExceptLineTerminator => {
            build_table(tables, |b| !ASCIICharProperties::is_line_terminator(b))
        }
        // A 1-byte literal consumes exactly one byte, so it is loop-safe.
        Insn::ByteSeq1(v) => {
            let b0 = v[0];
            build_table(tables, |b| b == b0)
        }
        _ => return None,
    })
}

fn plan(re: &CompiledRegex) -> Option<Plan> {
    let insns = &re.insns;
    let n = insns.len();
    if n == 0 || n > MAX_INSNS || re.groups as usize > MAX_GROUPS {
        DECLINED_LIMITS.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    let mut tables = Vec::new();
    let mut nodes = Vec::with_capacity(n);
    let unsupported = || {
        DECLINED_UNSUPPORTED.fetch_add(1, Ordering::Relaxed);
        None::<Plan>
    };
    let mut i = 0usize;
    while i < n {
        let insn = &insns[i];
        // Loops first: they consume their body insn.
        if let &Insn::Loop1CharBody { min_iters, max_iters, greedy, possessive } = insn {
            if i + 2 >= n {
                return unsupported();
            }
            // Only bodies that consume EXACTLY one byte per iteration are
            // sound here: backtracking a one-char loop steps one byte.
            let Some(t) = table_for(re, &insns[i + 1], &mut tables) else {
                return unsupported();
            };
            if min_iters as u64 > i32::MAX as u64 {
                DECLINED_LIMITS.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            nodes.push(Node::Loop {
                t,
                min: min_iters as u64,
                max: max_iters as u64,
                greedy,
                possessive,
                hint: re.skip_hint_ip == Some(i as u32),
            });
            nodes.push(Node::Skip);
            i += 2;
            continue;
        }
        if let Some(t) = table_for(re, insn, &mut tables) {
            nodes.push(Node::Test(t));
            i += 1;
            continue;
        }
        macro_rules! seq {
            ($v:expr) => {{
                nodes.push(Node::Seq($v.to_vec()));
            }};
        }
        match insn {
            Insn::Goal => nodes.push(Node::Goal),
            Insn::JustFail => nodes.push(Node::JustFail),
            &Insn::Jump { target } => nodes.push(Node::Jump(target as usize)),
            &Insn::Alt { secondary } => nodes.push(Node::Alt(secondary as usize)),
            &Insn::BeginCaptureGroup(g) => nodes.push(Node::Begin(g as u32)),
            &Insn::EndCaptureGroup(g) => nodes.push(Node::End(g as u32)),
            &Insn::ResetCaptureGroup(g) => nodes.push(Node::Reset(g as u32)),
            &Insn::StartOfLine { multiline } => {
                let lt = build_table(&mut tables, |b| ASCIICharProperties::is_line_terminator(b));
                nodes.push(Node::Sol { multiline, lt });
            }
            &Insn::EndOfLine { multiline } => {
                let lt = build_table(&mut tables, |b| ASCIICharProperties::is_line_terminator(b));
                nodes.push(Node::Eol { multiline, lt });
            }
            &Insn::WordBoundary { invert } => {
                let w = build_table(&mut tables, |b| ASCIICharProperties::is_word_char(b));
                nodes.push(Node::Wb { invert, w });
            }
            &Insn::WordBoundaryUnicodeICase { invert } => {
                let w = build_table(&mut tables, |b| {
                    ASCIICharProperties::is_word_char_unicode_icase(b)
                });
                nodes.push(Node::Wb { invert, w });
            }
            Insn::ByteSeq2(v) => seq!(v),
            Insn::ByteSeq3(v) => seq!(v),
            Insn::ByteSeq4(v) => seq!(v),
            Insn::ByteSeq5(v) => seq!(v),
            Insn::ByteSeq6(v) => seq!(v),
            Insn::ByteSeq7(v) => seq!(v),
            Insn::ByteSeq8(v) => seq!(v),
            Insn::ByteSeq9(v) => seq!(v),
            Insn::ByteSeq10(v) => seq!(v),
            Insn::ByteSeq11(v) => seq!(v),
            Insn::ByteSeq12(v) => seq!(v),
            Insn::ByteSeq13(v) => seq!(v),
            Insn::ByteSeq14(v) => seq!(v),
            Insn::ByteSeq15(v) => seq!(v),
            Insn::ByteSeq16(v) => seq!(v),
            // Everything else — EnterLoop/LoopAgain (general loops with
            // loop-data bookkeeping and the ES empty-loop rule), BackRef,
            // BackRefMulti, Lookahead, Lookbehind — stays interpreted.
            _ => return unsupported(),
        }
        i += 1;
    }
    // No branch may land on a consumed loop body.
    for node in &nodes {
        let target = match node {
            Node::Jump(t) | Node::Alt(t) => Some(*t),
            _ => None,
        };
        if let Some(t) = target {
            if t >= nodes.len() || matches!(nodes[t], Node::Skip) {
                return unsupported();
            }
        }
    }
    Some(Plan { nodes, tables })
}

// ---------------------------------------------------------------------------
// Code emission.
// ---------------------------------------------------------------------------

/// Backtrack entries are 4 u64s: [handler address, a, b, c].
/// On entry to a handler, r14 points one entry PAST the live entry, so its
/// fields are at [r14-32..r14). Register roles for the whole program:
///   r12 = input base   r13 = input length   rbx = position (byte offset)
///   rbp = capture-slot base (2 u64s per group; u64::MAX = unset)
///   rsi = backtrack stack base   r14 = stack top   r15 = last valid entry
///   rdi = ctx pointer (for the skip-hint store)
fn compile(re: &CompiledRegex) -> Option<JitCode> {
    let plan = plan(re)?;
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;
    let nodes = &plan.nodes;
    let labels: Vec<DynamicLabel> = nodes.iter().map(|_| ops.new_dynamic_label()).collect();
    let tbl_labels: Vec<DynamicLabel> =
        plan.tables.iter().map(|_| ops.new_dynamic_label()).collect();
    // Trailer handlers emitted after the main stream:
    // (handler label, continuation/secondary label, kind).
    enum Handler {
        Alt(DynamicLabel, DynamicLabel),
        Greedy(DynamicLabel, DynamicLabel),
        Lazy(DynamicLabel, DynamicLabel),
    }
    let mut handlers: Vec<Handler> = Vec::new();

    dynasm!(ops
        ; .arch x64
        ; push rbx
        ; push rbp
        ; push rsi
        ; push rdi
        ; push r12
        ; push r13
        ; push r14
        ; push r15
        ; mov rdi, rcx
        ; mov r12, [rdi + 0x00]
        ; mov r13, [rdi + 0x08]
        ; mov rbx, [rdi + 0x10]
        ; mov rbp, [rdi + 0x18]
        ; mov rsi, [rdi + 0x20]
        ; mov r14, rsi
        ; mov r15, [rdi + 0x28]
    );

    // Push a 32-byte backtrack entry whose handler is `h`; fields b/c are
    // stored by the caller-provided closure to keep register choices local.
    macro_rules! push_entry_header {
        (->rg) => {
            dynasm!(ops
                ; cmp r14, r15
                ; ja ->overflow
                ; lea rax, [->rg]
                ; mov [r14], rax
            )
        };
        ($h:expr) => {
            dynasm!(ops
                ; cmp r14, r15
                ; ja ->overflow
                ; lea rax, [=>$h]
                ; mov [r14], rax
            )
        };
    }

    for (ip, node) in nodes.iter().enumerate() {
        dynasm!(ops ; =>labels[ip]);
        match node {
            Node::Skip => {}
            Node::Test(t) => {
                dynasm!(ops
                    ; cmp rbx, r13
                    ; jae ->bt
                    ; movzx eax, BYTE [r12 + rbx]
                    ; lea rcx, [=>tbl_labels[*t]]
                    ; cmp BYTE [rcx + rax], 0
                    ; je ->bt
                    ; add rbx, 1
                );
            }
            Node::Seq(bytes) => {
                let len = bytes.len() as i32;
                dynasm!(ops
                    ; mov rax, rbx
                    ; add rax, len
                    ; cmp rax, r13
                    ; ja ->bt
                );
                let mut off = 0usize;
                while off < bytes.len() {
                    let rem = bytes.len() - off;
                    let disp = off as i32;
                    if rem >= 8 {
                        let chunk = i64::from_le_bytes(bytes[off..off + 8].try_into().unwrap());
                        dynasm!(ops
                            ; mov rcx, QWORD chunk
                            ; mov rdx, [r12 + rbx + disp]
                            ; cmp rdx, rcx
                            ; jne ->bt
                        );
                        off += 8;
                    } else if rem >= 4 {
                        let chunk = i32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
                        dynasm!(ops
                            ; cmp DWORD [r12 + rbx + disp], chunk
                            ; jne ->bt
                        );
                        off += 4;
                    } else if rem >= 2 {
                        let chunk = i16::from_le_bytes(bytes[off..off + 2].try_into().unwrap());
                        dynasm!(ops
                            ; cmp WORD [r12 + rbx + disp], chunk
                            ; jne ->bt
                        );
                        off += 2;
                    } else {
                        dynasm!(ops
                            ; cmp BYTE [r12 + rbx + disp], bytes[off] as i8
                            ; jne ->bt
                        );
                        off += 1;
                    }
                }
                dynasm!(ops ; mov rbx, rax);
            }
            &Node::Loop { t, min, max, greedy, possessive, hint } => {
                // Scan forward while the byte matches, bounded by
                // min(len, pos + max). rax = cursor; after the scan,
                // rdx = min_pos, rax = max_pos (mirrors run_scm_loop).
                dynasm!(ops
                    ; lea r9, [=>tbl_labels[t]]
                    ; mov rax, rbx
                );
                if max >= i32::MAX as u64 {
                    dynasm!(ops ; mov rcx, r13);
                } else {
                    dynasm!(ops
                        ; mov rcx, rbx
                        ; add rcx, max as i32
                        ; cmp rcx, r13
                        ; jbe >limok
                        ; mov rcx, r13
                        ; limok:
                    );
                }
                dynasm!(ops
                    ; scan:
                    ; cmp rax, rcx
                    ; jae >scandone
                    ; movzx edx, BYTE [r12 + rax]
                    ; cmp BYTE [r9 + rdx], 0
                    ; je >scandone
                    ; add rax, 1
                    ; jmp <scan
                    ; scandone:
                );
                if min > 0 {
                    dynasm!(ops
                        ; mov rdx, rbx
                        ; add rdx, min as i32
                        ; cmp rax, rdx
                        ; jb ->bt
                    );
                } else {
                    dynasm!(ops ; mov rdx, rbx);
                }
                if greedy && possessive {
                    // Provably-dead backtrack entry elided (possessify.rs);
                    // record the run end for the failed-attempt skip.
                    if hint {
                        dynasm!(ops ; mov [rdi + 0x30], rax);
                    }
                    dynasm!(ops ; mov rbx, rax);
                } else {
                    // rax/rdx are live (max_pos/min_pos): push via r8.
                    let h = ops.new_dynamic_label();
                    if greedy {
                        handlers.push(Handler::Greedy(h, labels[ip + 2]));
                    } else {
                        handlers.push(Handler::Lazy(h, labels[ip + 2]));
                    }
                    dynasm!(ops
                        ; cmp rdx, rax
                        ; je >nopush
                        ; cmp r14, r15
                        ; ja ->overflow
                        ; lea r8, [=>h]
                        ; mov [r14], r8
                        ; mov [r14 + 8], rdx
                        ; mov [r14 + 16], rax
                        ; add r14, 32
                        ; nopush:
                    );
                    if greedy {
                        dynasm!(ops ; mov rbx, rax);
                    } else {
                        dynasm!(ops ; mov rbx, rdx);
                    }
                }
            }
            &Node::Jump(t) => {
                dynasm!(ops ; jmp =>labels[t]);
            }
            &Node::Alt(sec) => {
                let h = ops.new_dynamic_label();
                handlers.push(Handler::Alt(h, labels[sec]));
                push_entry_header!(h);
                dynasm!(ops
                    ; mov [r14 + 8], rbx
                    ; add r14, 32
                );
            }
            &Node::Begin(g) => {
                let off = (g as i32) * 16;
                push_entry_header!(->rg);
                dynasm!(ops
                    ; mov QWORD [r14 + 8], off
                    ; mov rax, [rbp + off]
                    ; mov [r14 + 16], rax
                    ; mov rcx, [rbp + off + 8]
                    ; mov [r14 + 24], rcx
                    ; add r14, 32
                    ; mov [rbp + off], rbx
                );
            }
            &Node::End(g) => {
                let off = (g as i32) * 16;
                dynasm!(ops ; mov [rbp + off + 8], rbx);
            }
            &Node::Reset(g) => {
                let off = (g as i32) * 16;
                push_entry_header!(->rg);
                dynasm!(ops
                    ; mov QWORD [r14 + 8], off
                    ; mov rax, [rbp + off]
                    ; mov [r14 + 16], rax
                    ; mov rcx, [rbp + off + 8]
                    ; mov [r14 + 24], rcx
                    ; add r14, 32
                    ; mov QWORD [rbp + off], -1
                    ; mov QWORD [rbp + off + 8], -1
                );
            }
            &Node::Sol { multiline, lt } => {
                dynasm!(ops
                    ; test rbx, rbx
                    ; je >ok
                );
                if multiline {
                    dynasm!(ops
                        ; movzx eax, BYTE [r12 + rbx - 1]
                        ; lea rcx, [=>tbl_labels[lt]]
                        ; cmp BYTE [rcx + rax], 0
                        ; je ->bt
                    );
                } else {
                    dynasm!(ops ; jmp ->bt);
                }
                dynasm!(ops ; ok:);
            }
            &Node::Eol { multiline, lt } => {
                dynasm!(ops
                    ; cmp rbx, r13
                    ; jae >ok
                );
                if multiline {
                    dynasm!(ops
                        ; movzx eax, BYTE [r12 + rbx]
                        ; lea rcx, [=>tbl_labels[lt]]
                        ; cmp BYTE [rcx + rax], 0
                        ; je ->bt
                    );
                } else {
                    dynasm!(ops ; jmp ->bt);
                }
                dynasm!(ops ; ok:);
            }
            &Node::Wb { invert, w } => {
                dynasm!(ops
                    ; lea rcx, [=>tbl_labels[w]]
                    ; xor eax, eax
                    ; test rbx, rbx
                    ; je >noleft
                    ; movzx edx, BYTE [r12 + rbx - 1]
                    ; movzx eax, BYTE [rcx + rdx]
                    ; noleft:
                    ; xor edx, edx
                    ; cmp rbx, r13
                    ; jae >noright
                    ; movzx r8d, BYTE [r12 + rbx]
                    ; movzx edx, BYTE [rcx + r8]
                    ; noright:
                    ; cmp eax, edx
                );
                if invert {
                    // Boundary present (prev != curr) fails when inverted.
                    dynasm!(ops ; jne ->bt);
                } else {
                    dynasm!(ops ; je ->bt);
                }
            }
            Node::Goal => {
                dynasm!(ops
                    ; mov rax, rbx
                    ; jmp ->fin
                );
            }
            Node::JustFail => {
                dynasm!(ops ; jmp ->bt);
            }
        }
    }

    // --- Trailer: the backtrack dispatcher and shared/per-site handlers. ---
    dynasm!(ops
        // Pop-and-dispatch. The stack base means "exhausted": no match.
        ; ->bt:
        ; cmp r14, rsi
        ; jbe ->nomatch
        ; mov rax, [r14 - 32]
        ; jmp rax
        // Shared capture-group restore: [off, start, end]; keep unwinding.
        ; ->rg:
        ; mov rax, [r14 - 24]
        ; mov rcx, [r14 - 16]
        ; mov rdx, [r14 - 8]
        ; mov [rbp + rax], rcx
        ; mov [rbp + rax + 8], rdx
        ; sub r14, 32
        ; jmp ->bt
    );
    for h in &handlers {
        match h {
            // Alt: restore the saved position, take the secondary branch.
            &Handler::Alt(h, sec) => {
                dynasm!(ops
                    ; =>h
                    ; mov rbx, [r14 - 24]
                    ; sub r14, 32
                    ; jmp =>sec
                );
            }
            // GreedyLoop1Char: retreat max one byte toward min, or pop.
            &Handler::Greedy(h, cont) => {
                dynasm!(ops
                    ; =>h
                    ; mov rax, [r14 - 24]
                    ; mov rcx, [r14 - 16]
                    ; cmp rcx, rax
                    ; jne >step
                    ; sub r14, 32
                    ; jmp ->bt
                    ; step:
                    ; sub rcx, 1
                    ; mov [r14 - 16], rcx
                    ; mov rbx, rcx
                    ; jmp =>cont
                );
            }
            // NonGreedyLoop1Char: advance min one byte toward max, or pop.
            &Handler::Lazy(h, cont) => {
                dynasm!(ops
                    ; =>h
                    ; mov rax, [r14 - 24]
                    ; mov rcx, [r14 - 16]
                    ; cmp rax, rcx
                    ; jne >step
                    ; sub r14, 32
                    ; jmp ->bt
                    ; step:
                    ; add rax, 1
                    ; mov [r14 - 24], rax
                    ; mov rbx, rax
                    ; jmp =>cont
                );
            }
        }
    }
    dynasm!(ops
        ; ->nomatch:
        ; mov rax, -1
        ; jmp ->fin
        ; ->overflow:
        ; mov rax, -2
        ; ->fin:
        ; pop r15
        ; pop r14
        ; pop r13
        ; pop r12
        ; pop rdi
        ; pop rsi
        ; pop rbp
        ; pop rbx
        ; ret
    );
    // --- Data: the byte-class tables. ---
    for (i, tbl) in plan.tables.iter().enumerate() {
        dynasm!(ops ; =>tbl_labels[i]);
        for &b in tbl.iter() {
            ops.push(b);
        }
    }

    let buf = ops.finalize().ok()?;
    let entry = unsafe {
        core::mem::transmute::<*const u8, unsafe extern "win64" fn(*mut JitCtx) -> i64>(
            buf.ptr(dynasmrt::AssemblyOffset(0)),
        )
    };
    COMPILED.fetch_add(1, Ordering::Relaxed);
    Some(JitCode { _buf: buf, entry, groups: re.groups as usize })
}
