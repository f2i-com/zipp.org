//! `Error.prototype.stack` in V8's format (#78):
//!
//! ```text
//! Error: boom
//!     at inner (file.js:1:27)
//!     at outer (file.js:2:27)
//!     at file.js:3:13
//! ```
//!
//! An object gains [[ErrorData]] through `mark_error_data`, which records the
//! innermost `STACK_LIMIT` interpreter frames as (function, instruction)
//! pairs, as V8 records ten. The `stack` getter formats them the first time
//! it is read, with the header `Error.prototype.toString` gives the object
//! then, and keeps the text, as V8 does. Each frame reports the source
//! position of the call or `new` it is suspended in (`FuncProto::
//! call_sites`): every frame below the top is suspended at its call, and the
//! top one's instruction is recorded only where a construction or an eval
//! starts (`New`, `NewSpread`, `super(...)`, `DirectEval`; `sync_top_ip`),
//! which keeps the dispatch loop's hot arms as they were. So the top frame of
//! an error the engine raises itself (`null.x`) has no position
//! (`forget_top_position`), and one a native creates when called without
//! `new` reports that frame's last call. A compiled caller's `new` names its
//! function even when it has no interpreter frame (`jit_new_site`); other
//! frames the native tiers run without one do not appear, and a strict tail
//! call has replaced its caller's frame. Script names come from the host
//! (`Vm::script_name`) or a module's `import.meta.url`; anything else, eval
//! code included, is `<anonymous>`, so embedded code learns no host path
//! unless the host gives one. Where the host's text was compiled behind an
//! embedding preamble or inside an expression wrapper, the compiler recorded
//! the host's positions (`compile::SourceOrigin`): a preamble frame is named
//! `zipp:preamble`, and a wrapper's frame is left out.

use super::*;

/// Frames a stack records, V8's default `Error.stackTraceLimit`.
const STACK_LIMIT: usize = 10;

/// A frame's flag that it was entered by `new` (or `super(...)`).
const CONSTRUCT: u64 = 1 << 31;

/// A frame's flag that its instruction is not known: the top frame of an
/// error the engine raised itself (`forget_top_position`).
const NO_POS: u64 = 1 << 30;

/// `Vm::jit_new_site` while no compiled code is constructing.
pub(crate) const NO_JIT_SITE: u64 = u64::MAX;

/// Frames a stack keeps without an allocation of its own (`ErrorStack::
/// Short`): an error thrown and caught in a loop is rarely deeper, and an
/// allocation an error was a tenth of such a loop.
const SHORT: usize = 4;

/// What `Vm::error_stacks` holds for an error object.
pub(crate) enum ErrorStack {
    /// Not read yet: per frame, innermost first, `func << 32 | ip`, with
    /// `CONSTRUCT` set for a construction.
    Frames(Box<[u64]>),
    /// `Frames` of at most `SHORT` frames, the first `len` of `frames`.
    Short { len: u8, frames: [u64; SHORT] },
    /// The text the first read formatted (WTF-8).
    Text(Box<[u8]>),
}

/// The frames a capture keeps, on the stack: the innermost `skip` it is
/// handed are dropped, and nothing past `STACK_LIMIT` is kept.
struct Kept {
    frames: [u64; STACK_LIMIT],
    len: usize,
    skip: usize,
}

impl Kept {
    fn push(&mut self, rec: u64) {
        if self.skip > 0 {
            self.skip -= 1;
        } else if self.len < STACK_LIMIT {
            self.frames[self.len] = rec;
            self.len += 1;
        }
    }
}

impl ErrorStack {
    fn new(frames: &[u64]) -> ErrorStack {
        if frames.len() <= SHORT {
            let mut short = [0; SHORT];
            short[..frames.len()].copy_from_slice(frames);
            ErrorStack::Short { len: frames.len() as u8, frames: short }
        } else {
            ErrorStack::Frames(frames.into())
        }
    }

    fn frames(&self) -> &[u64] {
        match self {
            ErrorStack::Frames(frames) => frames,
            ErrorStack::Short { len, frames } => &frames[..*len as usize],
            ErrorStack::Text(_) => &[],
        }
    }

    fn frames_mut(&mut self) -> &mut [u64] {
        match self {
            ErrorStack::Frames(frames) => frames,
            ErrorStack::Short { len, frames } => &mut frames[..*len as usize],
            ErrorStack::Text(_) => &mut [],
        }
    }
}

impl<'p> Vm<'p> {
    /// Record the running frame's position for a construction about to run
    /// at `ip`, so an error it creates reports it (see the module docs).
    #[inline]
    pub(crate) fn sync_top_ip(&mut self, ip: usize) {
        if let Some(f) = self.frames.last_mut() {
            f.ip = ip + 1;
        }
    }

    /// Give heap object `idx` the [[ErrorData]] slot, and record the stack it
    /// was created on.
    pub(crate) fn mark_error_data(&mut self, idx: u32) {
        self.mark_constructed_error_data(idx, Value::UNDEFINED);
    }

