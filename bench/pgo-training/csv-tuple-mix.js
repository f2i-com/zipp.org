"use strict";

// Independent quoted-CSV state machine.  Token boundaries live in ONE array
// of tuple objects; this intentionally avoids the scored source scanner's three
// parallel integer arrays, adjacent triple-push shape, delimiter predicate, and
// two-lane rotate/add checksum.
var state = 0x2e5aa61d;
function random(bound) {
  state = (Math.imul(state ^ (state >>> 15), 1588635695) + 2147483629) | 0;
  return (state >>> 0) % bound;
}

var SYLLABLES = ["amber", "birch", "cobalt", "delta", "ember", "fjord", "grove", "harbor"];
function cellText(row, column) {
  var text = SYLLABLES[(row * 3 + column + random(SYLLABLES.length)) & 7] + "-" + random(100000);
  if ((row + column) % 11 === 0) text += ",zone";
  if ((row * 5 + column) % 17 === 0) text += ' "quoted"';
  return text;
}

function quoteCell(text) {
  if (text.indexOf(",") < 0 && text.indexOf('"') < 0) return text;
  var pieces = ['"'];
  for (var i = 0; i < text.length; i++) {
    var ch = text[i];
    pieces.push(ch === '"' ? '""' : ch);
  }
  pieces.push('"');
  return pieces.join("");
}

var ROWS = 14000;
var COLUMNS = 5;
var rows = new Array(ROWS);
for (var row = 0; row < ROWS; row++) {
  var cells = new Array(COLUMNS);
  for (var column = 0; column < COLUMNS; column++) cells[column] = quoteCell(cellText(row, column));
  rows[row] = cells.join(",");
}
var csv = rows.join("\r\n");

function scanCsv(text) {
  var tuples = [];
  var begin = 0;
  var record = 0;
  var quoted = false;
  var cursor = 0;
  while (cursor <= text.length) {
    var code = cursor < text.length ? text.charCodeAt(cursor) : 10;
    if (quoted) {
      if (code === 34) {
        if (cursor + 1 < text.length && text.charCodeAt(cursor + 1) === 34) {
          cursor += 2;
          continue;
        }
        quoted = false;
      }
      cursor++;
      continue;
    }
    if (code === 34 && cursor === begin) {
      quoted = true;
      cursor++;
      continue;
    }
    if (code === 44 || code === 10) {
      var limit = code === 10 && cursor > begin && text.charCodeAt(cursor - 1) === 13 ? cursor - 1 : cursor;
      tuples.push([record, begin, limit]);
      begin = cursor + 1;
      if (code === 10) record++;
    }
    cursor++;
  }
  return tuples;
}

function foldTuple(text, tuple, lanes) {
  var begin = tuple[1], limit = tuple[2];
  if (begin < limit && text.charCodeAt(begin) === 34) { begin++; limit--; }
  var left = lanes[0];
  var right = lanes[1];
  left = (left + Math.imul(tuple[0] + 0x632be5ab, 0x7feb352d)) | 0;
  left = (left + (left >>> 6)) | 0;
  right = (right ^ left ^ (limit - begin)) | 0;
  for (var i = begin; i < limit; i++) {
    var code = text.charCodeAt(i);
    if (code === 34 && i + 1 < limit && text.charCodeAt(i + 1) === 34) i++;
    left = (left + code + Math.imul(right ^ code, 0x165667b1)) | 0;
    left = (left << 11) | (left >>> 21);
    right = (right + Math.imul(code + i, 0x27d4eb2f)) | 0;
    right = (right << 3) | (right >>> 29);
  }
  lanes[0] = left;
  lanes[1] = right;
}

var tuples = scanCsv(csv);
var lanes = [0x2e5aa61d, 0x2468ace0];
var sampledLength = 0;
for (var pass = 0; pass < 4; pass++) {
  for (var i = pass; i < tuples.length; i += 4) {
    foldTuple(csv, tuples[i], lanes);
    sampledLength += tuples[i][2] - tuples[i][1];
  }
}
var hash = (lanes[0] ^ lanes[1] ^ (lanes[0] >>> 9) ^ (lanes[1] << 5)) >>> 0;
var summary = "csv-tuples=" + csv.length + ":" + tuples.length + ":" + sampledLength + ":" + hash;
var EXPECTED = "csv-tuples=960995:70000:876997:158174722";
if (summary !== EXPECTED) throw new Error("CSV tuple PGO checksum mismatch: " + summary);
console.log(summary);
