// The parse-shape limits in the hardened profile are calibrated against THIS
// artifact: a wasm module linked with -zstack-size=1048576. Every other test of
// them runs natively, where a frame costs an order of magnitude more and the
// stack is whatever the host thread was given, so nothing here can be inferred
// from a `cargo test` run.
//
// Two halves, and both matter. v0.0.1 shipped limits that rejected working code
// because the only browser check in the release workflow parsed
// `console.log("zipp-web-release-smoke")` — three tokens deep, which no nesting
// limit above zero can reject. So: real reduced application source must parse,
// and hostile depth must come back as a SyntaxError with the engine still
// usable, never as a trap.
const fs = require("fs");
const path = require("path");
const { Engine } = require("./pkg/zipp_wasm.js");

const CORPUS = path.resolve(__dirname, "../../../../tests/syntax-corpus");

let pass = 0;
let fail = 0;
const ok = (label, cond, extra = "") => {
  if (cond) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label} ${extra}`); }
};

// Returns "" when the source parsed and ran, the engine's SyntaxError when it
// did not, or "TRAP: ..." when the module itself died. A trap is the failure
// this whole mechanism exists to prevent: the shadow stack runs off the bottom
// of linear memory, and the instance cannot be re-entered afterwards.
function parse(source) {
  let engine;
  try {
    engine = new Engine();
  } catch (e) {
    return `TRAP: engine unusable (${e && e.message})`;
  }
  try {
    const result = engine.initScript(source);
    return result && result.error ? String(result.error) : "";
  } catch (e) {
    return `TRAP: ${e && e.message ? e.message : e}`;
  } finally {
    try { engine.dispose(); } catch { /* a trapped instance cannot be disposed */ }
  }
}

console.log("real application sources (tests/syntax-corpus):");
const files = fs.readdirSync(CORPUS).filter((f) => f.endsWith(".js")).sort();
ok(`corpus present at ${CORPUS}`, files.length >= 5, `found ${files.length}`);
for (const name of files) {
  const error = parse(fs.readFileSync(path.join(CORPUS, name), "utf8"));
  ok(`${name} parses`, error === "", error);
}

// Depths far past any plausible limit, in the shapes measured to be the most
// expensive per unit of guard budget: an `else if` ladder spends one recursion
// level per arm, an arrow chain runs the recursion counter three times ahead of
// the AST depth, and the two flat chains are parsed iteratively and caught only
// by the chain guard and the completed-AST validator.
console.log("hostile depth is rejected, not trapped:");
const hostile = {
  parentheses: (n) => `${"(".repeat(n)}0${")".repeat(n)};`,
  blocks: (n) => `${"{".repeat(n)}0;${"}".repeat(n)}`,
  functions: (n) => `${"function f(){".repeat(n)}0;${"}".repeat(n)}`,
  arrows: (n) => `${"()=>".repeat(n)}1;`,
  "else-if": (n) => {
    let source = "if(x===0){f(0);}";
    for (let arm = 1; arm < n; arm++) source += `else if(x===${arm}){f(${arm});}`;
    return source;
  },
  members: (n) => `a${".x".repeat(n)};`,
  binary: (n) => `${"1+".repeat(n)}1;`,
  patterns: (n) => `let ${"[".repeat(n)}x${"]".repeat(n)} = [];`,
};
for (const [name, build] of Object.entries(hostile)) {
  for (const depth of [1_000, 20_000]) {
    const error = parse(build(depth));
    ok(
      `${name} x${depth} fails closed`,
      error.includes("sandbox limit"),
      error === "" ? "accepted a source deeper than any limit" : error,
    );
  }
}

// The instance must still be alive after all of that. This is the assertion
// that separates a guard from a crash: past the stack ceiling the module traps
// on a linear-memory access and every later entry traps too, so an engine that
// still runs here proves nothing above reached the ceiling.
ok("engine still usable afterwards", parse("var x = 1 + 1;") === "");

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
