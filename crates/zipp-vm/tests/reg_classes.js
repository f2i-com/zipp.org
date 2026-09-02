var src = "";
for (var k = 0; k < 40; k++) src += "let a" + k + " = 1; // line\n/* block */ if (a" + k + " > 0) { a" + k + " += 2; }\n";
var kinds = [], starts = [], ends = [];
function tokenize() {
  kinds = []; starts = []; ends = [];
  var i = 0, n = src.length;
  while (i < n) {
    var c = src.charCodeAt(i);
    if (c === 32 || c === 10 || c === 9) { i++; continue; }
    var st = i;
    if (c === 47) {
      var cc = src.charCodeAt(i + 1);
      if (cc === 47) {
        i += 2;
        while (i < n && src.charCodeAt(i) !== 10) i++;
        kinds.push(5); starts.push(st); ends.push(i); continue;
      }
      if (cc === 42) {
        i += 2;
        while (i < n && !(src.charCodeAt(i) === 42 && src.charCodeAt(i + 1) === 47)) i++;
        i += 2;
        kinds.push(5); starts.push(st); ends.push(i); continue;
      }
    }
    if ((c >= 97 && c <= 122) || (c >= 65 && c <= 90) || c === 95) {
      i++;
      while (i < n) { var d = src.charCodeAt(i); if (!((d >= 97 && d <= 122) || (d >= 48 && d <= 57))) break; i++; }
      kinds.push(1); starts.push(st); ends.push(i); continue;
    }
    if (c >= 48 && c <= 57) { while (i < n && src.charCodeAt(i) >= 48 && src.charCodeAt(i) <= 57) i++; kinds.push(2); starts.push(st); ends.push(i); continue; }
    kinds.push(3); starts.push(st); ends.push(i + 1); i++;
  }
  return kinds.length;
}
var total = 0, h = 0;
for (var r = 0; r < 200; r++) { total += tokenize(); for (var j = 0; j < kinds.length; j++) h = (h * 31 + kinds[j] * 7 + starts[j] + ends[j]) | 0; }
console.log(total + ":" + h);
// A DataView swizzle whose endian flag is a comparison written straight into
// the call's argument window: the planner fuses the flag (B263 admits the
// dead-flag shape, where the argument slot has no other definition).
var dvbuf = new ArrayBuffer(1024);
var dv = new DataView(dvbuf, 0, 1024);
for (var q = 0; q < 256; q++) dv.setInt32(q * 4, (q * 2654435761) | 0, true);
var bsum = 0;
for (var rr = 0; rr < 300; rr++) {
  for (var o = 0; o < 1024; o += 4) {
    var le = (o >> 2) & 1;
    var v = dv.getUint32(o, le === 1);
    bsum = (bsum + (v >>> 24) + (v & 255) + dv.getUint16(o, le === 0) + dv.getInt8(o + 2)) | 0;
  }
}
console.log("dv:" + bsum);
