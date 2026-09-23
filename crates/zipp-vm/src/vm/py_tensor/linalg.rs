//! Dense double-precision factorizations behind the bundled `torch.linalg`
//! (`frontend/python/lib/torch_linalg.py`), one matrix per call.
//!
//! Every routine mirrors the Python fallback's algorithm and conventions so
//! the two paths agree to the documented tolerances (1e-12 relative in
//! float64): LU with partial pivoting (the first largest magnitude, as
//! LAPACK getrf), Householder QR with LAPACK's signs, Cholesky with potrf's
//! failure layout, one-sided Jacobi SVD, and Hessenberg + Francis
//! double-shift QR (EISPACK hqr/hqr2) for the general eigenproblem. The
//! symmetric eigenproblem uses Householder tridiagonalization and the
//! implicit QL iteration (EISPACK tred2/tql2) instead of the fallback's
//! cyclic Jacobi; eigenvalues are ascending and every eigenvector gets the
//! fallback's sign rule, so only (numerically) repeated eigenvalues can give
//! a different basis of their eigenspace.
//!
//! Matrices are row-major `f64` slices. Work is metered through [`Budget`]:
//! each routine spends roughly one unit per multiply-add as it goes and
//! returns `None` when the budget's poll asks it to stop.

/// Units of work between two polls.
const POLL_UNITS: u64 = 1 << 16;

/// The work meter a kernel spends into.
pub(crate) struct Budget<'a> {
    spent: u64,
    since: u64,
    poll: &'a mut dyn FnMut() -> bool,
}

impl<'a> Budget<'a> {
    /// `poll()` returns true when the kernel must stop.
    pub(crate) fn new(poll: &'a mut dyn FnMut() -> bool) -> Self {
        Budget { spent: 0, since: 0, poll }
    }

    /// Records `units` of work; `None` when the poll asks to stop.
    pub(crate) fn spend(&mut self, units: u64) -> Option<()> {
        self.spent = self.spent.saturating_add(units);
        self.since = self.since.saturating_add(units);
        if self.since >= POLL_UNITS {
            self.since = 0;
            if (self.poll)() {
                return None;
            }
        }
        Some(())
    }

    /// The work recorded so far.
    pub(crate) fn spent(&self) -> u64 {
        self.spent
    }
}

/// The operations a caller can ask a work bound for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    /// `lu(m, n)`.
    Lu,
    /// `lu_solve` / `tri_solve` of an n x n system with k right-hand sides (`m` = n, `n` = k).
    Solve,
    /// `cholesky(n)` (`m` = n).
    Cholesky,
    /// `householder(m, n)` plus `form_q` of up to m columns.
    Qr,
    /// `eigh(n)` (`m` = n).
    Eigh,
    /// `svd(m, n, full)`; `full` is taken to be true.
    Svd,
    /// `eig(n)` (`m` = n).
    Eig,
}

/// An upper bound on the units `op` spends on an m x n input (iterative
/// kernels: their iteration caps times the work per iteration), saturating.
pub(crate) fn bound(op: Op, m: usize, n: usize) -> u64 {
    let (m, n) = (m as u64, n as u64);
    let mul = |a: u64, b: u64| a.saturating_mul(b);
    let k = m.min(n);
    let big = m.max(n);
    let base = mul(m, n).saturating_add(16);
    match op {
        Op::Lu => mul(mul(m, n), k).saturating_add(base),
        Op::Solve => mul(mul(m, m), n.saturating_add(1)).saturating_add(base),
        Op::Cholesky => mul(mul(m, m), m).saturating_add(base),
        Op::Qr => mul(mul(mul(m, n), k), 2).saturating_add(mul(mul(mul(m, m), k), 2)).saturating_add(base),
        Op::Eigh => {
            let tri = mul(mul(mul(m, m), m), 6);
            let ql = mul(mul(30, m), mul(m, m.saturating_add(10)));
            tri.saturating_add(ql).saturating_add(mul(30, m)).saturating_add(mul(m, m)).saturating_add(base)
        }
        Op::Svd => {
            // 80 sweeps over k(k-1)/2 pairs, each a dot of `big` and two
            // rotations; the basis completions; the final norms.
            let pairs = mul(k, k) / 2 + 1;
            let sweeps = mul(80, mul(pairs, mul(big, 4).saturating_add(mul(k, 2))));
            let complete = mul(mul(mul(big, big), mul(big, big)), 16).saturating_add(mul(mul(big, big), 16));
            sweeps.saturating_add(complete).saturating_add(mul(mul(big, big), 4)).saturating_add(base)
        }
        Op::Eig => {
            let hess = mul(mul(mul(m, m), m), 6);
            let qr = mul(mul(60, m.saturating_add(1)), mul(mul(m, m.saturating_add(1)), 12));
            let back = mul(mul(mul(m, m), m), 3);
            hess.saturating_add(qr).saturating_add(back).saturating_add(base)
        }
    }
}

// ---- LU ------------------------------------------------------------------------------------

/// A partial-pivoting LU factorization (`_lu`).
pub(crate) struct Lu {
    /// The m x n working matrix: unit-lower L below the diagonal, U on and above.
    pub(crate) lu: Vec<f64>,
    /// `perm[i]`: the source row of row i.
    pub(crate) perm: Vec<usize>,
    /// LAPACK's 1-based row swaps, one per column of min(m, n).
    pub(crate) pivots: Vec<i32>,
    /// The permutation's sign.
    pub(crate) sign: f64,
    /// The 1-based index of the first zero pivot, 0 if none.
    pub(crate) info: usize,
}

/// Partial-pivoting LU of an m x n matrix: getrf's pivot choice (the first
/// largest magnitude); a zero pivot records `info` and skips its column.
pub(crate) fn lu(a: &[f64], m: usize, n: usize, b: &mut Budget) -> Option<Lu> {
    let mut w = a[..m * n].to_vec();
    let mut perm: Vec<usize> = (0..m).collect();
    let k_max = m.min(n);
    let mut pivots = Vec::with_capacity(k_max);
    let mut sign = 1.0;
    let mut info = 0usize;
    for k in 0..k_max {
        let mut p = k;
        let mut best = w[k * n + k].abs();
        for i in k + 1..m {
            let v = w[i * n + k].abs();
            if v > best {
                best = v;
                p = i;
            }
        }
        pivots.push((p + 1) as i32);
        if p != k {
            for j in 0..n {
                w.swap(k * n + j, p * n + j);
            }
            perm.swap(k, p);
            sign = -sign;
        }
        let piv = w[k * n + k];
        if piv == 0.0 {
            if info == 0 {
                info = k + 1;
            }
            continue;
        }
        let (top, rest) = w.split_at_mut((k + 1) * n);
        let rk = &top[k * n..k * n + n];
        for i in 0..m - k - 1 {
            let ri = &mut rest[i * n..i * n + n];
            let f = ri[k] / piv;
            ri[k] = f;
            if f != 0.0 {
                for j in k + 1..n {
                    ri[j] -= f * rk[j];
                }
            }
        }
        b.spend(((m - k) * (n - k)) as u64 + 1)?;
    }
    Some(Lu { lu: w, perm, pivots, sign, info })
}

/// Solves A X = B from `lu`'s factors of an n x n A; `rhs` is n x k.
/// A zero pivot divides as IEEE does (inf or NaN), as LAPACK's getrs.
pub(crate) fn lu_solve(lu: &[f64], perm: &[usize], n: usize, rhs: &[f64], k: usize, b: &mut Budget) -> Option<Vec<f64>> {
    let mut x = vec![0.0; n * k];
    for i in 0..n {
        let src = perm[i];
        x[i * k..i * k + k].copy_from_slice(&rhs[src * k..src * k + k]);
    }
    for i in 0..n {
        let (done, cur) = x.split_at_mut(i * k);
        let xi = &mut cur[..k];
        for j in 0..i {
            let f = lu[i * n + j];
            if f != 0.0 {
                let xj = &done[j * k..j * k + k];
                for c in 0..k {
                    xi[c] -= f * xj[c];
                }
            }
        }
        b.spend((i * k + 1) as u64)?;
    }
    for i in (0..n).rev() {
        let (head, tail) = x.split_at_mut((i + 1) * k);
        let xi = &mut head[i * k..];
        for j in i + 1..n {
            let f = lu[i * n + j];
            if f != 0.0 {
                let xj = &tail[(j - i - 1) * k..(j - i) * k];
                for c in 0..k {
                    xi[c] -= f * xj[c];
                }
            }
        }
        let d = lu[i * n + i];
        for v in xi.iter_mut() {
            *v /= d;
        }
        b.spend(((n - i) * k + 1) as u64)?;
    }
    Some(x)
}

