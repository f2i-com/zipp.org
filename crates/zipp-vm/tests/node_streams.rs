//! node:stream and node:readline (`lang/js/node/builtins`).
//!
//! The stream program orders a Readable's events against a tick and a
//! promise job, pipes through a Transform into a slow Writable, reads lines
//! split across chunks (CRLF too) from readline, breaks out of a
//! `for await` over a Readable (which destroys it), uses the promise
//! `pipeline`, `finished` on an errored stream, the legacy
//! `_stream_readable` name, a Readable's `map`/`toArray`, and a
//! `pipeline` into `process.stdout`, which stays writable. The expected
//! lines are Node 24's.

#[test]
fn node_streams_and_readline_behave_as_node() {
    zipp_vm::set_node_runtime(true, vec!["zipp".into(), "app.js".into()]);
    let outcome = zipp_vm::run(STREAMS).expect("compiles");
    assert_eq!(outcome.error, None, "{:?}", outcome.errput);
    let lines: Vec<&str> = outcome.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(
        lines,
        [
            r#"data:a,data:b,tick,end,close,micro"#,
            r#"lines three"#,
            r#"break destroyed true"#,
            r#"total 30"#,
            r#"finished bad"#,
            r#"legacy true true"#,
            r#"map pp,qq"#,
            r#"piped"#,
            r#"after pipe ok true"#,
        ]
    );
}

const STREAMS: &str = r####"const { Readable, Writable, Transform, PassThrough, pipeline, finished } = require("stream");
const { pipeline: pipelineP } = require("stream/promises");
const readline = require("readline");
const out = [];
const order = [];

const r = new Readable({ read() {} });
r.on("data", (c) => order.push("data:" + c));
r.on("end", () => order.push("end"));
r.on("close", () => order.push("close"));
r.push("a"); r.push("b"); r.push(null);
process.nextTick(() => order.push("tick"));
Promise.resolve().then(() => order.push("micro"));

const upper = new Transform({ transform(chunk, enc, cb) { cb(null, String(chunk).toUpperCase()); } });
const sink = [];
const w = new Writable({ write(chunk, enc, cb) { sink.push(String(chunk)); setTimeout(cb, 1); }, highWaterMark: 2 });
pipeline(Readable.from(["x", "y", "z"]), upper, w, (err) => out.push("pipeline " + (err ? err.message : "ok") + " " + sink.join("")));

async function main() {
  const lines = [];
  const pt = new PassThrough();
  const rl = readline.createInterface({ input: pt, crlfDelay: Infinity });
  pt.write("one\r\ntw"); pt.write("o\nthree"); pt.end();
  for await (const line of rl) lines.push(line);
  out.push("lines " + lines.join("|"));

  const src = Readable.from([1, 2, 3, 4, 5], { objectMode: true });
  for await (const v of src) { if (v === 2) break; }
  out.push("break destroyed " + src.destroyed);

  let total = 0;
  await pipelineP(Readable.from(["10", "20"]), new Writable({ write(c, e, cb) { total += +c; cb(); } }));
  out.push("total " + total);

  const errStream = new Readable({ read() { this.destroy(new Error("bad")); } });
  await new Promise((res) => finished(errStream.resume(), (err) => { out.push("finished " + err.message); res(); }));

  out.push("legacy " + (require("_stream_readable") === Readable) + " " + (require("node:stream").Stream === require("stream")));
  const collected = await Readable.from(["p", "q"]).map((s) => s + s).toArray();
  out.push("map " + collected.join(","));
}
main().then(() => {
  out.unshift(order.join(","));
  console.log(out.join("\n"));
  pipeline(Readable.from(["piped\n"]), process.stdout, (err) => {
    process.stdout.write("after pipe " + (err ? err.message : "ok") + " " + process.stdout.writable + "\n");
  });
});
"####;
