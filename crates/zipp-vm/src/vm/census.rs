//! The storage census (`ZIPP_CENSUS`): at the end of a run, what the VM's
//! code, caches, frames, heap and JIT hold, as flat `(key, value)` pairs for
//! the CLI to write out. It reads existing fields once and changes nothing.
//! Built only with the `census` feature, so no timed or shipped build
//! carries it (an unused function still moves a fat-LTO build's layout).
//!
//! Each byte has one owner: a function's code and tables are counted once
//! however many of the VM's lists name it, the static key plans by the VM's
//! own retained-bytes account, and the heap by its table and payload
//! estimate. Vectors are counted at capacity, which is what they hold.

use super::*;
use std::mem::{size_of, size_of_val};

static CENSUS: std::sync::Mutex<Option<Vec<(String, u64)>>> = std::sync::Mutex::new(None);

/// The census the last run recorded, once.
pub(crate) fn take_census() -> Option<Vec<(String, u64)>> {
    CENSUS.lock().ok()?.take()
}

impl<'p> Vm<'p> {
    /// Record this VM's census when `ZIPP_CENSUS` asks for one.
    pub(crate) fn record_census(&self) {
        if std::env::var_os("ZIPP_CENSUS").is_none() {
            return;
        }
        let census = self.storage_census();
        if let Ok(mut slot) = CENSUS.lock() {
            *slot = Some(census);
        }
    }

