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
use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Condvar, Mutex};

/// Which side of the agent API this Vm is: the CLI entry Vm (`Main`) or an
/// `agent.start` worker thread's Vm (`Worker`). Stage D's event-loop
/// stay-alive rule branches on this; stage C only records it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // read by stage D (cross-thread wait/notify)
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