    /// [`Vm::mark_error_data`] for an error a `super(...)` chain creates
    /// with `new_target` (`class MyErr extends Error`): the stack starts at
    /// the frame that ran `new MyErr`, leaving out the constructors' own
    /// activations, as V8 leaves them out.
    pub(crate) fn mark_constructed_error_data(&mut self, idx: u32, new_target: Value) {
        self.error_data.insert(idx);
        if self.frames.is_empty() {
            return;
        }
        let skip = if new_target == Value::UNDEFINED {
            0
        } else {
            self.frames.iter().rev().take_while(|f| f.new_target == new_target).count()
        };
        // A compiled caller's `new` (`jit_new_site`): the frames from `depth`
        // up are the construction's, and the caller sits just below them,
        // either as the interpreter frame its region runs in, whose ip it
        // corrects, or as no frame at all.
        let (site, depth) = self.jit_new_site;
        let site = (site != NO_JIT_SITE && depth <= self.frames.len()).then(|| {
            let (func, ip) = ((site >> 32) as u32, site as u32 as u64);
            (depth, func, (func as u64) << 32 | ((ip + 1) & (NO_POS - 1)))
        });
        let mut kept = Kept { frames: [0; STACK_LIMIT], len: 0, skip };
        for (i, f) in self.frames.iter().enumerate().rev() {
            let construct = if f.new_target == Value::UNDEFINED { 0 } else { CONSTRUCT };
            let mut rec = (f.func as u64) << 32 | (f.ip as u64 & (NO_POS - 1)) | construct;
            if let Some((depth, func, at)) = site {
                if i + 1 == depth {
                    if f.func == func {
                        rec = at | construct;
                    } else {
                        kept.push(at);
                    }
                }
            }
            kept.push(rec);
            if kept.len == STACK_LIMIT {
                break;
            }
        }
        if let Some((0, _, at)) = site {
            kept.push(at);
        }
        self.error_stacks.insert(idx, ErrorStack::new(&kept.frames[..kept.len]));
    }

    /// Mark the innermost frame of error object `idx`'s stack as having no
    /// known position: an error the engine raised from an instruction the
    /// interpreter did not record (see the module docs). Better no line than
    /// the line of that frame's last call.
    pub(crate) fn forget_top_position(&mut self, idx: u32) {
        if let Some(top) = self
            .error_stacks
            .get_mut(&idx)
            .and_then(|stack| stack.frames_mut().first_mut())
        {
            *top |= NO_POS;
        }
    }

    /// The `stack` of error object `idx`: its header and frames, formatted
    /// on the first read.
    pub(crate) fn error_stack_value(&mut self, idx: u32) -> Result<Value, Thrown> {
        let frames = match self.error_stacks.get(&idx) {
            Some(ErrorStack::Text(text)) => {
                let text = text.to_vec();
                return Ok(self.alloc_wtf8_str(text));
            }
            Some(stack) => stack.frames().to_vec(),
            None => Vec::new(),
        };
        // The header as Error.prototype.toString makes it, whatever the
        // object's own `toString` is: it may run getters.
        let header = self.call_native(crate::vm::native::ERROR_TO_STRING, Value::heap(idx), &[])?;
        let mut text = self.to_wtf8_string(header)?;
        for f in frames {
            text.extend_from_slice(self.stack_frame_line(f).as_bytes());
        }
        self.error_stacks
            .insert(idx, ErrorStack::Text(text.clone().into_boxed_slice()));
        Ok(self.alloc_wtf8_str(text))
    }

    /// One recorded frame as a line of the stack, its leading newline
    /// included: `\n    at name (script:line:col)`, `new name` for a
    /// construction, and no name or parentheses for an unnamed function.
    /// A frame of an embedding preamble's code names `zipp:preamble`, and
    /// one of the wrapper around a host's expression is no line at all
    /// (`CallSite::PLUMBING`, `CallSite::HIDDEN`).
    fn stack_frame_line(&self, f: u64) -> String {
        use crate::bytecode::CallSite;
        let func = (f >> 32) as u32;
        let ip = (f & (NO_POS - 1)) as u32;
        let proto = self.func(func as usize);
        // A frame records the instruction after the one it is suspended at.
        let pos = if f & NO_POS == 0 { proto.position_at(ip.saturating_sub(1)) } else { None };
        let preamble = match pos {
            Some((CallSite::HIDDEN, _)) => return String::new(),
            Some((line, _)) => line & CallSite::PLUMBING != 0,
            None => proto.is_plumbing(),
        };
        let mut loc = if preamble { "zipp:preamble".to_string() } else { self.stack_script_name(func) };
        match pos {
            // Before the host's line 1 (its offsets moved it there).
            Some((0, _)) | None => {}
            Some((line, col)) => loc.push_str(&format!(":{}:{col}", line & !CallSite::PLUMBING)),
        }
        // A class constructor's proto is named `C.constructor`; V8 says `new C`.
        let name = proto.name.strip_suffix(".constructor").unwrap_or(&proto.name);
        let construct = f & CONSTRUCT != 0;
        if !construct && (name.is_empty() || name.starts_with('<')) {
            format!("\n    at {loc}")
        } else {
            let new = if construct { "new " } else { "" };
            let name = if name.is_empty() || name.starts_with('<') { "<anonymous>" } else { name };
            format!("\n    at {new}{name} ({loc})")
        }
    }

    /// The script a function's frames name: its module's `import.meta.url`,
    /// the host's name for the main program, else `<anonymous>`.
    fn stack_script_name(&self, func: u32) -> String {
        #[cfg(not(feature = "wasm-no-fs-loader"))]
        if let Some(&(_, _, ns)) = self
            .module_func_ranges
            .iter()
            .find(|&&(start, end, _)| start <= func && func < end)
        {
            if let Some(url) = self.module_url(ns) {
                return url;
            }
        }
        match &self.script_name {
            Some(name) if (func as usize) < self.main_func_count => name.to_string(),
            _ => "<anonymous>".to_string(),
        }
    }
}
