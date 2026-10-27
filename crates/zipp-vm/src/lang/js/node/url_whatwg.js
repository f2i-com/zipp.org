// WHATWG `URL` and `URLSearchParams` (see mod.rs): the URL Standard's basic URL
// parser, serializer and setters, and the form-urlencoded query list, shaped as
// Node's globals are (error codes, enumerable accessors, inspect output).
(function () {
  "use strict";
  const fromCharCode = String.fromCharCode;
  const fromCodePoint = String.fromCodePoint;
  const defineProperty = Object.defineProperty;
  const getOwnPropertyDescriptor = Object.getOwnPropertyDescriptor;
  const ReflectOwnKeys = Reflect.ownKeys;
  const ArrayIsArray = Array.isArray;
  const SymbolIterator = Symbol.iterator;
  const inspectCustom = Symbol.for("nodejs.util.inspect.custom");
  const hasNormalize = typeof "".normalize === "function";

  // ---- strings --------------------------------------------------------------
  // Every input is a USVString: lone surrogates become U+FFFD before parsing.
  function fixSurrogates(s) {
    let out = "", last = 0;
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if (c < 0xD800 || c > 0xDFFF) continue;
      if (c <= 0xDBFF && i + 1 < s.length) {
        const d = s.charCodeAt(i + 1);
        if (d >= 0xDC00 && d <= 0xDFFF) { i++; continue; }
      }
      out += s.slice(last, i) + "\uFFFD";
      last = i + 1;
    }
    return last === 0 ? s : out + s.slice(last);
  }
  const usv = typeof "".isWellFormed === "function" && typeof "".toWellFormed === "function"
    ? (s) => (s.isWellFormed() ? s : s.toWellFormed())
    : fixSurrogates;

  function fromCharCodes(units) {
    if (units.length < 4096) return fromCharCode.apply(null, units);
    let out = "";
    for (let i = 0; i < units.length; i += 4096) out += fromCharCode.apply(null, units.slice(i, i + 4096));
    return out;
  }

  // Percent-encode sets as bits over ASCII; every non-ASCII code point is in all
  // of them. FORM (application/x-www-form-urlencoded) also writes space as "+".
  const C0_SET = 1, FRAGMENT_SET = 2, QUERY_SET = 4, SPECIAL_QUERY_SET = 8, PATH_SET = 16, USERINFO_SET = 32, FORM_SET = 64;
  const ENCODE = new Uint8Array(128);
  for (let c = 0; c < 128; c++) {
    const ch = fromCharCode(c);
    let m = 0;
    if (c < 0x20 || c === 0x7F) m = 255;
    else {
      if (' "<>`'.includes(ch)) m |= FRAGMENT_SET;
      if (' "#<>'.includes(ch)) m |= QUERY_SET | SPECIAL_QUERY_SET | PATH_SET | USERINFO_SET | FORM_SET;
      if (ch === "'") m |= SPECIAL_QUERY_SET | FORM_SET;
      if ("?^`{}".includes(ch)) m |= PATH_SET | USERINFO_SET | FORM_SET;
      if ("/:;=@[\\]|".includes(ch)) m |= USERINFO_SET | FORM_SET;
      if ("$%&+,".includes(ch)) m |= FORM_SET; // the component set's additions
      if ("!()~".includes(ch)) m |= FORM_SET;
    }
    ENCODE[c] = m;
  }
  const HEX = [];
  for (let i = 0; i < 256; i++) HEX.push((i < 16 ? "%0" : "%") + i.toString(16).toUpperCase());

  function utf8Hex(cp) {
    if (cp < 0x800) return HEX[0xC0 | (cp >> 6)] + HEX[0x80 | (cp & 63)];
    if (cp < 0x10000) return HEX[0xE0 | (cp >> 12)] + HEX[0x80 | ((cp >> 6) & 63)] + HEX[0x80 | (cp & 63)];
    return HEX[0xF0 | (cp >> 18)] + HEX[0x80 | ((cp >> 12) & 63)] + HEX[0x80 | ((cp >> 6) & 63)] + HEX[0x80 | (cp & 63)];
  }

  // UTF-8 percent-encodes s[start, end) with `set`, returning the slice itself
  // when nothing needs encoding (the common case).
  function encode(s, start, end, set) {
    let out = "", last = start;
    for (let i = start; i < end; i++) {
      const c = s.charCodeAt(i);
      if (c < 0x80) {
        if ((ENCODE[c] & set) === 0) continue;
        out += s.slice(last, i) + (c === 0x20 && set === FORM_SET ? "+" : HEX[c]);
      } else {
        let cp = c;
        if (c >= 0xD800 && c <= 0xDFFF) {
          const d = i + 1 < end ? s.charCodeAt(i + 1) : 0;
          if (c <= 0xDBFF && d >= 0xDC00 && d <= 0xDFFF) cp = 0x10000 + ((c - 0xD800) << 10) + (d - 0xDC00);
          else cp = 0xFFFD;
        }
        out += s.slice(last, i) + utf8Hex(cp);
        if (cp > 0xFFFF) i++;
      }
      last = i + 1;
    }
    if (last !== start) return out + s.slice(last, end);
    return start === 0 && end === s.length ? s : s.slice(start, end);
  }

  function hexValue(c) {
    if (c >= 0x30 && c <= 0x39) return c - 0x30;
    c |= 0x20;
    return c >= 0x61 && c <= 0x66 ? c - 0x57 : -1;
  }

  // UTF-8 decode (WHATWG: each maximal invalid subpart becomes one U+FFFD).
  function utf8Decode(b) {
    const n = b.length, units = [];
    for (let i = 0; i < n;) {
      const x = b[i];
      if (x < 0x80) { units.push(x); i++; continue; }
      let need = 0, cp = 0, lower = 0x80, upper = 0xBF;
      if (x >= 0xC2 && x <= 0xDF) { need = 1; cp = x & 0x1F; }
      else if (x >= 0xE0 && x <= 0xEF) { need = 2; cp = x & 0x0F; if (x === 0xE0) lower = 0xA0; else if (x === 0xED) upper = 0x9F; }
      else if (x >= 0xF0 && x <= 0xF4) { need = 3; cp = x & 0x07; if (x === 0xF0) lower = 0x90; else if (x === 0xF4) upper = 0x8F; }
      else { units.push(0xFFFD); i++; continue; }
      let j = i + 1;
      for (; need > 0; need--, j++) {
        const y = j < n ? b[j] : -1;
        if (y < lower || y > upper) break;
        cp = (cp << 6) | (y & 0x3F);
        lower = 0x80; upper = 0xBF;
      }
      if (need > 0) units.push(0xFFFD);
      else if (cp > 0xFFFF) { cp -= 0x10000; units.push(0xD800 | (cp >> 10), 0xDC00 | (cp & 0x3FF)); }
      else units.push(cp);
      i = j;
    }
    return fromCharCodes(units);
  }

  // Percent-decodes s[start, end) and UTF-8 decodes the bytes; `plus` turns "+"
  // into a space first (form-urlencoded). Only runs of %XX are byte-decoded: the
  // text between them is already well-formed, so the result is the same.
  const byteRun = [];
  function decode(s, start, end, plus) {
    let out = "", last = start;
    for (let i = start; i < end;) {
      const c = s.charCodeAt(i);
      if (c === 0x25 && i + 2 < end) {
        let hi = hexValue(s.charCodeAt(i + 1)), lo = hexValue(s.charCodeAt(i + 2));
        if ((hi | lo) >= 0) {
          out += s.slice(last, i);
          byteRun.length = 0;
          for (;;) {
            byteRun.push((hi << 4) | lo);
            i += 3;
            if (i + 2 >= end || s.charCodeAt(i) !== 0x25) break;
            hi = hexValue(s.charCodeAt(i + 1));
            lo = hexValue(s.charCodeAt(i + 2));
            if ((hi | lo) < 0) break;
          }
          out += utf8Decode(byteRun);
          last = i;
          continue;
        }
      } else if (c === 0x2B && plus) {
        out += s.slice(last, i) + " ";
        last = i + 1;
      }
      i++;
    }
    if (last !== start) return out + s.slice(last, end);
    return start === 0 && end === s.length ? s : s.slice(start, end);
  }

  // ---- hosts ----------------------------------------------------------------
  // Bit 1: forbidden host code point; bit 2: forbidden domain code point (adds
  // the other C0 controls, "%" and DEL).
  const FORBIDDEN = new Uint8Array(128);
  for (const ch of "\0\t\n\r #/:<>?@[\\]^|") FORBIDDEN[ch.charCodeAt(0)] = 3;
  for (let c = 0; c < 0x20; c++) FORBIDDEN[c] |= 2;
  FORBIDDEN[0x25] |= 2;
  FORBIDDEN[0x7F] |= 2;

  // Returns the serialized host, or null on failure.
  function parseHost(s, isOpaque) {
    if (s.charCodeAt(0) === 0x5B) {
      if (s.charCodeAt(s.length - 1) !== 0x5D) return null;
      const a = parseIPv6(s, 1, s.length - 1);
      return a === null ? null : "[" + serializeIPv6(a) + "]";
    }
    if (isOpaque) {
      for (let i = 0; i < s.length; i++) {
        const c = s.charCodeAt(i);
        if (c < 0x80 && (FORBIDDEN[c] & 1) !== 0) return null;
      }
      return encode(s, 0, s.length, C0_SET);
    }
    // An ASCII domain without "%" only needs lowercasing: the URL Standard's
    // domain to ASCII leaves ASCII input at that, even "xn--" labels. A
    // forbidden code point fails only once the host is known to be ASCII
    // ("<" + U+0338 maps to U+226E).
    let upper = false, forbidden = false;
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if (c >= 0x80 || c === 0x25) return parseDomain(s);
      if ((FORBIDDEN[c] & 2) !== 0) forbidden = true;
      else if (c >= 0x41 && c <= 0x5A) upper = true;
    }
    if (forbidden) return null;
    if (upper) s = s.toLowerCase();
    return endsInNumber(s) ? parseIPv4(s) : s;
  }

  function parseDomain(s) {
    const domain = decode(s, 0, s.length, false);
    let ascii = true;
    for (let i = 0; i < domain.length; i++) if (domain.charCodeAt(i) >= 0x80) { ascii = false; break; }
    const result = ascii ? domain.toLowerCase() : domainToASCII(domain);
    if (result === null || result === "") return null;
    for (let i = 0; i < result.length; i++) {
      const c = result.charCodeAt(i);
      if (c < 0x80 && (FORBIDDEN[c] & 2) !== 0) return null;
    }
    return endsInNumber(result) ? parseIPv4(result) : result;
  }

  // The last label (ignoring one trailing empty label) is all digits, or an
  // IPv4 number such as "0x1f".
  function endsInNumber(d) {
    let end = d.length;
    if (end > 0 && d.charCodeAt(end - 1) === 0x2E) end--;
    if (end === 0) return false;
    const start = d.lastIndexOf(".", end - 1) + 1;
    if (start === end) return false;
    let digits = true;
    for (let i = start; i < end; i++) {
      const c = d.charCodeAt(i);
      if (c < 0x30 || c > 0x39) { digits = false; break; }
    }
    return digits || ipv4Number(d, start, end) >= 0;
  }

  // An IPv4 part in decimal, octal ("0" prefix) or hex ("0x"); -1 on failure.
  function ipv4Number(s, start, end) {
    if (start === end) return -1;
    let radix = 10;
    if (end - start >= 2 && s.charCodeAt(start) === 0x30) {
      if ((s.charCodeAt(start + 1) | 0x20) === 0x78) { radix = 16; start += 2; }
      else { radix = 8; start += 1; }
    }
    let n = 0;
    for (let i = start; i < end; i++) {
      const c = s.charCodeAt(i);
      const d = radix === 16 ? hexValue(c) : c - 0x30;
      if (d < 0 || d >= radix) return -1;
      n = n * radix + d;
    }
    return n;
  }

  function parseIPv4(d) {
    let end = d.length;
    if (d.charCodeAt(end - 1) === 0x2E) end--;
    const nums = [];
    for (let start = 0; ;) {
      let dot = d.indexOf(".", start);
      if (dot < 0 || dot > end) dot = end;
      if (nums.length === 4) return null;
      const n = ipv4Number(d, start, dot);
      if (n < 0) return null;
      nums.push(n);
      if (dot === end) break;
      start = dot + 1;
    }
    const count = nums.length;
    let ipv4 = nums[count - 1];
    if (ipv4 >= Math.pow(256, 5 - count)) return null;
    for (let i = 0; i < count - 1; i++) {
      if (nums[i] > 255) return null;
      ipv4 += nums[i] * Math.pow(256, 3 - i);
    }
    return (ipv4 >>> 24) + "." + ((ipv4 >>> 16) & 255) + "." + ((ipv4 >>> 8) & 255) + "." + (ipv4 & 255);
  }

  function parseIPv6(s, p, end) {
    const a = [0, 0, 0, 0, 0, 0, 0, 0];
    let piece = 0, compress = -1;
    if (p < end && s.charCodeAt(p) === 0x3A) {
      if (p + 1 >= end || s.charCodeAt(p + 1) !== 0x3A) return null;
      p += 2;
      compress = ++piece;
    }
    while (p < end) {
      if (piece === 8) return null;
      let c = s.charCodeAt(p);
      if (c === 0x3A) {
        if (compress !== -1) return null;
        p++;
        compress = ++piece;
        continue;
      }
      let value = 0, length = 0;
      while (length < 4 && p < end) {
        const h = hexValue(s.charCodeAt(p));
        if (h < 0) break;
        value = value * 16 + h;
        p++;
        length++;
      }
      c = p < end ? s.charCodeAt(p) : -1;
      if (c === 0x2E) {
        // An embedded IPv4 address fills the last two pieces.
        if (length === 0) return null;
        p -= length;
        if (piece > 6) return null;
        let seen = 0;
        while (p < end) {
          let v4 = -1;
          c = s.charCodeAt(p);
          if (seen > 0) {
            if (c !== 0x2E || seen >= 4) return null;
            c = ++p < end ? s.charCodeAt(p) : -1;
          }
          if (c < 0x30 || c > 0x39) return null;
          while (c >= 0x30 && c <= 0x39) {
            if (v4 === -1) v4 = c - 0x30;
            else if (v4 === 0) return null;
            else v4 = v4 * 10 + c - 0x30;
            if (v4 > 255) return null;
            c = ++p < end ? s.charCodeAt(p) : -1;
          }
          a[piece] = a[piece] * 0x100 + v4;
          seen++;
          if (seen === 2 || seen === 4) piece++;
        }
        if (seen !== 4) return null;
        break;
      } else if (c === 0x3A) {
        if (++p >= end) return null;
      } else if (c !== -1) return null;
      a[piece++] = value;
    }
    if (compress !== -1) {
      let swaps = piece - compress;
      piece = 7;
      while (piece !== 0 && swaps > 0) {
        const t = a[piece];
        a[piece] = a[compress + swaps - 1];
        a[compress + swaps - 1] = t;
        piece--;
        swaps--;
      }
    } else if (piece !== 8) return null;
    return a;
  }

  // Compresses the first longest run of two or more zero pieces.
  function serializeIPv6(a) {
    let best = -1, bestLen = 1;
    for (let i = 0; i < 8;) {
      if (a[i] !== 0) { i++; continue; }
      let j = i;
      while (j < 8 && a[j] === 0) j++;
      if (j - i > bestLen) { best = i; bestLen = j - i; }
      i = j;
    }
    let out = "";
    for (let i = 0; i < 8; i++) {
      if (i === best) {
        out += i === 0 ? "::" : ":";
        i += bestLen - 1;
        continue;
      }
      out += a[i].toString(16);
      if (i !== 7) out += ":";
    }
    return out;
  }

  // ---- IDNA -----------------------------------------------------------------
  // UTS #46 ToASCII for non-ASCII domains, approximated without its mapping
  // tables: lowercase + NFKC stand in for the mapping, Unicode properties for
  // the disallowed set, and the bidi and joiner rules use script ranges.
  // Punycode (RFC 3492) over code point arrays.
  const BASE = 36, TMIN = 1, TMAX = 26, SKEW = 38, DAMP = 700, MAXINT = 0x7FFFFFFF;
  function adapt(delta, numPoints, first) {
    delta = first ? Math.floor(delta / DAMP) : delta >> 1;
    delta += Math.floor(delta / numPoints);
    let k = 0;
    while (delta > ((BASE - TMIN) * TMAX) >> 1) { delta = Math.floor(delta / (BASE - TMIN)); k += BASE; }
    return k + Math.floor(((BASE - TMIN + 1) * delta) / (delta + SKEW));
  }
  const punyDigit = (d) => fromCharCode(d < 26 ? 97 + d : 22 + d);
  function punycodeEncode(cps) {
    let out = "";
    for (let i = 0; i < cps.length; i++) if (cps[i] < 0x80) out += fromCharCode(cps[i]);
    const basic = out.length;
    if (basic > 0) out += "-";
    let n = 128, delta = 0, bias = 72, h = basic;
    while (h < cps.length) {
      let m = MAXINT;
      for (let i = 0; i < cps.length; i++) if (cps[i] >= n && cps[i] < m) m = cps[i];
      if (m - n > Math.floor((MAXINT - delta) / (h + 1))) return null;
      delta += (m - n) * (h + 1);
      n = m;
      for (let i = 0; i < cps.length; i++) {
        const c = cps[i];
        if (c < n && ++delta > MAXINT) return null;
        if (c !== n) continue;
        let q = delta;
        for (let k = BASE; ; k += BASE) {
          const t = k <= bias ? TMIN : k >= bias + TMAX ? TMAX : k - bias;
          if (q < t) break;
          out += punyDigit(t + ((q - t) % (BASE - t)));
          q = Math.floor((q - t) / (BASE - t));
        }
        out += punyDigit(q);
        bias = adapt(delta, h + 1, h === basic);
        delta = 0;
        h++;
      }
      delta++;
      n++;
    }
    return out;
  }
  function punycodeDecode(s) {
    const delim = s.lastIndexOf("-");
    const out = [];
    for (let j = 0; j < delim; j++) out.push(s.charCodeAt(j));
    let n = 128, i = 0, bias = 72;
    for (let p = delim < 0 ? 0 : delim + 1; p < s.length;) {
      const oldi = i;
      let w = 1;
      for (let k = BASE; ; k += BASE) {
        if (p >= s.length) return null;
        const c = s.charCodeAt(p++);
        const l = c | 0x20;
        const digit = c >= 0x30 && c <= 0x39 ? c - 22 : l >= 0x61 && l <= 0x7A ? l - 0x61 : -1;
        if (digit < 0 || digit > Math.floor((MAXINT - i) / w)) return null;
        i += digit * w;
        const t = k <= bias ? TMIN : k >= bias + TMAX ? TMAX : k - bias;
        if (digit < t) break;
        if (w > Math.floor(MAXINT / (BASE - t))) return null;
        w *= BASE - t;
      }
      const len = out.length + 1;
      bias = adapt(i - oldi, len, oldi === 0);
      if (Math.floor(i / len) > MAXINT - n) return null;
      n += Math.floor(i / len);
      i %= len;
      if (n > 0x10FFFF || (n >= 0xD800 && n <= 0xDFFF)) return null;
      out.splice(i++, 0, n);
    }
    return fromCodePoint.apply(null, out);
  }

  // Regexps for the non-ASCII domain path, built on first use. Each is compiled
  // from a string (a literal the engine's regexp syntax rejected would fail the
  // whole file) and falls back alone: to its BMP part, or to never matching.
  let idna = null;
  function idnaTables() {
    if (idna !== null) return idna;
    const re = (src, fallback, flags = "u") => {
      try { return new RegExp(src, flags); } catch { return fallback; }
    };
    const never = /(?!)/;
    const rtlRange = String.raw`\u0590-\u08FF\uFB1D-\uFDFF\uFE70-\uFEFF\u{10800}-\u{10FFF}\u{1E800}-\u{1EFFF}`;
    idna = {
      ignored: re(String.raw`[\u00AD\u034F\u115F\u1160\u17B4\u17B5\u180B-\u180F\u200B\u2060-\u2064\u206A-\u206F\u3164\uFE00-\uFE0F\uFEFF\uFFA0\u{1BCA0}-\u{1BCA3}\u{1D173}-\u{1D17A}\u{E0100}-\u{E01EF}]`,
        /[\u00AD\u034F\u115F\u1160\u17B4\u17B5\u180B-\u180F\u200B\u2060-\u2064\u206A-\u206F\u3164\uFE00-\uFE0F\uFEFF\uFFA0]/g, "gu"),
      // Disallowed code points NFKC would turn into dots (U+2025 is not "..").
      dotted: re(String.raw`[\u2024-\u2026\u2488-\u249B\uFE12\u{1F100}]`, /[\u2024-\u2026\u2488-\u249B\uFE12]/),
      disallowed: re(String.raw`[\p{Cc}\p{Cs}\p{Co}\p{Cn}\p{Z}\p{Noncharacter_Code_Point}\u2FF0-\u2FFF\uFFFC\uFFFD]|(?![\u200C\u200D])\p{Cf}`, /[\u2FF0-\u2FFF\uFFFC\uFFFD]/),
      leadingMark: re(String.raw`^\p{M}`, never),
      rtl: re("[" + rtlRange + "]", never),
      nsm: re(String.raw`^[\p{Mn}\p{Me}]$`, never),
      an: re(String.raw`^[\u0600-\u0605\u0660-\u0669\u066B\u066C\u06DD\u0890\u0891\u08E2\u{10D30}-\u{10D39}\u{10E60}-\u{10E7E}]$`, never),
      en: re(String.raw`^[0-9\u06F0-\u06F9]$`, never),
      rtlLetter: re(String.raw`^(?![\p{Z}\p{C}\u060C\u06DE\u06E9\uFD3E\uFD3F])[` + rtlRange + "]$", never),
      ltr: re(String.raw`^[\p{L}\p{Mc}\p{Nd}\p{Nl}]$`, never),
      virama: re(String.raw`[\u094D\u09CD\u0A4D\u0ACD\u0B4D\u0BCD\u0C4D\u0CCD\u0D3B\u0D3C\u0D4D\u0DCA\u0E3A\u0EBA\u0F84\u1039\u103A\u1714\u1715\u1734\u17D2\u1A60\u1B44\u1BAA\u1BAB\u1BF2\u1BF3\u2D7F\uA806\uA82C\uA8C4\uA953\uA9C0\uAAF6\uABED\u{10A3F}\u{11046}\u{11070}\u{1107F}\u{110B9}\u{11133}\u{11134}\u{111C0}\u{11235}\u{112EA}\u{1134D}\u{11442}\u{114C2}\u{115BF}\u{1163F}\u{116B6}\u{1172B}\u{11839}\u{1193D}\u{1193E}\u{119E0}\u{11A34}\u{11A47}\u{11A99}\u{11C3F}\u{11D44}\u{11D45}\u{11D97}]$`, never),
      joining: re(String.raw`[\p{Script=Arabic}\p{Script=Syriac}\p{Script=Nko}\p{Script=Mandaic}\p{Script=Mongolian}\p{Script=Phags_Pa}\p{Script=Manichaean}\p{Script=Psalter_Pahlavi}\p{Script=Adlam}\p{Script=Hanifi_Rohingya}\p{Script=Sogdian}]`, never),
    };
    return idna;
  }

  function mapDomain(s, t) {
    s = s.replace(/\u03A3/g, "\u03C3").toLowerCase(); // no final-sigma context
    if (hasNormalize) s = s.normalize("NFKC").toLowerCase();
    s = s.replace(t.ignored, "").replace(/\u3002/g, ".");
    return hasNormalize ? s.normalize("NFC") : s;
  }

  function isASCII(s) {
    for (let i = 0; i < s.length; i++) if (s.charCodeAt(i) >= 0x80) return false;
    return true;
  }

  function validLabel(label, t) {
    if (hasNormalize && label.normalize("NFC") !== label) return false;
    if (label.startsWith("xn--") || t.leadingMark.test(label) || t.disallowed.test(label)) return false;
    if (label.indexOf("\u200C") < 0 && label.indexOf("\u200D") < 0) return true;
    // CheckJoiners: ZWJ/ZWNJ only after a virama, or ZWNJ between joining letters.
    const cps = Array.from(label);
    for (let i = 0; i < cps.length; i++) {
      if (cps[i] !== "\u200C" && cps[i] !== "\u200D") continue;
      if (i > 0 && t.virama.test(cps[i - 1])) continue;
      if (cps[i] === "\u200D") return false;
      let k = i - 1, m = i + 1;
      while (k >= 0 && t.nsm.test(cps[k])) k--;
      while (m < cps.length && t.nsm.test(cps[m])) m++;
      if (k < 0 || m >= cps.length || !t.joining.test(cps[k]) || !t.joining.test(cps[m])) return false;
    }
    return true;
  }

  // RFC 5893's six rules, applied to every label once any label has R, AL or AN.
  function bidiClass(ch, t) {
    if (t.nsm.test(ch)) return "NSM";
    if (t.an.test(ch)) return "AN";
    if (t.en.test(ch)) return "EN";
    if (t.rtlLetter.test(ch)) return "R";
    if (t.ltr.test(ch)) return "L";
    return "ON";
  }
  function bidiLabel(label, t) {
    const cls = Array.from(label, (ch) => bidiClass(ch, t));
    if (cls.length === 0) return true;
    let last = cls.length - 1;
    while (last > 0 && cls[last] === "NSM") last--;
    if (cls[0] === "R") {
      if (cls.includes("L") || (cls.includes("EN") && cls.includes("AN"))) return false;
      return cls[last] === "R" || cls[last] === "EN" || cls[last] === "AN";
    }
    if (cls[0] !== "L" || cls.includes("R") || cls.includes("AN")) return false;
    return cls[last] === "L" || cls[last] === "EN";
  }

  function domainToASCII(domain) {
    const t = idnaTables();
    if (t.dotted.test(domain)) return null;
    const labels = mapDomain(domain, t).split(".");
    const unicode = [];
    let bidi = false;
    for (let i = 0; i < labels.length; i++) {
      const label = labels[i];
      let u = label;
      if (label.startsWith("xn--")) {
        if (!isASCII(label)) return null;
        // A decoded label must be non-ASCII, valid, and unchanged by mapping.
        u = punycodeDecode(label.slice(4));
        if (u === null || isASCII(u) || mapDomain(u, t) !== u || !validLabel(u, t)) return null;
      } else if (!isASCII(label)) {
        if (!validLabel(label, t)) return null;
        const cps = [];
        for (const ch of label) cps.push(ch.codePointAt(0));
        const enc = punycodeEncode(cps);
        if (enc === null) return null;
        labels[i] = "xn--" + enc;
      }
      if (!bidi && t.rtl.test(u)) {
        for (const ch of u) {
          const k = bidiClass(ch, t);
          if (k === "R" || k === "AN") { bidi = true; break; }
        }
      }
      unicode.push(u);
    }
    if (bidi) for (let i = 0; i < unicode.length; i++) if (!bidiLabel(unicode[i], t)) return null;
    return labels.join(".");
  }

  // ---- the basic URL parser -------------------------------------------------
  // A URL record. `path` is the path's serialization: "/a/b" for a list, the
  // string itself when `opaque`; `sp` is the special-scheme default port (0 for
  // file, -1 when not special). `href` caches the serialization; `dirty` marks a
  // query that its URLSearchParams changed and has not written back yet.
  function Rec() {
    this.scheme = "";
    this.sp = -1;
    this.username = "";
    this.password = "";
    this.host = null;
    this.port = null;
    this.path = "";
    this.opaque = false;
    this.query = null;
    this.fragment = null;
    this.href = null;
    this.dirty = false;
  }

  function specialPort(scheme) {
    switch (scheme) {
      case "http": case "ws": return 80;
      case "https": case "wss": return 443;
      case "ftp": return 21;
      case "file": return 0;
      default: return -1;
    }
  }

  const isAlpha = (c) => ((c | 0x20) >= 0x61 && (c | 0x20) <= 0x7A);
  function isSchemeChar(c) {
    return isAlpha(c) || (c >= 0x30 && c <= 0x39) || c === 0x2B || c === 0x2D || c === 0x2E;
  }
  // s[i, i+2) is a Windows drive letter ("C:" or "C|").
  function isDrive(s, i) {
    const c = s.charCodeAt(i + 1);
    return isAlpha(s.charCodeAt(i)) && (c === 0x3A || c === 0x7C);
  }
  function startsWithDrive(s, i, len) {
    if (len - i < 2 || !isDrive(s, i)) return false;
    if (len - i === 2) return true;
    const c = s.charCodeAt(i + 2);
    return c === 0x2F || c === 0x5C || c === 0x3F || c === 0x23;
  }
  // The first path segment is a normalized drive letter ("/C:").
  function pathHasDrive(path) {
    return path.length >= 3 && isAlpha(path.charCodeAt(1)) && path.charCodeAt(2) === 0x3A &&
      (path.length === 3 || path.charCodeAt(3) === 0x2F);
  }
  function shortenPath(u) {
    const path = u.path;
    if (u.sp === 0 && path.length === 3 && pathHasDrive(path)) return;
    const i = path.lastIndexOf("/");
    if (i >= 0) u.path = path.slice(0, i);
  }
  // 1 for ".", 2 for "..", either possibly as "%2e".
  function dotSegment(s, start, end) {
    const n = end - start;
    if (n === 1) return s.charCodeAt(start) === 0x2E ? 1 : 0;
    if (n === 2) return s.charCodeAt(start) === 0x2E && s.charCodeAt(start + 1) === 0x2E ? 2 : 0;
    if (n === 3) return isPct2e(s, start) ? 1 : 0;
    if (n === 4) {
      if (s.charCodeAt(start) === 0x2E) return isPct2e(s, start + 1) ? 2 : 0;
      return isPct2e(s, start) && s.charCodeAt(start + 3) === 0x2E ? 2 : 0;
    }
    if (n === 6) return isPct2e(s, start) && isPct2e(s, start + 3) ? 2 : 0;
    return 0;
  }
  function isPct2e(s, i) {
    return s.charCodeAt(i) === 0x25 && s.charCodeAt(i + 1) === 0x32 && (s.charCodeAt(i + 2) | 0x20) === 0x65;
  }

  const SCHEME_START = 1, NO_SCHEME = 2, SPECIAL_RELATIVE_OR_AUTHORITY = 3, PATH_OR_AUTHORITY = 4,
    RELATIVE = 5, RELATIVE_SLASH = 6, SPECIAL_AUTHORITY_SLASHES = 7, SPECIAL_AUTHORITY_IGNORE_SLASHES = 8,
    AUTHORITY = 9, HOST = 10, HOSTNAME = 11, PORT = 12, FILE = 13, FILE_SLASH = 14, FILE_HOST = 15,
    PATH_START = 16, PATH = 17, OPAQUE_PATH = 18, QUERY = 19, FRAGMENT = 20;

  // The basic URL parser over a well-formed string. Without `url` it trims and
  // returns a new record, or null on failure; with `url` and a state override
  // (a setter) it edits `url` in place, as far as the spec's steps get. Each
  // state consumes a whole run (a segment, the query...) rather than one code
  // point at a time.
  function basicParse(input, base, url, override) {
    let start = 0, end = input.length;
    if (url === undefined) {
      url = new Rec();
      override = 0;
      while (start < end && input.charCodeAt(start) <= 0x20) start++;
      while (end > start && input.charCodeAt(end - 1) <= 0x20) end--;
      if (start !== 0 || end !== input.length) input = input.slice(start, end);
    }
    for (let i = 0; i < input.length; i++) {
      const c = input.charCodeAt(i);
      if (c < 0x0E && (c === 0x09 || c === 0x0A || c === 0x0D)) { input = input.replace(/[\t\n\r]/g, ""); break; }
    }
    const len = input.length;
    let state = override || SCHEME_START;
    let p = 0;
    for (;;) {
      const c = p < len ? input.charCodeAt(p) : -1;
      switch (state) {
        case SCHEME_START: {
          if (!isAlpha(c)) {
            if (override) return null;
            state = NO_SCHEME;
            p--;
            break;
          }
          let q = p + 1;
          while (q < len && isSchemeChar(input.charCodeAt(q))) q++;
          if (q >= len || input.charCodeAt(q) !== 0x3A) {
            if (override) return null;
            state = NO_SCHEME;
            p = -1; // start over: no scheme after all
            break;
          }
          let scheme = input.slice(p, q);
          for (let i = 0; i < scheme.length; i++) {
            const d = scheme.charCodeAt(i);
            if (d >= 0x41 && d <= 0x5A) { scheme = scheme.toLowerCase(); break; }
          }
          const sp = specialPort(scheme);
          p = q;
          if (override) {
            // The protocol setter cannot move between special and not, or make
            // a URL with credentials, a port or an empty host change to/from file.
            if ((url.sp >= 0) !== (sp >= 0)) return url;
            if (sp === 0 && (url.username !== "" || url.password !== "" || url.port !== null)) return url;
            if (url.sp === 0 && url.host === "") return url;
            url.scheme = scheme;
            url.sp = sp;
            if (url.port !== null && url.port === sp) url.port = null;
            return url;
          }
          url.scheme = scheme;
          url.sp = sp;
          if (sp === 0) state = FILE;
          else if (sp > 0 && base !== null && base.scheme === scheme) state = SPECIAL_RELATIVE_OR_AUTHORITY;
          else if (sp > 0) state = SPECIAL_AUTHORITY_SLASHES;
          else if (p + 1 < len && input.charCodeAt(p + 1) === 0x2F) { state = PATH_OR_AUTHORITY; p++; }
          else { url.opaque = true; state = OPAQUE_PATH; }
          break;
        }
        case NO_SCHEME:
          if (base === null || (base.opaque && c !== 0x23)) return null;
          if (base.opaque) {
            url.scheme = base.scheme;
            url.sp = base.sp;
            url.path = base.path;
            url.opaque = true;
            url.query = base.query;
            url.fragment = "";
            state = FRAGMENT;
          } else {
            state = base.sp !== 0 ? RELATIVE : FILE;
            p--;
          }
          break;
        case SPECIAL_RELATIVE_OR_AUTHORITY:
          if (c === 0x2F && p + 1 < len && input.charCodeAt(p + 1) === 0x2F) { state = SPECIAL_AUTHORITY_IGNORE_SLASHES; p++; }
          else { state = RELATIVE; p--; }
          break;
        case PATH_OR_AUTHORITY:
          if (c === 0x2F) state = AUTHORITY;
          else { state = PATH; p--; }
          break;
        case RELATIVE:
          url.scheme = base.scheme;
          url.sp = base.sp;
          if (c === 0x2F || (c === 0x5C && url.sp >= 0)) state = RELATIVE_SLASH;
          else {
            url.username = base.username;
            url.password = base.password;
            url.host = base.host;
            url.port = base.port;
            url.path = base.path;
            url.query = base.query;
            if (c === 0x3F) { url.query = ""; state = QUERY; }
            else if (c === 0x23) { url.fragment = ""; state = FRAGMENT; }
            else if (c !== -1) {
              url.query = null;
              shortenPath(url);
              state = PATH;
              p--;
            }
          }
          break;
        case RELATIVE_SLASH:
          if (url.sp >= 0 && (c === 0x2F || c === 0x5C)) state = SPECIAL_AUTHORITY_IGNORE_SLASHES;
          else if (c === 0x2F) state = AUTHORITY;
          else {
            url.username = base.username;
            url.password = base.password;
            url.host = base.host;
            url.port = base.port;
            state = PATH;
            p--;
          }
          break;
        case SPECIAL_AUTHORITY_SLASHES:
          state = SPECIAL_AUTHORITY_IGNORE_SLASHES;
          if (c === 0x2F && p + 1 < len && input.charCodeAt(p + 1) === 0x2F) p++;
          else p--;
          break;
        case SPECIAL_AUTHORITY_IGNORE_SLASHES:
          if (c !== 0x2F && c !== 0x5C) { state = AUTHORITY; p--; }
          break;
        case AUTHORITY: {
          // Credentials end at the last "@" before the authority ends; earlier
          // "@"s are part of them (and percent-encoded).
          const special = url.sp >= 0;
          let stop = p, at = -1;
          for (; stop < len; stop++) {
            const d = input.charCodeAt(stop);
            if (d === 0x2F || d === 0x3F || d === 0x23 || (d === 0x5C && special)) break;
            if (d === 0x40) at = stop;
          }
          if (at >= 0) {
            const colon = input.indexOf(":", p);
            if (colon < 0 || colon > at) url.username = encode(input, p, at, USERINFO_SET);
            else {
              url.username = encode(input, p, colon, USERINFO_SET);
              url.password = encode(input, colon + 1, at, USERINFO_SET);
            }
            if (at + 1 === stop) return null;
            p = at;
          } else p--;
          state = HOST;
          break;
        }
        case HOST:
        case HOSTNAME: {
          if (override && url.sp === 0) { state = FILE_HOST; p--; break; }
          const special = url.sp >= 0;
          let stop = p, brackets = false, d = -1;
          for (; stop < len; stop++) {
            d = input.charCodeAt(stop);
            if (d === 0x3A && !brackets) break;
            if (d === 0x2F || d === 0x3F || d === 0x23 || (d === 0x5C && special)) break;
            if (d === 0x5B) brackets = true;
            else if (d === 0x5D) brackets = false;
          }
          if (stop < len && d === 0x3A) {
            if (stop === p || override === HOSTNAME) return null;
            const host = parseHost(input.slice(p, stop), !special);
            if (host === null) return null;
            url.host = host;
            state = PORT;
            p = stop;
          } else {
            if (special && stop === p) return null;
            if (override && stop === p && (url.username !== "" || url.password !== "" || url.port !== null)) return null;
            const host = stop === p ? "" : parseHost(input.slice(p, stop), !special);
            if (host === null) return null;
            url.host = host;
            if (override) return url;
            state = PATH_START;
            p = stop - 1;
          }
          break;
        }
        case PORT: {
          let stop = p;
          while (stop < len && input.charCodeAt(stop) >= 0x30 && input.charCodeAt(stop) <= 0x39) stop++;
          const d = stop < len ? input.charCodeAt(stop) : -1;
          if (!(d === -1 || d === 0x2F || d === 0x3F || d === 0x23 || (d === 0x5C && url.sp >= 0) || override)) return null;
          if (stop !== p) {
            let port = 0;
            for (let i = p; i < stop; i++) {
              port = port * 10 + input.charCodeAt(i) - 0x30;
              if (port > 65535) return null;
            }
            url.port = port === url.sp && url.sp > 0 ? null : port;
            if (override) return url;
          }
          if (override) return null;
          state = PATH_START;
          p = stop - 1;
          break;
        }
        case FILE:
          url.scheme = "file";
          url.sp = 0;
          url.host = "";
          if (c === 0x2F || c === 0x5C) state = FILE_SLASH;
          else if (base !== null && base.sp === 0) {
            url.host = base.host;
            url.path = base.path;
            url.query = base.query;
            if (c === 0x3F) { url.query = ""; state = QUERY; }
            else if (c === 0x23) { url.fragment = ""; state = FRAGMENT; }
            else if (c !== -1) {
              url.query = null;
              if (!startsWithDrive(input, p, len)) shortenPath(url);
              else url.path = "";
              state = PATH;
              p--;
            }
          } else { state = PATH; p--; }
          break;
        case FILE_SLASH:
          if (c === 0x2F || c === 0x5C) state = FILE_HOST;
          else {
            if (base !== null && base.sp === 0) {
              url.host = base.host;
              if (!startsWithDrive(input, p, len) && pathHasDrive(base.path)) url.path += base.path.slice(0, 3);
            }
            state = PATH;
            p--;
          }
          break;
        case FILE_HOST: {
          let stop = p;
          for (; stop < len; stop++) {
            const d = input.charCodeAt(stop);
            if (d === 0x2F || d === 0x5C || d === 0x3F || d === 0x23) break;
          }
          if (!override && stop - p === 2 && isDrive(input, p)) {
            // "file://C:/": the drive letter is the first path segment.
            state = PATH;
            p--;
          } else if (stop === p) {
            url.host = "";
            if (override) return url;
            state = PATH_START;
            p--;
          } else {
            let host = parseHost(input.slice(p, stop), false);
            if (host === null) return null;
            if (host === "localhost") host = "";
            url.host = host;
            if (override) return url;
            state = PATH_START;
            p = stop - 1;
          }
          break;
        }
        case PATH_START:
          if (url.sp >= 0) {
            state = PATH;
            if (c !== 0x2F && c !== 0x5C) p--;
          } else if (!override && c === 0x3F) { url.query = ""; state = QUERY; }
          else if (!override && c === 0x23) { url.fragment = ""; state = FRAGMENT; }
          else if (c !== -1) {
            state = PATH;
            if (c !== 0x2F) p--;
          } else if (override && url.host === null) url.path += "/";
          break;
        case PATH: {
          // One segment, up to "/" ("\" too when special), or "?"/"#" outside setters.
          const special = url.sp >= 0;
          let stop = p, plain = true, d = -1;
          for (; stop < len; stop++) {
            d = input.charCodeAt(stop);
            if (d === 0x2F || (d === 0x5C && special) || (!override && (d === 0x3F || d === 0x23))) break;
            if (d >= 0x80 || (ENCODE[d] & PATH_SET) !== 0) plain = false;
          }
          if (stop === len) d = -1;
          const slash = d === 0x2F || (d === 0x5C && special);
          const dots = stop - p <= 6 ? dotSegment(input, p, stop) : 0;
          if (dots === 2) {
            shortenPath(url);
            if (!slash) url.path += "/";
          } else if (dots === 1) {
            if (!slash) url.path += "/";
          } else if (url.sp === 0 && url.path === "" && stop - p === 2 && isDrive(input, p)) {
            url.path = "/" + input[p] + ":";
          } else {
            url.path += "/" + (plain ? input.slice(p, stop) : encode(input, p, stop, PATH_SET));
          }
          if (d === 0x3F) { url.query = ""; state = QUERY; }
          else if (d === 0x23) { url.fragment = ""; state = FRAGMENT; }
          p = stop;
          break;
        }
        case OPAQUE_PATH: {
          let stop = p;
          while (stop < len) {
            const d = input.charCodeAt(stop);
            if (d === 0x3F || d === 0x23) break;
            stop++;
          }
          // A space just before "?" or "#" is written "%20", so the path never
          // ends in a space when the query or fragment goes away.
          if (stop < len && stop > p && input.charCodeAt(stop - 1) === 0x20) {
            url.path += encode(input, p, stop - 1, C0_SET) + "%20";
          } else url.path += encode(input, p, stop, C0_SET);
          if (stop < len) {
            if (input.charCodeAt(stop) === 0x3F) { url.query = ""; state = QUERY; }
            else { url.fragment = ""; state = FRAGMENT; }
          }
          p = stop;
          break;
        }
        case QUERY: {
          let stop = len;
          if (!override) {
            stop = input.indexOf("#", p);
            if (stop < 0) stop = len;
          }
          url.query += encode(input, p, stop, url.sp >= 0 ? SPECIAL_QUERY_SET : QUERY_SET);
          if (stop < len) { url.fragment = ""; state = FRAGMENT; }
          p = stop;
          break;
        }
        case FRAGMENT:
          url.fragment += encode(input, p, len, FRAGMENT_SET);
          p = len;
          break;
      }
      if (p >= len) break;
      p++;
    }
    return url;
  }

  function serialize(u) {
    let out = u.scheme + ":";
    if (u.host !== null) {
      out += "//";
      if (u.username !== "" || u.password !== "") {
        out += u.username;
        if (u.password !== "") out += ":" + u.password;
        out += "@";
      }
      out += u.host;
      if (u.port !== null) out += ":" + u.port;
    } else if (!u.opaque && u.path.length > 1 && u.path.charCodeAt(1) === 0x2F) {
      out += "/."; // keeps "//x" from reading as a host
    }
    out += u.path;
    if (u.query !== null) out += "?" + u.query;
    if (u.fragment !== null) out += "#" + u.fragment;
    return out;
  }

  function serializeOrigin(u) {
    if (u.sp > 0) return u.scheme + "://" + u.host + (u.port === null ? "" : ":" + u.port);
    if (u.scheme === "blob") {
      const inner = basicParse(u.path, null);
      if (inner !== null && (inner.scheme === "http" || inner.scheme === "https")) return serializeOrigin(inner);
    }
    return "null";
  }

  // ---- errors (Node's codes) ------------------------------------------------
  function nodeError(Ctor, code, message) {
    const e = new Ctor(message);
    e.code = code;
    return e;
  }
  function invalidURL(input, base) {
    const e = nodeError(TypeError, "ERR_INVALID_URL", "Invalid URL");
    e.input = input;
    if (base !== undefined) e.base = base;
    return e;
  }
  const missingArgs = (what) => nodeError(TypeError, "ERR_MISSING_ARGS", "The " + what + " must be specified");
  const invalidThis = (type) => nodeError(TypeError, "ERR_INVALID_THIS", 'Value of "this" must be of type ' + type);
  const invalidTuple = () => nodeError(TypeError, "ERR_INVALID_TUPLE", "Each query pair must be an iterable [name, value] tuple");
  function describe(v) {
    if (v == null) return " Received " + v;
    if (typeof v === "function") return " Received function " + v.name;
    if (typeof v === "object") return v.constructor && v.constructor.name ? " Received an instance of " + v.constructor.name : " Received " + String(v);
    let shown = typeof v === "symbol" ? v.toString() : typeof v === "string" ? "'" + v + "'" : String(v);
    if (shown.length > 28) shown = shown.slice(0, 25) + "...";
    if (typeof v === "bigint") shown += "n";
    return " Received type " + typeof v + " (" + shown + ")";
  }

  // ---- URLSearchParams ------------------------------------------------------
  // The list is flat: [name0, value0, name1, value1, ...].
  function parseParams(s, start) {
    const list = [];
    const len = s.length;
    for (let i = start; i < len;) {
      let amp = i, eq = -1;
      for (; amp < len; amp++) {
        const c = s.charCodeAt(amp);
        if (c === 0x26) break;
        if (c === 0x3D && eq < 0) eq = amp;
      }
      if (amp > i) {
        if (eq < 0) list.push(decode(s, i, amp, true), "");
        else list.push(decode(s, i, eq, true), decode(s, eq + 1, amp, true));
      }
      i = amp + 1;
    }
    return list;
  }
  function serializeParams(list) {
    let out = "";
    for (let i = 0; i < list.length; i += 2) {
      if (i > 0) out += "&";
      const name = list[i], value = list[i + 1];
      out += encode(name, 0, name.length, FORM_SET) + "=" + encode(value, 0, value.length, FORM_SET);
    }
    return out;
  }

  // Stable sort of the pairs by name in UTF-16 code units.
  function sortPairs(a) {
    const n = a.length;
    if (n <= 2) return;
    if (n < 100) {
      for (let i = 2; i < n; i += 2) {
        const name = a[i], value = a[i + 1];
        let j = i - 2;
        for (; j >= 0 && a[j] > name; j -= 2) {
          a[j + 2] = a[j];
          a[j + 3] = a[j + 1];
        }
        a[j + 2] = name;
        a[j + 3] = value;
      }
      return;
    }
    let src = a, dst = new Array(n);
    for (let width = 2; width < n; width *= 2) {
      for (let lo = 0; lo < n; lo += 2 * width) {
        const mid = Math.min(lo + width, n), hi = Math.min(lo + 2 * width, n);
        let i = lo, j = mid, k = lo;
        while (i < mid && j < hi) {
          if (src[j] < src[i]) { dst[k++] = src[j++]; dst[k++] = src[j++]; }
          else { dst[k++] = src[i++]; dst[k++] = src[i++]; }
        }
        while (i < mid) dst[k++] = src[i++];
        while (j < hi) dst[k++] = src[j++];
      }
      const t = src; src = dst; dst = t;
    }
    if (src !== a) for (let i = 0; i < n; i++) a[i] = src[i];
  }

  const toStr = (v) => usv(`${v}`);

  // Cross-class access to private state, handed out by static blocks.
  let recOf, flushQuery, listOf, setList, linkParams, isParams;

  class URLSearchParams {
    #list = [];
    #url = null;
    constructor(init = undefined) {
      if (init === undefined) return;
      if ((typeof init === "object" && init !== null) || typeof init === "function") {
        const method = init[SymbolIterator];
        if (method === this[SymbolIterator] && #list in init) {
          this.#list = init.#list.slice();
        } else if (method != null) {
          if (typeof method !== "function") throw nodeError(TypeError, "ERR_ARG_NOT_ITERABLE", "Query pairs must be iterable");
          const list = this.#list;
          for (const pair of init) {
            if (pair == null) throw invalidTuple();
            if (ArrayIsArray(pair)) {
              if (pair.length !== 2) throw invalidTuple();
              list.push(toStr(pair[0]), toStr(pair[1]));
            } else {
              if ((typeof pair !== "object" && typeof pair !== "function") || typeof pair[SymbolIterator] !== "function") throw invalidTuple();
              let n = 0;
              for (const element of pair) {
                if (++n > 2) throw invalidTuple();
                list.push(toStr(element));
              }
              if (n !== 2) throw invalidTuple();
            }
          }
        } else {
          // A record: own enumerable keys; a repeated (after USV conversion) key
          // keeps its first place and its last value.
          const list = this.#list;
          const seen = new Map();
          const keys = ReflectOwnKeys(init);
          for (let i = 0; i < keys.length; i++) {
            const key = keys[i];
            const desc = getOwnPropertyDescriptor(init, key);
            if (desc === undefined || !desc.enumerable) continue;
            if (typeof key === "symbol") throw new TypeError("Cannot convert a Symbol value to a string");
            const name = usv(key), value = toStr(init[key]);
            const at = seen.get(name);
            if (at !== undefined) list[at] = value;
            else seen.set(name, list.push(name, value) - 1);
          }
        }
      } else {
        const s = toStr(init);
        if (s !== "") this.#list = parseParams(s, s.charCodeAt(0) === 0x3F ? 1 : 0);
      }
    }
    get size() {
      if (!isParams(this)) throw invalidThis("URLSearchParams");
      return this.#list.length / 2;
    }
    append(name, value) {
      if (!isParams(this)) throw invalidThis("URLSearchParams");
      if (arguments.length < 2) throw missingArgs('"name" and "value" arguments');
      this.#list.push(toStr(name), toStr(value));
      if (this.#url !== null) flushQuery(this.#url);
    }
    delete(name, value = undefined) {
      if (!isParams(this)) throw invalidThis("URLSearchParams");
      if (arguments.length < 1) throw missingArgs('"name" argument');
      const list = this.#list;
      name = toStr(name);
      if (value !== undefined) value = toStr(value);
      let k = 0;
      for (let i = 0; i < list.length; i += 2) {
        if (list[i] === name && (value === undefined || list[i + 1] === value)) continue;
        list[k++] = list[i];
        list[k++] = list[i + 1];
      }
      list.length = k;
      if (this.#url !== null) flushQuery(this.#url);
    }
    get(name) {
      if (!isParams(this)) throw invalidThis("URLSearchParams");
      if (arguments.length < 1) throw missingArgs('"name" argument');
      const list = this.#list;
      name = toStr(name);
      for (let i = 0; i < list.length; i += 2) if (list[i] === name) return list[i + 1];
      return null;
    }
    getAll(name) {
      if (!isParams(this)) throw invalidThis("URLSearchParams");
      if (arguments.length < 1) throw missingArgs('"name" argument');
      const list = this.#list, out = [];
      name = toStr(name);
      for (let i = 0; i < list.length; i += 2) if (list[i] === name) out.push(list[i + 1]);
      return out;
    }
    has(name, value = undefined) {
      if (!isParams(this)) throw invalidThis("URLSearchParams");
      if (arguments.length < 1) throw missingArgs('"name" argument');
      const list = this.#list;
      name = toStr(name);
      if (value !== undefined) value = toStr(value);
      for (let i = 0; i < list.length; i += 2) {
        if (list[i] === name && (value === undefined || list[i + 1] === value)) return true;
      }
      return false;
    }
    set(name, value) {
      if (!isParams(this)) throw invalidThis("URLSearchParams");
      if (arguments.length < 2) throw missingArgs('"name" and "value" arguments');
      const list = this.#list;
      name = toStr(name);
      value = toStr(value);
      let k = 0, found = false;
      for (let i = 0; i < list.length; i += 2) {
        if (list[i] === name) {
          if (found) continue;
          found = true;
          list[k++] = name;
          list[k++] = value;
        } else {
          list[k++] = list[i];
          list[k++] = list[i + 1];
        }
      }
      list.length = k;
      if (!found) list.push(name, value);
      if (this.#url !== null) flushQuery(this.#url);
    }
    sort() {
      if (!isParams(this)) throw invalidThis("URLSearchParams");
      sortPairs(this.#list);
      if (this.#url !== null) flushQuery(this.#url);
    }
    entries() {
      if (!isParams(this)) throw invalidThis("URLSearchParams");
      return new ParamsIterator(this, 2);
    }
    forEach(callback, thisArg = undefined) {
      if (!isParams(this)) throw invalidThis("URLSearchParams");
      if (typeof callback !== "function") {
        throw nodeError(TypeError, "ERR_INVALID_ARG_TYPE", 'The "callback" argument must be of type function.' + describe(callback));
      }
      // Re-read the list each step: a callback may replace it (url.search = ...).
      for (let i = 0; i < this.#list.length; i += 2) {
        const list = this.#list;
        callback.call(thisArg, list[i + 1], list[i], this);
      }
    }
    keys() {
      if (!isParams(this)) throw invalidThis("URLSearchParams");
      return new ParamsIterator(this, 0);
    }
    values() {
      if (!isParams(this)) throw invalidThis("URLSearchParams");
      return new ParamsIterator(this, 1);
    }
    toString() {
      if (!isParams(this)) throw invalidThis("URLSearchParams");
      return serializeParams(this.#list);
    }
    [inspectCustom](depth, options, inspect) {
      if (!isParams(this)) throw invalidThis("URLSearchParams");
      const name = (this.constructor && this.constructor.name) || "URLSearchParams";
      if (typeof depth === "number" && depth < 0) return options && options.stylize ? options.stylize("[Object]", "special") : "[Object]";
      const show = typeof inspect === "function"
        ? (v) => inspect(v, { ...options, depth: typeof depth === "number" ? depth - 1 : depth })
        : (v) => "'" + v + "'";
      const list = this.#list, parts = [];
      for (let i = 0; i < list.length; i += 2) parts.push(show(list[i]) + " => " + show(list[i + 1]));
      if (parts.length === 0) return name + " {}";
      const width = parts.reduce((n, s) => n + s.replace(/\u001b\[\d\d?m/g, "").length + 2, -2);
      const breakLength = options && typeof options.breakLength === "number" ? options.breakLength : 80;
      return width > breakLength ? name + " {\n  " + parts.join(",\n  ") + " }" : name + " { " + parts.join(", ") + " }";
    }
    static {
      listOf = (p) => p.#list;
      setList = (p, list) => { p.#list = list; };
      linkParams = (p, url) => { p.#url = url; };
      isParams = (o) => typeof o === "object" && o !== null && #list in o;
    }
  }

  const IteratorPrototype = Object.getPrototypeOf(Object.getPrototypeOf([][SymbolIterator]()));
  let isParamsIterator;
  class ParamsIterator {
    #params;
    #kind; // 0 keys, 1 values, 2 entries
    #index = 0;
    constructor(params, kind) {
      this.#params = params;
      this.#kind = kind;
    }
    next() {
      if (!isParamsIterator(this)) throw invalidThis("URLSearchParamsIterator");
      const list = listOf(this.#params), i = this.#index;
      if (i >= list.length) return { value: undefined, done: true };
      this.#index = i + 2;
      const kind = this.#kind;
      return { value: kind === 0 ? list[i] : kind === 1 ? list[i + 1] : [list[i], list[i + 1]], done: false };
    }
    [inspectCustom](depth, options, inspect) {
      if (!isParamsIterator(this)) throw invalidThis("URLSearchParamsIterator");
      const list = listOf(this.#params), kind = this.#kind, rest = [];
      for (let i = this.#index; i < list.length; i += 2) rest.push(kind === 0 ? list[i] : kind === 1 ? list[i + 1] : [list[i], list[i + 1]]);
      const body = typeof inspect === "function" ? inspect(rest, options).slice(1, -1).trim() : rest.join(", ");
      return "URLSearchParams Iterator {" + (body === "" ? "}" : " " + body + " }");
    }
    static {
      isParamsIterator = (o) => typeof o === "object" && o !== null && #params in o;
    }
  }
  Object.setPrototypeOf(ParamsIterator.prototype, IteratorPrototype);
  delete ParamsIterator.prototype.constructor;

  // ---- URL ------------------------------------------------------------------
  const fromRecord = {}; // constructor token: adopt `adopted` instead of parsing
  let adopted = null;

  // A base that is a URL whose stringification is ours is used as it stands
  // (no reparse); `null` when the base must be parsed from its string.
  function urlBase(base) {
    if (typeof base !== "object" || base === null) return null;
    const r = recOf(base);
    if (r === null || base.toString !== URL.prototype.toString || base[Symbol.toPrimitive] !== undefined) return null;
    if (r.dirty) syncQuery(base, r);
    return r;
  }
  let syncQuery;

  class URL {
    #r;
    #params = null;
    constructor(input, base = undefined) {
      if (input === fromRecord) {
        this.#r = adopted;
        adopted = null;
        return;
      }
      if (arguments.length === 0) throw missingArgs('"url" argument');
      input = toStr(input);
      let b = null;
      if (base !== undefined) {
        b = urlBase(base);
        if (b === null) {
          base = toStr(base);
          b = basicParse(base, null);
          if (b === null) throw invalidURL(input, base);
        }
      }
      const r = basicParse(input, b);
      if (r === null) throw invalidURL(input, base === undefined ? undefined : typeof base === "string" ? base : `${base}`);
      this.#r = r;
    }
    static parse(input, base = undefined) {
      if (arguments.length === 0) throw missingArgs('"url" argument');
      input = toStr(input);
      let b = null;
      if (base !== undefined) {
        b = urlBase(base) || basicParse(toStr(base), null);
        if (b === null) return null;
      }
      const r = basicParse(input, b);
      if (r === null) return null;
      adopted = r;
      return new URL(fromRecord);
    }
    static canParse(input, base = undefined) {
      if (arguments.length === 0) throw missingArgs('"url" argument');
      input = toStr(input);
      let b = null;
      if (base !== undefined) {
        b = urlBase(base) || basicParse(toStr(base), null);
        if (b === null) return false;
      }
      return basicParse(input, b) !== null;
    }
    toString() {
      const r = this.#r;
      if (r.dirty) syncQuery(this, r);
      return r.href === null ? (r.href = serialize(r)) : r.href;
    }
    get href() {
      const r = this.#r;
      if (r.dirty) syncQuery(this, r);
      return r.href === null ? (r.href = serialize(r)) : r.href;
    }
    set href(value) {
      value = `${value}`;
      const r = basicParse(usv(value), null);
      if (r === null) throw invalidURL(value);
      this.#r = r;
      if (this.#params !== null) setList(this.#params, r.query === null ? [] : parseParams(r.query, 0));
    }
    get origin() {
      return serializeOrigin(this.#r);
    }
    get protocol() {
      return this.#r.scheme + ":";
    }
    set protocol(value) {
      value = toStr(value);
      const r = this.#r;
      basicParse(value + ":", null, r, SCHEME_START);
      r.href = null;
    }
    get username() {
      return this.#r.username;
    }
    set username(value) {
      value = toStr(value);
      const r = this.#r;
      if (r.host === null || r.host === "" || r.sp === 0) return;
      r.username = encode(value, 0, value.length, USERINFO_SET);
      r.href = null;
    }
    get password() {
      return this.#r.password;
    }
    set password(value) {
      value = toStr(value);
      const r = this.#r;
      if (r.host === null || r.host === "" || r.sp === 0) return;
      r.password = encode(value, 0, value.length, USERINFO_SET);
      r.href = null;
    }
    get host() {
      const r = this.#r;
      if (r.host === null) return "";
      return r.port === null ? r.host : r.host + ":" + r.port;
    }
    set host(value) {
      value = toStr(value);
      const r = this.#r;
      if (r.opaque) return;
      basicParse(value, null, r, HOST);
      r.href = null;
    }
    get hostname() {
      const host = this.#r.host;
      return host === null ? "" : host;
    }
    set hostname(value) {
      value = toStr(value);
      const r = this.#r;
      if (r.opaque) return;
      basicParse(value, null, r, HOSTNAME);
      r.href = null;
    }
    get port() {
      const port = this.#r.port;
      return port === null ? "" : "" + port;
    }
    set port(value) {
      value = toStr(value);
      const r = this.#r;
      if (r.host === null || r.host === "" || r.sp === 0) return;
      if (value === "") r.port = null;
      else basicParse(value, null, r, PORT);
      r.href = null;
    }
    get pathname() {
      return this.#r.path;
    }
    set pathname(value) {
      value = toStr(value);
      const r = this.#r;
      if (r.opaque) return;
      r.path = "";
      basicParse(value, null, r, PATH_START);
      r.href = null;
    }
    get search() {
      const r = this.#r;
      if (r.dirty) syncQuery(this, r);
      const q = r.query;
      return q === null || q === "" ? "" : "?" + q;
    }
    set search(value) {
      value = toStr(value);
      const r = this.#r;
      r.href = null;
      r.dirty = false;
      if (value === "") r.query = null;
      else {
        r.query = "";
        basicParse(value.charCodeAt(0) === 0x3F ? value.slice(1) : value, null, r, QUERY);
      }
      if (this.#params !== null) setList(this.#params, r.query === null ? [] : parseParams(r.query, 0));
    }
    get searchParams() {
      if (this.#params === null) {
        const params = new URLSearchParams();
        const r = this.#r;
        if (r.query !== null) setList(params, parseParams(r.query, 0));
        linkParams(params, this);
        this.#params = params;
      }
      return this.#params;
    }
    get hash() {
      const f = this.#r.fragment;
      return f === null || f === "" ? "" : "#" + f;
    }
    set hash(value) {
      value = toStr(value);
      const r = this.#r;
      r.href = null;
      if (value === "") { r.fragment = null; return; }
      r.fragment = "";
      basicParse(value.charCodeAt(0) === 0x23 ? value.slice(1) : value, null, r, FRAGMENT);
    }
    toJSON() {
      const r = this.#r;
      if (r.dirty) syncQuery(this, r);
      return r.href === null ? (r.href = serialize(r)) : r.href;
    }
    [inspectCustom](depth, options, inspect) {
      if (typeof depth === "number" && depth < 0) return this;
      const obj = {
        href: this.href, origin: this.origin, protocol: this.protocol, username: this.username,
        password: this.password, host: this.host, hostname: this.hostname, port: this.port,
        pathname: this.pathname, search: this.search, searchParams: this.searchParams, hash: this.hash,
      };
      const name = (this.constructor && this.constructor.name) || "URL";
      return typeof inspect === "function" ? name + " " + inspect(obj, options) : this;
    }
    static {
      recOf = (u) => (typeof u === "object" && u !== null && #r in u ? u.#r : null);
      // A params change marks the query stale; it is serialized on next read.
      flushQuery = (u) => {
        const r = u.#r;
        r.dirty = true;
        r.href = null;
      };
      syncQuery = (u, r) => {
        const q = serializeParams(listOf(u.#params));
        r.dirty = false;
        r.query = q === "" ? null : q;
        r.href = null;
      };
    }
  }

  // Node's shape: enumerable accessors and methods, a non-enumerable tag.
  for (const key of ["toString", "href", "origin", "protocol", "username", "password", "host", "hostname", "port", "pathname", "search", "searchParams", "hash", "toJSON"]) {
    const desc = getOwnPropertyDescriptor(URL.prototype, key);
    desc.enumerable = true;
    defineProperty(URL.prototype, key, desc);
  }
  for (const key of ["parse", "canParse"]) {
    const desc = getOwnPropertyDescriptor(URL, key);
    desc.enumerable = true;
    defineProperty(URL, key, desc);
  }
  for (const key of ["size", "append", "delete", "get", "getAll", "has", "set", "sort", "entries", "forEach", "keys", "values", "toString"]) {
    const desc = getOwnPropertyDescriptor(URLSearchParams.prototype, key);
    desc.enumerable = true;
    defineProperty(URLSearchParams.prototype, key, desc);
  }
  defineProperty(URLSearchParams.prototype, SymbolIterator, { value: URLSearchParams.prototype.entries, writable: true, configurable: true });
  defineProperty(ParamsIterator.prototype, "next", { enumerable: true });
  defineProperty(URL.prototype, Symbol.toStringTag, { value: "URL", configurable: true });
  defineProperty(URLSearchParams.prototype, Symbol.toStringTag, { value: "URLSearchParams", configurable: true });
  defineProperty(ParamsIterator.prototype, Symbol.toStringTag, { value: "URLSearchParams Iterator", configurable: true });
  return { URL, URLSearchParams };
})
