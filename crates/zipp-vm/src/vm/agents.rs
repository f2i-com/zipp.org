//! test262 multi-agent host hooks — the `$262.agent.*` worker subsystem.
//!
//! The harness contract (test262 INTERPRETING.md + harness/atomicsHelper.js):
//! `agent.start(src)` runs `src` in a CONCURRENT agent (a `std::thread` with
//! its own Vm/realm), blocking until the worker signals it is running.
//! `agent.broadcast(sab[, id])` blocks until every started agent has
//! *retrieved* the message — the worker acks on RECEIPT, before invoking its
//! `receiveBroadcast` callback (ack-after-callback deadlocks: callbacks
//! typically enter a blocking `Atomics.wait` immediately). Workers
//! `report(x)` ToString'd values the main agent pops with `getReport()`
//! (null when empty); `sleep(ms)` blocks; `leaving()` marks a worker done
//! (its thread may linger parked — the CLI runs one test per process, so
//! process exit reaps it).
//!
//! Nothing is shared between agents except [`AgentShared`] (reports +
//! handles) and the `Arc<SharedMem>` bytes behind each SharedArrayBuffer;
//! timers/microtasks/heaps are strictly per-Vm.

use super::*;
use crate::heap::{AbData, HeapObj, SharedMem};
use crate::value::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};

/// Which side of the agent API this Vm is: the CLI entry Vm (`Main`) or an
/// `agent.start` worker thread's Vm (`Worker`). The event loop's stay-alive
/// rule branches on this: a Worker also parks for INFINITE-deadline
/// waitAsync waiters (the main agent's notify is what ends them).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AgentRole {
    Main,
    Worker,
}

/// State shared by ALL agents of one test run (main + every worker), behind
/// one `Arc` cloned into each worker thread. Lazily created on the first
/// `agent.start`, so a run that never starts an agent pays nothing.
#[derive(Default)]
pub(crate) struct AgentShared {
    /// The `$262.agent.report` FIFO: workers push ToString'd values, the main
    /// agent's `getReport()` pops (null when empty).
    pub(crate) reports: Mutex<VecDeque<String>>,
    /// One handle per started agent — `broadcast` sends each a message and
    /// then collects exactly one ack per handle.
    pub(crate) agents: Mutex<Vec<AgentHandle>>,
}

/// The main agent's channel to one started worker.
pub(crate) struct AgentHandle {
    tx: mpsc::Sender<BroadcastMsg>,
}

/// One `$262.agent.broadcast` delivery: the truly-shared SAB bytes (the
/// worker re-wraps the SAME `Arc<SharedMem>` in its own heap), the numeric
/// id, and a fresh ack channel the worker signals ON RECEIPT.
pub(crate) struct BroadcastMsg {
    mem: Arc<SharedMem>,
    id: f64,
    ack: mpsc::Sender<()>,
}

/// Flip a `started` latch and wake the main agent blocked in `agent_start`.
fn signal_started(started: &Arc<(Mutex<bool>, Condvar)>) {
    let (lock, cv) = &**started;
    *lock.lock().unwrap() = true;
    cv.notify_all();
}

/// A worker agent's thread main: compile `src` IN-THREAD (the oxc `Allocator`
/// is per-thread and the thread owns its `Program`, so the Vm's `'p` borrow is
/// this scope), run it with a fresh realm sharing only `shared`, then serve
/// broadcasts until the channel disconnects (= the main Vm dropped).
fn agent_worker(
    src: String,
    shared: Arc<AgentShared>,
    rx: mpsc::Receiver<BroadcastMsg>,
    started: Arc<(Mutex<bool>, Condvar)>,
) {
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &src, SourceType::unambiguous()).parse();
    let program = if parsed.errors.is_empty() {
        crate::compile::compile_program(&parsed.program, &src).ok()
    } else {
        None
    };
    let Some(program) = program else {
        // A worker that fails to compile must STILL signal `started` (or
        // agent.start deadlocks) and still ack broadcasts (broadcast blocks
        // on one ack per started agent). The failure itself is unobservable
        // to the harness, like a worker throw.
        signal_started(&started);
        while let Ok(msg) = rx.recv() {
            let _ = msg.ack.send(());
        }
        return;
    };
    let mut vm = Vm::new(&program);
    vm.agent_shared = Some(shared);
    vm.agent_role = AgentRole::Worker;
    signal_started(&started);
    // Run the script + its event loop. A throwing worker script is swallowed
    // (workers don't load the harness; the main agent can only observe
    // reports) — the broadcast loop below still runs so main never deadlocks.
    let _ = vm.run();
    while let Ok(msg) = rx.recv() {
        // Ack on RECEIPT, before the callback runs (see module docs).
        let _ = msg.ack.send(());
        // Materialise the SAB in THIS Vm's heap over the same shared bytes,
        // then invoke the registered callback as cb(sab, id). The fresh sab
        // Value lives on the Rust stack across the re-entrant call — suspend
        // GC for its scope (MEMORY invariant).
        {
            let _gc = vm.gc_lock_guard();
            let sab = Value::heap(vm.adopt_shared_buffer(msg.mem));
            let cb = vm.broadcast_cb;
            if vm.is_callable(cb) {
                let _ = vm.call_value(cb, Value::UNDEFINED, &[sab, Value::num(msg.id)]);
            }
        }
        // Settle anything the callback queued (async callbacks, timers) with
        // the same run-to-completion driver the script path uses.
        vm.run_event_loop();
    }
}

