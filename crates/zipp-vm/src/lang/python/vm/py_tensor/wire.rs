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
pub(crate) const WIRE_VERSION: u32 = 3;

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
const TAG_SMALL_BIGINT: u8 = 8;

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
            Arg::SmallBigInt(b) => {
                w.u8(TAG_SMALL_BIGINT);
                w.u8(*b as u8);
            }
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

/// One buffer the kernels see: the bytes the request carried, from `lo`,
/// and every view of the buffer moved down by `base` (`lo` rounded down to
/// [`ALIGN`]), so `data` starts at `base` and an element keeps its alignment.
/// It once started at byte 0, zeros up to `lo`: a 64-byte view 16 MiB into
/// its buffer cost the decoder a 16 MiB allocation.
pub(crate) struct WireBuffer {
    pub id: u32,
    pub detached: bool,
    pub lo: usize,
    pub base: usize,
    pub data: Vec<u8>,
    pub written: bool,
}

/// What a view's offset keeps modulo, when its buffer is moved down to the
/// bytes the request carries: the widest element's size, and more.
const ALIGN: usize = 16;

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
            TAG_SMALL_BIGINT => Arg::SmallBigInt(r.u8()? != 0),
            _ => return None,
        });
    }
    let nbuf = r.u32()? as usize;
    let mut buffers: Vec<WireBuffer> = Vec::with_capacity(nbuf.min(64));
    for _ in 0..nbuf {
        let id = r.u32()?;
        let detached = r.u8()? != 0;
        let lo = r.usize()?;
        let len = r.usize()?;
        let bytes = r.take(len)?;
        // One entry per buffer, as the encoder writes them.
        if buffers.iter().any(|b| b.id == id) {
            return None;
        }
        // At most ALIGN - 1 bytes beyond what the request carried.
        let base = lo - lo % ALIGN;
        let mut data = Vec::new();
        data.try_reserve_exact((lo - base).checked_add(len)?).ok()?;
        data.resize(lo - base, 0);
        data.extend_from_slice(bytes);
        buffers.push(WireBuffer { id, detached, lo, base, data, written: false });
    }
    // Each view moves with its buffer. The encoder sends every view's whole
    // range, so none starts below its buffer's base; a request in which one
    // does is not one this module can run (it declines it).
    for arg in &mut args {
        if let Arg::View(v) = arg {
            if let Some(b) = buffers.iter().find(|b| b.id == v.buffer) {
                v.offset = v.offset.checked_sub(b.base)?;
            }
        }
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
            // Back at the offsets the request named.
            let carried = &b.data[b.lo - b.base..];
            w.u32(b.id);
            w.u64(b.lo as u64);
            w.u64(carried.len() as u64);
            w.0.extend_from_slice(carried);
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

#[cfg(test)]
mod tests {
    use super::*;
    // `args` beside this file, in the engine and in the torch package alike.
    use super::super::args::{KIND_F32, KIND_F64, KIND_U8};

    const OPEN: Budget = Budget { unlimited: true, open: true, heap_room: u64::MAX, steps_left: None };

    fn view(buffer: u32, kind: u8, offset: usize, len: usize) -> Arg {
        Arg::View(View { buffer, kind, offset, len })
    }

    fn views(host: &WireHost) -> Vec<View> {
        host.args.iter().filter_map(|a| if let Arg::View(v) = a { Some(*v) } else { None }).collect()
    }

    /// `buffers[id]` as the engine holds it, a request over `args`, decoded
    /// as the torch module decodes it.
    fn round_trip(buffers: &[Vec<u8>], args: &[Arg]) -> WireHost {
        let request = encode_request(1, args, OPEN, |id| buffers.get(id as usize).map(|b| &b[..]));
        decode_request(&request).expect("a request the encoder wrote decodes")
    }

    /// What the engine does with the response: each written buffer's bytes
    /// copied back at the offset it names (`Vm::pt_bridge`).
    fn apply(buffers: &mut [Vec<u8>], response: &[u8]) {
        for (id, lo, bytes) in decode_response(response).expect("the response decodes").written {
            buffers[id as usize][lo..lo + bytes.len()].copy_from_slice(bytes);
        }
    }

    #[test]
    fn a_high_view_allocates_what_it_carries_and_writes_back_in_place() {
        // 64 bytes at the end of a 16 MiB buffer: the decoder allocated the
        // whole 16 MiB (zeros up to the view) and now 64 bytes.
        let at = 16 << 20;
        let mut buffers = vec![(0..at + 64).map(|i| i as u8).collect::<Vec<u8>>()];
        let mut host = round_trip(&buffers, &[view(0, KIND_F32, at, 16)]);
        assert_eq!(host.buffers[0].data.len(), 64);
        let v = views(&host)[0];
        assert_eq!(v.offset, 0);
        assert_eq!(&host.bytes(0).unwrap()[v.offset..v.offset + 64], &buffers[0][at..]);
        host.bytes_mut(0).unwrap()[v.offset..v.offset + 4].copy_from_slice(&[9, 9, 9, 9]);
        let expected = { let mut b = buffers[0].clone(); b[at..at + 4].copy_from_slice(&[9, 9, 9, 9]); b };
        apply(&mut buffers, &host.response(Outcome::Done));
        assert_eq!(buffers[0], expected);
    }

    #[test]
    fn a_view_keeps_its_alignment_and_one_byte_tails_stay_one_byte() {
        // A one-byte view at an odd offset: at most ALIGN - 1 bytes of prefix,
        // and the offset keeps its value modulo ALIGN.
        let buffers = vec![vec![7u8; 1_000_004]];
        let host = round_trip(&buffers, &[view(0, KIND_U8, 1_000_003, 1)]);
        let v = views(&host)[0];
        assert_eq!(v.offset % ALIGN, 1_000_003 % ALIGN);
        assert!(host.buffers[0].data.len() <= 1 + ALIGN - 1);
        assert_eq!(host.bytes(0).unwrap()[v.offset], 7);
    }

    #[test]
    fn views_of_one_buffer_move_together_and_stay_one_alias() {
        // Overlapping f64 and u8 views and a distant f32 view of one buffer,
        // and a second buffer: each buffer's views keep their distances.
        let mut buffers = vec![(0..4096u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>(), vec![1u8; 64]];
        let args = [view(0, KIND_F64, 1024, 4), view(0, KIND_U8, 1029, 8), view(0, KIND_F32, 4000, 8), view(1, KIND_U8, 32, 8)];
        let mut host = round_trip(&buffers, &args);
        let v = views(&host);
        assert_eq!((v[1].offset - v[0].offset, v[2].offset - v[0].offset), (5, 4000 - 1024));
        assert_eq!(v[0].offset % 8, 0);
        assert_eq!(host.buffers[0].data.len(), 4032 - 1024);
        // A write through one view is seen through the other, and returns to
        // the engine's buffer in place.
        host.bytes_mut(0).unwrap()[v[1].offset] = 200;
        assert_eq!(host.bytes(0).unwrap()[v[0].offset + 5], 200);
        let mut expected = buffers.clone();
        expected[0][1029] = 200;
        apply(&mut buffers, &host.response(Outcome::Declined));
        assert_eq!(buffers, expected);
    }

    #[test]
    fn empty_and_detached_buffers_still_decode() {
        let buffers = vec![vec![5u8; 128]];
        let host = round_trip(&buffers, &[view(0, KIND_F32, 96, 0), view(3, KIND_F32, 0, 4)]);
        let v = views(&host);
        assert_eq!(v[0].offset % ALIGN, 0);
        assert!(host.buffers[0].data.is_empty());
        assert!(host.buffers[1].detached && host.bytes(3).is_none());
        assert_eq!(v[1].offset, 0);
    }

    #[test]
    fn a_malformed_request_is_declined() {
        let buffers = vec![vec![0u8; 256]];
        let good = encode_request(1, &[view(0, KIND_U8, 128, 16)], OPEN, |id| buffers.get(id as usize).map(|b| &b[..]));
        assert!(decode_request(&good).is_some());
        // The view's offset is the u64 after the tag, buffer and kind bytes
        // that follow `nargs`; one below the buffer's base cannot be run.
        let mut low = good.clone();
        let at = 4 + 1 + 8 + 8 + 4 + 1 + 4 + 1;
        low[at..at + 8].copy_from_slice(&64u64.to_le_bytes());
        assert!(decode_request(&low).is_none());
        // The same buffer twice.
        let twice = encode_request(1, &[view(0, KIND_U8, 128, 16)], OPEN, |id| buffers.get(id as usize).map(|b| &b[..]));
        let nbuf_at = twice.len() - (4 + 1 + 8 + 8 + 16) - 4;
        let mut dup = twice[..nbuf_at].to_vec();
        dup.extend_from_slice(&2u32.to_le_bytes());
        dup.extend_from_slice(&twice[nbuf_at + 4..]);
        dup.extend_from_slice(&twice[nbuf_at + 4..]);
        assert!(decode_request(&dup).is_none());
        // Truncated.
        assert!(decode_request(&good[..good.len() - 1]).is_none());
    }
}