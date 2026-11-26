//! Properties kept beside arrays, functions and other built-in objects
//! (`Vm::arr_props`, `Vm::fn_props`), written while their holders are old.
//!
//! The values of those property maps are roots. A minor collection walked
//! every map of both tables to root them, and the tables keep an entry for
//! every old holder until a major collection prunes them: a program that
//! gives each of its arrays one property of its own (a DOM's node lists,
//! anything `Object.freeze` touched) had tens of thousands of maps there, all
//! old, walked at every minor. That was 6% of a long run building DOM
//! documents, and it grew with the run.
//!
//! A minor now roots only the maps handed out for writing since the last
//! collection (`PropsTable::touched`): a value stored before it was rooted by
//! that collection, survived it and is old.
//!
//! The program ages holders of every kind past several collections (arrays,
//! functions, bound functions, Maps, Sets, Dates, RegExps, typed arrays,
//! match results, sparse arrays, frozen arrays, an Array subclass), then
//! writes fresh objects into them every way there is (assignment, a computed
//! key, `defineProperty` of data and of an accessor, `Object.assign`,
//! `Reflect.set`, delete and re-add, an element past the dense part, a setter
//! that stores), collects, and reads everything back; four rounds, then
//! holders made and filled between collections, and holders dropped.
//!
//! The modes below include the collector's own check: `ZIPP_NURSERY_VERIFY=1`
//! runs a full mark beside every minor and stops on the first live young
//! object the minor missed. With the log switched off (`PropsTable::note`
//! made to do nothing) this program fails both ways: its answers are wrong,
//! and the check stops at the first value a minor did not root.
//!
//! The expected lines are Node 24's.

const PROGRAM: &str = r#"
const out = [];
const log = (...a) => out.push(a.join(" "));
let junk;
function churn(n) { for (let i = 0; i < n; i++) junk = { i, pad: [i, i + 1] }; }
const j = JSON.stringify;
// Holders of every kind, each with one property of its own, then aged past a few collections.
const N = 120;
const arrays = [], fns = [], maps = [], sets = [], dates = [], regexes = [], typed = [], matches = [], sparse = [], frozen = [], bound = [];
class List extends Array { constructor() { super(); this.owner = { kind: "list" }; } }
const lists = [];
for (let i = 0; i < N; i++) {
  const a = [i]; a.tag = { id: i }; arrays.push(a);
  const f = function () { return i; }; f.meta = { id: i }; fns.push(f);
  const m = new Map(); m.note = { id: i }; maps.push(m);
  const s = new Set(); s.note = { id: i }; sets.push(s);
  const d = new Date(0); d.note = { id: i }; dates.push(d);
  const r = /a(b)?/g; r.note = { id: i }; regexes.push(r);
  const t = new Uint8Array(2); t.note = { id: i }; typed.push(t);
  const x = /(\d)(\d)?/.exec("7" + i); matches.push(x);
  const sp = []; sp[100000 + i] = { id: i }; sparse.push(sp);
  frozen.push(Object.freeze([{ id: i }]));
  const b = f.bind(null); b.meta = { id: i }; bound.push(b);
  lists.push(new List());
}
churn(40000);
log("aged", arrays[7].tag.id, fns[7].meta.id, maps[7].note.id, sets[7].note.id, dates[7].note.id, regexes[7].note.id, typed[7].note.id, matches[7].index, j(matches[7]), sparse[7][100007].id, frozen[7][0].id, bound[7].meta.id, lists[7].owner.kind);
// Every way of writing, on old holders, with values allocated now; then collections; then read back.
function write(round) {
  for (let i = 0; i < N; i++) {
    const tag = round + ":" + i;
    arrays[i].tag = { id: tag, deep: { list: [tag] } };
    arrays[i]["k" + (i & 3)] = [tag];
    fns[i].meta = { id: tag };
    fns[i].extra = () => tag;
    Object.defineProperty(fns[i], "viaDefine", { value: { id: tag }, configurable: true, writable: true });
    Object.defineProperty(arrays[i], "acc", { get: () => ({ id: tag }), set(v) { this.tag = v; }, configurable: true });
    maps[i].note = { id: tag }; sets[i].note = { id: tag }; dates[i].note = { id: tag }; regexes[i].note = { id: tag }; typed[i].note = { id: tag };
    Object.assign(maps[i], { assigned: { id: tag } });
    Reflect.set(sets[i], "reflected", { id: tag });
    matches[i].groups = { id: tag }; matches[i].added = { id: tag }; matches[i][0] = "m" + tag;
    sparse[i][200000 + i] = { id: tag }; sparse[i].named = { id: tag };
    bound[i].meta = { id: tag };
    lists[i].owner = { kind: tag }; lists[i].push({ id: tag });
    if (i % 50 === 0) { delete arrays[i].tag; arrays[i].tag = { id: "re" + tag }; }
  }
}
function read(i) {
  return [arrays[i].tag.id, arrays[i].tag.deep ? arrays[i].tag.deep.list[0] : "-", arrays[i]["k" + (i & 3)][0], fns[i].meta.id, fns[i].extra(), fns[i].viaDefine.id, arrays[i].acc.id,
    maps[i].note.id, sets[i].note.id, dates[i].note.id, regexes[i].note.id, typed[i].note.id, maps[i].assigned.id, sets[i].reflected.id,
    matches[i].groups.id, matches[i].added.id, matches[i][0], matches[i].index, sparse[i][200000 + i].id, sparse[i].named.id, sparse[i][100000 + i].id, bound[i].meta.id, lists[i].owner.kind, lists[i][lists[i].length - 1].id].join(",");
}
for (let round = 0; round < 4; round++) {
  write(round);
  churn(25000);
  let ok = 0; for (let i = 0; i < N; i++) if (read(i) === read(i)) ok++;
  log("round", round, ok, read(0), read(N - 1));
}
// A setter that stores into the same table, an accessor's own functions, and holders made and filled between collections.
arrays[3].acc = { id: "through setter" };
churn(25000);
log("setter", arrays[3].tag.id, typeof Object.getOwnPropertyDescriptor(arrays[3], "acc").get, typeof Object.getOwnPropertyDescriptor(arrays[3], "acc").set);
const late = [];
for (let i = 0; i < 2000; i++) { const a = []; a.tag = { id: i }; const f = () => i; f.tag = { id: i }; late.push(a, f); if (i % 400 === 0) churn(12000); }
let sum = 0; for (let i = 0; i < late.length; i++) sum += late[i].tag.id;
log("late", late.length, sum);
// Holders dropped while others stay: the tables shrink and the survivors keep their values.
arrays.length = 10; fns.length = 10; maps.length = 10; late.length = 20;
churn(60000);
log("after drop", read(3).length > 0, arrays[9].tag.id, fns[9].meta.id, late[19].tag.id, Object.keys(arrays[9]).join(","));
console.log(out.join("\n"));
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output.iter().flat_map(|l| l.lines().map(str::to_string)).collect()
}