/// Solves T X = B for a triangular n x n T (only that triangle is read);
/// `rhs` is n x k. `unit`: T's diagonal is taken as ones.
pub(crate) fn tri_solve(t: &[f64], n: usize, rhs: &[f64], k: usize, upper: bool, unit: bool, b: &mut Budget) -> Option<Vec<f64>> {
    let mut x = rhs[..n * k].to_vec();
    let step = |i: usize, x: &mut [f64]| {
        let js: Box<dyn Iterator<Item = usize>> = if upper { Box::new(i + 1..n) } else { Box::new(0..i) };
        for j in js {
            let f = t[i * n + j];
            if f != 0.0 {
                for c in 0..k {
                    let v = x[j * k + c];
                    x[i * k + c] -= f * v;
                }
            }
        }
        if !unit {
            let d = t[i * n + i];
            for c in 0..k {
                x[i * k + c] /= d;
            }
        }
    };
    if upper {
        for i in (0..n).rev() {
            step(i, &mut x);
            b.spend(((n - i) * k + 1) as u64)?;
        }
    } else {
        for i in 0..n {
            step(i, &mut x);
            b.spend(((i + 1) * k + 1) as u64)?;
        }
    }
    Some(x)
}

// ---- Cholesky ------------------------------------------------------------------------------

/// The Python runtime's `math.fsum`, which the fallback sums with: Kahan's
/// compensated sum, left to right, the compensation not added back. Summing
/// exactly as it does keeps the native factors bit for bit the fallback's.
#[derive(Clone, Copy)]
struct Fsum {
    s: f64,
    c: f64,
}

impl Fsum {
    fn new() -> Self {
        Fsum { s: 0.0, c: 0.0 }
    }

    #[inline]
    fn add(&mut self, x: f64) {
        let y = x - self.c;
        let t = self.s + y;
        self.c = (t - self.s) - y;
        self.s = t;
    }

    fn value(self) -> f64 {
        self.s
    }
}

/// Per-column `Fsum`s over the rows of a matrix: `add_row` adds `x[c]` (or
/// `f * x[c]`) to column c's sum, so every column sums in row order.
struct Fsums {
    s: Vec<f64>,
    c: Vec<f64>,
}

impl Fsums {
    fn new(n: usize) -> Self {
        Fsums { s: vec![0.0; n], c: vec![0.0; n] }
    }

    fn clear(&mut self) {
        self.s.iter_mut().for_each(|x| *x = 0.0);
        self.c.iter_mut().for_each(|x| *x = 0.0);
    }

    /// Column `lo + j` gains `f * row[j]`.
    #[inline]
    fn add_scaled(&mut self, lo: usize, f: f64, row: &[f64]) {
        let (s, c) = (&mut self.s[lo..lo + row.len()], &mut self.c[lo..lo + row.len()]);
        for ((sj, cj), &x) in s.iter_mut().zip(c.iter_mut()).zip(row) {
            let y = f * x - *cj;
            let t = *sj + y;
            *cj = (t - *sj) - y;
            *sj = t;
        }
    }
}

/// The Python runtime's `math.hypot(a, b)` (the engine's `Math.hypot`):
/// scaled by the larger magnitude, the squares summed with Kahan's
/// compensation.
fn hypot(a: f64, b: f64) -> f64 {
    if a.is_infinite() || b.is_infinite() {
        return f64::INFINITY;
    }
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    let max = a.abs().max(b.abs());
    if max == 0.0 {
        return 0.0;
    }
    let (mut sum, mut comp) = (0.0f64, 0.0f64);
    for v in [a, b] {
        let x = v / max;
        let summand = x * x - comp;
        let pre = sum + summand;
        comp = (pre - sum) - summand;
        sum = pre;
    }
    max * sum.sqrt()
}

/// The lower Cholesky factor of the n x n `a` (its lower triangle is read)
/// and LAPACK's info. On failure at column j (1-based `info`), the failing
/// diagonal holds its non-positive pivot and the rows after it keep the
/// input's lower triangle from that column on, as potrf leaves them.
pub(crate) fn cholesky(a: &[f64], n: usize, b: &mut Budget) -> Option<(Vec<f64>, usize)> {
    let mut l = vec![0.0; n * n];
    for j in 0..n {
        let mut acc = Fsum::new();
        for k in 0..j {
            let v = l[j * n + k];
            acc.add(v * v);
        }
        let s = a[j * n + j] - acc.value();
        if !(s > 0.0) {
            l[j * n + j] = s;
            for i in j + 1..n {
                for c in j..=i {
                    l[i * n + c] = a[i * n + c];
                }
            }
            return Some((l, j + 1));
        }
        let d = s.sqrt();
        l[j * n + j] = d;
        for i in j + 1..n {
            let mut acc = Fsum::new();
            for k in 0..j {
                acc.add(l[i * n + k] * l[j * n + k]);
            }
            l[i * n + j] = (a[i * n + j] - acc.value()) / d;
        }
        b.spend(((n - j) * (j + 1)) as u64 + 1)?;
    }
    Some((l, 0))
}

// ---- QR ------------------------------------------------------------------------------------

/// LAPACK geqrf on an m x n matrix: the working matrix (R on and above the
/// diagonal, the Householder vectors below it, their leading 1 implied) and
/// the taus. Each reflector's beta has the opposite sign of its alpha.
pub(crate) fn householder(a: &[f64], m: usize, n: usize, b: &mut Budget) -> Option<(Vec<f64>, Vec<f64>)> {
    let mut w = a[..m * n].to_vec();
    let k_max = m.min(n);
    let mut taus = Vec::with_capacity(k_max);
    let mut acc = vec![0.0; n];
    let mut sums = Fsums::new(n);
    for j in 0..k_max {
        let alpha = w[j * n + j];
        let mut ss = Fsum::new();
        for i in j + 1..m {
            let v = w[i * n + j];
            ss.add(v * v);
        }
        let xnorm = ss.value().sqrt();
        if xnorm == 0.0 {
            taus.push(0.0);
            continue;
        }
        let beta = -hypot(alpha, xnorm).copysign(alpha);
        let tau = (beta - alpha) / beta;
        let scale = 1.0 / (alpha - beta);
        for i in j + 1..m {
            w[i * n + j] *= scale;
        }
        w[j * n + j] = beta;
        // w_c = A[j][c] + fsum_i(v_i A[i][c]), every column summing in row order.
        sums.clear();
        for i in j + 1..m {
            let vi = w[i * n + j];
            sums.add_scaled(j + 1, vi, &w[i * n + j + 1..i * n + n]);
        }
        for c in j + 1..n {
            acc[c] = (w[j * n + c] + sums.s[c]) * tau;
            w[j * n + c] -= acc[c];
        }
        for i in j + 1..m {
            let vi = w[i * n + j];
            let row = &mut w[i * n + j + 1..i * n + n];
            for (x, &a_) in row.iter_mut().zip(&acc[j + 1..n]) {
                *x -= a_ * vi;
            }
        }
        taus.push(tau);
        b.spend((2 * (m - j) * (n - j)) as u64 + 1)?;
    }
    Some((w, taus))
}

/// The first `cols` columns of H_0 H_1 ... H_{k-1} from `householder`'s
/// m x n working matrix (LAPACK orgqr).
pub(crate) fn form_q(w: &[f64], taus: &[f64], m: usize, n: usize, cols: usize, b: &mut Budget) -> Option<Vec<f64>> {
    let mut q = vec![0.0; m * cols];
    for i in 0..m.min(cols) {
        q[i * cols + i] = 1.0;
    }
    let mut acc = vec![0.0; cols];
    let mut live = vec![false; cols];
    let mut sums = Fsums::new(cols);
    for j in (0..taus.len()).rev() {
        let tau = taus[j];
        if tau == 0.0 {
            continue;
        }
        sums.clear();
        for i in j + 1..m {
            sums.add_scaled(0, w[i * n + j], &q[i * cols..i * cols + cols]);
        }
        for c in 0..cols {
            // A column whose w is exactly zero is left alone.
            let wc = q[j * cols + c] + sums.s[c];
            live[c] = wc != 0.0;
            if live[c] {
                acc[c] = wc * tau;
                q[j * cols + c] -= acc[c];
            }
        }
        for i in j + 1..m {
            let vi = w[i * n + j];
            let row = &mut q[i * cols..i * cols + cols];
            for c in 0..cols {
                if live[c] {
                    row[c] -= acc[c] * vi;
                }
            }
        }
        b.spend((2 * (m - j) * cols) as u64 + 1)?;
    }
    Some(q)
}

// ---- sign rule -----------------------------------------------------------------------------

/// The fallback's `_fix_sign`: negate `v` (and `other` with it) unless its
/// largest-magnitude entry (the first, within a relative 1e-12) is positive.
fn fix_sign(v: &mut [f64], other: Option<&mut [f64]>) {
    let mut best = 0.0f64;
    let mut sign = 1.0f64;
    for &x in v.iter() {
        if x.abs() > best + 1e-12 * best {
            best = x.abs();
            sign = if x < 0.0 { -1.0 } else { 1.0 };
        }
    }
    if sign < 0.0 {
        for x in v.iter_mut() {
            *x = -*x;
        }
        if let Some(o) = other {
            for x in o.iter_mut() {
                *x = -*x;
            }
        }
    }
}