impl<'p> Vm<'p> {
    /// The lazily-created cross-agent state (first use allocates it).
    fn agent_shared(&mut self) -> Arc<AgentShared> {
        match &self.agent_shared {
            Some(s) => Arc::clone(s),
            None => {
                let s = Arc::new(AgentShared::default());
                self.agent_shared = Some(Arc::clone(&s));
                s
            }
        }
    }

    /// `$262.agent.start(src)`: spawn a worker agent thread and BLOCK until it
    /// signals it is running (INTERPRETING.md). The worker compiles and runs
    /// `src` on its own thread with its own Vm/realm; only `agent_shared` (and
    /// any broadcast SAB bytes) are shared.
    pub(crate) fn agent_start(&mut self, src: String) {
        let shared = self.agent_shared();
        let (tx, rx) = mpsc::channel();
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        shared.agents.lock().unwrap().push(AgentHandle { tx });
        let started2 = Arc::clone(&started);
        let shared2 = Arc::clone(&shared);
        std::thread::spawn(move || agent_worker(src, shared2, rx, started2));
        let (lock, cv) = &*started;
        let mut running = lock.lock().unwrap();
        while !*running {
            running = cv.wait(running).unwrap();
        }
    }

    /// `$262.agent.broadcast(sab[, id])`: send the SAB's shared bytes to every
    /// started agent, then BLOCK until each has retrieved its message (one ack
    /// per agent; acks precede callback invocation, so a callback that
    /// immediately enters a blocking wait cannot deadlock this).
    pub(crate) fn agent_broadcast(&mut self, sab: Value, id: f64) -> Result<(), Thrown> {
        let mem = match sab.is_heap().then(|| self.heap.get(sab.heap_index())) {
            Some(HeapObj::ArrayBuffer { data: AbData::Shared(m), .. }) => Arc::clone(m),
            _ => {
                return Err(Thrown(
                    "TypeError: $262.agent.broadcast requires a SharedArrayBuffer".into(),
                ))
            }
        };
        let Some(shared) = self.agent_shared.clone() else {
            return Ok(()); // no agent ever started — nothing to retrieve
        };
        // Send every message first (each with a fresh ack channel), THEN
        // collect the acks with the agents lock released.
        let mut acks = Vec::new();
        {
            let agents = shared.agents.lock().unwrap();
            for h in agents.iter() {
                let (ack_tx, ack_rx) = mpsc::channel();
                if h.tx.send(BroadcastMsg { mem: Arc::clone(&mem), id, ack: ack_tx }).is_ok() {
                    acks.push(ack_rx);
                }
            }
        }
        for ack in acks {
            let _ = ack.recv();
        }
        Ok(())
    }

    /// `$262.agent.report(x)` body: push an already-ToString'd value onto the
    /// shared FIFO (workers report; main pushing is harmless).
    pub(crate) fn agent_push_report(&mut self, s: String) {
        let shared = self.agent_shared();
        shared.reports.lock().unwrap().push_back(s);
    }

    /// Pop the oldest worker-agent report (FIFO). `None` until the agent
    /// subsystem has anything queued — `$262.agent.getReport()` then
    /// returns `null`, which the harness polls against.
    pub(crate) fn agent_pop_report(&mut self) -> Option<String> {
        self.agent_shared
            .as_ref()
            .and_then(|s| s.reports.lock().unwrap().pop_front())
    }

    /// Wrap broadcast-received shared bytes as a SharedArrayBuffer in THIS
    /// Vm's heap: same `Arc<SharedMem>` (aliasing the broadcaster's memory),
    /// registered + proto-linked exactly like `alloc_shared_array_buffer`.
    pub(crate) fn adopt_shared_buffer(&mut self, mem: Arc<SharedMem>) -> u32 {
        let idx = self
            .heap
            .alloc(HeapObj::ArrayBuffer { data: AbData::Shared(mem), detached: false });
        self.shared_buffers.insert(idx);
        if self.sab_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.sab_proto));
        }
        idx
    }
}