#[test]
fn props_minor_answers_as_node() {
    assert_eq!(
        run_ok(PROGRAM),
        [
            "aged 7 7 7 7 7 7 7 0 [\"77\",\"7\",\"7\"] 7 7 7 list",
            "round 0 120 re0:0,-,0:0,0:0,0:0,0:0,0:0,0:0,0:0,0:0,0:0,0:0,0:0,0:0,0:0,0:0,m0:0,0,0:0,0:0,0,0:0,0:0,0:0 0:119,0:119,0:119,0:119,0:119,0:119,0:119,0:119,0:119,0:119,0:119,0:119,0:119,0:119,0:119,0:119,m0:119,0,0:119,0:119,119,0:119,0:119,0:119",
            "round 1 120 re1:0,-,1:0,1:0,1:0,1:0,1:0,1:0,1:0,1:0,1:0,1:0,1:0,1:0,1:0,1:0,m1:0,0,1:0,1:0,0,1:0,1:0,1:0 1:119,1:119,1:119,1:119,1:119,1:119,1:119,1:119,1:119,1:119,1:119,1:119,1:119,1:119,1:119,1:119,m1:119,0,1:119,1:119,119,1:119,1:119,1:119",
            "round 2 120 re2:0,-,2:0,2:0,2:0,2:0,2:0,2:0,2:0,2:0,2:0,2:0,2:0,2:0,2:0,2:0,m2:0,0,2:0,2:0,0,2:0,2:0,2:0 2:119,2:119,2:119,2:119,2:119,2:119,2:119,2:119,2:119,2:119,2:119,2:119,2:119,2:119,2:119,2:119,m2:119,0,2:119,2:119,119,2:119,2:119,2:119",
            "round 3 120 re3:0,-,3:0,3:0,3:0,3:0,3:0,3:0,3:0,3:0,3:0,3:0,3:0,3:0,3:0,3:0,m3:0,0,3:0,3:0,0,3:0,3:0,3:0 3:119,3:119,3:119,3:119,3:119,3:119,3:119,3:119,3:119,3:119,3:119,3:119,3:119,3:119,3:119,3:119,m3:119,0,3:119,3:119,119,3:119,3:119,3:119",
            "setter through setter function function",
            "late 4000 3998000",
            "after drop true 3:9 3:9 9 0,tag,k1",
        ]
    );
}

/// The same answers with the JIT compiling at once, with no native code,
/// under the collector's full-mark check, and with every map walked as
/// before. (Not with a collection at every safe point: those are major
/// collections, which walk every map either way.)
#[test]
fn props_minor_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
        ("verify", "ZIPP_NURSERY_VERIFY", "1"),
        ("every map", "ZIPP_NO_PROPS_TOUCHED", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("props_minor_answers")
            .arg("--nocapture")
            .env(key, value)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the filter matched nothing in {name} mode"
        );
    }
}
