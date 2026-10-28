#!/usr/bin/env node
// The TypeScript frontend's boundary contract, adapting to the artifact's
// `zippProfile().languages`: the javascript-typescript module runs a
// TypeScript script behind the same preamble as JavaScript (its types read
// and dropped, enums, namespaces and parameter properties lowered), its
// bindings reachable as a script's are; every other module refuses
// "typescript" and names the module that has it. JavaScript runs in both.
"use strict";
const { Engine, zippProfile } = require("./pkg/zipp_wasm.js");

const languages = JSON.parse(zippProfile()).languages;

let pass = 0, fail = 0;
function ok(label, cond, extra = "") {
  if (cond) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label} ${extra}`); }
}
function eq(label, got, want) {
  const g = JSON.stringify(got), w = JSON.stringify(want);
  if (g === w) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}\n         got  ${g}\n         want ${w}`); }
}
function thrown(fn) {
  try { fn(); return null; } catch (e) { return String(e); }
}

const TS = [
  "enum Color { Red, Green = 5, Blue }",
  "interface Point { readonly x: number; y?: number }",
  "namespace Geo {",
  "  export const origin: Point = { x: 0, y: 0 };",
  "  export function len(p: Point): number { return Math.hypot(p.x, p.y ?? 0); }",
  "}",
  "class Box<T> {",
  "  constructor(private readonly items: T[] = []) {}",
  "  add(item: T): this { this.items.push(item); return this; }",
  "  get size(): number { return this.items.length; }",
  "}",
  "const box = new Box<number>().add(1).add(2);",
  "let total: number = box.size + Color.Blue + Geo.len({ x: 3, y: 4 });",
  "function twice(n: number): number { return n * 2; }",
  "console.log(`total ${total} ${Color[5]} ${Geo.origin.x}`);",
  "",
].join("\n");

ok("profile lists JavaScript first", languages[0] === "javascript", JSON.stringify(languages));

if (languages.includes("typescript")) {
  const e = new Engine();
  let symbols;
  const err = thrown(() => { symbols = e.initSource(TS, "typescript"); });
  ok("typescript initSource succeeds", err === null, err);
  eq("typescript output drains through takeOutput", e.takeOutput(), ["total 13 Green 0"]);
  ok("a TypeScript script's bindings are in its symbol map",
    symbols !== undefined && "total" in symbols && "twice" in symbols, JSON.stringify(symbols));
  eq("callFunction reaches a TypeScript function", e.callFunction("twice", [21]), 42);
  e.dispose();

  const bad = new Engine();
  const compileErr = thrown(() => bad.initSource("let x: = 1;\n", "typescript"));
  ok("a TypeScript syntax error is a compile error", compileErr !== null && /SyntaxError/.test(compileErr), compileErr);
  ok("a failed TypeScript initialization disposes the engine", bad.disposed);
} else {
  const e = new Engine();
  const err = thrown(() => e.initSource(TS, "typescript"));
  ok("typescript is refused, naming the module that has it",
    err !== null && /javascript-typescript/.test(err), err);
  if (!e.disposed) e.dispose();
}

{
  const e = new Engine();
  const err = thrown(() => e.initSource("console.log('js ' + (1 + 2));\n", "javascript"));
  ok("javascript initSource succeeds", err === null, err);
  eq("javascript output drains through takeOutput", e.takeOutput(), ["js 3"]);
  e.dispose();
}

console.log(`typescript-frontend: ${pass} passed, ${fail} failed`);
process.exit(fail === 0 ? 0 : 1);
