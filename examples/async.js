// async/await, Promise combinators, and the microtask queue.
const sleep = (ms) => new Promise((res) => setTimeout(res, ms));

async function fetchThing(id) {
  await sleep(10 - id);            // finishes out of order on purpose
  if (id === 3) throw new Error("thing 3 is broken");
  return { id, value: id * id };
}

async function main() {
  const ok = await Promise.all([1, 2].map(fetchThing));
  console.log("all:", JSON.stringify(ok));

  const settled = await Promise.allSettled([1, 3].map(fetchThing));
  for (const r of settled) {
    console.log(r.status === "fulfilled" ? `ok ${r.value.id}` : `failed: ${r.reason.message}`);
  }

  console.log("first done:", (await Promise.race([fetchThing(1), fetchThing(2)])).id);
}

main().then(() => console.log("done"));