/// A stable ascending order of `keys` (Python's `sorted` on floats).
fn stable_order(keys: &[f64]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..keys.len()).collect();
    order.sort_by(|&x, &y| keys[x].partial_cmp(&keys[y]).unwrap_or(std::cmp::Ordering::Equal));
    order
}

// ---- symmetric eigenproblem ----------------------------------------------------------------

/// Eigenvalues (ascending) and eigenvectors (the columns of an n x n
/// row-major matrix) of the symmetric n x n `a`: Householder
/// tridiagonalization and the implicit QL iteration (EISPACK tred2/tql2).
/// Every eigenvector takes the fallback's sign rule.
pub(crate) fn eigh(a: &[f64], n: usize, b: &mut Budget) -> Option<(Vec<f64>, Vec<f64>)> {
    if n == 0 {
        return Some((Vec::new(), Vec::new()));
    }
    let mut v = a[..n * n].to_vec();
    let mut d = vec![0.0; n];
    let mut e = vec![0.0; n];
    tred2(&mut v, &mut d, &mut e, n, b)?;
    // The QL rotations act on eigenvector columns: keep them as rows.
    let mut vt = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..n {
            vt[j * n + i] = v[i * n + j];
        }
    }
    tql2(&mut vt, &mut d, &mut e, n, b)?;
    let order = stable_order(&d);
    let vals: Vec<f64> = order.iter().map(|&i| d[i]).collect();
    let mut cols: Vec<Vec<f64>> = order.iter().map(|&j| vt[j * n..j * n + n].to_vec()).collect();
    let mut out = vec![0.0; n * n];
    for (j, col) in cols.iter_mut().enumerate() {
        fix_sign(col, None);
        for k in 0..n {
            out[k * n + j] = col[k];
        }
    }
    b.spend((n * n) as u64)?;
    Some((vals, out))
}

/// Householder reduction of the symmetric `v` to tridiagonal form (d the
/// diagonal, e the subdiagonal in e[1..]), `v` becoming the accumulated
/// orthogonal transformation (JAMA's tred2).
fn tred2(v: &mut [f64], d: &mut [f64], e: &mut [f64], n: usize, b: &mut Budget) -> Option<()> {
    for j in 0..n {
        d[j] = v[(n - 1) * n + j];
    }
    for i in (1..n).rev() {
        let mut scale = 0.0;
        let mut h = 0.0;
        for k in 0..i {
            scale += d[k].abs();
        }
        if scale == 0.0 {
            e[i] = d[i - 1];
            for j in 0..i {
                d[j] = v[(i - 1) * n + j];
                v[i * n + j] = 0.0;
                v[j * n + i] = 0.0;
            }
        } else {
            for k in 0..i {
                d[k] /= scale;
                h += d[k] * d[k];
            }
            let mut f = d[i - 1];
            let mut g = h.sqrt();
            if f > 0.0 {
                g = -g;
            }
            e[i] = scale * g;
            h -= f * g;
            d[i - 1] = f - g;
            for x in e.iter_mut().take(i) {
                *x = 0.0;
            }
            for j in 0..i {
                f = d[j];
                v[j * n + i] = f;
                g = e[j] + v[j * n + j] * f;
                for k in j + 1..i {
                    g += v[k * n + j] * d[k];
                    e[k] += v[k * n + j] * f;
                }
                e[j] = g;
            }
            f = 0.0;
            for j in 0..i {
                e[j] /= h;
                f += e[j] * d[j];
            }
            let hh = f / (h + h);
            for j in 0..i {
                e[j] -= hh * d[j];
            }
            for j in 0..i {
                f = d[j];
                g = e[j];
                for k in j..i {
                    v[k * n + j] -= f * e[k] + g * d[k];
                }
                d[j] = v[(i - 1) * n + j];
                v[i * n + j] = 0.0;
            }
        }
        d[i] = h;
        b.spend((2 * i * i) as u64 + 1)?;
    }
    for i in 0..n - 1 {
        v[(n - 1) * n + i] = v[i * n + i];
        v[i * n + i] = 1.0;
        let h = d[i + 1];
        if h != 0.0 {
            for k in 0..=i {
                d[k] = v[k * n + i + 1] / h;
            }
            for j in 0..=i {
                let mut g = 0.0;
                for k in 0..=i {
                    g += v[k * n + i + 1] * v[k * n + j];
                }
                for k in 0..=i {
                    v[k * n + j] -= g * d[k];
                }
            }
        }
        for k in 0..=i {
            v[k * n + i + 1] = 0.0;
        }
        b.spend((2 * (i + 1) * (i + 1)) as u64 + 1)?;
    }
    for j in 0..n {
        d[j] = v[(n - 1) * n + j];
        v[(n - 1) * n + j] = 0.0;
    }
    v[(n - 1) * n + n - 1] = 1.0;
    e[0] = 0.0;
    Some(())
}

/// The implicit QL iteration on the tridiagonal (d, e), rotating the
/// eigenvectors along; `vt` holds them as rows (JAMA's tql2 on V's
/// transpose, without its sort). At most 30 iterations per
/// eigenvalue: a matrix with NaNs leaves them in place instead of looping.
fn tql2(vt: &mut [f64], d: &mut [f64], e: &mut [f64], n: usize, b: &mut Budget) -> Option<()> {
    for i in 1..n {
        e[i - 1] = e[i];
    }
    e[n - 1] = 0.0;
    let mut f = 0.0f64;
    let mut tst1 = 0.0f64;
    let eps = f64::EPSILON;
    for l in 0..n {
        tst1 = tst1.max(d[l].abs() + e[l].abs());
        let mut m = l;
        while m < n - 1 {
            if e[m].abs() <= eps * tst1 {
                break;
            }
            m += 1;
        }
        if m > l {
            let mut iter = 0;
            loop {
                iter += 1;
                let mut g = d[l];
                let mut p = (d[l + 1] - g) / (2.0 * e[l]);
                let mut r = p.hypot(1.0);
                if p < 0.0 {
                    r = -r;
                }
                d[l] = e[l] / (p + r);
                d[l + 1] = e[l] * (p + r);
                let dl1 = d[l + 1];
                let mut h = g - d[l];
                for x in d.iter_mut().skip(l + 2) {
                    *x -= h;
                }
                f += h;
                p = d[m];
                let mut c = 1.0;
                let mut c2 = c;
                let mut c3 = c;
                let el1 = e[l + 1];
                let mut s = 0.0;
                let mut s2 = 0.0;
                for i in (l..m).rev() {
                    c3 = c2;
                    c2 = c;
                    s2 = s;
                    g = c * e[i];
                    h = c * p;
                    r = p.hypot(e[i]);
                    e[i + 1] = s * r;
                    s = e[i] / r;
                    c = p / r;
                    p = c * d[i] - s * g;
                    d[i + 1] = h + s * (c * g + s * d[i]);
                    let (lo, hi) = vt.split_at_mut((i + 1) * n);
                    let (ri, ri1) = (&mut lo[i * n..], &mut hi[..n]);
                    for k in 0..n {
                        let hk = ri1[k];
                        ri1[k] = s * ri[k] + c * hk;
                        ri[k] = c * ri[k] - s * hk;
                    }
                }
                p = -s * s2 * c3 * el1 * e[l] / dl1;
                e[l] = s * p;
                d[l] = c * p;
                b.spend(((m - l) * (n + 10)) as u64 + 1)?;
                if !(e[l].abs() > eps * tst1) || iter >= 30 {
                    break;
                }
            }
        }
        d[l] += f;
        e[l] = 0.0;
    }
    Some(())
}

// ---- SVD -----------------------------------------------------------------------------------

fn dot(a: &[f64], b: &[f64]) -> f64 {
    let mut s = 0.0;
    for i in 0..a.len() {
        s += a[i] * b[i];
    }
    s
}

/// The fallback's `_complete_basis`: extends orthonormal columns (each of
/// length m) to m, each time with the unit vector whose residual against
/// the basis so far is largest, orthogonalized twice.
fn complete_basis(cols: &[Vec<f64>], m: usize, b: &mut Budget) -> Option<Vec<Vec<f64>>> {
    let mut cols: Vec<Vec<f64>> = cols.to_vec();
    while cols.len() < m {
        let mut best: Vec<f64> = Vec::new();
        let mut best_norm = -1.0f64;
        for e in 0..m {
            let mut v = vec![0.0; m];
            v[e] = 1.0;
            for _ in 0..2 {
                for c in cols.iter() {
                    let d = dot(c, &v);
                    for i in 0..m {
                        v[i] -= d * c[i];
                    }
                }
            }
            let nrm = dot(&v, &v).sqrt();
            if nrm > best_norm {
                best_norm = nrm;
                best = v;
            }
            b.spend((4 * m * cols.len() + m) as u64 + 1)?;
        }
        cols.push(best.iter().map(|x| x / best_norm).collect());
    }
    Some(cols)
}

