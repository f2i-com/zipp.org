//! The command line runs Python on the VM JIT (the embedding API's Python
//! states do not). Every program here must print exactly what the
//! interpreter prints (`ZIPP_PY_JIT=0`), with the JIT at its default
//! thresholds and with every body compiling at its first chance
//! (`ZIPP_JIT_THRESHOLD=1`), which drives the fused Python instructions'
//! inline paths, their out-of-line steps, the slow paths and the bails.
#![cfg(feature = "python")]
use std::{fs, path::PathBuf, process::Command};

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new(source: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "zipp-pyjit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("main.py"), source).unwrap();
        Self(path)
    }
    fn run(&self, env: &[(&str, &str)]) -> (bool, String, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_zipp"));
        cmd.current_dir(&self.0).args(["py", "main.py"]);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.env_remove("ZIPP_NOJIT");
        let out = cmd.output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Runs the program with the JIT off, checks it prints `expected` (CPython
/// 3.13's output for it), then asserts every JIT configuration reproduces
/// the interpreter's run exactly (status, stdout and stderr).
fn same_everywhere(source: &str, expected: &str) {
    let f = Fixture::new(source);
    let interp = f.run(&[("ZIPP_PY_JIT", "0")]);
    assert!(interp.0, "interpreter run failed: {}", interp.2);
    assert_eq!(interp.1.replace('\r', ""), expected, "the interpreter's output differs from CPython's");
    for env in [
        &[][..],
        &[("ZIPP_JIT_THRESHOLD", "1")][..],
        &[("ZIPP_JIT_THRESHOLD", "2"), ("ZIPP_GC_STRESS", "1")][..],
    ] {
        let jit = f.run(env);
        assert_eq!(jit, interp, "JIT run under {env:?} differs from the interpreter");
    }
}

#[test]
fn int_and_float_arithmetic() {
    same_everywhere(
        r#"
def ints(n):
    acc = 0
    x = -300
    big = 2 ** 70
    for i in range(n):
        x = x + 7
        acc += x * 3 - i // 5 + (i % 7) - (-i) // 3 + (-i) % 4
        acc ^= i & 255
        acc |= 1
        if x > 1100:
            x = -300
        big = big + i
        if i < 5 or i == n - 1:
            print(i, x, acc, big, x < 0, x <= 1023, x == 1023, x != -256)
    return acc, big

def floats(n):
    s = 0.0
    t = 1.0
    out = []
    for i in range(n):
        s = s + i * 0.5 - 1.25
        t = t * 1.0001
        u = s / (i + 1)
        if i % 17 == 0:
            out.append((s, t, u, s < t, s >= u, -0.0 * i, i / 4))
    print(out[:3], out[-1])
    print(0.0 * -1, -0.0 + 0.0, float("inf") - float("inf") != 0, float("nan") == float("nan"))
    zs = 0
    for i in range(30):
        try:
            zs += 1 / (i % 3)
        except ZeroDivisionError:
            zs -= 1
    print(zs)
    return s

def mixed(n):
    a = 1
    b = 2.5
    for i in range(n):
        a = a + 1
        b = b + a
        if a > b:
            print("never")
    return a, b, a * 2.0, 3 // 2, 3.0 // 2, -7 % 3, 7.5 % 2

print(ints(3000))
print(floats(2000))
print(mixed(500))
"#,
        EXPECTED_ARITHMETIC,
    );
}

#[test]
fn containers_attributes_and_their_failures() {
    same_everywhere(
        r#"
class P:
    def __init__(self, x, y):
        self.x = x
        self.y = y
    @property
    def s(self):
        return self.x + self.y

def run(n):
    p = P(0, 1)
    d = {"a": 1, "b": 2}
    l = [0] * 10
    t = (1, 2, 3)
    s = "hello"
    miss = 0
    tot = 0
    for i in range(n):
        p.x = p.x + p.y
        tot += p.s
        d["a"] = d["a"] + 1
        l[i % 10] = l[i % 10] + i
        tot += t[i % 3] + len(s) + ord(s[i % 5])
        try:
            tot += d["zz"]
        except KeyError:
            miss += 1
        try:
            tot += l[20]
        except IndexError:
            miss += 1
        if i % 50 == 0:
            p = P(i, -i)
        if i % 97 == 0:
            d = {"a": i}
        tot += [i, i + 1][1] + len([x for x in t])
    return tot, miss, p.x, d, l

print(run(2000))
"#,
        EXPECTED_CONTAINERS,
    );
}

#[test]
fn calls_recursion_generators_and_control_flow() {
    same_everywhere(
        r#"
def fib(n):
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)

def gen(n):
    for i in range(n):
        yield i * i

def kw(a, b=2, *args, c=3, **kw):
    return a + b + c + len(args) + len(kw)

class A:
    def m(self, v):
        return v + 1
class B(A):
    def m(self, v):
        return super().m(v) * 2

def loop(n):
    acc = 0
    b = B()
    for i in range(n):
        acc += b.m(i) + kw(i, c=i)
        if i % 3 == 0:
            continue
        for j in gen(3):
            acc += j
            if j > 1:
                break
        try:
            if i % 11 == 0:
                raise ValueError(i)
        except ValueError as e:
            acc -= e.args[0]
        finally:
            acc += 1
    return acc

print(fib(20), loop(1500), sum(gen(100)))
"#,
        EXPECTED_CALLS,
    );
}

/// Loops over generators (the generator step runs from compiled code),
/// `try`/`except`/`finally` inside a hot loop (normal and raising
/// iterations), exception construction and plain class instantiation.
#[test]
fn generators_exceptions_and_instances_in_loops() {
    same_everywhere(
        r#"class Err(Exception):
    pass

class P:
    def __init__(self, x, y):
        self.x = x
        self.y = y

class Q:
    pass

def squares(n):
    for i in range(n):
        yield i * i

def boom(n):
    for i in range(n):
        if i == n - 1:
            raise Err("gen", i)
        yield i

def tree(d):
    if d == 0:
        yield 1
        return
    for v in tree(d - 1):
        yield v
    for v in tree(d - 1):
        yield v + 1

def loops(n):
    acc = 0
    for v in squares(n):
        acc += v
    caught = 0
    for k in range(n):
        try:
            acc += k
            if k % 97 == 0:
                raise Err(k)
            if k % 89 == 0:
                {}["x"]
        except Err as e:
            caught += e.args[0]
        except KeyError:
            caught -= 1
        finally:
            acc -= 1
    pts = []
    for k in range(n):
        p = P(k, -k)
        q = Q()
        q.z = p.x + p.y + k
        pts.append(q.z)
    try:
        for v in boom(n):
            acc += v
    except Err as e:
        acc += e.args[1] * 1000
    t = 0
    for v in tree(9):
        t += v
    g = squares(n)
    first = [next(g) for _ in range(5)]
    return acc, caught, sum(pts), t, first

print(loops(3000))
print(loops(40))
"#,
        EXPECTED_GENERATORS,
    );
}

/// `finally` and `with` left abruptly from hot loops: `continue`, `break`
/// and `return` through a `finally`, a context manager swallowing an
/// exception, and raises from inside a `finally`. Compiled code leaves every
/// abrupt completion to the interpreter's routing; a loop with `continue` or
/// `break` inside a `try` (`JumpFinally`) stays interpreted, and the loop
/// raising from its `finally` compiles.
#[test]
fn abrupt_finally_paths() {
    same_everywhere(
        r#"class Err(Exception):
    pass

class Ctx:
    def __init__(self, log):
        self.log = log
    def __enter__(self):
        self.log.append(1)
        return self
    def __exit__(self, t, v, tb):
        self.log.append(2 if t is None else 3)
        return t is Err

def early(k):
    try:
        if k % 5 == 0:
            return k * 2
        return k
    finally:
        k += 1000

def abrupt(n):
    acc = 0
    log = []
    for k in range(n):
        try:
            if k % 7 == 0:
                continue
            if k == n - 3:
                break
            acc += early(k)
        finally:
            acc += 1
        with Ctx(log):
            if k % 13 == 0:
                raise Err(k)
            acc += 2
        try:
            try:
                if k % 17 == 0:
                    raise Err("inner")
            finally:
                if k % 34 == 0:
                    raise KeyError(k)
        except Err:
            acc += 3
        except KeyError as e:
            acc += e.args[0]
    return acc, len(log), sum(log)

def raise_in_finally(n):
    out = 0
    for k in range(n):
        try:
            try:
                out += k
            finally:
                if k == n - 1:
                    raise Err("last")
        except Err as e:
            out = -out
    return out

print(abrupt(3000), abrupt(50), raise_in_finally(2500))
"#,
        EXPECTED_FINALLY,
    );
}

/// Ints across the small (immediate) int range's edges (value.rs), in both
/// directions, through every inline arithmetic and comparison path of the
/// compiled loops (sums, differences, products, floor division and modulo
/// with every sign combination, bitwise operators, counters stepping over
/// the edge) against CPython's own answers.
#[test]
fn ints_across_the_immediate_range_edges() {
    same_everywhere(
        r#"LIM = 1 << 46
vals = [0, 1, -1, 2, -2, 3, -3, 7, -7, 1000, -1000, (1 << 32) + 5, -(1 << 32) - 5,
        LIM - 2, LIM - 1, LIM, LIM + 1, -LIM - 1, -LIM, -LIM + 1, (1 << 62) + 3, -(1 << 62) - 3,
        (1 << 70) + 9, -(1 << 70) - 9]

def table(n):
    acc = 0
    lines = []
    for r in range(n):
        for a in vals:
            for b in vals:
                s = a + b
                d = a - b
                p = a * b
                x = a & b
                o = a | b
                y = a ^ b
                if b != 0:
                    q = a // b
                    m = a % b
                else:
                    q = m = 0
                lt = a < b
                le = a <= b
                eq = a == b
                acc = (acc * 31 + s + d + (p % 1000003) + x + o + y + q + m + lt + le + eq) % (1 << 61)
                if r == 0 and (a in (LIM - 1, -LIM, 7) or b in (1, -1)):
                    lines.append((a, b, s, d, p, x, o, y, q, m, lt, le, eq))
    return acc, lines

def counters(n):
    i = LIM - n // 2
    total = 0
    while i < LIM + n // 2:
        total += i
        i += 1
    j = -LIM + n // 2
    while j > -LIM - n // 2:
        total -= j
        j -= 1
    k = 0
    for t in range(n):
        k += t * t
        k -= t // 3
        k ^= t
    return total, k

acc, lines = table(3)
print(acc)
for l in lines[:60]:
    print(*l)
print(counters(5000))
print(sum(range(LIM - 10, LIM + 10)), sum(range(-LIM - 10, -LIM + 10)))
print([LIM - 1 + i for i in range(3)], [-LIM + 1 - i for i in range(3)])
print(hash(LIM - 1) == hash(LIM - 1), {LIM - 1: 1, LIM: 2}[LIM], len({LIM - 1, LIM - 1, LIM}))
"#,
        EXPECTED_INT_EDGES,
    );
}

/// Attribute reads and writes on instances' layout-mode storage, which
/// compiled loops read and write inline (`codegen::py_attr_inline`), while
/// the class changes under them (a class attribute of the same name, a
/// property added and removed), an instance's storage turns into a table
/// (`del`), an instance changes class, young and old storage takes heap
/// values, and layouts grow.
#[test]
fn inline_attributes_follow_class_and_storage_changes() {
    same_everywhere(
        r#"class P:
    def __init__(self, x, y):
        self.x = x
        self.y = y

class Q(P):
    pass

class R:
    __slots__ = ()

def walk(ps, n):
    total = 0
    seven = property(lambda self: 7)
    for i in range(n):
        p = ps[i % len(ps)]
        p.x = p.x + p.y
        total += p.x
        if i == n // 2:
            # A class attribute of the same name: the instance's still wins.
            P.x = 1000
        if i == n // 3:
            # A property on the class: now it wins over the instance dict.
            P.y = seven
        if i == (2 * n) // 3:
            del P.y
    return total

def switch(n):
    a, b = P(1, 2), P(3, 4)
    got = []
    for i in range(n):
        o = a if i % 2 else b
        o.x = o.x * 2 % 1000003 + i
        if i == n // 2:
            del a.y            # a's storage turns into a table
            a.y = [i]          # a heap value
        if i == (3 * n) // 4:
            b.__class__ = Q    # another class, same layout
        got.append(o.x + (o.y if isinstance(o.y, int) else len(o.y)))
    return sum(got) % 1000000007, type(b).__name__

def heap_values(n):
    p = P([], {})
    keep = []
    for i in range(n):
        p.x = [i]              # a young heap value into the storage
        p.y = p.x
        if i % 100 == 0:
            keep.append(p.x)
            big = [0] * 1000   # churn so the storage ages
    return len(keep), p.x, p.y is p.x

def growing(n):
    out = 0
    for i in range(n):
        o = P(i, -i)
        if i % 3 == 0:
            o.z = i            # a longer layout
        out += o.x + o.y + getattr(o, "z", 0)
    return out

def young(ps, n):
    for i in range(n):
        p = ps[i % len(ps)]
        p.x = [i]              # heap values into storage that is still young
        p.y = p.x
    return sum(p.x[0] + p.y[0] for p in ps)

def young_rounds():
    s = 0
    for r in range(30):
        s += young([P(k, k) for k in range(10)], 3000)
    return s

print(walk([P(1, 2), P(3, 4), Q(5, 6)], 5000))
print(switch(4000))
print(heap_values(3000))
print(growing(3000))
print(young_rounds())
"#,
        EXPECTED_ATTRS,
    );
}

/// CPython 3.13's output for each program above.
const EXPECTED_ARITHMETIC: &str = r#"0 -293 -879 1180591620717411303424 True True False True
1 -286 -1731 1180591620717411303425 True True False True
2 -279 -2561 1180591620717411303427 True True False True
3 -272 -3369 1180591620717411303430 True True False True
4 -265 -4153 1180591620717411303434 True True False True
2999 1002 4252205 1180591620717415801924 False True False True
(4252205, 1180591620717415801924)
[(-1.25, 1.0001, -1.25, True, True, -0.0, 0.0), (54.0, 1.0018015308163057, 3.0, False, True, -0.0, 4.25), (253.75, 1.0035059565502389, 7.25, False, True, -0.0, 8.5)] (987040.0, 1.2201698259589988, 496.0, False, True, -0.0, 497.25)
-0.0 0.0 True False
5.0
997000.0
(501, 125752.5, 1002.0, 1, 1.0, 2, 1.5)
"#;
const EXPECTED_CONTAINERS: &str = r#"(-47393699, 4000, -93600, {'a': 1999}, [199000, 199200, 199400, 199600, 199800, 200000, 200200, 200400, 200600, 200800])
"#;
const EXPECTED_CALLS: &str = r#"6765 4440679 328350
"#;
const EXPECTED_GENERATORS: &str = r#"(9007490501, 45072, 4498500, 2816, [0, 1, 4, 9, 16])
(61021, 0, 780, 2816, [0, 1, 4, 9, 16])
"#;
const EXPECTED_FINALLY: &str = r#"(4739982, 5136, 7902) (1283, 80, 123) -3123750
"#;
const EXPECTED_INT_EDGES: &str = r#"879134846686371138
0 1 1 -1 0 0 1 1 0 0 True True False
0 -1 -1 1 0 0 -1 -1 0 0 False False False
1 1 2 0 1 1 1 0 1 0 False True True
1 -1 0 2 -1 1 -1 -2 -1 0 False False False
-1 1 0 -2 -1 1 -1 -2 -1 0 True True False
-1 -1 -2 0 1 -1 -1 0 1 0 False True True
2 1 3 1 2 0 3 3 2 0 False False False
2 -1 1 3 -2 2 -1 -3 -2 0 False False False
-2 1 -1 -3 -2 0 -1 -1 -2 0 True True False
-2 -1 -3 -1 2 -2 -1 1 2 0 True True False
3 1 4 2 3 1 3 2 3 0 False False False
3 -1 2 4 -3 3 -1 -4 -3 0 False False False
-3 1 -2 -4 -3 1 -3 -4 -3 0 True True False
-3 -1 -4 -2 3 -3 -1 2 3 0 True True False
7 0 7 7 0 0 7 7 0 0 False False False
7 1 8 6 7 1 7 6 7 0 False False False
7 -1 6 8 -7 7 -1 -8 -7 0 False False False
7 2 9 5 14 2 7 5 3 1 False False False
7 -2 5 9 -14 6 -1 -7 -4 -1 False False False
7 3 10 4 21 3 7 4 2 1 False False False
7 -3 4 10 -21 5 -1 -6 -3 -2 False False False
7 7 14 0 49 7 7 0 1 0 False True True
7 -7 0 14 -49 1 -1 -2 -1 0 False False False
7 1000 1007 -993 7000 0 1007 1007 0 7 True True False
7 -1000 -993 1007 -7000 0 -993 -993 -1 -993 False False False
7 4294967301 4294967308 -4294967294 30064771107 5 4294967303 4294967298 0 7 True True False
7 -4294967301 -4294967294 4294967308 -30064771107 3 -4294967297 -4294967300 -1 -4294967294 False False False
7 70368744177662 70368744177669 -70368744177655 492581209243634 6 70368744177663 70368744177657 0 7 True True False
7 70368744177663 70368744177670 -70368744177656 492581209243641 7 70368744177663 70368744177656 0 7 True True False
7 70368744177664 70368744177671 -70368744177657 492581209243648 0 70368744177671 70368744177671 0 7 True True False
7 70368744177665 70368744177672 -70368744177658 492581209243655 1 70368744177671 70368744177670 0 7 True True False
7 -70368744177665 -70368744177658 70368744177672 -492581209243655 7 -70368744177665 -70368744177672 -1 -70368744177658 False False False
7 -70368744177664 -70368744177657 70368744177671 -492581209243648 0 -70368744177657 -70368744177657 -1 -70368744177657 False False False
7 -70368744177663 -70368744177656 70368744177670 -492581209243641 1 -70368744177657 -70368744177658 -1 -70368744177656 False False False
7 4611686018427387907 4611686018427387914 -4611686018427387900 32281802128991715349 3 4611686018427387911 4611686018427387908 0 7 True True False
7 -4611686018427387907 -4611686018427387900 4611686018427387914 -32281802128991715349 5 -4611686018427387905 -4611686018427387910 -1 -4611686018427387900 False False False
7 1180591620717411303433 1180591620717411303440 -1180591620717411303426 8264141345021879124031 1 1180591620717411303439 1180591620717411303438 0 7 True True False
7 -1180591620717411303433 -1180591620717411303426 1180591620717411303440 -8264141345021879124031 7 -1180591620717411303433 -1180591620717411303440 -1 -1180591620717411303426 False False False
-7 1 -6 -8 -7 1 -7 -8 -7 0 True True False
-7 -1 -8 -6 7 -7 -1 6 7 0 True True False
1000 1 1001 999 1000 0 1001 1001 1000 0 False False False
1000 -1 999 1001 -1000 1000 -1 -1001 -1000 0 False False False
-1000 1 -999 -1001 -1000 0 -999 -999 -1000 0 True True False
-1000 -1 -1001 -999 1000 -1000 -1 999 1000 0 True True False
4294967301 1 4294967302 4294967300 4294967301 1 4294967301 4294967300 4294967301 0 False False False
4294967301 -1 4294967300 4294967302 -4294967301 4294967301 -1 -4294967302 -4294967301 0 False False False
-4294967301 1 -4294967300 -4294967302 -4294967301 1 -4294967301 -4294967302 -4294967301 0 True True False
-4294967301 -1 -4294967302 -4294967300 4294967301 -4294967301 -1 4294967300 4294967301 0 True True False
70368744177662 1 70368744177663 70368744177661 70368744177662 0 70368744177663 70368744177663 70368744177662 0 False False False
70368744177662 -1 70368744177661 70368744177663 -70368744177662 70368744177662 -1 -70368744177663 -70368744177662 0 False False False
70368744177663 0 70368744177663 70368744177663 0 0 70368744177663 70368744177663 0 0 False False False
70368744177663 1 70368744177664 70368744177662 70368744177663 1 70368744177663 70368744177662 70368744177663 0 False False False
70368744177663 -1 70368744177662 70368744177664 -70368744177663 70368744177663 -1 -70368744177664 -70368744177663 0 False False False
70368744177663 2 70368744177665 70368744177661 140737488355326 2 70368744177663 70368744177661 35184372088831 1 False False False
70368744177663 -2 70368744177661 70368744177665 -140737488355326 70368744177662 -1 -70368744177663 -35184372088832 -1 False False False
70368744177663 3 70368744177666 70368744177660 211106232532989 3 70368744177663 70368744177660 23456248059221 0 False False False
70368744177663 -3 70368744177660 70368744177666 -211106232532989 70368744177661 -1 -70368744177662 -23456248059221 0 False False False
70368744177663 7 70368744177670 70368744177656 492581209243641 7 70368744177663 70368744177656 10052677739666 1 False False False
70368744177663 -7 70368744177656 70368744177670 -492581209243641 70368744177657 -1 -70368744177658 -10052677739667 -6 False False False
70368744177663 1000 70368744178663 70368744176663 70368744177663000 1000 70368744177663 70368744176663 70368744177 663 False False False
(703687441776635000, 41649933959)
1407374883553270 -1407374883553290
[70368744177663, 70368744177664, 70368744177665] [-70368744177663, -70368744177664, -70368744177665]
True 2 2
"#;
const EXPECTED_ATTRS: &str = r#"20857498
(8993745, 'Q')
(30, [2999], True)
1498500
1796700
"#;