// ── the process-global Atomics waiter registry (stage D) ──
//
// Spec WaiterList: one FIFO of suspended `Atomics.wait` calls and pending
// `Atomics.waitAsync` promises per (shared memory, byte address), shared by
// every agent thread in the process. The registry Mutex is the spec's
// critical section: the waited value is re-loaded and the waiter enqueued
// under it, so a `notify` can neither slip between the check and the enqueue
// (lost wakeup) nor observe a half-registered waiter.
//
// LOCK ORDER: registry FIRST, then any cell/mailbox lock — never the
// reverse, and no lock is ever held across user-code reentry.

/// Key of one waiter list: (`Arc::as_ptr` of the `SharedMem`, absolute byte
/// address of the element). Only Shared (SAB-backed) accesses register —
/// the pointer identifies the same memory across every agent's heap (workers
/// re-wrap the same Arc) and is only ever COMPARED, never dereferenced.
pub(crate) type WaiterKey = (usize, usize);

/// One registered waiter, FIFO-ordered per key. A blocked `Atomics.wait`
/// parks on its own [`WaitCell`]; an `Atomics.waitAsync` records its Vm's
/// [`Mailbox`] plus the unique id its `async_waiters` tuple carries.
pub(crate) enum WaiterEntry {
    Sync(Arc<WaitCell>),
    Async { mailbox: Arc<Mailbox>, id: u64 },
}

/// What a blocked `Atomics.wait` parks on: `state` 0 = waiting, 1 = notified.
/// `notify` flips the state under the registry lock; the waiter re-checks it
/// around every condvar wake, so a spurious wakeup (or a plain data op on the
/// address — see the no-spurious-wakeup-* tests) never ends the wait early.
pub(crate) struct WaitCell {
    state: Mutex<u8>,
    cv: Condvar,
}

/// A Vm's cross-thread `Atomics.waitAsync` wake channel: `notify` (from any
/// agent) pushes the woken waiter's id and signals `cv`; the owning Vm's
/// event loop sleeps on `cv` instead of a plain `thread::sleep` (so a remote
/// notify wakes it immediately) and drains `wakes` first each iteration,
/// resolving the matching promises "ok".
#[derive(Default)]
pub(crate) struct Mailbox {
    pub(crate) wakes: Mutex<Vec<u64>>,
    pub(crate) cv: Condvar,
}

/// The per-(memory, address) FIFO waiter lists (see module-level lock order).
fn waiters() -> &'static Mutex<HashMap<WaiterKey, VecDeque<WaiterEntry>>> {
    static WAITERS: OnceLock<Mutex<HashMap<WaiterKey, VecDeque<WaiterEntry>>>> = OnceLock::new();
    WAITERS.get_or_init(Default::default)
}

/// Unique ids tying a registry `Async` entry to one Vm `async_waiters` tuple
/// (0 is reserved for "never registered").
fn next_waiter_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// `timeout_ms` (clamped >= 0, possibly +inf) as an `Instant` deadline; None
/// = wait forever. Finite values are capped (~1 year) so
/// `Duration::from_secs_f64` cannot overflow on absurd-but-finite timeouts.
pub(crate) fn finite_deadline(timeout_ms: f64) -> Option<std::time::Instant> {
    if timeout_ms.is_finite() {
        let secs = (timeout_ms / 1000.0).min(86_400.0 * 365.0);
        Some(std::time::Instant::now() + std::time::Duration::from_secs_f64(secs))
    } else {
        None
    }
}

/// How a blocking `Atomics.wait` ended.
pub(crate) enum WaitOutcome {
    NotEqual,
    TimedOut,
    Ok,
}

