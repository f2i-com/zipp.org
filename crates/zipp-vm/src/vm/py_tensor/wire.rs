//! A native tensor kernel call across modules: the engine (built without the
//! torch package's kernels) sends the call to the torch package's own
//! WebAssembly module (zipp_torch.wasm) through one host call, as bytes.
//!
//! The request carries the operation, the arguments ([`Arg`]), what the
//! budget admits ([`Budget`]), and the bytes of every array buffer a view
//! argument reads: from the lowest byte any view of it covers to the highest.
//! The kernels run over those bytes exactly as they run over the engine's
//! heap ([`WireHost`] is their `Host`), so they compute the same values and
//! make the same decisions. The response carries the answer, the steps to
//! charge, and every buffer the kernels wrote, which the engine copies back
//! whatever the answer (a kernel that wrote and then declined leaves the same
//! bytes behind as it would in-process).
//!
//! Little-endian throughout. Request: `op:u32`, budget (`flags:u8`,
//! `heap_room:u64`, `steps_left:u64`), `nargs:u32` and each argument (a tag
//! byte, then its payload), `nbuf:u32` and each buffer (`id:u32`,
//! `detached:u8`, `lo:u64`, `len:u64`, bytes). Response: outcome `u8`,
//! `charged:u64`, `ndirty:u32` and each written buffer (`id:u32`, `lo:u64`,
//! `len:u64`, bytes).
#![allow(dead_code)]
use super::args::{Arg, Outcome, View};

/// Bumped with any change to this layout, `Arg`, or the operation table.
pub(crate) const WIRE_VERSION: u32 = 1;

/// What `Vm::native_kernel_admits` decides from, as data.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Budget {
    /// No meter: everything is admitted.
    pub unlimited: bool,
    /// The meter has not stopped (no terminal status, trace or abort).
    pub open: bool,
    /// Heap bytes left under the ceiling (`usize::MAX` without one).
    pub heap_room: u64,
    /// Steps left, when finite.
    pub steps_left: Option<u64>,
}

impl Budget {
    pub(crate) fn admits(&self, cost: u64, transient: usize) -> bool {
        if self.unlimited {
            return true;
        }
        self.open && (transient as u64) <= self.heap_room && self.steps_left.map_or(true, |left| left >= cost)
    }
}

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let out = self.b.get(self.at..end)?;
        self.at = end;
        Some(out)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn usize(&mut self) -> Option<usize> {
        usize::try_from(self.u64()?).ok()
    }
}

const TAG_OTHER: u8 = 0;
const TAG_NUM: u8 = 1;
const TAG_VIEW: u8 = 2;
const TAG_INTS_NONE: u8 = 3;
const TAG_INTS: u8 = 4;
const TAG_BOOL: u8 = 5;
const TAG_NULL: u8 = 6;
const TAG_UNDEFINED: u8 = 7;

/// The request for `op(args)`. `bytes(id)` is buffer `id`'s whole contents,
/// or `None` when it is detached.
pub(crate) fn encode_request<'b>(op: u32, args: &[Arg], budget: Budget, bytes: impl Fn(u32) -> Option<&'b [u8]>) -> Vec<u8> {
    let mut w = Writer(Vec::new());
    w.u32(op);
    w.u8(budget.unlimited as u8 | (budget.open as u8) << 1 | (budget.steps_left.is_some() as u8) << 2);
    w.u64(budget.heap_room);
    w.u64(budget.steps_left.unwrap_or(0));
    w.u32(args.len() as u32);
    // Each buffer's covered range, in first-seen order.
    let mut ranges: Vec<(u32, usize, usize)> = Vec::new();
    for arg in args {
        match arg {
            Arg::Other => w.u8(TAG_OTHER),
            Arg::Num(x) => {
                w.u8(TAG_NUM);
                w.u64(x.to_bits());
            }
            Arg::View(v) => {
                w.u8(TAG_VIEW);
                w.u32(v.buffer);
                w.u8(v.kind);
                w.u64(v.offset as u64);
                w.u64(v.len as u64);
                let lo = v.offset;
                let hi = v.offset.saturating_add(v.len.saturating_mul(v.size()));
                match ranges.iter_mut().find(|r| r.0 == v.buffer) {
                    Some(r) => {
                        r.1 = r.1.min(lo);
                        r.2 = r.2.max(hi);
                    }
                    None => ranges.push((v.buffer, lo, hi)),
                }
            }
            Arg::Ints(None) => w.u8(TAG_INTS_NONE),
            Arg::Ints(Some(items)) => {
                w.u8(TAG_INTS);
                w.u32(items.len() as u32);
                for &i in items {
                    w.u64(i as u64);
                }
            }
            Arg::Bool(b) => {
                w.u8(TAG_BOOL);
                w.u8(*b as u8);
            }
            Arg::Null => w.u8(TAG_NULL),
            Arg::Undefined => w.u8(TAG_UNDEFINED),
        }
    }
    w.u32(ranges.len() as u32);
    for (id, lo, hi) in ranges {
        w.u32(id);
        match bytes(id) {
            Some(data) => {
                let hi = hi.min(data.len());
                let lo = lo.min(hi);
                w.u8(0);
                w.u64(lo as u64);
                w.u64((hi - lo) as u64);
                w.0.extend_from_slice(&data[lo..hi]);
            }
            None => {
                w.u8(1);
                w.u64(0);
                w.u64(0);
            }
        }
    }
    w.0
}

