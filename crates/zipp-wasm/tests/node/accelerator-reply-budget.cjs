#!/usr/bin/env node
"use strict";

const assert = require("node:assert/strict");
const { Engine } = require("./pkg/zipp_wasm.js");

const LIMIT = 32 * 1024 * 1024;
const cases = [
  // JSON's two quote characters put this just over the UTF-16 pre-copy cap.
  ["ASCII", () => "a".repeat(LIMIT - 1)],
  // Below the UTF-16 cap, but over the exact UTF-8 byte cap.
  ["non-ASCII", () => "\u0800".repeat(Math.floor(LIMIT / 3) + 1)],
];

for (const [label, reply] of cases) {
  const engine = new Engine();
  engine.setAccelBridge({ compile: reply });
  engine.setSyncHostCapabilities(["accel.compile"]);
  const result = engine.initScript(`
    var outcome = "accepted";
    try { accel.compile("x", "y"); }
    catch (error) { outcome = String(error); }
  `);
  assert.equal(result.error, undefined, `${label}: script initialized`);
  assert.match(
    engine.evalInContext("outcome"),
    /^RangeError: host bridge reply exceeds the 33554432-byte limit$/,
    `${label}: oversized accelerator reply is rejected`,
  );
  engine.dispose();
}

console.log("accelerator reply budget passed");
