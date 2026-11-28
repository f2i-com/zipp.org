// One application workload, run once cold and then `iters` times warm, in the
// engine that runs this file. Prints one JSON line.
//
//   node run.mjs <linkedom|defuddle|webforai> [iters]
//   zipp run run.mjs <linkedom|defuddle|webforai> [iters]
//
// The three packages are used as published (see package.json): nothing here
// or in them is adapted to ZIPP. `bench.py` runs this file under each engine
// and compares the lines.
import { Defuddle } from 'defuddle/node';
import { parseHTML } from 'linkedom';
import { htmlToMarkdown } from 'webforai';

const kind = process.argv[2] || 'defuddle';
const iters = Number(process.argv[3] || 15);

// A 48-section article: 21,977 bytes of HTML, built here so the input is the
// same bytes for every engine.
const html = '<!doctype html><html><head><title>Engineering notes</title></head><body><nav>Navigation <a href="/index">Index</a></nav><main><article><h1>Engineering notes</h1>' +
  Array.from({ length: 48 }, (_, i) =>
    '<section class="exp" data-id="' + i + '"><h2>Experiment ' + i + '</h2><p>We measured the response of the circuit over a range of frequencies. The amplifier gain changes as the load and the supply voltage vary. The results illustrate the interaction between impedance and bandwidth &amp; gain &lt;100&gt;.</p><ul><li>Frequency: ' + (100 + i) + ' kHz</li><li><a class="note-link" href="/notes/' + i + '">Measurement notes</a></li></ul><pre><code>sample = input * gain;</code></pre></section>'
  ).join('') + '</article></main><footer>Footer</footer></body></html>';
const url = 'https://example.com/article';

const workloads = {
  // Parse the page, then two selector queries over the tree.
  linkedom() {
    const { document } = parseHTML(html);
    const heads = document.querySelectorAll('section.exp > h2');
    const links = document.querySelectorAll('ul li a.note-link[href^="/notes/"]');
    let out = '';
    for (let i = 0; i < heads.length; i++) out += heads[i].textContent + '|' + links[i].getAttribute('href') + '\n';
    return out;
  },
  // Parse the page, find the article and write it as Markdown.
  async defuddle() {
    const { document } = parseHTML(html);
    const result = await Defuddle(document, url, { markdown: true, useAsync: false });
    return result?.contentMarkdown ?? result?.content ?? '';
  },
  // HTML to Markdown through webforai's own parser and pipeline.
  webforai() {
    return htmlToMarkdown(html, { baseUrl: url });
  },
};

const work = workloads[kind];
if (!work) throw new Error('unknown workload ' + kind);

const now = () => Number(process.hrtime.bigint()) / 1e6;
const start = now();
const reference = await work();
const firstMs = now() - start;
const warm = [];
for (let i = 0; i < iters; i++) {
  const t = now();
  const result = await work();
  warm.push(now() - t);
  if (result !== reference) throw new Error('the output changed between runs');
}
// `head` and `tail` let the runner check that every engine wrote the same
// text without printing 16 KB of it.
console.log(JSON.stringify({
  kind,
  outLen: reference.length,
  head: reference.slice(0, 80),
  tail: reference.slice(-80),
  firstMs,
  warm,
}));
