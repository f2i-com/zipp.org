//! test262 multi-agent host hooks — the `$262.agent.*` surface.
//!
//! The harness contract (test262 INTERPRETING.md + harness/atomicsHelper.js):
//! `agent.start(src)` runs `src` in a concurrent agent; `agent.broadcast(sab)`
//! blocks until every started agent has *retrieved* the message via its
//! `receiveBroadcast` callback registration; workers `report(str)` strings the
//! main agent pops with `getReport()` (null when empty); `sleep(ms)` blocks;
//! `leaving()` marks a worker done.

use super::Vm;

impl<'p> Vm<'p> {
    /// Pop the oldest worker-agent report (FIFO). `None` until the agent
    /// subsystem has anything queued — `$262.agent.getReport()` then
    /// returns `null`, which the harness polls against.
    pub(crate) fn agent_pop_report(&mut self) -> Option<String> {
        None
    }
}