/// Suspend the calling agent in `Atomics.wait` (spec DoWait): under the
/// registry lock re-load the element (`still_equal` runs the SeqCst load) and
/// bail "not-equal" on a mismatch, "timed-out" on a zero timeout; otherwise
/// enqueue a fresh cell FIFO and park on it until notified or the deadline
/// lapses. A lapsed waiter deregisters itself under the registry lock —
/// finding its entry already gone means a `notify` popped (and counted) it
/// concurrently, and that wake wins: "ok".
pub(crate) fn sync_wait(
    key: WaiterKey,
    timeout_ms: f64,
    still_equal: impl FnOnce() -> bool,
) -> WaitOutcome {
    let cell = Arc::new(WaitCell { state: Mutex::new(0), cv: Condvar::new() });
    {
        let mut reg = waiters().lock().unwrap();
        if !still_equal() {
            return WaitOutcome::NotEqual;
        }
        if timeout_ms == 0.0 {
            return WaitOutcome::TimedOut;
        }
        reg.entry(key).or_default().push_back(WaiterEntry::Sync(Arc::clone(&cell)));
    }
    let deadline = finite_deadline(timeout_ms);
    let mut st = cell.state.lock().unwrap();
    loop {
        if *st == 1 {
            return WaitOutcome::Ok;
        }
        match deadline {
            None => st = cell.cv.wait(st).unwrap(),
            Some(d) => {
                let now = std::time::Instant::now();
                if now >= d {
                    break;
                }
                st = cell.cv.wait_timeout(st, d - now).unwrap().0;
            }
        }
    }
    // Deadline lapsed with state still 0: drop the cell lock BEFORE taking
    // the registry lock (lock order) and remove our own entry if it survives.
    drop(st);
    let mut reg = waiters().lock().unwrap();
    let removed = match reg.get_mut(&key) {
        Some(q) => {
            let before = q.len();
            q.retain(|e| !matches!(e, WaiterEntry::Sync(c) if Arc::ptr_eq(c, &cell)));
            let removed = q.len() != before;
            if q.is_empty() {
                reg.remove(&key);
            }
            removed
        }
        None => false,
    };
    if removed {
        WaitOutcome::TimedOut
    } else {
        WaitOutcome::Ok
    }
}

/// What `Atomics.waitAsync` decided under the registry lock.
pub(crate) enum AsyncWaitDecision {
    NotEqual,
    TimedOut,
    Registered(u64),
}

/// Register an `Atomics.waitAsync` waiter: the same atomic check-and-enqueue
/// as [`sync_wait`], but the enqueued entry carries the caller's mailbox and
/// a fresh id instead of a parked cell. The caller files the id (with the
/// promise + deadline) in its `async_waiters`.
pub(crate) fn register_async_waiter(
    key: WaiterKey,
    timeout_ms: f64,
    mailbox: &Arc<Mailbox>,
    still_equal: impl FnOnce() -> bool,
) -> AsyncWaitDecision {
    let mut reg = waiters().lock().unwrap();
    if !still_equal() {
        return AsyncWaitDecision::NotEqual;
    }
    if timeout_ms == 0.0 {
        return AsyncWaitDecision::TimedOut;
    }
    let id = next_waiter_id();
    reg.entry(key)
        .or_default()
        .push_back(WaiterEntry::Async { mailbox: Arc::clone(mailbox), id });
    AsyncWaitDecision::Registered(id)
}

/// `Atomics.notify`: pop up to `count` (may be +inf) waiters FIFO and wake
/// each — a Sync waiter's cell is flipped + signalled and a remote Async
/// waiter's id is delivered to its Vm's mailbox, both UNDER the registry lock
/// (so a deadline that later finds its registry entry gone can rely on the
/// wake already sitting in its mailbox); an Async waiter belonging to the
/// CALLING Vm (`own_mailbox`) is instead returned for direct in-place
/// resolution. Returns (total woken, own ids).
pub(crate) fn notify_waiters(
    key: WaiterKey,
    count: f64,
    own_mailbox: &Arc<Mailbox>,
) -> (f64, Vec<u64>) {
    let mut woken = 0.0;
    let mut own = Vec::new();
    let mut reg = waiters().lock().unwrap();
    if let Some(q) = reg.get_mut(&key) {
        while woken < count {
            let Some(e) = q.pop_front() else { break };
            match e {
                WaiterEntry::Sync(cell) => {
                    *cell.state.lock().unwrap() = 1;
                    cell.cv.notify_one();
                }
                WaiterEntry::Async { mailbox, id } => {
                    if Arc::ptr_eq(&mailbox, own_mailbox) {
                        own.push(id);
                    } else {
                        mailbox.wakes.lock().unwrap().push(id);
                        mailbox.cv.notify_all();
                    }
                }
            }
            woken += 1.0;
        }
        if q.is_empty() {
            reg.remove(&key);
        }
    }
    (woken, own)
}

/// Deadline expiry for one waitAsync registration: remove entry `id` under
/// the registry lock. True = it was still queued (the caller resolves
/// "timed-out"); false = a notify already popped it (the caller skips the
/// resolution — the "ok" wake is guaranteed to be in the caller's mailbox,
/// see [`notify_waiters`]).
pub(crate) fn take_async_waiter(key: WaiterKey, id: u64) -> bool {
    let mut reg = waiters().lock().unwrap();
    let Some(q) = reg.get_mut(&key) else { return false };
    let before = q.len();
    q.retain(|e| !matches!(e, WaiterEntry::Async { id: i, .. } if *i == id));
    let removed = q.len() != before;
    if q.is_empty() {
        reg.remove(&key);
    }
    removed
}