/// The fallback's `_svd_tall`: one-sided Jacobi on the n columns of the
/// m x n `a` (m >= n). Returns S descending, U's n columns and V's n
/// columns (each a Vec).
fn svd_tall(cols_in: Vec<Vec<f64>>, m: usize, n: usize, b: &mut Budget) -> Option<(Vec<f64>, Vec<Vec<f64>>, Vec<Vec<f64>>)> {
    let mut cols = cols_in;
    let mut v: Vec<Vec<f64>> = (0..n).map(|j| (0..n).map(|i| if i == j { 1.0 } else { 0.0 }).collect()).collect();
    for _sweep in 0..80 {
        let mut rotated = false;
        let mut nrm: Vec<f64> = cols.iter().map(|c| dot(c, c)).collect();
        for p in 0..n.saturating_sub(1) {
            let (lo, hi) = cols.split_at_mut(p + 1);
            let cp = &mut lo[p];
            let (vlo, vhi) = v.split_at_mut(p + 1);
            let vp = &mut vlo[p];
            for q in p + 1..n {
                let cq = &mut hi[q - p - 1];
                let mut c = 0.0;
                for i in 0..m {
                    c += cp[i] * cq[i];
                }
                if c == 0.0 {
                    continue;
                }
                let a_ = nrm[p];
                let b_ = nrm[q];
                if c.abs() <= 1e-16 * (a_ * b_).abs().sqrt() {
                    continue;
                }
                rotated = true;
                let zeta = (b_ - a_) / (2.0 * c);
                let t = if zeta.abs() > 1e150 { 0.5 / zeta } else { 1.0f64.copysign(zeta) / (zeta.abs() + (1.0 + zeta * zeta).sqrt()) };
                let cs = 1.0 / (1.0 + t * t).sqrt();
                let sn = cs * t;
                nrm[p] = a_ - t * c;
                nrm[q] = b_ + t * c;
                for i in 0..m {
                    let x = cp[i];
                    let y = cq[i];
                    cp[i] = cs * x - sn * y;
                    cq[i] = sn * x + cs * y;
                }
                let vq = &mut vhi[q - p - 1];
                for i in 0..n {
                    let x = vp[i];
                    let y = vq[i];
                    vp[i] = cs * x - sn * y;
                    vq[i] = sn * x + cs * y;
                }
                if nrm[p] < 0.01 * a_ {
                    nrm[p] = dot(cp, cp);
                }
                if nrm[q] < 0.01 * b_ {
                    nrm[q] = dot(cq, cq);
                }
            }
            b.spend(((n - p) * (3 * m + 2 * n)) as u64 + 1)?;
        }
        if !rotated {
            break;
        }
    }
    let sig: Vec<f64> = cols.iter().map(|c| dot(c, c).sqrt()).collect();
    let neg: Vec<f64> = sig.iter().map(|s| -s).collect();
    let order = stable_order(&neg);
    let s: Vec<f64> = order.iter().map(|&j| sig[j]).collect();
    let mut vc: Vec<Vec<f64>> = order.iter().map(|&j| v[j].clone()).collect();
    let smax = if s.is_empty() { 0.0 } else { s[0] };
    let tiny = smax * (m.max(n) as f64) * 2.220446049250313e-16;
    let mut u: Vec<Option<Vec<f64>>> = Vec::with_capacity(n);
    for (idx, &j) in order.iter().enumerate() {
        if s[idx] > tiny && s[idx] > 1e-300 {
            u.push(Some(cols[j].iter().map(|x| x / s[idx]).collect()));
        } else {
            u.push(None);
        }
    }
    let good: Vec<Vec<f64>> = u.iter().flatten().cloned().collect();
    if good.len() < n {
        let full = complete_basis(&good, m, b)?;
        let mut extra = full.into_iter().skip(good.len());
        for slot in u.iter_mut() {
            if slot.is_none() {
                *slot = extra.next();
            }
        }
    }
    let mut uc: Vec<Vec<f64>> = u.into_iter().map(|x| x.unwrap_or_default()).collect();
    for idx in 0..n {
        fix_sign(&mut vc[idx], Some(&mut uc[idx]));
    }
    b.spend((m * n + n * n) as u64 + 1)?;
    Some((s, uc, vc))
}

/// SVD of the m x n `a` (the fallback's `_svd`): U (m x ucols), S (k,
/// descending) and Vh (vrows x n), where ucols = m and vrows = n when
/// `full`, else both k = min(m, n).
pub(crate) fn svd(a: &[f64], m: usize, n: usize, full: bool, b: &mut Budget) -> Option<(Vec<f64>, Vec<f64>, Vec<f64>)> {
    let (s, mut ucols, mut vcols) = if m >= n {
        let cols: Vec<Vec<f64>> = (0..n).map(|j| (0..m).map(|i| a[i * n + j]).collect()).collect();
        svd_tall(cols, m, n, b)?
    } else {
        // The columns of a^T are a's rows.
        let cols: Vec<Vec<f64>> = (0..m).map(|i| a[i * n..i * n + n].to_vec()).collect();
        let (s, vc, uc) = svd_tall(cols, n, m, b)?;
        (s, uc, vc)
    };
    if full {
        ucols = complete_basis(&ucols, m, b)?;
        vcols = complete_basis(&vcols, n, b)?;
    }
    let uc = ucols.len();
    let mut u = vec![0.0; m * uc];
    for (j, col) in ucols.iter().enumerate() {
        for i in 0..m {
            u[i * uc + j] = col[i];
        }
    }
    let mut vh = Vec::with_capacity(vcols.len() * n);
    for row in vcols.iter() {
        vh.extend_from_slice(row);
    }
    Some((u, s, vh))
}

// ---- general eigenproblem ------------------------------------------------------------------

/// Eigenvalues and eigenvectors of a general real matrix.
pub(crate) struct Eig {
    /// Real parts of the eigenvalues, in Schur-diagonal order (a conjugate
    /// pair with the positive imaginary part first).
    pub(crate) wr: Vec<f64>,
    /// Imaginary parts.
    pub(crate) wi: Vec<f64>,
    /// Real parts of the eigenvectors: column j (row-major n x n) is the
    /// eigenvector of eigenvalue j.
    pub(crate) vr: Vec<f64>,
    /// Imaginary parts of the eigenvectors.
    pub(crate) vi: Vec<f64>,
}

/// Reduction of the n x n `h` to upper Hessenberg form by the fallback's
/// Householder reflections (`_hessenberg`), accumulating them into `q`
/// (A = Q H Q^T).
fn hessenberg(h: &mut [f64], q: &mut [f64], n: usize, b: &mut Budget) -> Option<()> {
    for i in 0..n {
        for j in 0..n {
            q[i * n + j] = if i == j { 1.0 } else { 0.0 };
        }
    }
    let mut v = vec![0.0; n];
    let mut acc = vec![0.0; n];
    let mut sums = Fsums::new(n);
    for k in 0..n.saturating_sub(2) {
        let r = n - k - 1;
        let mut ss = Fsum::new();
        for i in 1..r {
            let x = h[(k + 1 + i) * n + k];
            ss.add(x * x);
        }
        let xnorm = ss.value().sqrt();
        if xnorm == 0.0 {
            continue;
        }
        let alpha = h[(k + 1) * n + k];
        let beta = -hypot(alpha, xnorm).copysign(alpha);
        v[0] = alpha - beta;
        for i in 1..r {
            v[i] = h[(k + 1 + i) * n + k];
        }
        let mut vv = Fsum::new();
        for &t in v[..r].iter() {
            vv.add(t * t);
        }
        let f = 2.0 / vv.value();
        // Left: rows k+1.. of h, w_c = f * fsum_i(v_i h[k+1+i][c]), every
        // column summing in row order.
        sums.clear();
        for i in 0..r {
            sums.add_scaled(0, v[i], &h[(k + 1 + i) * n..(k + 2 + i) * n]);
        }
        for c in 0..n {
            acc[c] = f * sums.s[c];
        }
        for i in 0..r {
            let vi = v[i];
            let row = &mut h[(k + 1 + i) * n..(k + 2 + i) * n];
            for (x, &w) in row.iter_mut().zip(&acc) {
                if w != 0.0 {
                    *x -= w * vi;
                }
            }
        }
        // Right: every row of h, then of q.
        for mat in [&mut *h, &mut *q] {
            for row in mat.chunks_exact_mut(n) {
                let seg = &mut row[k + 1..];
                let mut sum = Fsum::new();
                for i in 0..r {
                    sum.add(seg[i] * v[i]);
                }
                let w = f * sum.value();
                if w != 0.0 {
                    for i in 0..r {
                        seg[i] -= w * v[i];
                    }
                }
            }
        }
        for i in k + 2..n {
            h[i * n + k] = 0.0;
        }
        b.spend((5 * r * n) as u64 + 1)?;
    }
    Some(())
}

fn sgn(a: f64, b: f64) -> f64 {
    if b >= 0.0 {
        a.abs()
    } else {
        -a.abs()
    }
}

