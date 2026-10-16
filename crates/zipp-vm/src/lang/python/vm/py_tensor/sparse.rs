//! The native loop behind `_zipp_tensor.sp_spmm` (torch.sparse.mm and the
//! other sparse @ dense products): tensor.js's `spSpmm`, line for line.
//! Each nonzero (r, c, x) adds x * dense[c, j] to row r of an f64
//! accumulator, in nonzero order, with the JavaScript loop's operations
//! (no fused multiply-add), so the caller's store rounds exactly the values
//! the JavaScript loop would.

/// The row (or column) index an f64 index element names, when it is an
/// integer in `0..bound`.
#[inline]
fn index(x: f64, bound: usize) -> Option<usize> {
    if !(x >= 0.0) || x.fract() != 0.0 || x >= bound as f64 {
        return None;
    }
    Some(x as usize)
}

/// `acc` [m, n] += the product of the sparse [m, k] matrix (`rows`,
/// `cols`, `vals`, one nonzero each) and the dense row-major [k, n]
/// `dense`. `None` when an index is out of range (the JavaScript loop then
/// raises) or `poll` asks to stop between blocks of work.
pub(super) fn spmm(
    rows: &[f64],
    cols: &[f64],
    vals: &[f64],
    dense: &[f64],
    acc: &mut [f64],
    m: usize,
    k: usize,
    n: usize,
    poll_units: usize,
    mut poll: impl FnMut() -> bool,
) -> Option<()> {
    let nnz = rows.len();
    if cols.len() != nnz || vals.len() != nnz || dense.len() != k.checked_mul(n)? || acc.len() != m.checked_mul(n)? {
        return None;
    }
    // Every index first: a bad one leaves `acc` for the JavaScript loop to
    // report, as it would have before writing anything.
    for e in 0..nnz {
        index(rows[e], m)?;
        index(cols[e], k)?;
    }
    let per_poll = (poll_units / n.max(1)).max(1);
    for e in 0..nnz {
        if e % per_poll == per_poll - 1 && poll() {
            return None;
        }
        let (r, c, x) = (rows[e] as usize, cols[e] as usize, vals[e]);
        let out = &mut acc[r * n..(r + 1) * n];
        let src = &dense[c * n..(c + 1) * n];
        for (o, &d) in out.iter_mut().zip(src) {
            *o += x * d;
        }
    }
    Some(())
}

/// tensor.js's `spCoalesce`: the nonzeros in stable ascending key order,
/// a run of equal keys becoming one entry whose `block` values are copied
/// from its first nonzero and the others added in order, each sum passed
/// through `store` (the rounding a typed-array store makes). Fills
/// `first` (each entry's first nonzero) and `out`, returns the entry count,
/// or `None` when `poll` asks to stop.
#[allow(clippy::too_many_arguments)]
pub(super) fn coalesce(
    keys: &[f64],
    vals: &[f64],
    block: usize,
    store: impl Fn(f64) -> f64,
    first: &mut [f64],
    out: &mut [f64],
    poll_units: usize,
    mut poll: impl FnMut() -> bool,
) -> Option<usize> {
    let n = keys.len();
    if vals.len() != n.checked_mul(block)? || first.len() != n || out.len() != vals.len() || keys.iter().any(|k| k.is_nan()) {
        return None;
    }
    // A stable sort: equal keys keep their order, as the JavaScript merge
    // sort (or its packed native sort) leaves them.
    let mut perm: Vec<usize> = Vec::new();
    perm.try_reserve_exact(n).ok()?;
    perm.extend(0..n);
    perm.sort_by(|&x, &y| keys[x].total_cmp(&keys[y]));
    let per_poll = (poll_units / block.max(1)).max(1);
    let mut groups = 0usize;
    let mut prev = 0.0f64;
    for (j, &p) in perm.iter().enumerate() {
        if j % per_poll == per_poll - 1 && poll() {
            return None;
        }
        let k = keys[p];
        let src = &vals[p * block..(p + 1) * block];
        if j == 0 || k != prev {
            first[groups] = p as f64;
            out[groups * block..(groups + 1) * block].copy_from_slice(src);
            groups += 1;
        } else {
            let dst = &mut out[(groups - 1) * block..groups * block];
            for (o, &x) in dst.iter_mut().zip(src) {
                *o = store(*o + x);
            }
        }
        prev = k;
    }
    Some(groups)
}

/// tensor.js's `spKeys`: each nonzero's row-major linear key over the
/// sparse dims, `indices` holding one row of `nnz` per dim. The sum runs
/// from the last dim, as the JavaScript loop's does.
pub(super) fn keys(indices: &[f64], sizes: &[usize], nnz: usize, out: &mut [f64]) -> Option<()> {
    if indices.len() != sizes.len().checked_mul(nnz)? || out.len() != nnz {
        return None;
    }
    let mut stride = 1.0f64;
    for d in (0..sizes.len()).rev() {
        let row = &indices[d * nnz..(d + 1) * nnz];
        for (o, &i) in out.iter_mut().zip(row) {
            *o += i * stride;
        }
        stride *= sizes[d] as f64;
    }
    Some(())
}

/// tensor.js's `spMerge` (PyTorch's add_out_sparse_contiguous): one pass
/// over t's and s's nonzeros as if each list were sorted by key, the
/// smaller key first and equal keys summed into zeros, s's values scaled
/// by `alpha` (through `store`) unless it is 1. Fills `take` (each result
/// entry's position in t's then s's nonzeros) and `out`; returns the
/// entry count.
#[allow(clippy::too_many_arguments)]
pub(super) fn merge(
    tk: &[f64],
    tv: &[f64],
    sk: &[f64],
    sv: &[f64],
    block: usize,
    alpha: f64,
    store: impl Fn(f64) -> f64,
    take: &mut [f64],
    out: &mut [f64],
) -> Option<usize> {
    let (tn, sn) = (tk.len(), sk.len());
    if tv.len() != tn.checked_mul(block)? || sv.len() != sn.checked_mul(block)? || take.len() != tn + sn || out.len() != (tn + sn).checked_mul(block)? {
        return None;
    }
    let scaled = alpha != 1.0;
    let (mut r, mut i, mut j) = (0usize, 0usize, 0usize);
    while i < tn || j < sn {
        let cmp = if i >= tn {
            -1
        } else if j >= sn {
            1
        } else if tk[i] < sk[j] {
            1
        } else if tk[i] > sk[j] {
            -1
        } else {
            0
        };
        let dst = &mut out[r * block..(r + 1) * block];
        if cmp >= 0 {
            take[r] = i as f64;
            for (o, &x) in dst.iter_mut().zip(&tv[i * block..(i + 1) * block]) {
                *o = store(*o + x);
            }
            i += 1;
        }
        if cmp <= 0 {
            take[r] = (tn + j) as f64;
            for (o, &x) in dst.iter_mut().zip(&sv[j * block..(j + 1) * block]) {
                let x = if scaled { store(alpha * x) } else { x };
                *o = store(*o + x);
            }
            j += 1;
        }
        r += 1;
    }
    Some(r)
}
