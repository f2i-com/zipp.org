//! The Python-visible hash and dict/set order that the native table
//! (`vm/py_table.rs`) reproduces, pinned: `hash()` of numbers, None, bools
//! and tuples of them against CPython 3.13 (str hashes are randomized in
//! CPython, so ZIPP's own str hash is checked against the native one by the
//! library's `py_table` tests), and set/dict iteration order as ZIPP's
//! runtime produces it today: CPython's order where the runtime's rule
//! (insertion order, small non-negative ints ascending) agrees with it, and
//! the runtime's bucket order where it does not (NaNs, big ints sharing a
//! small int's bucket). The stage that moves dicts and sets onto the native
//! table must keep every line.
#![cfg(feature = "python")]
use zipp_vm::frontend::{compile_source, Frontend, PythonMode};

fn execute(source: &str) -> Vec<String> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let mut result =
                compile_source(&source, Frontend::Python { mode: PythonMode::Module }).expect("compile");
            let state = result.state_mut();
            state.set_limits(200_000_000, None);
            state.run_init().expect("run");
            state.take_output().join("\n").lines().map(str::to_owned).collect()
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

const HASHES_PY: &str = r#"
vals = [0, 1, -1, -2, 2, 1023, 1024, -256, -257, 2**31, 2**53-1, 2**53, 2**53+1, -(2**53)-1,
        2**61-2, 2**61-1, 2**61, 2**62, 2**63, 2**64, -(2**64), 2**127-1, 2**127, -(2**127), 2**128,
        (2**61-1)*5, (2**61-1)*5+1, -((2**61-1)*3), 10**30, 3**100, -(7**90),
        0.0, -0.0, 1.0, -1.0, 1.5, -1.5, 0.1, 1e-300, 5e-324, -5e-324, 1e300, 1.7976931348623157e308,
        float('inf'), float('-inf'), 2.0**53, 2.0**61, 2.0**62, 1e22, 123456.789, -2.5e-8, 0.5, 2.0**-1014,
        True, False, None, (), (1,), (1, 2), (1.0, 2), (2**64, -1), (True, False), ((1, (2, None)), -1.5),
        (float('inf'), 0.5, (2**61-1,)),
]
for v in vals:
    print(repr(v), hash(v), sep='\t')
"#;

/// Printed by CPython 3.13 (`repr(v)`, tab, `hash(v)`); ZIPP matches every line.
const HASHES_CPYTHON: &str = "\
0\t0\n\
1\t1\n\
-1\t-2\n\
-2\t-2\n\
2\t2\n\
1023\t1023\n\
1024\t1024\n\
-256\t-256\n\
-257\t-257\n\
2147483648\t2147483648\n\
9007199254740991\t9007199254740991\n\
9007199254740992\t9007199254740992\n\
9007199254740993\t9007199254740993\n\
-9007199254740993\t-9007199254740993\n\
2305843009213693950\t2305843009213693950\n\
2305843009213693951\t0\n\
2305843009213693952\t1\n\
4611686018427387904\t2\n\
9223372036854775808\t4\n\
18446744073709551616\t8\n\
-18446744073709551616\t-8\n\
170141183460469231731687303715884105727\t31\n\
170141183460469231731687303715884105728\t32\n\
-170141183460469231731687303715884105728\t-32\n\
340282366920938463463374607431768211456\t64\n\
11529215046068469755\t0\n\
11529215046068469756\t1\n\
-6917529027641081853\t0\n\
1000000000000000000000000000000\t465258685558744706\n\
515377520732011331036461129765621272702107522001\t1175369268131054105\n\
-11450477594321044359340126713545146077054004823284978858214566372120240027249\t-1286943511913312073\n\
0.0\t0\n\
-0.0\t0\n\
1.0\t1\n\
-1.0\t-2\n\
1.5\t1152921504606846977\n\
-1.5\t-1152921504606846977\n\
0.1\t230584300921369408\n\
1e-300\t482449582752280463\n\
5e-324\t16777216\n\
-5e-324\t-16777216\n\
1e+300\t1224995262755759164\n\
1.7976931348623157e+308\t2234066890152476671\n\
inf\t314159\n\
-inf\t-314159\n\
9007199254740992.0\t9007199254740992\n\
2.305843009213694e+18\t1\n\
4.611686018427388e+18\t2\n\
1e+22\t1864712049423028464\n\
123456.789\t1819310134279660096\n\
-2.5e-08\t-789396629831109982\n\
0.5\t1152921504606846976\n\
5.696189077778436e-306\t8388608\n\
True\t1\n\
False\t0\n\
None\t4238894112\n\
()\t5740354900026072187\n\
(1,)\t-6644214454873602895\n\
(1, 2)\t-3550055125485641917\n\
(1.0, 2)\t-3550055125485641917\n\
(18446744073709551616, -1)\t4705214496270574563\n\
(True, False)\t-5164621852614943976\n\
((1, (2, None)), -1.5)\t-7285692056298788131\n\
(inf, 0.5, (2305843009213693951,))\t-727292456935941207\n\
";

