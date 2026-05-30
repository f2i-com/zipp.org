// In-place insertion sort + recursive binary search on a fixed array — an
// imperative native algorithm (nested loops, array index/setindex, recursion).
// Uses only native-eligible features, so it runs on the interpreter, --jit, and
// --llvm.

// Sort `a` in place (ascending). Returns its length.
function sort(a: i64[]): i64 {
  const n = len(a);
  for (let i = 1; i < n; i = i + 1) {
    const key = a[i];
    let j = i - 1;
    while (j >= 0 && a[j] > key) {
      // `j >= 0` short-circuits, so a[-1] is never read
      a[j + 1] = a[j];
      j = j - 1;
    }
    a[j + 1] = key;
  }
  return n;
}

// Recursive binary search over the sorted range [lo, hi]; index of `target`, or -1.
function bsearch(a: i64[], lo: i64, hi: i64, target: i64): i64 {
  if (lo > hi) {
    return -1;
  }
  const mid = (lo + hi) / 2;
  if (a[mid] === target) {
    return mid;
  }
  if (a[mid] < target) {
    return bsearch(a, mid + 1, hi, target);
  }
  return bsearch(a, lo, mid - 1, target);
}

function main(): i64 {
  const a = [9, 3, 7, 1, 8, 2, 6, 5, 4, 0];
  sort(a); // a becomes 0,1,2,…,9
  for (let i = 0; i < len(a); i = i + 1) {
    console.log(a[i]);
  }
  const found = bsearch(a, 0, len(a) - 1, 7); // 7
  const missing = bsearch(a, 0, len(a) - 1, 99); // -1
  return a[0] * 100 + found * 10 + (missing + 1); // 0 + 70 + 0 = 70
}