/// (ar + i ai) / (br + i bi), scaled (Smith).
fn cdiv(ar: f64, ai: f64, br: f64, bi: f64) -> (f64, f64) {
    if br.abs() > bi.abs() {
        let r = bi / br;
        let d = br + r * bi;
        ((ar + r * ai) / d, (ai - r * ar) / d)
    } else {
        let r = br / bi;
        let d = bi + r * br;
        ((r * ar + ai) / d, (r * ai - ar) / d)
    }
}

/// Eigen-decomposition of the n x n `a`: Hessenberg reduction, the
/// fallback's Francis double-shift QR (EISPACK hqr, extended to the full
/// Schur form and its vectors as hqr2), eigenvectors by back-substitution.
/// `Err(())`: the iteration did not converge (60 iterations on one
/// eigenvalue).
pub(crate) fn eig(a: &[f64], n: usize, b: &mut Budget) -> Option<Result<Eig, ()>> {
    let mut hm = a[..n * n].to_vec();
    let mut qm = vec![0.0; n * n];
    hessenberg(&mut hm, &mut qm, n, b)?;
    // 1-based working copies, as the EISPACK code indexes them.
    let w1 = n + 1;
    let mut h = vec![0.0; w1 * w1];
    let mut z = vec![0.0; w1 * w1];
    for i in 0..n {
        for j in 0..n {
            h[(i + 1) * w1 + j + 1] = hm[i * n + j];
            z[(i + 1) * w1 + j + 1] = qm[i * n + j];
        }
    }
    let at = |i: usize, j: usize| i * w1 + j;
    let mut wr = vec![0.0; w1];
    let mut wi = vec![0.0; w1];
    let mut anorm = 0.0f64;
    for i in 1..=n {
        for j in (i.max(2) - 1)..=n {
            anorm += h[at(i, j)].abs();
        }
    }
    let mut nn = n;
    let mut t = 0.0f64;
    let (mut p, mut q, mut r): (f64, f64, f64);
    let (mut x, mut y, mut zz, mut w);
    while nn >= 1 {
        let mut its = 0;
        loop {
            let mut l = nn;
            while l >= 2 {
                let mut s = h[at(l - 1, l - 1)].abs() + h[at(l, l)].abs();
                if s == 0.0 {
                    s = anorm;
                }
                if h[at(l, l - 1)].abs() + s == s {
                    h[at(l, l - 1)] = 0.0;
                    break;
                }
                l -= 1;
            }
            if l < 1 {
                l = 1;
            }
            x = h[at(nn, nn)];
            if l == nn {
                h[at(nn, nn)] = x + t;
                wr[nn] = x + t;
                wi[nn] = 0.0;
                nn -= 1;
            } else {
                y = h[at(nn - 1, nn - 1)];
                w = h[at(nn, nn - 1)] * h[at(nn - 1, nn)];
                if l == nn - 1 {
                    p = 0.5 * (y - x);
                    q = p * p + w;
                    zz = q.abs().sqrt();
                    x += t;
                    h[at(nn, nn)] = x;
                    h[at(nn - 1, nn - 1)] = y + t;
                    if q >= 0.0 {
                        zz = p + sgn(zz, p);
                        wr[nn - 1] = x + zz;
                        wr[nn] = x + zz;
                        if zz != 0.0 {
                            wr[nn] = x - w / zz;
                        }
                        wi[nn - 1] = 0.0;
                        wi[nn] = 0.0;
                        // Triangularize the 2x2 block (hqr2).
                        let xs = h[at(nn, nn - 1)];
                        let s = xs.abs() + zz.abs();
                        let mut pp = xs / s;
                        let mut qq = zz / s;
                        let rr = (pp * pp + qq * qq).sqrt();
                        pp /= rr;
                        qq /= rr;
                        for j in nn - 1..=n {
                            let v = h[at(nn - 1, j)];
                            h[at(nn - 1, j)] = qq * v + pp * h[at(nn, j)];
                            h[at(nn, j)] = qq * h[at(nn, j)] - pp * v;
                        }
                        for i in 1..=nn {
                            let v = h[at(i, nn - 1)];
                            h[at(i, nn - 1)] = qq * v + pp * h[at(i, nn)];
                            h[at(i, nn)] = qq * h[at(i, nn)] - pp * v;
                        }
                        for i in 1..=n {
                            let v = z[at(i, nn - 1)];
                            z[at(i, nn - 1)] = qq * v + pp * z[at(i, nn)];
                            z[at(i, nn)] = qq * z[at(i, nn)] - pp * v;
                        }
                    } else {
                        wr[nn - 1] = x + p;
                        wr[nn] = x + p;
                        wi[nn - 1] = zz;
                        wi[nn] = -zz;
                    }
                    nn -= 2;
                } else {
                    if its == 60 {
                        return Some(Err(()));
                    }
                    if its == 10 || its == 20 {
                        t += x;
                        for i in 1..=nn {
                            h[at(i, i)] -= x;
                        }
                        let s = h[at(nn, nn - 1)].abs() + h[at(nn - 1, nn - 2)].abs();
                        x = 0.75 * s;
                        y = x;
                        w = -0.4375 * s * s;
                    }
                    its += 1;
                    let mut m = nn - 2;
                    let mut zm;
                    loop {
                        zm = h[at(m, m)];
                        r = x - zm;
                        let s0 = y - zm;
                        p = (r * s0 - w) / h[at(m + 1, m)] + h[at(m, m + 1)];
                        q = h[at(m + 1, m + 1)] - zm - r - s0;
                        r = h[at(m + 2, m + 1)];
                        let s = p.abs() + q.abs() + r.abs();
                        p /= s;
                        q /= s;
                        r /= s;
                        if m == l {
                            break;
                        }
                        let u = h[at(m, m - 1)].abs() * (q.abs() + r.abs());
                        let v = p.abs() * (h[at(m - 1, m - 1)].abs() + zm.abs() + h[at(m + 1, m + 1)].abs());
                        if u + v == v {
                            break;
                        }
                        m -= 1;
                    }
                    for i in m + 2..=nn {
                        h[at(i, i - 2)] = 0.0;
                        if i != m + 2 {
                            h[at(i, i - 3)] = 0.0;
                        }
                    }
                    let mut k = m;
                    while k < nn {
                        if k != m {
                            p = h[at(k, k - 1)];
                            q = h[at(k + 1, k - 1)];
                            r = 0.0;
                            if k != nn - 1 {
                                r = h[at(k + 2, k - 1)];
                            }
                            x = p.abs() + q.abs() + r.abs();
                            if x != 0.0 {
                                p /= x;
                                q /= x;
                                r /= x;
                            }
                        }
                        let s = sgn((p * p + q * q + r * r).sqrt(), p);
                        if s != 0.0 {
                            if k == m {
                                if l != m {
                                    h[at(k, k - 1)] = -h[at(k, k - 1)];
                                }
                            } else {
                                h[at(k, k - 1)] = -s * x;
                            }
                            p += s;
                            x = p / s;
                            y = q / s;
                            zm = r / s;
                            q /= p;
                            r /= p;
                            for j in k..=n {
                                let mut pv = h[at(k, j)] + q * h[at(k + 1, j)];
                                if k != nn - 1 {
                                    pv += r * h[at(k + 2, j)];
                                    h[at(k + 2, j)] -= pv * zm;
                                }
                                h[at(k + 1, j)] -= pv * y;
                                h[at(k, j)] -= pv * x;
                            }
                            let mmin = if nn < k + 3 { nn } else { k + 3 };
                            for i in 1..=mmin {
                                let mut pv = x * h[at(i, k)] + y * h[at(i, k + 1)];
                                if k != nn - 1 {
                                    pv += zm * h[at(i, k + 2)];
                                    h[at(i, k + 2)] -= pv * r;
                                }
                                h[at(i, k + 1)] -= pv * q;
                                h[at(i, k)] -= pv;
                            }
                            for i in 1..=n {
                                let mut pv = x * z[at(i, k)] + y * z[at(i, k + 1)];
                                if k != nn - 1 {
                                    pv += zm * z[at(i, k + 2)];
                                    z[at(i, k + 2)] -= pv * r;
                                }
                                z[at(i, k + 1)] -= pv * q;
                                z[at(i, k)] -= pv;
                            }
                        }
                        k += 1;
                    }
                    b.spend((12 * n * (nn - m + 1)) as u64 + 1)?;
                }
            }
            if nn < 1 || l + 1 >= nn {
                break;
            }
        }
    }
    // Back-substitution for the eigenvectors of the quasi-triangular h.
    if anorm != 0.0 {
        let eps = f64::EPSILON;
        for en in (1..=n).rev() {
            let p = wr[en];
            let q = wi[en];
            let na = en.wrapping_sub(1);
            if q == 0.0 {
                let mut m = en;
                h[at(en, en)] = 1.0;
                let (mut zs, mut ss) = (0.0f64, 0.0f64);
                for i in (1..en).rev() {
                    let w = h[at(i, i)] - p;
                    let mut r = 0.0;
                    for j in m..=en {
                        r += h[at(i, j)] * h[at(j, en)];
                    }
                    if wi[i] < 0.0 {
                        zs = w;
                        ss = r;
                        continue;
                    }
                    m = i;
                    if wi[i] == 0.0 {
                        let mut t = w;
                        if t == 0.0 {
                            t = eps * anorm;
                        }
                        h[at(i, en)] = -r / t;
                    } else {
                        let x = h[at(i, i + 1)];
                        let y = h[at(i + 1, i)];
                        let qq = (wr[i] - p) * (wr[i] - p) + wi[i] * wi[i];
                        let t = (x * ss - zs * r) / qq;
                        h[at(i, en)] = t;
                        h[at(i + 1, en)] = if x.abs() > zs.abs() { (-r - w * t) / x } else { (-ss - y * t) / zs };
                    }
                    let t = h[at(i, en)].abs();
                    if eps * t * t > 1.0 {
                        for j in i..=en {
                            h[at(j, en)] /= t;
                        }
                    }
                }
            } else if q < 0.0 {
                // The second of a pair: its vector is column na + i column en.
                let mut m = na;
                if h[at(en, na)].abs() > h[at(na, en)].abs() {
                    h[at(na, na)] = q / h[at(en, na)];
                    h[at(na, en)] = -(h[at(en, en)] - p) / h[at(en, na)];
                } else {
                    let (cr, ci) = cdiv(0.0, -h[at(na, en)], h[at(na, na)] - p, q);
                    h[at(na, na)] = cr;
                    h[at(na, en)] = ci;
                }
                h[at(en, na)] = 0.0;
                h[at(en, en)] = 1.0;
                let (mut zs, mut rs, mut ss) = (0.0f64, 0.0f64, 0.0f64);
                for i in (1..na).rev() {
                    let w = h[at(i, i)] - p;
                    let mut ra = 0.0;
                    let mut sa = 0.0;
                    for j in m..=en {
                        ra += h[at(i, j)] * h[at(j, na)];
                        sa += h[at(i, j)] * h[at(j, en)];
                    }
                    if wi[i] < 0.0 {
                        zs = w;
                        rs = ra;
                        ss = sa;
                        continue;
                    }
                    m = i;
                    if wi[i] == 0.0 {
                        let (cr, ci) = cdiv(-ra, -sa, w, q);
                        h[at(i, na)] = cr;
                        h[at(i, en)] = ci;
                    } else {
                        let x = h[at(i, i + 1)];
                        let y = h[at(i + 1, i)];
                        let mut vr = (wr[i] - p) * (wr[i] - p) + wi[i] * wi[i] - q * q;
                        let vi = (wr[i] - p) * 2.0 * q;
                        if vr == 0.0 && vi == 0.0 {
                            vr = eps * anorm * (w.abs() + q.abs() + x.abs() + y.abs() + zs.abs());
                        }
                        let (cr, ci) = cdiv(x * rs - zs * ra + q * sa, x * ss - zs * sa - q * ra, vr, vi);
                        h[at(i, na)] = cr;
                        h[at(i, en)] = ci;
                        if x.abs() > zs.abs() + q.abs() {
                            h[at(i + 1, na)] = (-ra - w * h[at(i, na)] + q * h[at(i, en)]) / x;
                            h[at(i + 1, en)] = (-sa - w * h[at(i, en)] - q * h[at(i, na)]) / x;
                        } else {
                            let (cr, ci) = cdiv(-rs - y * h[at(i, na)], -ss - y * h[at(i, en)], zs, q);
                            h[at(i + 1, na)] = cr;
                            h[at(i + 1, en)] = ci;
                        }
                    }
                    let t = h[at(i, na)].abs().max(h[at(i, en)].abs());
                    if eps * t * t > 1.0 {
                        for j in i..=en {
                            h[at(j, na)] /= t;
                            h[at(j, en)] /= t;
                        }
                    }
                }
            }
            b.spend((en * en) as u64 + 1)?;
        }
        // Back to the original basis: z = z * (the triangular vectors).
        let mut col = vec![0.0; w1];
        for j in (1..=n).rev() {
            for i in 1..=n {
                let mut s = 0.0;
                for k in 1..=j {
                    s += z[at(i, k)] * h[at(k, j)];
                }
                col[i] = s;
            }
            for i in 1..=n {
                z[at(i, j)] = col[i];
            }
            b.spend((n * j) as u64 + 1)?;
        }
    }
    // Normalize: unit norm; a complex vector turned so its largest-modulus
    // component is real and positive; a real one gets the sign rule.
    let mut vr = vec![0.0; n * n];
    let mut vi = vec![0.0; n * n];
    let mut j = 1;
    while j <= n {
        if wi[j] == 0.0 || j == n {
            let mut v: Vec<f64> = (1..=n).map(|i| z[at(i, j)]).collect();
            let nrm = dot(&v, &v).sqrt();
            if nrm > 0.0 {
                v.iter_mut().for_each(|x| *x /= nrm);
            }
            fix_sign(&mut v, None);
            for i in 0..n {
                vr[i * n + j - 1] = v[i];
            }
            j += 1;
        } else {
            let re: Vec<f64> = (1..=n).map(|i| z[at(i, j)]).collect();
            let im: Vec<f64> = (1..=n).map(|i| z[at(i, j + 1)]).collect();
            let nrm = (dot(&re, &re) + dot(&im, &im)).sqrt();
            let mut best = 0.0f64;
            let mut k = 0usize;
            for i in 0..n {
                let md = re[i].hypot(im[i]);
                if md > best + 1e-12 * best {
                    best = md;
                    k = i;
                }
            }
            // Multiply by conj(v_k) / (|v_k| * nrm).
            let (cr, ci) = if best > 0.0 && nrm > 0.0 { (re[k] / best / nrm, -im[k] / best / nrm) } else { (1.0, 0.0) };
            for i in 0..n {
                let a_ = re[i] * cr - im[i] * ci;
                let b_ = re[i] * ci + im[i] * cr;
                vr[i * n + j - 1] = a_;
                vi[i * n + j - 1] = b_;
                vr[i * n + j] = a_;
                vi[i * n + j] = -b_;
            }
            vi[k * n + j - 1] = 0.0;
            vi[k * n + j] = 0.0;
            j += 2;
        }
    }
    Some(Ok(Eig { wr: wr[1..].to_vec(), wi: wi[1..].to_vec(), vr, vi }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nopoll() -> impl FnMut() -> bool {
        || false
    }

    fn mat(m: usize, n: usize, seed: f64, bidx: usize) -> Vec<f64> {
        let mut v = Vec::new();
        for i in 0..m {
            for j in 0..n {
                v.push((1.3 * i as f64 + 0.7 * j as f64 + seed + 2.1 * bidx as f64).sin() + if i == j { 0.9 } else { 0.0 });
            }
        }
        v
    }

    fn matmul(a: &[f64], b: &[f64], m: usize, k: usize, n: usize) -> Vec<f64> {
        let mut o = vec![0.0; m * n];
        for i in 0..m {
            for p in 0..k {
                for j in 0..n {
                    o[i * n + j] += a[i * k + p] * b[p * n + j];
                }
            }
        }
        o
    }

    fn tr(a: &[f64], m: usize, n: usize) -> Vec<f64> {
        let mut o = vec![0.0; m * n];
        for i in 0..m {
            for j in 0..n {
                o[j * m + i] = a[i * n + j];
            }
        }
        o
    }

    fn maxdiff(a: &[f64], b: &[f64]) -> f64 {
        assert_eq!(a.len(), b.len());
        a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max)
    }

    fn eye(n: usize) -> Vec<f64> {
        let mut o = vec![0.0; n * n];
        for i in 0..n {
            o[i * n + i] = 1.0;
        }
        o
    }

    #[test]
    fn lu_and_solves() {
        let mut p = nopoll();
        let mut b = Budget::new(&mut p);
        for (m, n) in [(4, 4), (5, 3), (3, 5), (1, 1), (7, 7)] {
            let a = mat(m, n, 0.3, 0);
            let f = lu(&a, m, n, &mut b).unwrap();
            let k = m.min(n);
            let mut l = vec![0.0; m * k];
            let mut u = vec![0.0; k * n];
            for i in 0..m {
                for j in 0..k {
                    l[i * k + j] = if j < i { f.lu[i * n + j] } else if i == j { 1.0 } else { 0.0 };
                }
            }
            for i in 0..k {
                for j in i..n {
                    u[i * n + j] = f.lu[i * n + j];
                }
            }
            let lu_ = matmul(&l, &u, m, k, n);
            let mut pa = vec![0.0; m * n];
            for i in 0..m {
                pa[i * n..i * n + n].copy_from_slice(&a[f.perm[i] * n..f.perm[i] * n + n]);
            }
            assert!(maxdiff(&lu_, &pa) < 1e-13);
            assert_eq!(f.info, 0);
        }
        let a = mat(6, 6, 1.1, 1);
        let f = lu(&a, 6, 6, &mut b).unwrap();
        let inv = lu_solve(&f.lu, &f.perm, 6, &eye(6), 6, &mut b).unwrap();
        assert!(maxdiff(&matmul(&a, &inv, 6, 6, 6), &eye(6)) < 1e-12);
        let sing = [1.0, 2.0, 2.0, 4.0];
        let f = lu(&sing, 2, 2, &mut b).unwrap();
        assert_eq!(f.info, 2);
        assert_eq!(f.pivots, vec![2, 2]);
        assert_eq!(f.sign, -1.0);
        let x = lu_solve(&f.lu, &f.perm, 2, &[1.0, 1.0], 1, &mut b).unwrap();
        assert!(x.iter().any(|v| !v.is_finite()));
        // Triangular solves.
        let t = mat(5, 5, 0.8, 0);
        let rhs = mat(5, 2, 0.4, 0);
        for upper in [false, true] {
            for unit in [false, true] {
                let x = tri_solve(&t, 5, &rhs, 2, upper, unit, &mut b).unwrap();
                let mut tt = vec![0.0; 25];
                for i in 0..5 {
                    for j in 0..5 {
                        let keep = if upper { j >= i } else { j <= i };
                        tt[i * 5 + j] = if i == j && unit { 1.0 } else if keep { t[i * 5 + j] } else { 0.0 };
                    }
                }
                assert!(maxdiff(&matmul(&tt, &x, 5, 5, 2), &rhs) < 1e-12);
            }
        }
        assert!(b.spent() > 0);
    }

    #[test]
    fn cholesky_ok_and_failure_layout() {
        let mut p = nopoll();
        let mut b = Budget::new(&mut p);
        let a = mat(5, 5, 0.5, 0);
        let mut s = matmul(&a, &tr(&a, 5, 5), 5, 5, 5);
        for i in 0..5 {
            s[i * 5 + i] += 5.0;
        }
        let (l, info) = cholesky(&s, 5, &mut b).unwrap();
        assert_eq!(info, 0);
        assert!(maxdiff(&matmul(&l, &tr(&l, 5, 5), 5, 5, 5), &s) < 1e-12);
        let bad = [4.0, 2.0, 1.0, 2.0, 1.0, 3.0, 1.0, 3.0, 2.0];
        let (l, info) = cholesky(&bad, 3, &mut b).unwrap();
        assert_eq!(info, 2);
        assert_eq!(l[0], 2.0);
        assert_eq!(l[3], 1.0);
        assert_eq!(l[4], 0.0);
        assert_eq!(&l[6..9], &[0.5, 3.0, 2.0]);
        let (l, info) = cholesky(&[], 0, &mut b).unwrap();
        assert!(l.is_empty() && info == 0);
    }

    #[test]
    fn qr_reconstructs_with_lapack_signs() {
        let mut p = nopoll();
        let mut b = Budget::new(&mut p);
        for (m, n) in [(5, 3), (3, 5), (4, 4), (1, 3), (3, 1)] {
            let a = mat(m, n, 2.0, 0);
            let (w, taus) = householder(&a, m, n, &mut b).unwrap();
            let k = m.min(n);
            for cols in [k, m] {
                let q = form_q(&w, &taus, m, n, cols, &mut b).unwrap();
                let rows = cols;
                let mut r = vec![0.0; rows * n];
                for i in 0..rows.min(m) {
                    for j in i..n {
                        r[i * n + j] = w[i * n + j];
                    }
                }
                assert!(maxdiff(&matmul(&q, &r, m, cols, n), &a) < 1e-12, "{m}x{n}");
                let qtq = matmul(&tr(&q, m, cols), &q, cols, m, cols);
                assert!(maxdiff(&qtq, &eye(cols)) < 1e-12);
            }
            // geqrf's beta is -sign(alpha) * norm: R's diagonal signs.
            let d0 = w[0];
            assert!(d0 * a[0] < 0.0 || taus[0] == 0.0);
        }
    }

    #[test]
    fn eigh_ascending_with_sign_rule() {
        let mut p = nopoll();
        let mut b = Budget::new(&mut p);
        for n in [1usize, 2, 5, 12] {
            let a = mat(n, n, 1.1, 0);
            let s: Vec<f64> = (0..n * n).map(|k| a[k] + a[(k % n) * n + k / n]).collect();
            let (w, v) = eigh(&s, n, &mut b).unwrap();
            for i in 1..n {
                assert!(w[i - 1] <= w[i]);
            }
            let mut vd = v.clone();
            for i in 0..n {
                for j in 0..n {
                    vd[i * n + j] *= w[j];
                }
            }
            assert!(maxdiff(&matmul(&s, &v, n, n, n), &vd) < 1e-12);
            assert!(maxdiff(&matmul(&tr(&v, n, n), &v, n, n, n), &eye(n)) < 1e-12);
            for j in 0..n {
                let col: Vec<f64> = (0..n).map(|i| v[i * n + j]).collect();
                let mut c2 = col.clone();
                fix_sign(&mut c2, None);
                assert_eq!(col, c2);
            }
        }
        let (w, v) = eigh(&[], 0, &mut b).unwrap();
        assert!(w.is_empty() && v.is_empty());
        // A repeated eigenvalue.
        let (w, v) = eigh(&eye(3), 3, &mut b).unwrap();
        assert_eq!(w, vec![1.0, 1.0, 1.0]);
        assert!(maxdiff(&matmul(&tr(&v, 3, 3), &v, 3, 3, 3), &eye(3)) < 1e-15);
    }

    fn check_svd(a: &[f64], m: usize, n: usize, full: bool, b: &mut Budget) {
        let (u, s, vh) = svd(a, m, n, full, b).unwrap();
        let k = m.min(n);
        let (uc, vr) = if full { (m, n) } else { (k, k) };
        assert_eq!(u.len(), m * uc);
        assert_eq!(s.len(), k);
        assert_eq!(vh.len(), vr * n);
        for i in 1..k {
            assert!(s[i - 1] >= s[i]);
        }
        let mut us = vec![0.0; m * k];
        for i in 0..m {
            for j in 0..k {
                us[i * k + j] = u[i * uc + j] * s[j];
            }
        }
        let rec = matmul(&us, &vh[..k * n], m, k, n);
        assert!(maxdiff(&rec, a) < 1e-12, "{m}x{n} full={full}");
        assert!(maxdiff(&matmul(&tr(&u, m, uc), &u, uc, m, uc), &eye(uc)) < 1e-12);
        assert!(maxdiff(&matmul(&vh, &tr(&vh, vr, n), vr, n, vr), &eye(vr)) < 1e-12);
    }

    #[test]
    fn svd_shapes_rank_deficient_and_full() {
        let mut p = nopoll();
        let mut b = Budget::new(&mut p);
        for (m, n) in [(5, 3), (3, 5), (4, 4), (1, 1), (6, 1), (1, 6)] {
            let a = mat(m, n, 0.8, 0);
            check_svd(&a, m, n, false, &mut b);
            check_svd(&a, m, n, true, &mut b);
        }
        let ones = vec![1.0; 15];
        check_svd(&ones, 5, 3, false, &mut b);
        check_svd(&ones, 5, 3, true, &mut b);
        check_svd(&ones, 3, 5, true, &mut b);
        let zeros = vec![0.0; 12];
        check_svd(&zeros, 4, 3, true, &mut b);
        let (u, s, vh) = svd(&[], 0, 3, false, &mut b).unwrap();
        assert!(u.is_empty() && s.is_empty() && vh.is_empty());
        let (u, s, vh) = svd(&[], 0, 3, true, &mut b).unwrap();
        assert!(u.is_empty() && s.is_empty());
        assert_eq!(vh.len(), 9);
    }

    fn check_eig(a: &[f64], n: usize, b: &mut Budget) -> Eig {
        let e = eig(a, n, b).unwrap().unwrap();
        let scale = a.iter().fold(1.0f64, |m, x| m.max(x.abs()));
        for j in 0..n {
            // A v = lambda v, in complex arithmetic.
            let (lr, li) = (e.wr[j], e.wi[j]);
            let mut nrm = 0.0;
            let mut best = 0.0f64;
            for i in 0..n {
                let (vr, vi) = (e.vr[i * n + j], e.vi[i * n + j]);
                nrm += vr * vr + vi * vi;
                best = best.max(vr.hypot(vi));
                let mut ar = 0.0;
                let mut ai = 0.0;
                for k in 0..n {
                    ar += a[i * n + k] * e.vr[k * n + j];
                    ai += a[i * n + k] * e.vi[k * n + j];
                }
                let rr = lr * vr - li * vi;
                let ri = lr * vi + li * vr;
                assert!((ar - rr).abs() < 1e-11 * scale && (ai - ri).abs() < 1e-11 * scale, "residual col {j}");
            }
            assert!((nrm - 1.0).abs() < 1e-13);
            if li != 0.0 {
                let k = (0..n).find(|&i| e.vr[i * n + j].hypot(e.vi[i * n + j]) >= best * (1.0 - 1e-12)).unwrap();
                assert_eq!(e.vi[k * n + j], 0.0);
                assert!(e.vr[k * n + j] > 0.0);
            }
        }
        let mut j = 0;
        while j < n {
            if e.wi[j] != 0.0 {
                assert!(e.wi[j] > 0.0 && e.wi[j + 1] == -e.wi[j] && e.wr[j] == e.wr[j + 1]);
                for i in 0..n {
                    assert_eq!(e.vr[i * n + j], e.vr[i * n + j + 1]);
                    assert_eq!(e.vi[i * n + j], -e.vi[i * n + j + 1]);
                }
                j += 2;
            } else {
                j += 1;
            }
        }
        e
    }

    #[test]
    fn eig_real_and_complex_spectra() {
        let mut p = nopoll();
        let mut b = Budget::new(&mut p);
        let rot = [0.0, -1.0, 1.0, 0.0];
        let e = check_eig(&rot, 2, &mut b);
        assert_eq!(e.wi, vec![1.0, -1.0]);
        // Two complex pairs.
        let m4 = [1.0, -2.0, 0.5, 0.0, 2.0, 1.0, 0.0, 0.3, 0.0, 0.1, -1.0, -3.0, 0.2, 0.0, 3.0, -1.0];
        let e = check_eig(&m4, 4, &mut b);
        assert_eq!(e.wi.iter().filter(|x| **x != 0.0).count(), 4);
        for n in [1usize, 3, 5, 8, 12] {
            let a = mat(n, n, 0.3, 0);
            check_eig(&a, n, &mut b);
        }
        // Symmetric: a real spectrum.
        let a = mat(6, 6, 1.7, 0);
        let s: Vec<f64> = (0..36).map(|k| a[k] + a[(k % 6) * 6 + k / 6]).collect();
        let e = check_eig(&s, 6, &mut b);
        assert!(e.wi.iter().all(|x| *x == 0.0));
        // Triangular with distinct diagonal.
        let t = [1.0, 2.0, 3.0, 0.0, 4.0, 5.0, 0.0, 0.0, 6.0];
        let e = check_eig(&t, 3, &mut b);
        let mut w = e.wr.clone();
        w.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(w, vec![1.0, 4.0, 6.0]);
        let e = eig(&[], 0, &mut b).unwrap().unwrap();
        assert!(e.wr.is_empty());
        let z = check_eig(&[0.0; 9], 3, &mut b);
        assert!(z.wr.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn poll_interrupts() {
        let mut calls = 0;
        let mut p = || {
            calls += 1;
            true
        };
        let mut b = Budget::new(&mut p);
        let a = mat(64, 64, 0.1, 0);
        assert!(svd(&a, 64, 64, false, &mut b).is_none());
    }

    #[test]
    fn bounds_cover_the_work() {
        let a = mat(20, 20, 0.2, 0);
        let s: Vec<f64> = (0..400).map(|k| a[k] + a[(k % 20) * 20 + k / 20]).collect();
        let runs: Vec<(Op, u64)> = {
            let mut out = Vec::new();
            let mut p = nopoll();
            let mut b = Budget::new(&mut p);
            lu(&a, 20, 20, &mut b).unwrap();
            out.push((Op::Lu, b.spent()));
            let mut p = nopoll();
            let mut b = Budget::new(&mut p);
            eigh(&s, 20, &mut b).unwrap();
            out.push((Op::Eigh, b.spent()));
            let mut p = nopoll();
            let mut b = Budget::new(&mut p);
            svd(&a, 20, 20, true, &mut b).unwrap();
            out.push((Op::Svd, b.spent()));
            let mut p = nopoll();
            let mut b = Budget::new(&mut p);
            let _ = eig(&a, 20, &mut b).unwrap();
            out.push((Op::Eig, b.spent()));
            let mut p = nopoll();
            let mut b = Budget::new(&mut p);
            let (w, t) = householder(&a, 20, 20, &mut b).unwrap();
            form_q(&w, &t, 20, 20, 20, &mut b).unwrap();
            out.push((Op::Qr, b.spent()));
            out
        };
        for (op, spent) in runs {
            assert!(spent <= bound(op, 20, 20), "{op:?} {spent}");
        }
    }

    #[test]
    #[ignore]
    fn timings() {
        use std::time::Instant;
        let mut p = nopoll();
        let mut b = Budget::new(&mut p);
        let t = |label: &str, f: &mut dyn FnMut()| {
            let s = Instant::now();
            f();
            println!("{label}: {:.2} ms", s.elapsed().as_secs_f64() * 1e3);
        };
        let a64 = mat(64, 64, 0.3, 0);
        let s64: Vec<f64> = (0..64 * 64).map(|k| a64[k] + a64[(k % 64) * 64 + k / 64]).collect();
        let a256 = mat(256, 256, 0.3, 0);
        let mut spd = matmul(&a256, &tr(&a256, 256, 256), 256, 256, 256);
        for i in 0..256 {
            spd[i * 256 + i] += 256.0;
        }
        t("svd 64", &mut || {
            svd(&a64, 64, 64, false, &mut b).unwrap();
        });
        t("svd 64 full", &mut || {
            svd(&a64, 64, 64, true, &mut b).unwrap();
        });
        t("eigh 64", &mut || {
            eigh(&s64, 64, &mut b).unwrap();
        });
        t("eig 64", &mut || {
            let _ = eig(&a64, 64, &mut b).unwrap();
        });
        t("lu+inverse 256", &mut || {
            let f = lu(&a256, 256, 256, &mut b).unwrap();
            lu_solve(&f.lu, &f.perm, 256, &eye(256), 256, &mut b).unwrap();
        });
        t("qr 256", &mut || {
            let (w, taus) = householder(&a256, 256, 256, &mut b).unwrap();
            form_q(&w, &taus, 256, 256, 256, &mut b).unwrap();
        });
        t("cholesky 256", &mut || {
            cholesky(&spd, 256, &mut b).unwrap();
        });
        t("tri_solve 256x256", &mut || {
            tri_solve(&spd, 256, &eye(256), 256, false, false, &mut b).unwrap();
        });
        let s256: Vec<f64> = (0..256 * 256).map(|k| a256[k] + a256[(k % 256) * 256 + k / 256]).collect();
        t("eigh 256", &mut || {
            eigh(&s256, 256, &mut b).unwrap();
        });
        t("svd 256", &mut || {
            svd(&a256, 256, 256, false, &mut b).unwrap();
        });
        t("eig 256", &mut || {
            let _ = eig(&a256, 256, &mut b).unwrap();
        });
    }

    /// Reads `oracle_in.txt` (lines "op m n v0 v1 ..."), writes
    /// `oracle_out.txt` for `oracle.py`.
    #[test]
    #[ignore]
    fn oracle() {
        let text = std::fs::read_to_string("oracle_in.txt").unwrap();
        let mut out = String::new();
        let mut p = nopoll();
        let mut b = Budget::new(&mut p);
        for line in text.lines() {
            let mut it = line.split_whitespace();
            let op = it.next().unwrap();
            let m: usize = it.next().unwrap().parse().unwrap();
            let n: usize = it.next().unwrap().parse().unwrap();
            let a: Vec<f64> = it.map(|x| x.parse().unwrap()).collect();
            let fmt = |v: &[f64]| v.iter().map(|x| format!("{x:e}")).collect::<Vec<_>>().join(" ");
            match op {
                "eig" => {
                    let e = eig(&a, n, &mut b).unwrap().unwrap();
                    out += &format!("{} | {} | {} | {}\n", fmt(&e.wr), fmt(&e.wi), fmt(&e.vr), fmt(&e.vi));
                }
                "eigh" => {
                    let (w, v) = eigh(&a, n, &mut b).unwrap();
                    out += &format!("{} | {}\n", fmt(&w), fmt(&v));
                }
                "lu" => {
                    let f = lu(&a, m, n, &mut b).unwrap();
                    let inv = if m == n { lu_solve(&f.lu, &f.perm, n, &a, n, &mut b).unwrap() } else { Vec::new() };
                    out += &format!("{} | {} | {}
", fmt(&f.lu), f.info, fmt(&inv));
                }
                "qr" => {
                    let (w, taus) = householder(&a, m, n, &mut b).unwrap();
                    let q = form_q(&w, &taus, m, n, m, &mut b).unwrap();
                    out += &format!("{} | {} | {}
", fmt(&w), fmt(&taus), fmt(&q));
                }
                "chol" => {
                    let (l, info) = cholesky(&a, n, &mut b).unwrap();
                    out += &format!("{} | {}
", fmt(&l), info);
                }
                _ => {
                    let (u, s, vh) = svd(&a, m, n, false, &mut b).unwrap();
                    out += &format!("{} | {} | {}\n", fmt(&u), fmt(&s), fmt(&vh));
                }
            }
        }
        std::fs::write("oracle_out.txt", out).unwrap();
    }
}
