// node show.cjs FINDINGS.jsonl [LABEL_A [LABEL_B]] : print each finding of
// fuzz.cjs with the case and the first difference between two outputs (by
// default Node and the reference ZIPP engine), error messages stripped when
// one side is Node.
'use strict';
const fs = require('node:fs');
const [file, la = 'node', lb] = process.argv.slice(2);
const strip = l => (l === undefined ? l : (l.indexOf(' || ') < 0 ? l : l.slice(0, l.indexOf(' || '))));
for (const line of fs.readFileSync(file, 'utf8').split('\n').filter(Boolean)) {
  const r = JSON.parse(line);
  const labels = Object.keys(r).filter(k => !['seed', 'i', 'case', 'node', 'nodeDiff', 'change'].includes(k));
  const b = lb || labels[0];
  let x = r[la], y = r[b];
  if (la === 'node' || b === 'node') { x = strip(x); y = strip(y); }
  if (x === y) continue;
  let k = 0;
  while (k < x.length && k < y.length && x[k] === y[k]) k++;
  const from = Math.max(0, k - 60);
  const c = r.case;
  console.log(`${r.seed}/${r.i} ${JSON.stringify(c[0])} /${c[1]} subject(${c[2].length})=${JSON.stringify(c[2].slice(0, 60))}${c[2].length > 60 ? '...' : ''} lastIndex=${c[3]}`);
  console.log(`  ${la}: ...${x.slice(from, k + 120)}`);
  console.log(`  ${b}: ...${y.slice(from, k + 120)}`);
}
