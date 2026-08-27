"use strict";
// Ordering litmus for an eager Promise.all resolve-element collapse.
// Every case prints a SEQUENCE; node is the oracle. The optimisation is only
// legal if every one of these is byte-identical with it on and off.
const log = [];
const L = (s) => log.push(s);
let pending = 0;
const done = () => { if (--pending === 0) console.log(log.join(",")); };

function caseAllFulfilledVsThen() {
  pending++;
  const t = [];
  // N element jobs vs M unrelated jobs queued right after: the result's
  // reaction must land behind every job queued during the element jobs.
  Promise.all([Promise.resolve(1), Promise.resolve(2), Promise.resolve(3)])
    .then(() => t.push("all"));
  Promise.resolve().then(() => t.push("g1"));
  Promise.resolve().then(() => t.push("g2"));
  Promise.resolve().then(() => t.push("g3"));
  Promise.resolve().then(() => {}).then(() => {}).then(() => {}).then(() => {
    L("A:" + t.join("|"));
    done();
  });
}

function caseFewerUnrelated() {
  pending++;
  const t = [];
  Promise.all([Promise.resolve(1), Promise.resolve(2), Promise.resolve(3)])
    .then(() => t.push("all"));
  Promise.resolve().then(() => t.push("g1"));
  Promise.resolve().then(() => {}).then(() => {}).then(() => {}).then(() => {
    L("B:" + t.join("|"));
    done();
  });
}

function caseThenAttachedLater() {
  pending++;
  const t = [];
  const p = Promise.all([Promise.resolve(1), Promise.resolve(2)]);
  // Attach in a LATER job — the result may already be settled by then.
  Promise.resolve().then(() => { t.push("g"); p.then(() => t.push("all")); });
  Promise.resolve().then(() => {}).then(() => {}).then(() => {}).then(() => {
    L("C:" + t.join("|"));
    done();
  });
}

function caseMixedPending() {
  pending++;
  const t = [];
  let release;
  const slow = new Promise((r) => { release = r; });
  Promise.all([Promise.resolve(1), slow, Promise.resolve(3)])
    .then(() => t.push("all"));
  Promise.resolve().then(() => t.push("g1"));
  Promise.resolve().then(() => { t.push("release"); release(9); });
  Promise.resolve().then(() => {}).then(() => {}).then(() => {}).then(() => {}).then(() => {
    L("D:" + t.join("|"));
    done();
  });
}

function caseInsideAJob() {
  pending++;
  const t = [];
  // The queue is NON-empty when Promise.all runs — the eager lane must be off.
  Promise.resolve().then(() => {
    Promise.all([Promise.resolve(1), Promise.resolve(2)]).then(() => t.push("all"));
    Promise.resolve().then(() => t.push("g1"));
    Promise.resolve().then(() => t.push("g2"));
  });
  Promise.resolve().then(() => {}).then(() => {}).then(() => {}).then(() => {}).then(() => {
    L("E:" + t.join("|"));
    done();
  });
}

function caseNested() {
  pending++;
  const t = [];
  Promise.all([
    Promise.all([Promise.resolve(1), Promise.resolve(2)]),
    Promise.resolve(3),
  ]).then(() => t.push("outer"));
  Promise.resolve().then(() => t.push("g1"));
  Promise.resolve().then(() => t.push("g2"));
  Promise.resolve().then(() => {}).then(() => {}).then(() => {}).then(() => {}).then(() => {
    L("F:" + t.join("|"));
    done();
  });
}

function caseEmptyAndSingle() {
  pending++;
  const t = [];
  Promise.all([]).then(() => t.push("empty"));
  Promise.all([Promise.resolve(1)]).then(() => t.push("one"));
  Promise.resolve().then(() => t.push("g1"));
  Promise.resolve().then(() => {}).then(() => {}).then(() => {}).then(() => {
    L("G:" + t.join("|"));
    done();
  });
}

function caseThenable() {
  pending++;
  const t = [];
  const thenable = { then(res) { t.push("thenCalled"); res(7); } };
  Promise.all([Promise.resolve(1), thenable, Promise.resolve(3)])
    .then(() => t.push("all"));
  Promise.resolve().then(() => t.push("g1"));
  Promise.resolve().then(() => {}).then(() => {}).then(() => {}).then(() => {}).then(() => {
    L("H:" + t.join("|"));
    done();
  });
}

function caseRejectMidway() {
  pending++;
  const t = [];
  Promise.all([Promise.resolve(1), Promise.reject(new Error("x")), Promise.resolve(3)])
    .then(() => t.push("all"), () => t.push("caught"));
  Promise.resolve().then(() => t.push("g1"));
  Promise.resolve().then(() => t.push("g2"));
  Promise.resolve().then(() => {}).then(() => {}).then(() => {}).then(() => {}).then(() => {
    L("I:" + t.join("|"));
    done();
  });
}

function caseAllSettledAndRace() {
  pending++;
  const t = [];
  Promise.allSettled([Promise.resolve(1), Promise.resolve(2)]).then(() => t.push("settled"));
  Promise.race([Promise.resolve(1), Promise.resolve(2)]).then(() => t.push("race"));
  Promise.any([Promise.resolve(1), Promise.resolve(2)]).then(() => t.push("any"));
  Promise.resolve().then(() => t.push("g1"));
  Promise.resolve().then(() => {}).then(() => {}).then(() => {}).then(() => {}).then(() => {
    L("J:" + t.join("|"));
    done();
  });
}

caseAllFulfilledVsThen();
caseFewerUnrelated();
caseThenAttachedLater();
caseMixedPending();
caseInsideAJob();
caseNested();
caseEmptyAndSingle();
caseThenable();
caseRejectMidway();
caseAllSettledAndRace();
