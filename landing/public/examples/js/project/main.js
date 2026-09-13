// The JavaScript twin of examples/python/project: the same bouncing balls,
// written for the playground's JavaScript mode. Every file in the folder is
// run in one shared global scope (the entry last), like successive <script>
// tags, so `stepBall` and `inside` from physics.js are plain globals here.
// The `ui` object is provided by the playground; the engine itself has no
// drawing API.
"use strict";

const W = 480;
const H = 320;
const balls = []; // {x, y, vx, vy}
let paddle = W / 2 - 40;
let frames = 0;

function spawn(x, y) {
  if (balls.length < 40) balls.push({ x, y, vx: 3, vy: -4 });
}

spawn(W / 2, H / 2);
ui.canvas(W, H);
console.log("balls ready:", balls.length);

function on_click(x, y) {
  spawn(x, y);
}

function on_key(key) {
  if (key === " ") spawn(W / 2, 40);
}

function update() {
  if (ui.key("ArrowLeft")) paddle -= 6;
  if (ui.key("ArrowRight")) paddle += 6;
  for (const b of balls) {
    stepBall(b, W, H);
    if (inside(b.x, b.y, paddle, H - 24, 80, 12) && b.vy > 0) b.vy = -b.vy;
  }
  frames += 1;
}

function draw() {
  ui.clear("#10141c");
  for (const b of balls) ui.circle(b.x, b.y, 8, "#ffcc00");
  ui.rect(paddle, H - 24, 80, 12, "#4cc2ff");
  ui.font(14);
  ui.text(10, 20, `balls: ${balls.length}   frame: ${frames}`, "#e6edf3");
  ui.text(10, H - 6, "click to add a ball, arrows move the paddle, space serves", "#8b949e");
  if (ui.button(W - 90, 8, 82, 26, "reset")) balls.length = 1;
}
