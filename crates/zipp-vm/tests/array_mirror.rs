//! Element reads that compiled code answers through the heap's array mirror
//! (`Heap::arr_mirror`: an Array's element base and length, read by emitted
//! code without a helper call) stay exact through every way an array can
//! change between two reads: growth, shrinking, `length`, holes, an accessor
//! defined on an index, an indexed prototype, in-place reordering, a mapped
//! `arguments` object, a replaced prototype, and keys that are doubles. The
//! functions are warmed so the JIT tiers run them. The expected line is Node
//! 24's for the same program.

#[test]
fn array_element_reads_follow_every_mutation() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [r#"10,4,15,5,6,3,,1,,,,9,6,42,43,proto,proto,,30,,30,10,7,7,0,7,7,2,7,99,99,2.5,b,o,6,,5,,,,6,,x,99,,false,from-proto,5,x,"#]);
}

const PROGRAM: &str = r####"// Element reads that compiled code answers from its array mirror stay exact
// through every way an array can change between reads.
const out = [];
function at(a, i) { return a[i]; }
function sum(a) { let s = 0; for (let i = 0; i < a.length; i++) s += a[i]; return s; }
const warm = [1, 2, 3];
for (let r = 0; r < 20000; r++) { at(warm, r % 3); sum(warm); }
const a = [1, 2, 3, 4];
out.push(sum(a), at(a, 3));
a.push(5); out.push(sum(a), at(a, 4));
a.pop(); a.pop(); out.push(sum(a), a.length, at(a, 3));
a.length = 1; out.push(at(a, 0), at(a, 1), at(a, 2));
a[5] = 9; out.push(at(a, 4), at(a, 5), a.length);
Object.defineProperty(a, 0, { get() { return 42; }, configurable: true });
out.push(at(a, 0), sum([at(a, 0), 1]));
Array.prototype[3] = "proto"; out.push(at(a, 3), at([], 3)); delete Array.prototype[3];
out.push(at(a, 3));
const b = [10, 20, 30]; at(b, 0);
b.splice(1, 1); out.push(at(b, 1), at(b, 2));
b.reverse(); out.push(at(b, 0));
b.sort((x, y) => x - y); out.push(at(b, 0));
const c = [1, 2, 3]; at(c, 1); c.fill(7); out.push(at(c, 1));
c.copyWithin(0, 1); out.push(at(c, 0));
c.unshift(0); out.push(at(c, 0), at(c, 3));
c.shift(); c.shift(); out.push(at(c, 0), c.length);
Object.freeze(c); out.push(at(c, 1));
function mapped(x) { at(arguments, 0); x = 99; return at(arguments, 0); }
out.push(mapped(1), mapped(2));
out.push(at(new Float64Array([1.5, 2.5]), 1), at("abc", 1), at({ 0: "o" }, 0));
const d = [5, 6, 7];
out.push(at(d, 1.0), at(d, 0.5), at(d, -0), at(d, -1), at(d, 2 ** 32), at(d, NaN), at(d, "1"));
const e = [1, 2, 3]; at(e, 0);
e.length = 0; out.push(at(e, 0)); e.push("x"); out.push(at(e, 0));
const f = []; for (let i = 0; i < 100; i++) { f.push(i); if (at(f, i) !== i) out.push("bad " + i); }
out.push(at(f, 99));
const g = [1, , 3]; out.push(at(g, 1), 1 in g);
Object.setPrototypeOf(g, { 1: "from-proto" }); out.push(at(g, 1));
class Sub extends Array {} const s = Sub.of(4, 5); out.push(at(s, 1));
const h = [0, 1]; at(h, 0); h[0] = "x"; out.push(at(h, 0)); h[1] = undefined; out.push(at(h, 1));
console.log(out.join(","));
"####;