/// Set and dict order; see the module comment.
const ORDER_PY: &str = r#"
nan = float('nan')
M = 2**61 - 1
cases = [
    {3, 1, 2}, {7, 3, 100}, {True, 2}, {2, 1.0}, {1.0, 2}, {9, 5, 3, 4, 31}, set(range(20, 0, -1)),
    {'b', 'a', 'c'}, {None, 0, ''}, {(1, 2), (0,), 1}, {nan, 1, nan}, {M + 1, 'a', 1}, {0, 5, M},
    {-1, -2}, {1, -1, 0}, {2**64, 8, 3}, set([5, 4, 3, 2, 1, 0]) | {100}, {4, 2} | {3, 1},
    {1, 2, 3, 4, 5} & {5, 3, 1}, {10, 20, 30} - {20}, {1, 'x'} ^ {'x', 2},
]
for s in cases:
    print(list(s))
s = {5, 0, M}; s.discard(0); s.add(0); print(list(s))
s = {0, 7, M}; s.discard(0); s.add(3); s.add(0); print(list(s))
s = set(); [s.add(x) for x in (40, 30, 20, 10)]; print(s.pop(), list(s))
d = {}; d['b'] = 1; d['a'] = 2; d[1] = 3; d[1.0] = 4; del d['b']; d['b'] = 5; print(list(d.items()))
print(list(frozenset([3, 1, 2])), list(set(frozenset({'q', 'p'}))))
"#;

/// ZIPP's runtime today. Lines 2, 4, 8, 10, 11, 15, 22-24 and 26 differ from
/// CPython's (hash-table) order; the rest agree.
const ORDER_ZIPP: &str = "\
[1, 2, 3]\n\
[7, 3, 100]\n\
[True, 2]\n\
[2, 1.0]\n\
[1.0, 2]\n\
[3, 4, 5, 9, 31]\n\
[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20]\n\
['b', 'a', 'c']\n\
[None, 0, '']\n\
[(1, 2), (0,), 1]\n\
[nan, nan, 1]\n\
[2305843009213693952, 1, 'a']\n\
[0, 2305843009213693951, 5]\n\
[-1, -2]\n\
[1, -1, 0]\n\
[18446744073709551616, 8, 3]\n\
[0, 1, 2, 3, 4, 5, 100]\n\
[1, 2, 3, 4]\n\
[1, 3, 5]\n\
[10, 30]\n\
[1, 2]\n\
[5, 2305843009213693951, 0]\n\
[2305843009213693951, 0, 7, 3]\n\
40 [30, 20, 10]\n\
[('a', 2), (1, 4), ('b', 5)]\n\
[1, 2, 3] ['q', 'p']\n\
";

#[test]
fn hash_of_numbers_none_bools_and_tuples_is_cpythons() {
    let got = execute(HASHES_PY);
    let want: Vec<&str> = HASHES_CPYTHON.lines().collect();
    assert_eq!(got, want);
}

#[test]
fn set_and_dict_order_is_pinned() {
    let got = execute(ORDER_PY);
    let want: Vec<&str> = ORDER_ZIPP.lines().collect();
    assert_eq!(got, want);
}
