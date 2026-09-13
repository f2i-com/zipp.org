// Physics helpers shared with main.js through the playground's single
// global scope (this file is listed before the entry).
"use strict";

function stepBall(b, w, h) {
  b.x += b.vx;
  b.y += b.vy;
  if (b.x < 8 || b.x > w - 8) b.vx = -b.vx;
  if (b.y < 8 || b.y > h - 8) b.vy = -b.vy;
}

function inside(px, py, x, y, w, h) {
  return x <= px && px < x + w && y <= py && py < y + h;
}
