// node:url — file URLs and the legacy parse/format. (WHATWG `URL` is the
// global's, when it has one.)
(function (host, builtin) {
  "use strict";
  const win = host.platform === "win32";
  function fileURLToPath(url) {
    const href = typeof url === "string" ? url : url && url.href;
    if (typeof href !== "string" || !href.startsWith("file:")) {
      const e = new TypeError("The URL must be of scheme file");
      e.code = "ERR_INVALID_URL_SCHEME";
      throw e;
    }
    let rest = href.slice(5);
    let host_ = "";
    if (rest.startsWith("//")) {
      const end = rest.indexOf("/", 2);
      host_ = end < 0 ? rest.slice(2) : rest.slice(2, end);
      rest = end < 0 ? "/" : rest.slice(end);
    }
    rest = rest.replace(/[?#].*$/, "");
    const path = decodeURIComponent(rest);
    if (win) {
      if (host_ && host_ !== "localhost") return "\\\\" + host_ + path.replace(/\//g, "\\");
      const p = path.replace(/^\/([A-Za-z]:)/, "$1");
      return p.replace(/\//g, "\\");
    }
    return path;
  }
  function pathToFileURL(path) {
    const pathMod = builtin("path");
    let p = pathMod.resolve(String(path));
    if (win) p = "/" + p.replace(/\\/g, "/");
    const href = "file://" + encodeURI(p).replace(/[?#]/g, encodeURIComponent);
    if (typeof URL === "function") return new URL(href);
    return { href, protocol: "file:", pathname: encodeURI(p), toString() { return href; }, toJSON() { return href; } };
  }
  function parse(url) {
    const m = /^(?:([a-z][a-z0-9+.-]*:))?(?:\/\/(?:([^@/]*)@)?([^/:?#]*)(?::(\d+))?)?([^?#]*)(\?[^#]*)?(#.*)?$/i.exec(String(url));
    if (!m) return null;
    const [, protocol = null, auth = null, hostname = null, port = null, pathname = null, search = null, hash = null] = m;
    const host_ = hostname ? hostname + (port ? ":" + port : "") : null;
    return {
      protocol: protocol && protocol.toLowerCase(), slashes: url.includes("//") || null, auth, host: host_ && host_.toLowerCase(),
      port, hostname: hostname && hostname.toLowerCase(), hash, search, query: search ? search.slice(1) : null,
      pathname: pathname || (host_ ? "/" : null), path: (pathname || "") + (search || "") || null, href: String(url),
    };
  }
  function format(obj) {
    if (typeof obj === "string") return obj;
    if (obj && typeof obj.href === "string" && !obj.protocol) return obj.href;
    let s = obj.protocol || "";
    if (obj.slashes || obj.host || obj.hostname) s += "//";
    if (obj.auth) s += obj.auth + "@";
    s += obj.host || (obj.hostname || "") + (obj.port ? ":" + obj.port : "");
    s += obj.pathname || "";
    s += obj.search || (obj.query ? "?" + obj.query : "");
    s += obj.hash || "";
    return s;
  }
  return { fileURLToPath, pathToFileURL, parse, format, resolve: (from, to) => (typeof URL === "function" ? new URL(to, from).href : to), URL: globalThis.URL, URLSearchParams: globalThis.URLSearchParams };
})
