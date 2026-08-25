// End-to-end: the REAL SnakeGame bundle's .logic, driven the way
// SoftNScriptRuntime drives it — init, read state, register listeners,
// dispatch keys, tick the game loop, sync window both ways.
const fs = require("fs");
const path = require("path");
const { Engine } = require("./pkg/zipp_wasm.js");

// Drives a real third-party bundle rather than a fixture: the point is that an
// app written against a different engine runs unmodified. Point SOFTN_REPO at a
// softn.com checkout; without one there is nothing to test, so skip.
const SOFTN_REPO = process.env.SOFTN_REPO || "../softn.com";
const BUNDLE = path.resolve(SOFTN_REPO, "softn/apps/demo/bundles/SnakeGame");
if (!fs.existsSync(path.join(BUNDLE, "logic/main.logic"))) {
  console.log(`skip: no SnakeGame bundle at ${BUNDLE} (set SOFTN_REPO)`);
  process.exit(0);
}
const src = fs.readFileSync(path.join(BUNDLE, "logic/main.logic"), "utf8");

let pass = 0, fail = 0;
const eq = (l, g, w) => {
  const a = JSON.stringify(g), b = JSON.stringify(w);
  if (a === b) { pass++; console.log(`  ok   ${l}`); }
  else { fail++; console.log(`  FAIL ${l}\n         got  ${a}\n         want ${b}`); }
};
const ok = (l, c, extra = "") => {
  if (c) { pass++; console.log(`  ok   ${l}`); }
  else { fail++; console.log(`  FAIL ${l} ${extra}`); }
};

// The app-scoped localStorage the adapter installs.
const store = {};
const ls = {
  getItem: (k) => (k in store ? store[k] : null),
  setItem: (k, v) => { store[k] = String(v); },
  removeItem: (k) => { delete store[k]; },
  clear: () => { for (const k of Object.keys(store)) delete store[k]; },
};

const e = new Engine();
e.setSyncHostCapabilities(["ls.getItem", "ls.setItem", "ls.removeItem", "ls.clear"]);
e.setLocalStorageBridge(ls);

console.log("— load the real bundle —");
const syms = e.initScript(src);
const names = Object.keys(syms);
ok("compiled", names.length > 0);
eq("state vars present", ["score", "gridItems", "running", "snake", "direction"].every((n) => syms[n]), true);
eq("functions present", ["_init", "tick", "startGame", "buildGridItems"].every((n) => syms[n]?.scope === "function"), true);
ok("window is addressable", !!syms.window, JSON.stringify(names));

console.log("— _init —");
e.callFunction("_init", []);
eq("snake seeded", e.getGlobalByIndex(syms.snake.index), [{ x: 10, y: 10 }, { x: 9, y: 10 }, { x: 8, y: 10 }]);
eq("food seeded", e.getGlobalByIndex(syms.food.index), { x: 15, y: 10 });
const grid0 = e.getGlobalByIndex(syms.gridItems.index);
ok("gridItems built (array of cells)", Array.isArray(grid0) && grid0.length >= 4, `len=${grid0 && grid0.length}`);
ok("cell has x/y/color", grid0[0] && "x" in grid0[0] && "color" in grid0[0], JSON.stringify(grid0 && grid0[0]));

console.log("— window two-way sync (the load-bearing one) —");
eq("listener registered", e.getEventListenerTypes(), ["keydown"]);
let win = e.getGlobalByIndex(syms.window.index);
eq("__snakeKeyListenerAdded set by _init", win.__snakeKeyListenerAdded, true);
eq("__snakeNextDir starts null", win.__snakeNextDir, null);

eq("ArrowUp reaches a handler", e.dispatchEvent("keydown", { type: "keydown", key: "ArrowUp" }), 1);
win = e.getGlobalByIndex(syms.window.index);
eq("handler wrote window.__snakeNextDir", win.__snakeNextDir, "up");

// Exactly what syncWindowToVM does: spread the read, add a key, write it back.
// The methods it could not see come back as null and must not land as null.
e.setGlobalByIndex(syms.window.index, { ...win, __hostAdded: 7 });
eq("host key landed", e.getGlobalByIndex(syms.window.index).__hostAdded, 7);
eq("addEventListener SURVIVED the write-back", e.dispatchEvent("keydown", { type: "keydown", key: "ArrowLeft" }), 1);
eq("and still works", e.getGlobalByIndex(syms.window.index).__snakeNextDir, "left");

console.log("— play the game —");
e.callFunction("startGame", []);
eq("running", e.getGlobalByIndex(syms.running.index), true);
eq("score reset", e.getGlobalByIndex(syms.score.index), 0);

// Steer right and tick; the snake must move.
e.dispatchEvent("keydown", { type: "keydown", key: "ArrowRight" });
const before = e.getGlobalByIndex(syms.snake.index)[0];
e.callFunction("tick", []);
const after = e.getGlobalByIndex(syms.snake.index)[0];
ok("snake head moved", before.x !== after.x || before.y !== after.y, `${JSON.stringify(before)} -> ${JSON.stringify(after)}`);

// startGame calls spawnFood(), which places food at random — so put it
// directly in front of the head instead. That makes the test deterministic AND
// exercises writing a nested object global back into the VM.
const head = e.getGlobalByIndex(syms.snake.index)[0];
e.setGlobalByIndex(syms.food.index, { x: head.x + 1, y: head.y });
eq("host-written food is visible to the script", e.evalInContext("food.x + ',' + food.y"), `${head.x + 1},${head.y}`);
e.callFunction("tick", []);
eq("ate the food and scored", e.getGlobalByIndex(syms.score.index), 1);
ok("snake grew", e.getGlobalByIndex(syms.snake.index).length === 4, `len=${e.getGlobalByIndex(syms.snake.index).length}`);
const g = e.getGlobalByIndex(syms.gridItems.index);
ok("gridItems keeps re-rendering", Array.isArray(g) && g.length > 0, `len=${g && g.length}`);

console.log("— run to game over, then persist —");
let over = false;
for (let i = 0; i < 3000 && !over; i++) {
  e.callFunction("tick", []);
  over = e.getGlobalByIndex(syms.gameOver.index) === true;
}
ok("reached game over", over);
eq("overlay text set", typeof e.getGlobalByIndex(syms.overlayText.index), "string");
ok("high score written to localStorage", "softn-snake-highscore" in store, JSON.stringify(store));

console.log("— reload persists the high score —");
const e2 = new Engine();
e2.setSyncHostCapabilities(["ls.getItem", "ls.setItem", "ls.removeItem", "ls.clear"]);
e2.setLocalStorageBridge(ls);
const s2 = e2.initScript(src);
e2.callFunction("_init", []);
eq("highScore restored", e2.getGlobalByIndex(s2.highScore.index), parseInt(store["softn-snake-highscore"], 10));

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
