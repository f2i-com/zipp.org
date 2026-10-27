//! A `for await` loop closes its iterator by awaiting `return()`
//! (AsyncIteratorClose), as Node does, on every way out of the loop: break,
//! labelled break and continue across loops, return, throw, a `return` in a
//! nested `try`/`finally`, and `gen.return()` at a `yield` inside; over
//! iterators whose `return()` settles late, throws, rejects, gives a
//! non-object or is missing, over a sync iterable and over async
//! generators. The expected lines are Node 24's (its wording of the
//! non-object error aside).

#[test]
fn for_await_closes_its_iterator_as_node_does() {
    let outcome = zipp_vm::run(FOR_AWAIT).expect("compiles");
    assert_eq!(outcome.error, None, "{:?}", outcome.errput);
    let lines: Vec<&str> = outcome.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(
        lines,
        [
            r#"breakIt                ok undefined         a.return(0) a.settled after"#,
            r#"returnIt               ok ret               a.return(0) a.settled"#,
            r#"throwIt                err boom             a.return(0) a.settled"#,
            r#"labelled               ok undefined         i.return(0) i.settled i.return(0) i.settled i.return(0) i.settled o.return(0) o.settled after"#,
            r#"nestedReturn           ok 0                 i.return(0) i.settled o.return(0) o.settled"#,
            r#"tryFinallyInside       ok r                 body-finally body-finally a.return(0) a.settled"#,
            r#"throwingReturn         err a sync-throw     a.return(0)"#,
            r#"throwingReturnOnThrow  err orig             a.return(0)"#,
            r#"rejectingReturn        err a rejected       a.return(0)"#,
            r#"nonObject              err iterator result is not an object a.return(0)"#,
            r#"nonObjectOnThrow       err orig             a.return(0)"#,
            r#"noReturn               ok undefined         after"#,
            r#"syncIterable           ok undefined         s.return after"#,
            r#"asyncGen               ok undefined         g.finally after"#,
            r#"asyncGenThrow          err t                g.finally"#,
            r#"exhaust                ok undefined         sum 10"#,
            r#"inAsyncGen             ok undefined         a.return(0) a.settled after"#,
            r#"genReturnAtYield       ok undefined         a.return(0) a.settled after"#,
        ]
    );
}

const FOR_AWAIT: &str = r####"// for await: every way out of the loop, and what the iterator's return() sees.
const log = [];
const tick = () => new Promise((r) => setTimeout(r, 1));
function source(name, opts = {}) {
  return {
    [Symbol.asyncIterator]() {
      let i = 0;
      return {
        next() { return Promise.resolve({ value: i++, done: i > 5 }); },
        ...(opts.noReturn ? {} : {
          return(...args) {
            log.push(`${name}.return(${args.length})`);
            if (opts.throwSync) throw new Error(name + " sync-throw");
            if (opts.nonObject) return Promise.resolve(42);
            if (opts.reject) return Promise.reject(new Error(name + " rejected"));
            return tick().then(() => { log.push(`${name}.settled`); return { done: true }; });
          },
        }),
      };
    },
  };
}
function syncSource(name) {
  return {
    [Symbol.iterator]() {
      let i = 0;
      return {
        next() { return { value: Promise.resolve(i++), done: i > 5 }; },
        return() { log.push(`${name}.return`); return { done: true }; },
      };
    },
  };
}
async function* agen(name) {
  try { for (let i = 0; ; i++) { await null; yield i; } }
  finally { await tick(); log.push(`${name}.finally`); }
}
const cases = {
  async breakIt() { for await (const x of source("a")) if (x === 2) break; log.push("after"); },
  async returnIt() { for await (const x of source("a")) if (x === 1) return "ret"; },
  async throwIt() { for await (const x of source("a")) if (x === 1) throw new Error("boom"); },
  async labelled() {
    outer: for await (const x of source("o")) {
      for await (const y of source("i")) { if (y === 1) continue outer; if (x === 2) break outer; }
    }
    log.push("after");
  },
  async nestedReturn() { for await (const x of source("o")) for await (const y of source("i")) return x + y; },
  async tryFinallyInside() {
    for await (const x of source("a")) {
      try { if (x === 1) return "r"; } finally { log.push("body-finally"); }
    }
  },
  async throwingReturn() { for await (const x of source("a", { throwSync: true })) break; },
  async throwingReturnOnThrow() { for await (const x of source("a", { throwSync: true })) throw new Error("orig"); },
  async rejectingReturn() { for await (const x of source("a", { reject: true })) break; },
  async nonObject() { for await (const x of source("a", { nonObject: true })) break; },
  async nonObjectOnThrow() { for await (const x of source("a", { nonObject: true })) throw new Error("orig"); },
  async noReturn() { for await (const x of source("a", { noReturn: true })) break; log.push("after"); },
  async syncIterable() { for await (const x of syncSource("s")) if (x === 2) break; log.push("after"); },
  async asyncGen() { for await (const x of agen("g")) if (x === 3) break; log.push("after"); },
  async asyncGenThrow() { for await (const x of agen("g")) throw new Error("t"); },
  async exhaust() { let s = 0; for await (const x of source("a")) s += x; log.push("sum " + s); },
  async inAsyncGen() {
    async function* g() { for await (const x of source("a")) { yield x; } }
    for await (const v of g()) if (v === 1) break;
    log.push("after");
  },
  async genReturnAtYield() {
    async function* g() { for await (const x of source("a")) { yield x; } }
    const it = g(); await it.next(); await it.return("early"); log.push("after");
  },
};
(async () => {
  for (const [name, fn] of Object.entries(cases)) {
    log.length = 0;
    let res;
    try { res = "ok " + (await fn()); } catch (e) { res = "err " + e.message; }
    console.log(name.padEnd(22), res.padEnd(20), log.join(" "));
  }
})();
"####;
