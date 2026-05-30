use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use indexmap::IndexMap;

use crate::token::{default_operators, TokenType};

/// Synchronous host call handler. The VM calls this during execution when
/// a script invokes `host.callSync(kind, argsArray)`. The handler receives
/// the call kind and serialized arguments, and returns a JSON string result
/// (or an error). This enables the host to provide synchronous external
/// operations (e.g., GPU compute) without pausing/resuming the VM.
pub type SyncHostCallFn = Arc<dyn Fn(&str, &[String]) -> Result<String, String> + Send + Sync>;

#[derive(Clone)]
pub struct ZippConfig {
    pub max_instructions: Option<u64>,
    pub max_wall_time_ms: Option<u64>,
    pub await_timeout_ms: Option<u64>,
    /// Maximum number of heap objects (strings, arrays, hashes, closures, etc.).
    pub max_heap_objects: Option<usize>,
    /// Maximum heap memory in bytes. Checked alongside max_heap_objects.
    /// Prevents OOM from scripts that grow existing objects (e.g., array.push in a loop).
    pub max_heap_bytes: Option<usize>,
    /// External abort flag — checked in the instruction loop alongside other limits.
    /// When set to `true`, execution terminates immediately with an error.
    /// Used by the host to kill stuck executions after an outer timeout fires.
    pub abort_flag: Option<Arc<AtomicBool>>,
    /// Synchronous host call handler for external operations (GPU compute, etc.).
    /// Called inline during VM execution — must return quickly to avoid timeout.
    pub sync_host_call: Option<SyncHostCallFn>,
    pub enable_vm_profiling: bool,
    /// **Reserved — currently a no-op.** The lexer's operator table is
    /// hard-coded; this field was intended to let embedders override
    /// operator tokens but the wiring through the lexer was never
    /// completed. Setting it has no effect today. The field is kept
    /// because removing it would be a SemVer-breaking change for any
    /// embedder that constructs `ZippConfig` with explicit
    /// fields rather than `..Default::default()`. It will either be
    /// wired through or removed in a future major release.
    ///
    /// Hidden from rustdoc until then so embedders aren't pointed at
    /// a non-functional knob.
    #[doc(hidden)]
    pub operators: IndexMap<&'static str, TokenType>,
    /// When `true`, [`crate::engine::ZippEngine::compile_script`]
    /// and [`crate::engine::ScriptState::eval_in_context`] will run with
    /// a partial AST if the parser produced errors but at least one
    /// statement parsed. Default `false` (fail-closed) — running half
    /// of a malformed script is almost always worse than failing the
    /// whole evaluation. Embedders that load very large legacy bundles
    /// where a few bad statements should not block the rest can opt in.
    pub allow_partial_parse: bool,
}

impl ZippConfig {
    /// Returns true if any execution-limit field is configured. The VM
    /// caches this in `enforce_limits` so dispatch can skip the
    /// per-batch check when no limits are set. Centralised here so
    /// every site that copies the config (constructors,
    /// `set_execution_limits`, pooled-VM reuse) stays in lockstep —
    /// the previous duplicated `is_some() || …` chain in
    /// `set_execution_limits` only checked two of the five fields and
    /// would silently disable heap/abort checks.
    #[inline]
    pub fn requires_limit_checks(&self) -> bool {
        self.max_instructions.is_some()
            || self.max_wall_time_ms.is_some()
            || self.max_heap_objects.is_some()
            || self.max_heap_bytes.is_some()
            || self.abort_flag.is_some()
    }
}

impl std::fmt::Debug for ZippConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZippConfig")
            .field("max_instructions", &self.max_instructions)
            .field("max_wall_time_ms", &self.max_wall_time_ms)
            .field("sync_host_call", &self.sync_host_call.is_some())
            .finish()
    }
}

impl Default for ZippConfig {
    fn default() -> Self {
        Self {
            max_instructions: Some(100_000_000),
            max_wall_time_ms: Some(5_000),
            await_timeout_ms: Some(30_000),
            max_heap_objects: Some(100_000),
            max_heap_bytes: Some(64 * 1024 * 1024),
            abort_flag: None,
            sync_host_call: None,
            enable_vm_profiling: false,
            operators: default_operators().into_iter().collect(),
            allow_partial_parse: false,
        }
    }
}
