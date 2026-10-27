//! Node's web events and their users (`lang/js/node/web_events.js`):
//! `AbortController`'s trusted `abort` event and reason, `DOMException`
//! codes, `structuredClone` throwing a `DataCloneError` DOMException, the
//! globals non-enumerable as Node's, `pipeline` handing a generator stage a
//! signal, `events.once`; with `Buffer.concat` over empty buffers, a
//! decoded BOM kept, `StringDecoder` holding a base64 group across writes,
//! an fs write stream read back through a read stream in 4-byte slices,
//! and Buffer's `indexOf`/`lastIndexOf`/`includes` with string needles.
//! The expected lines are Node 24's.

#[test]
fn node_web_events_and_fs_streams_behave_as_node() {
    zipp_vm::set_node_runtime(true, vec!["zipp".into(), "app.js".into()]);
    let outcome = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(outcome.error, None, "{:?}", outcome.errput);
    let lines: Vec<&str> = outcome.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(
        lines,
        [
            r#"6 4 1 2 true 2"#,
            r#"abcd abcd 3 "﻿hi""#,
            r#"abort abort true AbortError"#,
            r#"function false 20"#,
            r#"true DataCloneError 25"#,
            r#"AQIDBAUG"#,
            r#"pipeline ok"#,
            r#"once 42"#,
            r#"read "hello world\n" 3"#,
        ]
    );
}

const PROGRAM: &str = r####"const sb = Buffer.from("abcabc\n"); console.log(sb.indexOf("\n"), sb.lastIndexOf("b"), sb.lastIndexOf("bc", 3), sb.indexOf("c", "utf8"), sb.includes("\n"), Buffer.concat([sb.subarray(0, 2), sb.subarray(2)]).indexOf("ca"));
const a = Buffer.concat([Buffer.alloc(0), Buffer.from("ab"), Buffer.alloc(0), Buffer.from("cd")]);
const b = Buffer.concat([Buffer.from("abc"), Buffer.from("def")], 4);
const c = Buffer.from([0xef, 0xbb, 0xbf, 0x68, 0x69]).toString();
console.log(a.toString(), b.toString(), c.length, JSON.stringify(c));
const ac = new AbortController();
ac.signal.addEventListener("abort", (e) => console.log("abort", e.type, e.isTrusted, ac.signal.reason.name));
ac.abort();
console.log(typeof EventTarget, Object.keys(globalThis).includes("AbortController"), new DOMException("m", "AbortError").code);
try { structuredClone(() => 1); } catch (e) { console.log(e instanceof DOMException, e.name, e.code); }
const { once } = require("events");
const { Readable, pipeline } = require("stream");
const { StringDecoder } = require("string_decoder");
const d = new StringDecoder("base64");
console.log(d.write(Buffer.from([1, 2])) + d.write(Buffer.from([3, 4, 5, 6])) + d.end());
const ac2 = new AbortController();
pipeline(Readable.from(["x"]), async function* (src, { signal }) { for await (const c of src) yield c; }, (err) => console.log("pipeline", err ?? "ok"));
const fs = require("fs"), path = require("path"), os = require("os");
const f = path.join(os.tmpdir(), "zipp-fsstream-" + process.pid + ".txt");
const ws = fs.createWriteStream(f);
ws.write("hello "); ws.end("world\n");
ws.on("finish", () => {
  const chunks = [];
  fs.createReadStream(f, { encoding: "utf8", highWaterMark: 4 }).on("data", (c) => chunks.push(c)).on("close", () => {
    console.log("read", JSON.stringify(chunks.join("")), chunks.length);
    fs.unlinkSync(f);
  });
});
(async () => { const ee = new (require("events"))(); setTimeout(() => ee.emit("go", 42), 1); const [v] = await once(ee, "go"); console.log("once", v); })();
"####;
