"use strict";

// A warm-server scenario in one long-lived VM. Setup and 12k warmup requests
// happen once; the same closures, route table, caches and session objects then
// process sustained traffic with varied request shapes.
(function main() {
  const warmupRequests = 12000;
  const measuredRequests = 240000;

  function makeRouter() {
    const routes = new Map();
    const sessions = new Map();
    const cache = new Map();
    let handled = 0;

    function use(path, handler) {
      routes.set(path, handler);
    }

    function sessionFor(id) {
      let session = sessions.get(id);
      if (session === undefined) {
        session = { id, hits: 0, score: id & 255, lastPath: "" };
        sessions.set(id, session);
      }
      return session;
    }

    use("/user", (request, session) => ({
      status: 200,
      body: "user:" + request.user + ":" + session.hits,
      score: (session.score + request.delta) | 0
    }));
    use("/search", (request, session) => {
      const key = request.query + ":" + (request.page | 0);
      let body = cache.get(key);
      if (body === undefined) {
        body = request.query.toUpperCase() + "@" + request.page;
        cache.set(key, body);
      }
      return { body, score: session.score ^ body.length, status: 200 };
    });
    use("/event", (request, session) => {
      session.score = Math.imul(session.score ^ request.code, 33) | 0;
      return { status: request.ok ? 202 : 400, score: session.score, body: request.ok ? "ok" : "bad" };
    });

    return function handle(request) {
      handled++;
      const session = sessionFor(request.sessionId);
      session.hits++;
      session.lastPath = request.path;
      const handler = routes.get(request.path);
      if (handler === undefined) return { status: 404, score: handled, body: "missing" };
      const response = handler(request, session);
      if ((handled & 4095) === 0 && cache.size > 512) cache.clear();
      return response;
    };
  }

  function requestFor(serial) {
    const route = serial % 4;
    if (route === 0) {
      return { path: "/user", sessionId: serial & 2047, user: "u" + (serial & 255), delta: serial & 31 };
    }
    if (route === 1) {
      return { sessionId: serial & 2047, query: "q" + (serial & 127), path: "/search", page: serial & 7 };
    }
    if (route === 2) {
      return { code: serial & 1023, ok: (serial & 15) !== 0, sessionId: serial & 2047, path: "/event" };
    }
    return { extra: serial, path: "/missing", sessionId: serial & 2047 };
  }

  const handle = makeRouter();
  let checksum = 0;
  for (let i = 0; i < warmupRequests; i++) handle(requestFor(i));
  for (let i = 0; i < measuredRequests; i++) {
    const response = handle(requestFor(i + warmupRequests));
    checksum = (Math.imul(checksum ^ response.status, 33) + response.score + response.body.length) | 0;
  }
  console.log("warm-router", checksum, warmupRequests, measuredRequests);
})();