    fn storage_census(&self) -> Vec<(String, u64)> {
        let mut out: Vec<(String, u64)> = Vec::new();
        let mut put = |key: &str, value: usize| out.push((key.to_string(), value as u64));

        // The hot types' sizes on this target.
        put("layout.value", size_of::<Value>());
        put("layout.instr", size_of::<crate::bytecode::Instr>());
        put("layout.func_proto", size_of::<crate::bytecode::FuncProto>());
        put("layout.fused_code", size_of::<crate::bytecode::FusedCode>());
        put("layout.call_site", size_of::<crate::bytecode::CallSite>());
        put("layout.frame", size_of::<Frame>());
        put("layout.site_ic", size_of::<super::ic::SiteIc>());
        put("layout.ic_entry", size_of::<super::ic::IcEntry>());
        put("layout.heap_obj", size_of::<crate::heap::HeapObj>());

        // Code: every function the VM can run, the program's and those
        // installed since (eval, new Function, modules), each once.
        let mut seen = std::collections::HashSet::new();
        let (mut functions, mut instrs, mut instr_bytes, mut constant_bytes, mut string_constant_bytes) = (0, 0, 0, 0, 0);
        let (mut bigints, mut wtf8_bytes, mut upvalue_bytes, mut eval_sites, mut source_bytes) = (0, 0, 0, 0, 0);
        let (mut call_site_bytes, mut name_bytes, mut plan_handles, mut proto_bytes) = (0, 0, 0, 0);
        let (mut fused_functions, mut fused_bytes, mut step_bytes, mut switch_bytes) = (0, 0, 0, 0);
        for f in self.funcs.iter().chain(self.eval_funcs.iter()) {
            if !seen.insert(*f as *const crate::bytecode::FuncProto) {
                continue;
            }
            functions += 1;
            proto_bytes += size_of::<crate::bytecode::FuncProto>();
            instrs += f.code.len();
            instr_bytes += f.code.capacity() * size_of::<crate::bytecode::Instr>();
            constant_bytes += f.constants.capacity() * size_of::<Value>();
            string_constant_bytes += f.string_constants.capacity() * size_of::<String>()
                + f.string_constants.iter().map(String::capacity).sum::<usize>();
            plan_handles += size_of_val(f.static_key_plans.as_slice());
            bigints += f.bigint_consts.len();
            wtf8_bytes += size_of_val(f.wtf8_consts.as_slice());
            upvalue_bytes += size_of_val(f.upvalues.as_slice());
            eval_sites += f.eval_sites.len();
            source_bytes += f.source.capacity();
            call_site_bytes += size_of_val(&*f.call_sites);
            name_bytes += f.name.capacity();
            let (fused, steps, switches) = f.fused.census_bytes();
            fused_functions += usize::from(fused > 0);
            fused_bytes += fused;
            step_bytes += steps;
            switch_bytes += switches;
        }
        put("code.functions", functions);
        put("code.installed_functions", self.eval_funcs.len());
        put("code.func_proto_bytes", proto_bytes);
        put("code.instrs", instrs);
        put("code.instr_bytes", instr_bytes);
        put("code.fused_functions", fused_functions);
        put("code.fused_instr_bytes", fused_bytes);
        put("code.step_site_bytes", step_bytes);
        put("code.str_switch_bytes", switch_bytes);
        put("code.constant_bytes", constant_bytes);
        put("code.string_constant_bytes", string_constant_bytes);
        put("code.static_key_plan_handle_bytes", plan_handles);
        put("code.static_key_plan_sites", self.static_key_plan_sites);
        put("code.static_key_plan_bytes", self.static_key_plan_retained_bytes);
        put("code.bigint_constants", bigints);
        put("code.wtf8_constant_bytes", wtf8_bytes);
        put("code.upvalue_bytes", upvalue_bytes);
        put("code.eval_sites", eval_sites);
        put("code.source_bytes", source_bytes);
        put("code.call_site_bytes", call_site_bytes);
        put("code.name_bytes", name_bytes);

        // The interpreter's inline caches: a per-function table of optional
        // site pointers, as long as the function's code, and the sites.
        let (mut tables, mut table_slots, mut sites) = (0, 0, 0);
        let mut ways = [0usize; super::ic::IC_WAYS + 1];
        for table in self.site_ics.iter().flatten() {
            tables += 1;
            table_slots += table.len();
            for site in table.iter().flatten() {
                sites += 1;
                ways[usize::from(site.n).min(super::ic::IC_WAYS)] += 1;
            }
        }
        put("ic.function_slots", self.site_ics.capacity());
        put("ic.function_slot_bytes", self.site_ics.capacity() * size_of::<Option<Box<[Option<Box<super::ic::SiteIc>>]>>>());
        put("ic.site_tables", tables);
        put("ic.site_table_slots", table_slots);
        put("ic.site_table_bytes", table_slots * size_of::<Option<Box<super::ic::SiteIc>>>());
        put("ic.sites", sites);
        put("ic.site_bytes", sites * size_of::<super::ic::SiteIc>());
        for (n, count) in ways.iter().enumerate() {
            put(&format!("ic.sites_with_ways.{n}"), *count);
        }
        put("ic.dynamic_ic_bytes", self.dynamic_ic_bytes);
        put("ic.own_slot_cache_bytes", size_of_val(&*self.own_slot_cache));
        put("ic.keyed_slot_memo_bytes", size_of_val(&*self.keyed_slot_memo));
        put("ic.private_memo_bytes", size_of_val(&*self.private_memo));
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        put("ic.mi_cache_bytes", self.mi_cache.capacity() * size_of::<i32>());
        let probe_bytes: usize = self.in_probe_sites.iter().flatten().map(|t| size_of_val(&**t)).sum();
        put("ic.in_probe_site_bytes", probe_bytes + self.in_probe_sites.capacity() * size_of::<Option<Box<[u32]>>>());

        // Constant-string caches: the strings are heap values (counted with
        // the heap); these are the tables.
        put("const.string_cache_entries", self.const_string_cache.len());
        put("const.string_dense_bytes", self.const_string_dense.iter().map(|v| v.capacity() * size_of::<Value>()).sum::<usize>());
        put("const.string_text_entries", self.const_string_texts.len());
        put("const.key_string_entries", self.key_str_cache.len());

        // Frames, registers and globals.
        put("frames.capacity", self.frames.capacity());
        put("frames.bytes", self.frames.capacity() * size_of::<Frame>());
        // The register file's high-water mark is tracked for the JIT only.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        {
            put("regs.high_water", self.regs_hw);
            put("regs.high_water_bytes", self.regs_hw * size_of::<Value>());
        }
        put("globals.slots", self.globals.capacity());
        put("globals.bytes", self.globals.capacity() * size_of::<Value>());

        // The heap: its table of objects, the payload estimate, and kinds.
        let (slots, payload, payload_high, kinds) = self.heap.census();
        put("heap.table_slots", slots);
        put("heap.table_bytes", slots * size_of::<crate::heap::HeapObj>());
        put("heap.payload_bytes", payload);
        put("heap.payload_high_water_bytes", payload_high);
        // What an embedder reads (`ScriptState::heap_bytes`): the audit of the
        // VM's resident bytes, the heap's payload at its high-water mark.
        put("heap.vm_resident_bytes", self.heap_bytes());
        for (kind, n) in kinds {
            put(&format!("heap.kind.{kind}"), n);
        }

        // The JIT's machine code and its own caches.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        for (key, value) in self.jit.census() {
            out.push((format!("jit.{key}"), value));
        }
        out
    }
}