/// One buffer the kernels see: its bytes from `lo` placed at their own
/// offsets (the bytes below `lo`, which no view covers, are zeros).
pub(crate) struct WireBuffer {
    pub id: u32,
    pub detached: bool,
    pub lo: usize,
    pub data: Vec<u8>,
    pub written: bool,
}

/// A decoded request: the kernels' `Host` (see `kernels::Host`).
pub(crate) struct WireHost {
    pub op: u32,
    pub budget: Budget,
    pub args: Vec<Arg>,
    pub buffers: Vec<WireBuffer>,
    pub charged: u64,
}

pub(crate) fn decode_request(b: &[u8]) -> Option<WireHost> {
    let mut r = Reader { b, at: 0 };
    let op = r.u32()?;
    let flags = r.u8()?;
    let heap_room = r.u64()?;
    let steps = r.u64()?;
    let budget = Budget {
        unlimited: flags & 1 != 0,
        open: flags & 2 != 0,
        heap_room,
        steps_left: (flags & 4 != 0).then_some(steps),
    };
    let nargs = r.u32()? as usize;
    let mut args = Vec::with_capacity(nargs.min(64));
    for _ in 0..nargs {
        args.push(match r.u8()? {
            TAG_OTHER => Arg::Other,
            TAG_NUM => Arg::Num(f64::from_bits(r.u64()?)),
            TAG_VIEW => Arg::View(View {
                buffer: r.u32()?,
                kind: r.u8()?,
                offset: r.usize()?,
                len: r.usize()?,
            }),
            TAG_INTS_NONE => Arg::Ints(None),
            TAG_INTS => {
                let n = r.u32()? as usize;
                let mut items = Vec::with_capacity(n.min(1 << 16));
                for _ in 0..n {
                    items.push(r.usize()?);
                }
                Arg::Ints(Some(items))
            }
            TAG_BOOL => Arg::Bool(r.u8()? != 0),
            TAG_NULL => Arg::Null,
            TAG_UNDEFINED => Arg::Undefined,
            _ => return None,
        });
    }
    let nbuf = r.u32()? as usize;
    let mut buffers = Vec::with_capacity(nbuf.min(64));
    for _ in 0..nbuf {
        let id = r.u32()?;
        let detached = r.u8()? != 0;
        let lo = r.usize()?;
        let len = r.usize()?;
        let bytes = r.take(len)?;
        let mut data = vec![0u8; lo.checked_add(len)?];
        data[lo..].copy_from_slice(bytes);
        buffers.push(WireBuffer { id, detached, lo, data, written: false });
    }
    Some(WireHost { op, budget, args, buffers, charged: 0 })
}

impl WireHost {
    pub(crate) fn bytes(&self, id: u32) -> Option<&[u8]> {
        let b = self.buffers.iter().find(|b| b.id == id)?;
        (!b.detached).then_some(&b.data[..])
    }
    pub(crate) fn bytes_mut(&mut self, id: u32) -> Option<&mut [u8]> {
        let b = self.buffers.iter_mut().find(|b| b.id == id)?;
        if b.detached {
            return None;
        }
        b.written = true;
        Some(&mut b.data[..])
    }

    /// The response to this request, once the kernel answered `outcome`.
    pub(crate) fn response(&self, outcome: Outcome) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.u8(match outcome {
            Outcome::Declined => 0,
            Outcome::Done => 1,
            Outcome::Answer(false) => 2,
            Outcome::Answer(true) => 3,
            Outcome::Null => 4,
        });
        w.u64(self.charged);
        let written: Vec<&WireBuffer> = self.buffers.iter().filter(|b| b.written).collect();
        w.u32(written.len() as u32);
        for b in written {
            w.u32(b.id);
            w.u64(b.lo as u64);
            w.u64((b.data.len() - b.lo) as u64);
            w.0.extend_from_slice(&b.data[b.lo..]);
        }
        w.0
    }
}

/// A decoded response: the answer, the steps to charge, and the written
/// buffers' bytes from `lo`.
pub(crate) struct Response<'a> {
    pub outcome: Outcome,
    pub charged: u64,
    pub written: Vec<(u32, usize, &'a [u8])>,
}

pub(crate) fn decode_response(b: &[u8]) -> Option<Response<'_>> {
    let mut r = Reader { b, at: 0 };
    let outcome = match r.u8()? {
        0 => Outcome::Declined,
        1 => Outcome::Done,
        2 => Outcome::Answer(false),
        3 => Outcome::Answer(true),
        4 => Outcome::Null,
        _ => return None,
    };
    let charged = r.u64()?;
    let n = r.u32()? as usize;
    let mut written = Vec::with_capacity(n.min(64));
    for _ in 0..n {
        let id = r.u32()?;
        let lo = r.usize()?;
        let len = r.usize()?;
        written.push((id, lo, r.take(len)?));
    }
    Some(Response { outcome, charged, written })
}
