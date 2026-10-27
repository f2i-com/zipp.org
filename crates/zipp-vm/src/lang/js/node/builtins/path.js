// node:path — Node's POSIX and Windows path functions; the default is the
// running platform's.
(function (host) {
  "use strict";
  const isSep = (c) => c === 47 || c === 92;
  const isPosixSep = (c) => c === 47;
  const isLetter = (c) => (c >= 65 && c <= 90) || (c >= 97 && c <= 122);
  function check(p, name = "path") {
    if (typeof p !== "string") {
      throw new TypeError('The "' + name + '" argument must be of type string. Received ' + (p === null ? "null" : typeof p));
    }
  }
  // Resolve `.` and `..` in a path with no root.
  function normalizeString(path, allowAboveRoot, separator, sep) {
    let res = "";
    let lastSegmentLength = 0;
    let lastSlash = -1;
    let dots = 0;
    let code = 0;
    for (let i = 0; i <= path.length; ++i) {
      if (i < path.length) code = path.charCodeAt(i);
      else if (sep(code)) break;
      else code = 47;
      if (sep(code)) {
        if (lastSlash === i - 1 || dots === 1) {
          // empty segment or `.`
        } else if (dots === 2) {
          if (res.length < 2 || lastSegmentLength !== 2 || res.charCodeAt(res.length - 1) !== 46 || res.charCodeAt(res.length - 2) !== 46) {
            if (res.length > 2) {
              const lastSlashIndex = res.lastIndexOf(separator);
              if (lastSlashIndex === -1) {
                res = "";
                lastSegmentLength = 0;
              } else {
                res = res.slice(0, lastSlashIndex);
                lastSegmentLength = res.length - 1 - res.lastIndexOf(separator);
              }
              lastSlash = i;
              dots = 0;
              continue;
            } else if (res.length !== 0) {
              res = "";
              lastSegmentLength = 0;
              lastSlash = i;
              dots = 0;
              continue;
            }
          }
          if (allowAboveRoot) {
            res += res.length > 0 ? separator + ".." : "..";
            lastSegmentLength = 2;
          }
        } else {
          if (res.length > 0) res += separator + path.slice(lastSlash + 1, i);
          else res = path.slice(lastSlash + 1, i);
          lastSegmentLength = i - lastSlash - 1;
        }
        lastSlash = i;
        dots = 0;
      } else if (code === 46 && dots !== -1) {
        ++dots;
      } else {
        dots = -1;
      }
    }
    return res;
  }
  function format(sep, obj) {
    if (obj === null || typeof obj !== "object") {
      throw new TypeError('The "pathObject" argument must be of type object. Received ' + (obj === null ? "null" : typeof obj));
    }
    const dir = obj.dir || obj.root;
    const base = obj.base || (obj.name || "") + (obj.ext ? (obj.ext[0] === "." ? "" : ".") + obj.ext : "");
    if (!dir) return base;
    return dir === obj.root ? dir + base : dir + sep + base;
  }

  const posix = {
    sep: "/",
    delimiter: ":",
    resolve(...args) {
      let resolvedPath = "";
      let resolvedAbsolute = false;
      for (let i = args.length - 1; i >= -1 && !resolvedAbsolute; i--) {
        const path = i >= 0 ? args[i] : posixCwd();
        check(path, "paths[" + i + "]");
        if (path.length === 0) continue;
        resolvedPath = path + "/" + resolvedPath;
        resolvedAbsolute = path.charCodeAt(0) === 47;
      }
      resolvedPath = normalizeString(resolvedPath, !resolvedAbsolute, "/", isPosixSep);
      if (resolvedAbsolute) return "/" + resolvedPath;
      return resolvedPath.length > 0 ? resolvedPath : ".";
    },
    normalize(path) {
      check(path);
      if (path.length === 0) return ".";
      const isAbsolute = path.charCodeAt(0) === 47;
      const trailing = path.charCodeAt(path.length - 1) === 47;
      path = normalizeString(path, !isAbsolute, "/", isPosixSep);
      if (path.length === 0) {
        if (isAbsolute) return "/";
        return trailing ? "./" : ".";
      }
      if (trailing) path += "/";
      return isAbsolute ? "/" + path : path;
    },
    isAbsolute(path) {
      check(path);
      return path.length > 0 && path.charCodeAt(0) === 47;
    },
    join(...args) {
      if (args.length === 0) return ".";
      let joined;
      for (let i = 0; i < args.length; ++i) {
        const arg = args[i];
        check(arg);
        if (arg.length > 0) joined = joined === undefined ? arg : joined + "/" + arg;
      }
      return joined === undefined ? "." : posix.normalize(joined);
    },
    relative(from, to) {
      check(from, "from");
      check(to, "to");
      if (from === to) return "";
      from = posix.resolve(from);
      to = posix.resolve(to);
      if (from === to) return "";
      const fromStart = 1, fromEnd = from.length, fromLen = fromEnd - fromStart;
      const toStart = 1, toLen = to.length - toStart;
      const length = fromLen < toLen ? fromLen : toLen;
      let lastCommonSep = -1;
      let i = 0;
      for (; i < length; i++) {
        const fromCode = from.charCodeAt(fromStart + i);
        if (fromCode !== to.charCodeAt(toStart + i)) break;
        else if (fromCode === 47) lastCommonSep = i;
      }
      if (i === length) {
        if (toLen > length) {
          if (to.charCodeAt(toStart + i) === 47) return to.slice(toStart + i + 1);
          if (i === 0) return to.slice(toStart + i);
        } else if (fromLen > length) {
          if (from.charCodeAt(fromStart + i) === 47) lastCommonSep = i;
          else if (i === 0) lastCommonSep = 0;
        }
      }
      let out = "";
      for (i = fromStart + lastCommonSep + 1; i <= fromEnd; ++i) {
        if (i === fromEnd || from.charCodeAt(i) === 47) out += out.length === 0 ? ".." : "/..";
      }
      return out + to.slice(toStart + lastCommonSep);
    },
    toNamespacedPath(path) {
      return path;
    },
    dirname(path) {
      check(path);
      if (path.length === 0) return ".";
      const hasRoot = path.charCodeAt(0) === 47;
      let end = -1;
      let matchedSlash = true;
      for (let i = path.length - 1; i >= 1; --i) {
        if (path.charCodeAt(i) === 47) {
          if (!matchedSlash) {
            end = i;
            break;
          }
        } else {
          matchedSlash = false;
        }
      }
      if (end === -1) return hasRoot ? "/" : ".";
      if (hasRoot && end === 1) return "//";
      return path.slice(0, end);
    },
    basename(path, suffix) {
      if (suffix !== undefined) check(suffix, "suffix");
      check(path);
      return basenameWith(path, suffix, isPosixSep, 0);
    },
    extname(path) {
      check(path);
      return extnameWith(path, isPosixSep, 0);
    },
    format: (obj) => format("/", obj),
    parse(path) {
      check(path);
      return parseWith(path, isPosixSep, path.length > 0 && path.charCodeAt(0) === 47 ? 1 : 0, "/");
    },
    matchesGlob() {
      throw new Error("zipp: path.matchesGlob is not supported");
    },
  };

  function basenameWith(path, suffix, sep, rootEnd) {
    let start = rootEnd;
    let end = -1;
    let matchedSlash = true;
    if (suffix !== undefined && suffix.length > 0 && suffix.length <= path.length) {
      if (suffix === path) return "";
      let extIdx = suffix.length - 1;
      let firstNonSlashEnd = -1;
      for (let i = path.length - 1; i >= start; --i) {
        const code = path.charCodeAt(i);
        if (sep(code)) {
          if (!matchedSlash) {
            start = i + 1;
            break;
          }
        } else {
          if (firstNonSlashEnd === -1) {
            matchedSlash = false;
            firstNonSlashEnd = i + 1;
          }
          if (extIdx >= 0) {
            if (code === suffix.charCodeAt(extIdx)) {
              if (--extIdx === -1) end = i;
            } else {
              extIdx = -1;
              end = firstNonSlashEnd;
            }
          }
        }
      }
      if (start === end) end = firstNonSlashEnd;
      else if (end === -1) end = path.length;
      return path.slice(start, end);
    }
    for (let i = path.length - 1; i >= start; --i) {
      if (sep(path.charCodeAt(i))) {
        if (!matchedSlash) {
          start = i + 1;
          break;
        }
      } else if (end === -1) {
        matchedSlash = false;
        end = i + 1;
      }
    }
    if (end === -1) return "";
    return path.slice(start, end);
  }
  function extnameWith(path, sep, start) {
    let startDot = -1;
    let startPart = start;
    let end = -1;
    let matchedSlash = true;
    let preDotState = 0;
    for (let i = path.length - 1; i >= start; --i) {
      const code = path.charCodeAt(i);
      if (sep(code)) {
        if (!matchedSlash) {
          startPart = i + 1;
          break;
        }
        continue;
      }
      if (end === -1) {
        matchedSlash = false;
        end = i + 1;
      }
      if (code === 46) {
        if (startDot === -1) startDot = i;
        else if (preDotState !== 1) preDotState = 1;
      } else if (startDot !== -1) {
        preDotState = -1;
      }
    }
    if (startDot === -1 || end === -1 || preDotState === 0 || (preDotState === 1 && startDot === end - 1 && startDot === startPart + 1)) {
      return "";
    }
    return path.slice(startDot, end);
  }
  function parseWith(path, sep, rootEnd, separator) {
    const ret = { root: "", dir: "", base: "", ext: "", name: "" };
    if (path.length === 0) return ret;
    ret.root = path.slice(0, rootEnd);
    let startDot = -1;
    let startPart = rootEnd;
    let end = -1;
    let matchedSlash = true;
    let i = path.length - 1;
    let preDotState = 0;
    for (; i >= rootEnd; --i) {
      const code = path.charCodeAt(i);
      if (sep(code)) {
        if (!matchedSlash) {
          startPart = i + 1;
          break;
        }
        continue;
      }
      if (end === -1) {
        matchedSlash = false;
        end = i + 1;
      }
      if (code === 46) {
        if (startDot === -1) startDot = i;
        else if (preDotState !== 1) preDotState = 1;
      } else if (startDot !== -1) {
        preDotState = -1;
      }
    }
    if (end !== -1) {
      const start = startPart === 0 && rootEnd > 0 ? rootEnd : startPart;
      if (startDot === -1 || preDotState === 0 || (preDotState === 1 && startDot === end - 1 && startDot === startPart + 1)) {
        ret.base = ret.name = path.slice(start, end);
      } else {
        ret.name = path.slice(start, startDot);
        ret.base = path.slice(start, end);
        ret.ext = path.slice(startDot, end);
      }
    }
    if (startPart > rootEnd) ret.dir = path.slice(0, startPart - 1);
    else ret.dir = ret.root;
    void separator;
    return ret;
  }
  function posixCwd() {
    const c = host.cwd();
    return host.platform === "win32" ? c.replace(/\\/g, "/").replace(/^[A-Za-z]:/, "") : c;
  }

  // Windows: a root is `C:\`, `C:`, `\`, or `\\server\share\`.
  function rootLength(path) {
    const len = path.length;
    if (len === 0) return [0, "", false];
    const code = path.charCodeAt(0);
    if (len === 1) return isSep(code) ? [1, path, true] : [0, "", false];
    if (isSep(code)) {
      if (isSep(path.charCodeAt(1))) {
        let j = 2, last = j;
        while (j < len && !isSep(path.charCodeAt(j))) j++;
        if (j < len && j !== last) {
          last = j;
          while (j < len && isSep(path.charCodeAt(j))) j++;
          if (j < len && j !== last) {
            last = j;
            while (j < len && !isSep(path.charCodeAt(j))) j++;
            if (j === len) return [j, "\\\\" + path.slice(2, len).replace(/[\\/]+/, "\\"), true];
            if (j !== last) return [j, "\\\\" + path.slice(2, j).replace(/[\\/]+/, "\\"), true];
          }
        }
      }
      return [1, "\\", true];
    }
    if (isLetter(code) && path.charCodeAt(1) === 58) {
      const device = path.slice(0, 2);
      if (len > 2 && isSep(path.charCodeAt(2))) return [3, device, true];
      return [2, device, false];
    }
    return [0, "", false];
  }
  const win32 = {
    sep: "\\",
    delimiter: ";",
    resolve(...args) {
      let resolvedDevice = "";
      let resolvedTail = "";
      let resolvedAbsolute = false;
      for (let i = args.length - 1; i >= -1; i--) {
        let path;
        if (i >= 0) {
          path = args[i];
          check(path, "paths[" + i + "]");
          if (path.length === 0) continue;
        } else if (resolvedDevice.length === 0) {
          path = host.cwd();
        } else {
          path = host.cwd();
          if (path === undefined || (path.slice(0, 2).toLowerCase() !== resolvedDevice.toLowerCase() && path.charCodeAt(2) === 92)) {
            path = resolvedDevice + "\\";
          }
        }
        const [rootEnd, device, isAbsolute] = rootLength(path);
        if (device.length > 0) {
          if (resolvedDevice.length > 0) {
            if (device.toLowerCase() !== resolvedDevice.toLowerCase()) continue;
          } else {
            resolvedDevice = device;
          }
        }
        if (resolvedAbsolute) {
          if (resolvedDevice.length > 0) break;
        } else {
          resolvedTail = path.slice(rootEnd) + "\\" + resolvedTail;
          resolvedAbsolute = isAbsolute;
          if (isAbsolute && resolvedDevice.length > 0) break;
        }
      }
      resolvedTail = normalizeString(resolvedTail, !resolvedAbsolute, "\\", isSep);
      return resolvedAbsolute ? resolvedDevice + "\\" + resolvedTail : resolvedDevice + resolvedTail || ".";
    },
    normalize(path) {
      check(path);
      if (path.length === 0) return ".";
      const [rootEnd, device, isAbsolute] = rootLength(path);
      let tail = rootEnd < path.length ? normalizeString(path.slice(rootEnd), !isAbsolute, "\\", isSep) : "";
      if (tail.length === 0 && !isAbsolute) tail = ".";
      if (tail.length > 0 && isSep(path.charCodeAt(path.length - 1))) tail += "\\";
      if (device === undefined) return isAbsolute ? "\\" + tail : tail;
      return isAbsolute ? device + "\\" + tail : device + tail;
    },
    isAbsolute(path) {
      check(path);
      const len = path.length;
      if (len === 0) return false;
      const code = path.charCodeAt(0);
      return isSep(code) || (len > 2 && isLetter(code) && path.charCodeAt(1) === 58 && isSep(path.charCodeAt(2)));
    },
    join(...args) {
      if (args.length === 0) return ".";
      let joined, firstPart;
      for (let i = 0; i < args.length; ++i) {
        const arg = args[i];
        check(arg);
        if (arg.length > 0) {
          if (joined === undefined) joined = firstPart = arg;
          else joined += "\\" + arg;
        }
      }
      if (joined === undefined) return ".";
      let needsReplace = true;
      let slashCount = 0;
      if (isSep(firstPart.charCodeAt(0))) {
        ++slashCount;
        const firstLen = firstPart.length;
        if (firstLen > 1 && isSep(firstPart.charCodeAt(1))) {
          ++slashCount;
          if (firstLen > 2) {
            if (isSep(firstPart.charCodeAt(2))) ++slashCount;
            else needsReplace = false;
          }
        }
      }
      if (needsReplace) {
        while (slashCount < joined.length && isSep(joined.charCodeAt(slashCount))) slashCount++;
        if (slashCount >= 2) joined = "\\" + joined.slice(slashCount);
      }
      return win32.normalize(joined);
    },
    relative(from, to) {
      check(from, "from");
      check(to, "to");
      if (from === to) return "";
      const fromOrig = win32.resolve(from);
      const toOrig = win32.resolve(to);
      if (fromOrig === toOrig) return "";
      from = fromOrig.toLowerCase();
      to = toOrig.toLowerCase();
      if (from === to) return "";
      let fromStart = 0;
      while (fromStart < from.length && from.charCodeAt(fromStart) === 92) fromStart++;
      let fromEnd = from.length;
      while (fromEnd - 1 > fromStart && from.charCodeAt(fromEnd - 1) === 92) fromEnd--;
      const fromLen = fromEnd - fromStart;
      let toStart = 0;
      while (toStart < to.length && to.charCodeAt(toStart) === 92) toStart++;
      let toEnd = to.length;
      while (toEnd - 1 > toStart && to.charCodeAt(toEnd - 1) === 92) toEnd--;
      const toLen = toEnd - toStart;
      const length = fromLen < toLen ? fromLen : toLen;
      let lastCommonSep = -1;
      let i = 0;
      for (; i < length; i++) {
        const fromCode = from.charCodeAt(fromStart + i);
        if (fromCode !== to.charCodeAt(toStart + i)) break;
        else if (fromCode === 92) lastCommonSep = i;
      }
      if (i !== length) {
        if (lastCommonSep === -1) return toOrig;
      } else {
        if (toLen > length) {
          if (to.charCodeAt(toStart + i) === 92) return toOrig.slice(toStart + i + 1);
          if (i === 2) return toOrig.slice(toStart + i);
        }
        if (fromLen > length) {
          if (from.charCodeAt(fromStart + i) === 92) lastCommonSep = i;
          else if (i === 2) lastCommonSep = 3;
        }
        if (lastCommonSep === -1) lastCommonSep = 0;
      }
      let out = "";
      for (i = fromStart + lastCommonSep + 1; i <= fromEnd; ++i) {
        if (i === fromEnd || from.charCodeAt(i) === 92) out += out.length === 0 ? ".." : "\\..";
      }
      toStart += lastCommonSep;
      if (out.length > 0) return out + toOrig.slice(toStart, toEnd);
      if (toOrig.charCodeAt(toStart) === 92) ++toStart;
      return toOrig.slice(toStart, toEnd);
    },
    toNamespacedPath(path) {
      if (typeof path !== "string" || path.length === 0) return path;
      const resolved = win32.resolve(path);
      if (resolved.length <= 2) return path;
      if (resolved.charCodeAt(0) === 92) {
        if (resolved.charCodeAt(1) === 92) {
          const code = resolved.charCodeAt(2);
          if (code !== 63 && code !== 46) return "\\\\?\\UNC\\" + resolved.slice(2);
        }
      } else if (isLetter(resolved.charCodeAt(0)) && resolved.charCodeAt(1) === 58 && resolved.charCodeAt(2) === 92) {
        return "\\\\?\\" + resolved;
      }
      return path;
    },
    dirname(path) {
      check(path);
      const len = path.length;
      if (len === 0) return ".";
      const [rootEnd] = rootLength(path);
      let end = -1;
      let matchedSlash = true;
      for (let i = len - 1; i >= rootEnd; --i) {
        if (isSep(path.charCodeAt(i))) {
          if (!matchedSlash) {
            end = i;
            break;
          }
        } else {
          matchedSlash = false;
        }
      }
      if (end === -1) {
        if (rootEnd === 0) return ".";
        end = rootEnd;
      }
      return path.slice(0, end);
    },
    basename(path, suffix) {
      if (suffix !== undefined) check(suffix, "suffix");
      check(path);
      const start = path.length >= 2 && isLetter(path.charCodeAt(0)) && path.charCodeAt(1) === 58 ? 2 : 0;
      return basenameWith(path, suffix, isSep, start);
    },
    extname(path) {
      check(path);
      const start = path.length >= 2 && path.charCodeAt(1) === 58 && isLetter(path.charCodeAt(0)) ? 2 : 0;
      return extnameWith(path, isSep, start);
    },
    format: (obj) => format("\\", obj),
    parse(path) {
      check(path);
      const [rootEnd] = rootLength(path);
      return parseWith(path, isSep, rootEnd, "\\");
    },
    matchesGlob: posix.matchesGlob,
  };
  posix.win32 = win32.win32 = win32;
  posix.posix = win32.posix = posix;
  return host.platform === "win32" ? win32 : posix;
})
