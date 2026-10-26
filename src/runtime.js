// Minimal test runtime injected before a test module loads.
// PLACEHOLDER for the real @vitest/* framework, which gets baked into the V8 snapshot
// at M3 (compatible-by-construction). This M1 version exists only to run the
// non-timer/non-DOM logic subset of the gauntlet end-to-end and report pass/fail.

// ---- console shim (V8 contexts have none; Node/browser provide it) ----
// `--silent` (host → globalThis.__TT_SILENT): suppress test console output entirely. Checked at
// call time, not eval time — the host injects __TT_SILENT AFTER this module is snapshot-evaluated.
const __noop = () => {};
const __emit = (tag) => (...a) => { if (!globalThis.__TT_SILENT) globalThis.log(tag, ...a.map(String)); };
globalThis.console = {
  log: __emit('[log]'),
  info: __emit('[info]'),
  warn: __emit('[warn]'),
  error: __emit('[error]'),
  debug: __noop,
  trace: __noop,
  group: __noop,
  groupEnd: __noop,
  table: __noop,
  dir: __noop,
  assert: __noop,
  count: __noop,
  time: __noop,
  timeEnd: __noop,
};

// ---- process shim (no Node in our embedding) ----
// platform/arch come from the host binary (Rust injects __ttPlatform/__ttArch/__ttGlibc
// from its compile-time target). They MUST be the real host values: turbo-dom's index.js
// loader switches on process.platform/arch to require the matching prebuilt .node, and
// its isMusl() reads process.report().header.glibcVersionRuntime to pick gnu vs musl.
// Hardcoding 'darwin'/'arm64' made every non-mac host load the macOS .node -> dlopen fails.
globalThis.process = {
  env: { NODE_ENV: 'test', VITEST: 'true', TZ: 'UTC', TURBO_DOM_PARSER: 'native' },
  platform: globalThis.__ttPlatform || 'darwin',
  arch: globalThis.__ttArch || 'arm64',
  // glibcVersionRuntime truthy => glibc (gnu); falsy => musl. __ttGlibc is the glibc
  // version for a gnu build, '' for a musl build.
  report: { getReport: () => ({ header: { glibcVersionRuntime: globalThis.__ttGlibc || undefined } }) },
  version: 'v24.0.0',
  // napi: the Node-API version the native host implements (src/napi_host.rs NAPI_VERSION)
  versions: { node: '24.0.0', v8: '12.0.0', modules: '127', napi: '10' },
  argv: ['node', 'turbo-test'],
  cwd: () => globalThis.__ttProcessCwd || globalThis.__cwd || '/',
  nextTick: (fn, ...a) => __loop.nextTicks.push(() => fn(...a)),
  hrtime: Object.assign(() => [0, 0], { bigint: () => 0n }),
  exit: () => {},
  on: () => globalThis.process,
  off: () => globalThis.process,
  once: () => globalThis.process,
  emit: () => false,
  stdout: { write: () => true, isTTY: false },
  stderr: { write: () => true, isTTY: false },
};

// ===========================================================================
// Event loop + timers (M2). Native macro/micro/nextTick ordering.
//   - microtasks: native V8 Promise jobs, drained by Rust (perform_microtask_checkpoint)
//   - process.nextTick: own queue, drained BEFORE microtasks each turn (Node priority)
//   - macrotasks (setTimeout/Interval/Immediate): virtual-clock queue
// Real mode: Rust drive loop interleaves the three (no actual sleeping; the clock jumps
// to the next due timer — deterministic, order-preserving). Fake mode: vi.* drives the
// clock synchronously from test code. Foundation for binding real @sinonjs/fake-timers.
// ===========================================================================
function __reportUncaught(e) {
  try { globalThis.console.error('uncaught', (e && e.message) || e); } catch {}
}
// the native host (napi_fatal_exception, an exception left by an async native callback)
// reports through the same path
globalThis.__reportUncaught = __reportUncaught;
const __loop = {
  macro: [],       // {id, due, cb, interval, gen}
  // Internal one-shot timers (test-timeout enforcement). Kept SEPARATE from `macro` so they are
  // invisible to the fake-timer API (getTimerCount / runAllTimers / advance / clearAllTimers) —
  // otherwise a test's `vi.runAllTimers()` would fire the timeout reject. Driven only by the
  // real-mode drive loop (stepMacro) so a genuinely hung async test still times out.
  internal: [],    // {id, due, cb, gen}
  nextTicks: [],
  seq: 1,
  now: 0,          // logical clock (ms)
  gen: 0,          // file generation (isolate-reuse): timers from a prior file are dropped, unrun
  fake: false,
  systemTime: 0,   // wall-clock base used by Date when faking
  RealDate: Date,
  realNow: Date.now.bind(Date),
};
function __schedule(cb, delay, interval) {
  const id = __loop.seq++;
  __loop.macro.push({ id, due: __loop.now + Math.max(0, delay | 0), cb, interval, gen: __loop.gen });
  return id;
}
// Isolate-reuse: drop timers left over from a PRIOR file (gen < current) WITHOUT running them.
// A finished file's leaked setInterval / self-rescheduling setTimeout would otherwise fire in the
// next file and re-arm itself → the loop never settles → the 2M guard spins (run crawls/hangs).
// Dropping at fire-time (not at reset) lets the CURRENT file keep its own pending one-shots
// (clearing those broke ~40 async tests).
function __dropStaleTimers() {
  if (__loop.macro.length && __loop.macro.some((t) => t.gen < __loop.gen)) {
    __loop.macro = __loop.macro.filter((t) => t.gen >= __loop.gen);
  }
  if (__loop.internal.length && __loop.internal.some((t) => t.gen < __loop.gen)) {
    __loop.internal = __loop.internal.filter((t) => t.gen >= __loop.gen);
  }
}
function __clearTimer(id) { __loop.macro = __loop.macro.filter((t) => t.id !== id); }
// Internal one-shot timer (test-timeout). Separate queue → invisible to the fake-timer API.
function __scheduleInternal(cb, delay) {
  const id = __loop.seq++;
  __loop.internal.push({ id, due: __loop.now + Math.max(0, delay | 0), cb, gen: __loop.gen });
  return id;
}
function __clearInternal(id) { __loop.internal = __loop.internal.filter((t) => t.id !== id); }
function __dueSorted(upto) {
  return __loop.macro.filter((t) => t.due <= upto).sort((a, b) => a.due - b.due || a.id - b.id);
}
__loop.drainNextTicks = function () {
  let ran = false;
  while (__loop.nextTicks.length) {
    const q = __loop.nextTicks; __loop.nextTicks = [];
    for (const f of q) { ran = true; try { f(); } catch (e) { __reportUncaught(e); } }
  }
  return ran;
};
// Real mode: run the single earliest-due macrotask (Rust drains micro/nextTick around it).
// Considers BOTH the user macro queue and the internal test-timeout queue, firing whichever is
// due first — so a hung test (no user timers) still advances to its timeout and rejects.
__loop.stepMacro = function () {
  __dropStaleTimers();
  __loop.macro.sort((a, b) => a.due - b.due || a.id - b.id);
  __loop.internal.sort((a, b) => a.due - b.due || a.id - b.id);
  const m = __loop.macro[0];
  const it = __loop.internal[0];
  if (!m && !it) return false;
  // earliest-due wins; ties favor the user timer (matches scheduling order intent).
  const useInternal = it && (!m || it.due < m.due);
  if (useInternal) {
    __loop.now = Math.max(__loop.now, it.due);
    __clearInternal(it.id);
    try { it.cb(); } catch (e) { __reportUncaught(e); }
    return true;
  }
  __loop.now = Math.max(__loop.now, m.due);
  if (m.interval == null) __clearTimer(m.id);
  else m.due = __loop.now + m.interval;
  try { m.cb(); } catch (e) { __reportUncaught(e); }
  return true;
};
// Fake mode: advance the virtual clock, firing timers in order (synchronous).
__loop.advance = function (ms) {
  __dropStaleTimers();
  const target = __loop.now + Math.max(0, ms | 0);
  let guard = 0;
  while (guard++ < 1e6) {
    const next = __dueSorted(target)[0];
    if (!next) break;
    __loop.now = next.due;
    if (next.interval == null) __clearTimer(next.id);
    else next.due = __loop.now + next.interval;
    try { next.cb(); } catch (e) { __reportUncaught(e); }
    __loop.drainNextTicks();
  }
  __loop.now = target;
};
__loop.runAll = function () {
  let guard = 0;
  while (__loop.macro.length && guard++ < 1e6) {
    __loop.macro.sort((a, b) => a.due - b.due || a.id - b.id);
    const t = __loop.macro[0];
    __loop.now = Math.max(__loop.now, t.due);
    if (t.interval == null) __clearTimer(t.id);
    else t.due = __loop.now + t.interval; // intervals would loop forever; guard caps it
    try { t.cb(); } catch (e) { __reportUncaught(e); }
    __loop.drainNextTicks();
    if (t.interval != null) break; // don't spin a periodic timer in runAll
  }
};
__loop.runOnlyPending = function () {
  const snapshot = __loop.macro.slice().sort((a, b) => a.due - b.due || a.id - b.id);
  for (const t of snapshot) {
    if (!__loop.macro.includes(t)) continue;
    __loop.now = Math.max(__loop.now, t.due);
    if (t.interval == null) __clearTimer(t.id);
    else t.due = __loop.now + t.interval;
    try { t.cb(); } catch (e) { __reportUncaught(e); }
    __loop.drainNextTicks();
  }
};

// timer globals (bare V8 has none)
globalThis.setTimeout = (cb, d, ...a) => __schedule(() => cb(...a), d, null);
globalThis.setInterval = (cb, d, ...a) => __schedule(() => cb(...a), d, Math.max(1, d | 0));
globalThis.setImmediate = (cb, ...a) => __schedule(() => cb(...a), 0, null);
globalThis.clearTimeout = (id) => __clearTimer(id);
globalThis.clearInterval = (id) => __clearTimer(id);
globalThis.clearImmediate = (id) => __clearTimer(id);
if (typeof globalThis.queueMicrotask !== 'function') {
  globalThis.queueMicrotask = (cb) => Promise.resolve().then(cb);
}

// Date faking
function __installFakeDate() {
  const Real = __loop.RealDate;
  function FakeDate(...args) {
    if (!(this instanceof FakeDate)) return new Real(__loop.systemTime + __loop.now).toString();
    if (args.length === 0) return new Real(__loop.systemTime + __loop.now);
    return new Real(...args);
  }
  FakeDate.prototype = Real.prototype;
  FakeDate.now = () => __loop.systemTime + __loop.now;
  FakeDate.UTC = Real.UTC;
  FakeDate.parse = Real.parse;
  globalThis.Date = FakeDate;
}
// Default clock: a Date whose now() is a REAL wall-clock base + the VIRTUAL elapsed (__loop.now,
// which advances as the loop fires timers). So `const t=Date.now(); await sleep(100); Date.now()-t`
// reports ~100 even though no real time passed — elapsed-across-setTimeout assertions work, while
// timestamps stay ~real (base captured at install). Only `now()` and arg-less `new Date()` are
// virtualized; `new Date(args)` and the prototype stay real (instanceof works).
let __virtualBase = 0;
function __installVirtualDate() {
  const Real = __loop.RealDate;
  if (!__virtualBase) __virtualBase = __loop.realNow();
  function VDate(...args) {
    if (!(this instanceof VDate)) return new Real(__virtualBase + __loop.now).toString();
    if (args.length === 0) return new Real(__virtualBase + __loop.now);
    return new Real(...args);
  }
  VDate.prototype = Real.prototype;
  VDate.now = () => __virtualBase + __loop.now;
  VDate.UTC = Real.UTC;
  VDate.parse = Real.parse;
  globalThis.Date = VDate;
}
function __restoreDate() { __installVirtualDate(); }
__installVirtualDate();

// A vi.mock/doMock factory that threw (or rejected): the module is registered as this stand-in,
// and importing it throws the factory's error (vitest wraps it, with the cause; jest rethrows it
// as is) instead of silently falling back to the real module. The native loader asks
// __ttMockFactoryError(exports) on each import of a mocked path; any other access to the
// stand-in throws the same error.
const __ttMockErrors = new WeakMap();
function __ttFactoryErrorMock(e, vitest) {
  const err = vitest
    ? new Error('[vitest] There was an error when mocking a module. If you are using "vi.mock" factory, '
      + 'make sure there are no top level variables inside, since this call is hoisted to top of the file. '
      + 'Read more: https://vitest.dev/api/vi.html#vi-mock Caused by: ' + String(e), { cause: e })
    : e;
  const stand = new Proxy({}, { get: (t, k) => { if (typeof k === 'symbol' || k === 'then') return undefined; throw err; } });
  __ttMockErrors.set(stand, err);
  return stand;
}
globalThis.__ttFactoryErrorMock = __ttFactoryErrorMock;
globalThis.__ttMockFactoryError = (m) => __ttMockErrors.get(m);

// await any async mock-factory results before the native loader drains them; a rejected factory
// registers the error stand-in
globalThis.__resolvePendingMocks = async () => {
  const list = globalThis.__pendingMocks || [];
  for (const m of list) {
    if (m.exports && typeof m.exports.then === 'function') {
      try { m.exports = __ttStrictMock(await m.exports, m.key, m.spec); }
      catch (e) { m.exports = __ttFactoryErrorMock(e, m.vitest !== false); }
    }
  }
};

// hooks for the Rust drive loop
globalThis.__drainNextTicks = () => __loop.drainNextTicks();
// Run one due macrotask (used while pumping the loop to resolve async vi.mock factories that
// `await import(...)` — dynamic imports can resolve across macrotask turns, not just microtasks).
globalThis.__stepMacro = () => __loop.stepMacro();
globalThis.__hasNextTicks = () => __loop.nextTicks.length > 0;
globalThis.__stepMacro = () => __loop.stepMacro();
// ms until the earliest macrotask (user or internal) is due, -1 when none is queued: how long
// the Rust drive loop waits for native addon work before firing it
globalThis.__ttNextMacroDelay = () => {
  let due = Infinity;
  for (const t of __loop.macro) due = Math.min(due, t.due);
  for (const t of __loop.internal) due = Math.min(due, t.due);
  return due === Infinity ? -1 : Math.max(0, due - __loop.now);
};

// ---- minimal Web Storage. A real `Storage` class (not an object literal) so the methods live on
// `Storage.prototype` — tests do `vi.spyOn(Storage.prototype, 'setItem')`, which only intercepts
// localStorage/sessionStorage calls when those are genuine Storage instances. ----
class __Storage {
  constructor() { this._m = new Map(); }
  getItem(k) { return this._m.has(String(k)) ? this._m.get(String(k)) : null; }
  setItem(k, v) { this._m.set(String(k), String(v)); }
  removeItem(k) { this._m.delete(String(k)); }
  clear() { this._m.clear(); }
  key(i) { return Array.from(this._m.keys())[i] ?? null; }
  get length() { return this._m.size; }
}
globalThis.Storage = __Storage;
globalThis.localStorage = new __Storage();
globalThis.sessionStorage = new __Storage();

// ---- TextEncoder/TextDecoder (host globals; bare V8 has none) ----
if (typeof globalThis.TextEncoder === 'undefined') {
  globalThis.TextEncoder = class TextEncoder {
    get encoding() { return 'utf-8'; }
    encode(s) {
      s = String(s); const b = [];
      for (let i = 0; i < s.length; i++) {
        let c = s.charCodeAt(i);
        if (c < 0x80) b.push(c);
        else if (c < 0x800) b.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f));
        else if (c >= 0xd800 && c < 0xdc00) {
          const c2 = s.charCodeAt(++i);
          c = 0x10000 + ((c & 0x3ff) << 10) + (c2 & 0x3ff);
          b.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 0x3f), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
        } else b.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
      }
      return new Uint8Array(b);
    }
  };
}
if (typeof globalThis.TextDecoder === 'undefined') {
  globalThis.TextDecoder = class TextDecoder {
    constructor(enc) { this.encoding = enc || 'utf-8'; }
    decode(buf) {
      if (!buf) return '';
      const b = buf instanceof Uint8Array ? buf : new Uint8Array(buf.buffer || buf);
      let s = '';
      for (let i = 0; i < b.length;) {
        let c = b[i++];
        if (c < 0x80) s += String.fromCharCode(c);
        else if (c < 0xe0) s += String.fromCharCode(((c & 0x1f) << 6) | (b[i++] & 0x3f));
        else if (c < 0xf0) s += String.fromCharCode(((c & 0xf) << 12) | ((b[i++] & 0x3f) << 6) | (b[i++] & 0x3f));
        else {
          let cp = ((c & 0x7) << 18) | ((b[i++] & 0x3f) << 12) | ((b[i++] & 0x3f) << 6) | (b[i++] & 0x3f);
          cp -= 0x10000;
          s += String.fromCharCode(0xd800 + (cp >> 10), 0xdc00 + (cp & 0x3ff));
        }
      }
      return s;
    }
  };
}

// ---- Web platform globals (bare V8 lacks these) ----
if (typeof globalThis.URLSearchParams === 'undefined') {
  globalThis.URLSearchParams = class URLSearchParams {
    constructor(init) {
      this._ = []; // list of [key, value] pairs — supports multiple values per key (append)
      if (typeof init === 'string') {
        // application/x-www-form-urlencoded: '+' decodes to a space (decodeURIComponent alone leaves
        // it). Matches Node/jsdom so `new URLSearchParams('a=John+Doe').get('a') === 'John Doe'`.
        const dec = (s) => { try { return decodeURIComponent(s.replace(/\+/g, ' ')); } catch { return s; } };
        init.replace(/^\?/, '').split('&').filter(Boolean).forEach((p) => {
          const i = p.indexOf('='); const k = i < 0 ? p : p.slice(0, i); const v = i < 0 ? '' : p.slice(i + 1);
          this._.push([dec(k), dec(v)]);
        });
      } else if (Array.isArray(init)) {
        init.forEach(([k, v]) => this._.push([String(k), String(v)]));
      } else if (init && typeof init.forEach === 'function') {
        init.forEach((v, k) => this._.push([String(k), String(v)]));
      } else if (init && typeof init === 'object') {
        for (const k of Object.keys(init)) this._.push([k, String(init[k])]);
      }
    }
    get(k) { const e = this._.find((p) => p[0] === k); return e ? e[1] : null; }
    getAll(k) { return this._.filter((p) => p[0] === k).map((p) => p[1]); }
    set(k, v) { this._ = this._.filter((p) => p[0] !== k); this._.push([k, String(v)]); }
    append(k, v) { this._.push([k, String(v)]); }
    has(k) { return this._.some((p) => p[0] === k); }
    delete(k) { this._ = this._.filter((p) => p[0] !== k); }
    forEach(f) { this._.slice().forEach(([k, v]) => f(v, k, this)); }
    keys() { return this._.map((p) => p[0])[Symbol.iterator](); }
    values() { return this._.map((p) => p[1])[Symbol.iterator](); }
    entries() { return this._.map((p) => [p[0], p[1]])[Symbol.iterator](); }
    sort() { this._.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0)); }
    // form-urlencoded serialization: encode, then render space as '+' (Node/jsdom do; turbo-test
    // previously emitted %20, diverging on `name=John+Doe`).
    toString() { const enc = (s) => encodeURIComponent(s).replace(/%20/g, '+'); return this._.map(([k, v]) => `${enc(k)}=${enc(v)}`).join('&'); }
    [Symbol.iterator]() { return this.entries(); }
  };
}
if (typeof globalThis.URL === 'undefined') {
  globalThis.URL = class URL {
    constructor(url, base) {
      url = String(url);
      if (base !== undefined && !/^[a-z][a-z0-9+.-]*:/i.test(url)) {
        url = URL._resolve(url, new globalThis.URL(String(base)));
      }
      // Real URL throws on input without a valid scheme (e.g. "not a url"). Matching this is
      // required for validators built on `new URL()` (e.g. zod's .url()).
      if (!/^[a-z][a-z0-9+.-]*:/i.test(url)) {
        throw new TypeError('Invalid URL: ' + url);
      }
      // protocol is the scheme up to the first ':' (works for opaque schemes like
      // javascript:/data:/mailto: that have no '//' authority).
      const scheme = /^([a-z][a-z0-9+.-]*:)/i.exec(url);
      this.protocol = scheme ? scheme[1] : 'http:';
      const m = /^([a-z][a-z0-9+.-]*:)\/\/([^/?#]*)([^?#]*)(\?[^#]*)?(#.*)?$/i.exec(url);
      let searchStr;
      if (m) {
        this._hierarchical = true;
        this.host = m[2] || '';
        this.hostname = (m[2] || '').split(':')[0];
        this.port = (m[2] || '').split(':')[1] || '';
        this.pathname = URL._removeDots(m[3] || '/');
        searchStr = m[4] || '';
        this.hash = m[5] || '';
        this.origin = this.protocol + '//' + this.host;
      } else {
        // opaque URL (no authority): everything after the scheme is the path (sans query/hash).
        this._hierarchical = false;
        this.host = ''; this.hostname = ''; this.port = '';
        const rest = url.slice(this.protocol.length);
        const qi = rest.indexOf('?'); const hi = rest.indexOf('#');
        let cut = rest.length;
        if (qi >= 0) cut = Math.min(cut, qi);
        if (hi >= 0) cut = Math.min(cut, hi);
        this.pathname = rest.slice(0, cut);
        const after = rest.slice(cut);
        const am = /^(\?[^#]*)?(#.*)?$/.exec(after) || [];
        searchStr = am[1] || ''; this.hash = am[2] || ''; this.origin = 'null';
      }
      // searchParams is the SOURCE OF TRUTH for the query; `search`/`href`/`toString` derive from it
      // so post-construction `searchParams.set()/append()` mutations are reflected (real URL semantics
      // — previously href was frozen at construction and dropped them).
      this._sp = new globalThis.URLSearchParams(searchStr);
    }
    get searchParams() { return this._sp; }
    get search() { const s = this._sp.toString(); return s ? '?' + s : ''; }
    set search(v) { this._sp = new globalThis.URLSearchParams(String(v).replace(/^\?/, '')); }
    get href() {
      const auth = this._hierarchical ? this.protocol + '//' + this.host : this.protocol;
      return auth + this.pathname + this.search + (this.hash || '');
    }
    set href(v) { const u = new globalThis.URL(v); this.protocol = u.protocol; this.host = u.host; this.hostname = u.hostname; this.port = u.port; this.pathname = u.pathname; this.hash = u.hash; this.origin = u.origin; this._hierarchical = u._hierarchical; this._sp = u._sp; }
    toString() { return this.href; }
    // WHATWG relative-reference resolution against a parsed base (Node's `new URL(rel, base)`):
    // scheme-relative `//h/p`, absolute path `/p`, query-only `?q`, fragment-only `#f`, empty,
    // and a relative path merged with the base path's directory. Dot segments are removed by
    // the caller's pathname normalization. An opaque base (`mailto:x`) only takes a fragment.
    static _resolve(rel, b) {
      const auth = b.protocol + '//' + b.host;
      const noHash = b.href.replace(/#.*$/, '');
      if (!b._hierarchical) {
        if (rel.startsWith('#')) return noHash + rel;
        throw new TypeError('Invalid URL: ' + rel);
      }
      if (rel.startsWith('//')) return b.protocol + rel;
      if (rel.startsWith('/')) return auth + rel;
      if (rel === '') return noHash;
      if (rel.startsWith('#')) return noHash + rel;
      if (rel.startsWith('?')) return auth + b.pathname + rel;
      return auth + b.pathname.replace(/[^/]*$/, '') + rel;
    }
    // RFC 3986 / WHATWG dot-segment removal on a path (`/a/./b/../c` -> `/a/c`).
    static _removeDots(path) {
      const out = [];
      const segs = path.split('/');
      for (let i = 1; i < segs.length; i++) {
        const seg = segs[i];
        const last = i === segs.length - 1;
        if (seg === '..' || seg.toLowerCase() === '%2e%2e') {
          if (out.length) out.pop();
          if (last) out.push('');
        } else if (seg === '.' || seg.toLowerCase() === '%2e') {
          if (last) out.push('');
        } else {
          out.push(seg);
        }
      }
      return '/' + out.join('/');
    }
    static createObjectURL() { return 'blob:mock'; }
    static revokeObjectURL() {}
    static parse(url, base) { try { return new globalThis.URL(url, base); } catch { return null; } }
    static canParse(url, base) { try { new globalThis.URL(url, base); return true; } catch { return false; } }
  };
}
if (globalThis.URL && typeof globalThis.URL.parse !== 'function') {
  globalThis.URL.parse = (url, base) => { try { return new globalThis.URL(url, base); } catch { return null; } };
  globalThis.URL.canParse = (url, base) => { try { new globalThis.URL(url, base); return true; } catch { return false; } };
}
if (typeof globalThis.DOMException === 'undefined') {
  globalThis.DOMException = class DOMException extends Error {
    constructor(message, name) { super(message); this.name = name || 'Error'; this.code = 0; }
  };
}
globalThis.__STATUS_TEXT = globalThis.__STATUS_TEXT || {
  200: 'OK', 201: 'Created', 202: 'Accepted', 204: 'No Content', 301: 'Moved Permanently',
  302: 'Found', 304: 'Not Modified', 400: 'Bad Request', 401: 'Unauthorized', 403: 'Forbidden',
  404: 'Not Found', 405: 'Method Not Allowed', 409: 'Conflict', 422: 'Unprocessable Entity',
  429: 'Too Many Requests', 500: 'Internal Server Error', 501: 'Not Implemented',
  502: 'Bad Gateway', 503: 'Service Unavailable', 504: 'Gateway Timeout',
};
if (typeof globalThis.AbortController === 'undefined') {
  globalThis.AbortSignal = class AbortSignal {
    constructor() { this.aborted = false; this.reason = undefined; this.onabort = null; this._listeners = []; }
    addEventListener(type, fn) { if (type === 'abort') this._listeners.push(fn); }
    removeEventListener(type, fn) { this._listeners = this._listeners.filter((f) => f !== fn); }
    dispatchEvent(ev) { if (ev && ev.type === 'abort') this._fire(); return true; }
    _fire() { const ev = { type: 'abort', target: this }; if (typeof this.onabort === 'function') this.onabort(ev); this._listeners.slice().forEach((f) => { try { f(ev); } catch (e) {} }); }
    throwIfAborted() { if (this.aborted) throw this.reason; }
    static abort(reason) { const s = new globalThis.AbortSignal(); s.aborted = true; s.reason = reason || new DOMException('Aborted', 'AbortError'); return s; }
    static timeout() { return new globalThis.AbortSignal(); }
  };
  globalThis.AbortController = class AbortController {
    constructor() { this.signal = new globalThis.AbortSignal(); }
    abort(reason) { if (!this.signal || this.signal.aborted) return; this.signal.aborted = true; this.signal.reason = reason || new DOMException('Aborted', 'AbortError'); this.signal._fire(); }
  };
}
if (typeof globalThis.Blob === 'undefined') {
  globalThis.Blob = class Blob {
    constructor(parts = [], opts = {}) {
      this._parts = parts || [];
      this.type = (opts && opts.type) || '';
      this.size = this._parts.reduce((n, p) => {
        if (p == null) return n;
        if (typeof p === 'string') return n + p.length;
        if (typeof p.byteLength === 'number') return n + p.byteLength; // ArrayBuffer / TypedArray
        if (typeof p.size === 'number') return n + p.size;             // Blob / File
        if (typeof p.length === 'number') return n + p.length;
        return n;
      }, 0);
    }
    text() {
      return Promise.resolve(this._parts.map((p) => {
        if (typeof p === 'string') return p;
        if (p instanceof ArrayBuffer) return new globalThis.TextDecoder().decode(new Uint8Array(p));
        if (p && p.buffer instanceof ArrayBuffer) return new globalThis.TextDecoder().decode(p); // TypedArray
        if (p && typeof p._parts !== 'undefined') return p._parts.join(''); // nested Blob
        return String(p);
      }).join(''));
    }
    arrayBuffer() { return Promise.resolve(new ArrayBuffer(this.size)); }
    slice() { return new globalThis.Blob(); }
    stream() { return null; }
  };
}
if (typeof globalThis.File === 'undefined') {
  globalThis.File = class File extends globalThis.Blob {
    constructor(parts, name, opts) { super(parts, opts); this.name = String(name); this.lastModified = (opts && opts.lastModified) || 0; }
  };
}
// Pure base64 (no btoa<->Buffer interdependency — turbo-dom's btoa calls Buffer.from, so
// Buffer.toString('base64') must NOT call back into btoa or it infinitely recurses).
const __B64CHARS = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
globalThis.__b64encodeBytes = (bytes) => {
  let out = '';
  for (let i = 0; i < bytes.length; i += 3) {
    const b0 = bytes[i], b1 = i + 1 < bytes.length ? bytes[i + 1] : 0, b2 = i + 2 < bytes.length ? bytes[i + 2] : 0;
    out += __B64CHARS[b0 >> 2] + __B64CHARS[((b0 & 3) << 4) | (b1 >> 4)];
    out += i + 1 < bytes.length ? __B64CHARS[((b1 & 15) << 2) | (b2 >> 6)] : '=';
    out += i + 2 < bytes.length ? __B64CHARS[b2 & 63] : '=';
  }
  return out;
};
globalThis.__b64decodeToBytes = (str) => {
  str = String(str).replace(/[^A-Za-z0-9+/]/g, '');
  const bytes = [];
  for (let i = 0; i < str.length; i += 4) {
    const n = (__B64CHARS.indexOf(str[i]) << 18) | (__B64CHARS.indexOf(str[i + 1]) << 12) | ((str[i + 2] ? __B64CHARS.indexOf(str[i + 2]) : 0) << 6) | (str[i + 3] ? __B64CHARS.indexOf(str[i + 3]) : 0);
    bytes.push((n >> 16) & 255);
    if (i + 2 < str.length) bytes.push((n >> 8) & 255);
    if (i + 3 < str.length) bytes.push(n & 255);
  }
  return bytes;
};
if (typeof globalThis.btoa === 'undefined') {
  globalThis.btoa = (s) => { const str = String(s); const b = new Array(str.length); for (let i = 0; i < str.length; i++) b[i] = str.charCodeAt(i) & 255; return globalThis.__b64encodeBytes(b); };
  globalThis.atob = (s) => String.fromCharCode.apply(null, globalThis.__b64decodeToBytes(s));
}
if (typeof globalThis.FileReader === 'undefined') {
  globalThis.FileReader = class FileReader {
    constructor() { this.result = null; this.error = null; this.readyState = 0; this.onload = null; this.onerror = null; this.onloadend = null; this.onabort = null; }
    _emit(name, ev) { if (typeof this['on' + name] === 'function') this['on' + name](ev); }
    _read(blob, kind) {
      Promise.resolve().then(async () => {
        try {
          const text = blob && blob.text ? await blob.text() : '';
          if (kind === 'data') { const bytes = []; for (let i = 0; i < text.length; i++) bytes.push(text.charCodeAt(i) & 255); this.result = 'data:' + ((blob && blob.type) || 'application/octet-stream') + ';base64,' + globalThis.__b64encodeBytes(bytes); }
          else if (kind === 'buffer') { const u = new Uint8Array((text || '').length); for (let i = 0; i < u.length; i++) u[i] = text.charCodeAt(i) & 255; this.result = u.buffer; }
          else this.result = text;
          this.readyState = 2;
          const ev = { target: this };
          this._emit('load', ev); this._emit('loadend', ev);
        } catch (e) {
          this.error = e; this.readyState = 2;
          const ev = { target: this };
          this._emit('error', ev); this._emit('loadend', ev);
        }
      });
    }
    readAsDataURL(b) { this._read(b, 'data'); }
    readAsText(b) { this._read(b, 'text'); }
    readAsArrayBuffer(b) { this._read(b, 'buffer'); }
    addEventListener(n, f) { this['on' + n] = f; }
    removeEventListener(n) { this['on' + n] = null; }
    abort() { this._emit('abort', { target: this }); }
  };
}
// turbo-dom's installGlobals replaces Blob/File with SoA-backed versions whose .text()/byte
// reads return zero-filled content. Capture our working implementations so the DOM bootstrap
// can restore them (FileReader/fileToBase64/CSV parsing need real bytes).
globalThis.__ttRestoreFileApis = (() => {
  const B = globalThis.Blob, F = globalThis.File, FR = globalThis.FileReader;
  return () => { globalThis.Blob = B; globalThis.File = F; globalThis.FileReader = FR; };
})();
if (typeof globalThis.fetch === 'undefined') {
  globalThis.fetch = () => Promise.reject(new Error('fetch is not supported in turbo-test env'));
  globalThis.Headers = class Headers {
    constructor(init) {
      this._ = new Map();
      if (init) {
        if (typeof init.forEach === 'function' && !Array.isArray(init)) init.forEach((v, k) => this.set(k, v));
        else if (Array.isArray(init)) init.forEach(([k, v]) => this.set(k, v));
        else for (const k of Object.keys(init)) this.set(k, init[k]);
      }
    }
    get(k) { return this._.get(String(k).toLowerCase()) ?? null; }
    set(k, v) { this._.set(String(k).toLowerCase(), String(v)); }
    append(k, v) { const e = this._.get(String(k).toLowerCase()); this._.set(String(k).toLowerCase(), e ? e + ', ' + v : String(v)); }
    has(k) { return this._.has(String(k).toLowerCase()); }
    delete(k) { this._.delete(String(k).toLowerCase()); }
    forEach(f) { this._.forEach((v, k) => f(v, k, this)); }
    entries() { return this._.entries(); }
    keys() { return this._.keys(); }
    values() { return this._.values(); }
    getSetCookie() { const v = this._.get('set-cookie'); return v ? [v] : []; }
    [Symbol.iterator]() { return this._.entries(); }
  };
  globalThis.Request = class Request {
    constructor(url, init) {
      init = init || {};
      // define url as an own, writable, configurable prop — a plain `this.url = x` would invoke
      // a getter-only `url` on a subclass prototype (e.g. Next's NextRequest) and throw.
      const u = typeof url === 'object' && url ? String(url.url || url.href || url) : String(url);
      try { Object.defineProperty(this, 'url', { value: u, writable: true, configurable: true, enumerable: true }); }
      catch (e) {}
      this.method = init.method || 'GET';
      this.headers = init.headers instanceof globalThis.Headers ? init.headers : (() => { const h = new globalThis.Headers(); if (init.headers) for (const k of Object.keys(init.headers)) h.set(k, init.headers[k]); return h; })();
      this._body = init.body;
    }
    json() { return Promise.resolve(typeof this._body === 'string' ? JSON.parse(this._body) : this._body); }
    text() { return Promise.resolve(this._body == null ? '' : String(this._body)); }
    clone() { return new globalThis.Request(this.url, { method: this.method, headers: this.headers, body: this._body }); }
  };
  globalThis.Response = class Response {
    constructor(body, init) { this.body = body; this._body = body; this.status = (init && init.status) || 200; this.statusText = (init && init.statusText) || globalThis.__STATUS_TEXT[this.status] || ''; this.ok = this.status >= 200 && this.status < 300; this.headers = (init && init.headers instanceof globalThis.Headers) ? init.headers : new globalThis.Headers(init && init.headers); }
    json() { return Promise.resolve(typeof this._body === 'string' ? JSON.parse(this._body) : this._body); }
    text() { return Promise.resolve(this._body == null ? '' : String(this._body)); }
    clone() { return new globalThis.Response(this._body, { status: this.status }); }
    static json(data, init) { const r = new globalThis.Response(JSON.stringify(data), init); r._body = data; r.headers.set('content-type', 'application/json'); return r; }
    static error() { return new globalThis.Response(null, { status: 500 }); }
    static redirect(url, status) { const r = new globalThis.Response(null, { status: status || 302 }); r.headers.set('location', String(url)); return r; }
  };
}
if (typeof globalThis.FormData === 'undefined') {
  globalThis.FormData = class FormData { constructor() { this._ = []; } append(k, v) { this._.push([k, v]); } get(k) { const e = this._.find((x) => x[0] === k); return e ? e[1] : null; } getAll(k) { return this._.filter((x) => x[0] === k).map((x) => x[1]); } has(k) { return this._.some((x) => x[0] === k); } forEach(f) { this._.forEach(([k, v]) => f(v, k)); } };
}
if (typeof globalThis.structuredClone === 'undefined') {
  // V8's structured clone (src/structured_clone.rs), with Node's argument checks: the ArrayBuffers
  // of `transfer` move to the clone
  globalThis.structuredClone = function structuredClone(value, options) {
    if (arguments.length === 0) throw new TypeError('The "The value argument must be specified" argument must be specified');
    let transfer = [];
    const t = options == null ? undefined : options.transfer;
    if (t !== undefined) {
      if (t === null || (typeof t !== 'object' && typeof t !== 'function') || typeof t[Symbol.iterator] !== 'function') {
        throw new TypeError("Failed to execute 'structuredClone': transfer in Options cannot be converted to sequence.");
      }
      transfer = [...t];
    }
    return globalThis.__ttStructuredClone(value, transfer);
  };
}
if (typeof globalThis.global === 'undefined') globalThis.global = globalThis;
// Tell React it's in a test act() environment: concurrent work defers to the act queue instead
// of the real (MessageChannel) scheduler, so act() flushing doesn't re-enter React's work loop
// ("Should not already be working"). @testing-library/react sets this; our env must too.
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
// Minimal DOMMatrix/DOMPoint (SVG transform math used by charting libs at module load).
if (typeof globalThis.DOMMatrix === 'undefined') {
  class DOMMatrix {
    constructor(init) {
      this.a = 1; this.b = 0; this.c = 0; this.d = 1; this.e = 0; this.f = 0;
      this.m11 = 1; this.m12 = 0; this.m13 = 0; this.m14 = 0;
      this.m21 = 0; this.m22 = 1; this.m23 = 0; this.m24 = 0;
      this.m31 = 0; this.m32 = 0; this.m33 = 1; this.m34 = 0;
      this.m41 = 0; this.m42 = 0; this.m43 = 0; this.m44 = 1;
      this.is2D = true; this.isIdentity = true;
      if (Array.isArray(init) && init.length >= 6) { [this.a, this.b, this.c, this.d, this.e, this.f] = init; this.m11 = this.a; this.m12 = this.b; this.m21 = this.c; this.m22 = this.d; this.m41 = this.e; this.m42 = this.f; }
    }
    multiply() { return new DOMMatrix(); }
    multiplySelf() { return this; }
    preMultiplySelf() { return this; }
    translate() { return new DOMMatrix(); }
    translateSelf() { return this; }
    scale() { return new DOMMatrix(); }
    scaleSelf() { return this; }
    scale3d() { return new DOMMatrix(); }
    rotate() { return new DOMMatrix(); }
    rotateSelf() { return this; }
    rotateFromVector() { return new DOMMatrix(); }
    skewX() { return new DOMMatrix(); }
    skewY() { return new DOMMatrix(); }
    inverse() { return new DOMMatrix(); }
    invertSelf() { return this; }
    flipX() { return new DOMMatrix(); }
    flipY() { return new DOMMatrix(); }
    transformPoint(p) { return new globalThis.DOMPoint(p && p.x, p && p.y, p && p.z, p && p.w); }
    toFloat32Array() { return new Float32Array(16); }
    toFloat64Array() { return new Float64Array(16); }
    toString() { return 'matrix(1, 0, 0, 1, 0, 0)'; }
    static fromMatrix() { return new DOMMatrix(); }
    static fromFloat32Array() { return new DOMMatrix(); }
    static fromFloat64Array() { return new DOMMatrix(); }
  }
  globalThis.DOMMatrix = DOMMatrix;
  globalThis.DOMMatrixReadOnly = DOMMatrix;
  globalThis.WebKitCSSMatrix = DOMMatrix;
  globalThis.SVGMatrix = DOMMatrix;
}
// Web Streams. This used to be a stub whose reader always resolved `{ done: true }` — the globals
// existed (so `class X extends TransformStream` loaded) but any code that actually CONSUMED a
// stream (fetch body / SSE parsing) silently saw an empty stream. This is a working implementation:
// chunk queue + pull backpressure, reader/writer locking, cancel/abort propagation, tee, async
// iteration, pipeTo/pipeThrough. Not spec-exhaustive (no BYOB readers, no queuing strategy `size`).
// A host that already provides native streams (napi/Node) still wins via the guard.
if (typeof globalThis.TransformStream === 'undefined') {
  // `silent` pre-attaches a no-op catch: `reader.closed` / `writer.closed` reject when the stream
  // errors, and a test that never touches them must not trip an unhandled-rejection report.
  const __sDefer = (silent) => {
    let resolve;
    let reject;
    const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
    if (silent) promise.catch(() => {});
    return { promise, resolve, reject };
  };
  // Return a promise the caller may ignore without tripping unhandled-rejection reporting, while
  // still rejecting for a caller that does await it.
  const __sQuiet = (p) => { p.catch(() => {}); return p; };

  // ---- readable ----
  // Ask the source for more data. Re-pulls only when the last pull actually enqueued something,
  // so a source whose pull() never enqueues can't spin forever.
  const __rsPull = (s) => {
    if (s._err || s._closed || s._pulling || typeof s._src.pull !== 'function') return;
    if (s._queue.length >= s._hwm && !s._pending.length) return;
    s._pulling = true;
    const before = s._enq;
    Promise.resolve(s._startP)
      .then(() => s._src.pull(s._ctrl))
      .then(
        () => { s._pulling = false; if (s._enq !== before) __rsPull(s); },
        (e) => { s._pulling = false; try { s._ctrl.error(e); } catch (_) {} },
      );
  };
  const __rsRead = (s) => {
    if (s._queue.length) {
      const value = s._queue.shift();
      // A close() with chunks still queued stays readable until they drain (spec behavior).
      if (s._closed && !s._queue.length) s._closedD.resolve(undefined);
      __rsPull(s);
      return Promise.resolve({ value, done: false });
    }
    if (s._err) return Promise.reject(s._err.e);
    if (s._closed) return Promise.resolve({ value: undefined, done: true });
    const d = __sDefer();
    s._pending.push(d);
    __rsPull(s);
    return d.promise;
  };
  const __rsCancel = (s, reason) => {
    if (s._err) return Promise.reject(s._err.e);
    s._queue.length = 0;
    if (!s._closed) {
      s._closed = true;
      for (const p of s._pending.splice(0)) p.resolve({ value: undefined, done: true });
      s._closedD.resolve(undefined);
    }
    let r;
    try { r = typeof s._src.cancel === 'function' ? s._src.cancel(reason) : undefined; } catch (e) { return Promise.reject(e); }
    return Promise.resolve(r).then(() => undefined);
  };

  class ReadableStreamDefaultController {
    constructor(stream) { this._s = stream; }
    get desiredSize() {
      const s = this._s;
      if (s._err) return null;
      if (s._closed) return 0;
      return s._hwm - s._queue.length;
    }
    enqueue(chunk) {
      const s = this._s;
      if (s._closed || s._err) throw new TypeError('Cannot enqueue a chunk into a closed or errored readable stream');
      s._enq++;
      const p = s._pending.shift();
      if (p) p.resolve({ value: chunk, done: false });
      else s._queue.push(chunk);
    }
    close() {
      const s = this._s;
      if (s._closed || s._err) throw new TypeError('Cannot close a closed or errored readable stream');
      s._closed = true;
      for (const p of s._pending.splice(0)) p.resolve({ value: undefined, done: true });
      if (!s._queue.length) s._closedD.resolve(undefined);
    }
    error(e) {
      const s = this._s;
      if (s._err) return;
      s._err = { e };
      s._queue.length = 0;
      for (const p of s._pending.splice(0)) p.reject(e);
      s._closedD.reject(e);
    }
  }

  class ReadableStreamDefaultReader {
    constructor(stream) {
      if (stream._reader) throw new TypeError('This stream has already been locked for exclusive reading by another reader');
      this._s = stream;
      stream._reader = this;
    }
    get closed() { return this._s ? this._s._closedD.promise : Promise.resolve(undefined); }
    read() { return this._s ? __rsRead(this._s) : Promise.reject(new TypeError('Cannot read from a released reader')); }
    cancel(reason) { return this._s ? __rsCancel(this._s, reason) : Promise.reject(new TypeError('Cannot cancel a released reader')); }
    releaseLock() { if (this._s) { this._s._reader = null; this._s = null; } }
  }

  class ReadableStream {
    constructor(underlyingSource, strategy) {
      this._src = underlyingSource || {};
      this._hwm = strategy && typeof strategy.highWaterMark === 'number' ? strategy.highWaterMark : 1;
      this._queue = [];
      this._pending = [];
      this._closed = false;
      this._err = null;
      this._enq = 0;          // enqueue counter — drives the re-pull decision in __rsPull
      this._pulling = false;
      this._reader = null;
      this._closedD = __sDefer(true);
      this._ctrl = new ReadableStreamDefaultController(this);
      this._startP = undefined;
      if (typeof this._src.start === 'function') {
        try { this._startP = this._src.start(this._ctrl); } catch (e) { this._ctrl.error(e); }
      }
      if (this._startP && typeof this._startP.then === 'function') {
        this._startP.then(() => __rsPull(this), (e) => { try { this._ctrl.error(e); } catch (_) {} });
      } else {
        __rsPull(this);
      }
    }
    get locked() { return this._reader != null; }
    getReader(opts) {
      if (opts && opts.mode === 'byob') throw new TypeError('BYOB readers are not supported in the turbo-test env');
      return new ReadableStreamDefaultReader(this);
    }
    cancel(reason) {
      if (this._reader) return Promise.reject(new TypeError('Cannot cancel a stream locked by a reader'));
      return __rsCancel(this, reason);
    }
    tee() {
      const reader = this.getReader();
      const branches = [];
      let inflight = null;
      // One source read feeds both branches; whichever branch pulls first starts it.
      const pump = () => {
        if (!inflight) {
          inflight = reader.read().then(
            ({ value, done }) => {
              inflight = null;
              for (const b of branches) { try { if (done) b._ctrl.close(); else b._ctrl.enqueue(value); } catch (_) {} }
            },
            (e) => { inflight = null; for (const b of branches) { try { b._ctrl.error(e); } catch (_) {} } },
          );
        }
        return inflight;
      };
      let cancelled = 0;
      const src = { pull: pump, cancel: () => (++cancelled === 2 ? reader.cancel() : undefined) };
      branches.push(new ReadableStream(src), new ReadableStream(src));
      return branches;
    }
    pipeTo(dest, opts) {
      const o = opts || {};
      const reader = this.getReader();
      const writer = dest.getWriter();
      const pump = () => reader.read().then(({ value, done }) => {
        if (done) return o.preventClose ? undefined : writer.close();
        return Promise.resolve(writer.write(value)).then(pump);
      });
      return __sQuiet(pump().then(
        () => { reader.releaseLock(); writer.releaseLock(); },
        (e) => {
          reader.releaseLock();
          const p = o.preventAbort ? Promise.resolve() : Promise.resolve(writer.abort(e)).catch(() => {});
          return p.then(() => { writer.releaseLock(); throw e; });
        },
      ));
    }
    pipeThrough(pair, opts) {
      if (!pair || !pair.writable || !pair.readable) throw new TypeError('pipeThrough requires a { readable, writable } pair');
      this.pipeTo(pair.writable, opts);
      return pair.readable;
    }
    values(opts) { return this[Symbol.asyncIterator](opts); }
    [Symbol.asyncIterator]() {
      const reader = this.getReader();
      return {
        next: () => reader.read().then((r) => { if (r.done) reader.releaseLock(); return r; }),
        return: (v) => reader.cancel().then(() => { reader.releaseLock(); return { value: v, done: true }; }),
        [Symbol.asyncIterator]() { return this; },
      };
    }
  }

  // ---- writable ----
  const __wsError = (s, e) => {
    if (s._err || s._closedFlag) return;
    s._err = { e };
    s._closedD.reject(e);
    s._readyD.reject(e);
  };
  // Writes are serialized through a single promise chain so the sink sees them in order.
  const __wsWrite = (s, chunk) => {
    if (s._err) return Promise.reject(s._err.e);
    if (s._closedFlag) return Promise.reject(new TypeError('Cannot write to a closed stream'));
    s._chain = s._chain.then(() => (typeof s._sink.write === 'function' ? s._sink.write(chunk, s._ctrl) : undefined));
    const p = s._chain;
    p.catch((e) => __wsError(s, e));
    return __sQuiet(p.then(() => undefined));
  };
  const __wsClose = (s) => {
    if (s._err) return Promise.reject(s._err.e);
    if (s._closedFlag) return Promise.reject(new TypeError('Cannot close an already-closed stream'));
    s._closedFlag = true;
    const p = s._chain.then(() => (typeof s._sink.close === 'function' ? s._sink.close(s._ctrl) : undefined));
    s._chain = p;
    p.then(() => s._closedD.resolve(undefined), (e) => { s._err = { e }; s._closedD.reject(e); });
    return __sQuiet(p.then(() => undefined));
  };
  const __wsAbort = (s, reason) => {
    if (s._err) return Promise.resolve(undefined);
    s._ctrl.signal.aborted = true;
    s._ctrl.signal.reason = reason;
    __wsError(s, reason instanceof Error ? reason : new Error(reason === undefined ? 'The stream was aborted' : String(reason)));
    let r;
    try { r = typeof s._sink.abort === 'function' ? s._sink.abort(reason) : undefined; } catch (e) { return Promise.reject(e); }
    return Promise.resolve(r).then(() => undefined);
  };

  class WritableStreamDefaultController {
    constructor(stream) {
      this._s = stream;
      this.signal = { aborted: false, reason: undefined, addEventListener() {}, removeEventListener() {} };
    }
    error(e) { __wsError(this._s, e); }
  }

  class WritableStreamDefaultWriter {
    constructor(stream) {
      if (stream._writer) throw new TypeError('This stream has already been locked for exclusive writing by another writer');
      this._s = stream;
      stream._writer = this;
    }
    get closed() { return this._s ? this._s._closedD.promise : Promise.resolve(undefined); }
    get ready() { return this._s ? this._s._readyD.promise : Promise.resolve(undefined); }
    get desiredSize() { const s = this._s; if (!s || s._err) return null; return s._closedFlag ? 0 : s._hwm; }
    write(chunk) { return this._s ? __wsWrite(this._s, chunk) : Promise.reject(new TypeError('Cannot write with a released writer')); }
    close() { return this._s ? __wsClose(this._s) : Promise.reject(new TypeError('Cannot close with a released writer')); }
    abort(reason) { return this._s ? __wsAbort(this._s, reason) : Promise.reject(new TypeError('Cannot abort with a released writer')); }
    releaseLock() { if (this._s) { this._s._writer = null; this._s = null; } }
  }

  class WritableStream {
    constructor(underlyingSink, strategy) {
      this._sink = underlyingSink || {};
      this._hwm = strategy && typeof strategy.highWaterMark === 'number' ? strategy.highWaterMark : 1;
      this._writer = null;
      this._err = null;
      this._closedFlag = false;
      this._closedD = __sDefer(true);
      this._readyD = __sDefer(true);
      this._readyD.resolve(undefined);
      this._ctrl = new WritableStreamDefaultController(this);
      this._chain = Promise.resolve();
      if (typeof this._sink.start === 'function') {
        this._chain = __sQuiet(Promise.resolve().then(() => this._sink.start(this._ctrl)).catch((e) => { __wsError(this, e); throw e; }));
      }
    }
    get locked() { return this._writer != null; }
    getWriter() { return new WritableStreamDefaultWriter(this); }
    close() { return this._writer ? Promise.reject(new TypeError('Cannot close a stream locked by a writer')) : __wsClose(this); }
    abort(reason) { return this._writer ? Promise.reject(new TypeError('Cannot abort a stream locked by a writer')) : __wsAbort(this, reason); }
  }

  // ---- transform ----
  class TransformStreamDefaultController {
    constructor(ts) { this._ts = ts; }
    get desiredSize() { return this._ts.readable._ctrl.desiredSize; }
    enqueue(chunk) { this._ts.readable._ctrl.enqueue(chunk); }
    error(e) { try { this._ts.readable._ctrl.error(e); } catch (_) {} __wsError(this._ts.writable, e); }
    terminate() { try { this._ts.readable._ctrl.close(); } catch (_) {} }
  }

  class TransformStream {
    constructor(transformer, writableStrategy, readableStrategy) {
      const t = transformer || {};
      // Push-driven: transform() enqueues straight into the readable side (no readable-side
      // backpressure onto the writable side, which is the one spec behavior we don't model).
      this.readable = new ReadableStream({}, readableStrategy);
      const ctrl = new TransformStreamDefaultController(this);
      if (typeof t.start === 'function') t.start(ctrl);
      this.writable = new WritableStream({
        write: (chunk) => (typeof t.transform === 'function' ? t.transform(chunk, ctrl) : ctrl.enqueue(chunk)),
        close: () => Promise.resolve(typeof t.flush === 'function' ? t.flush(ctrl) : undefined)
          .then(() => { try { this.readable._ctrl.close(); } catch (_) {} }),
        abort: (reason) => { try { this.readable._ctrl.error(reason instanceof Error ? reason : new Error(String(reason))); } catch (_) {} },
      }, writableStrategy);
    }
  }

  globalThis.ReadableStream = ReadableStream;
  globalThis.ReadableStreamDefaultReader = ReadableStreamDefaultReader;
  globalThis.ReadableStreamDefaultController = ReadableStreamDefaultController;
  globalThis.WritableStream = WritableStream;
  globalThis.WritableStreamDefaultWriter = WritableStreamDefaultWriter;
  globalThis.WritableStreamDefaultController = WritableStreamDefaultController;
  globalThis.TransformStream = TransformStream;
  globalThis.TransformStreamDefaultController = TransformStreamDefaultController;
}
if (typeof globalThis.DOMPoint === 'undefined') {
  class DOMPoint {
    constructor(x, y, z, w) { this.x = x || 0; this.y = y || 0; this.z = z || 0; this.w = w == null ? 1 : w; }
    matrixTransform() { return this; }
    static fromPoint(p) { return new DOMPoint(p && p.x, p && p.y, p && p.z, p && p.w); }
  }
  globalThis.DOMPoint = DOMPoint;
  globalThis.DOMPointReadOnly = DOMPoint;
}
if (typeof globalThis.Buffer === 'undefined') {
  class Buffer extends Uint8Array {
    static from(d, enc) {
      if (typeof d === 'string') {
        if (enc === 'base64') { return new Buffer(globalThis.__b64decodeToBytes(d)); }
        if (enc === 'hex') { const b = new Buffer(d.length / 2); for (let i = 0; i < b.length; i++) b[i] = parseInt(d.substr(i * 2, 2), 16); return b; }
        return new Buffer(new globalThis.TextEncoder().encode(d));
      }
      if (Array.isArray(d) || d instanceof Uint8Array) return new Buffer(d);
      if (d instanceof ArrayBuffer) return new Buffer(new Uint8Array(d));
      return new Buffer(0);
    }
    static alloc(n, fill) { const b = new Buffer(n); if (fill != null) b.fill(typeof fill === 'string' ? fill.charCodeAt(0) : fill); return b; }
    static allocUnsafe(n) { return new Buffer(n); }
    static isBuffer(x) { return x instanceof Buffer; }
    static concat(list) { const len = list.reduce((n, b) => n + b.length, 0); const out = new Buffer(len); let o = 0; for (const b of list) { out.set(b, o); o += b.length; } return out; }
    toString(enc) {
      if (enc === 'hex') return Array.from(this).map((b) => b.toString(16).padStart(2, '0')).join('');
      if (enc === 'base64') return globalThis.__b64encodeBytes(Array.from(this));
      return new globalThis.TextDecoder().decode(this);
    }
    toJSON() { return { type: 'Buffer', data: Array.from(this) }; }
  }
  globalThis.Buffer = Buffer;
}
if (typeof globalThis.MessageChannel === 'undefined') {
  // React's scheduler posts work via MessageChannel; postMessage MUST deliver to the other
  // port's onmessage asynchronously (as a macrotask) or async React updates never flush.
  globalThis.MessageChannel = class MessageChannel {
    constructor() {
      const mk = () => ({
        onmessage: null,
        onmessageerror: null,
        _other: null,
        postMessage(data) {
          const o = this._other;
          if (o) globalThis.setTimeout(() => {
            const ev = { data };
            if (typeof o.onmessage === 'function') o.onmessage(ev);
            (o._listeners || []).forEach((f) => f(ev));
          }, 0);
        },
        close() {},
        start() {},
        addEventListener(t, f) { if (t === 'message') (this._listeners || (this._listeners = [])).push(f); },
        removeEventListener(t, f) { if (this._listeners) this._listeners = this._listeners.filter((x) => x !== f); },
      });
      this.port1 = mk(); this.port2 = mk();
      this.port1._other = this.port2; this.port2._other = this.port1;
    }
  };
  globalThis.MessagePort = class MessagePort { postMessage() {} close() {} start() {} addEventListener() {} removeEventListener() {} };
}
if (typeof globalThis.performance === 'undefined') {
  globalThis.performance = { now: () => 0, mark() {}, measure() {}, getEntriesByName: () => [], clearMarks() {}, clearMeasures() {} };
}
if (typeof globalThis.btoa === 'undefined') {
  globalThis.btoa = (s) => { const c = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'; let o = '', i = 0; s = String(s); while (i < s.length) { const a = s.charCodeAt(i++), b = s.charCodeAt(i++), d = s.charCodeAt(i++); o += c[a >> 2] + c[((a & 3) << 4) | (b >> 4)] + (isNaN(b) ? '=' : c[((b & 15) << 2) | (d >> 6)]) + (isNaN(d) ? '=' : c[d & 63]); } return o; };
  globalThis.atob = (s) => { const c = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'; s = String(s).replace(/=+$/, ''); let o = ''; for (let bc = 0, bs = 0, bu, i = 0; (bu = s.charAt(i++)); ) { const ci = c.indexOf(bu); if (ci < 0) continue; bs = bc % 4 ? bs * 64 + ci : ci; if (bc++ % 4) o += String.fromCharCode(255 & (bs >> ((-2 * bc) & 6))); } return o; };
}

// ---- EventTarget/Event (turbo-dom's Node extends EventTarget) ----
if (typeof globalThis.EventTarget === 'undefined') {
  globalThis.EventTarget = class EventTarget {
    constructor() { this.__listeners = Object.create(null); }
    addEventListener(type, fn) { if (!fn) return; (this.__listeners[type] || (this.__listeners[type] = [])).push(fn); }
    removeEventListener(type, fn) { const a = this.__listeners[type]; if (a) this.__listeners[type] = a.filter((x) => x !== fn); }
    dispatchEvent(ev) {
      ev.target = ev.target || this; ev.currentTarget = this;
      (this.__listeners[ev.type] || []).slice().forEach((fn) => { try { (fn.handleEvent || fn).call(this, ev); } catch {} });
      return !ev.defaultPrevented;
    }
  };
}
if (typeof globalThis.Event === 'undefined') {
  globalThis.Event = class Event {
    constructor(type, init) { init = init || {}; this.type = type; this.bubbles = !!init.bubbles; this.cancelable = !!init.cancelable; this.composed = !!init.composed; this.defaultPrevented = false; this.target = null; this.currentTarget = null; this.timeStamp = 0; }
    preventDefault() { this.defaultPrevented = true; }
    stopPropagation() {}
    stopImmediatePropagation() {}
  };
}
// MessageEvent — postMessage-style events (Calendly/Stripe embeds, BroadcastChannel, web workers).
// jsdom provides it; turbo-test didn't, so `new MessageEvent('message', { data })` / a component's
// window 'message' listener threw "MessageEvent is not defined".
if (typeof globalThis.MessageEvent === 'undefined') {
  globalThis.MessageEvent = class MessageEvent extends globalThis.Event {
    constructor(type, init) { super(type, init); init = init || {}; this.data = init.data == null ? null : init.data; this.origin = init.origin || ''; this.lastEventId = init.lastEventId || ''; this.source = init.source || null; this.ports = init.ports || []; }
  };
}
if (typeof globalThis.MutationObserver === 'undefined') {
  globalThis.MutationObserver = class MutationObserver { constructor(cb) { this._cb = cb; } observe() {} disconnect() {} takeRecords() { return []; } };
}
if (typeof globalThis.requestAnimationFrame === 'undefined') {
  globalThis.requestAnimationFrame = (cb) => globalThis.setTimeout(() => cb(0), 0);
  globalThis.cancelAnimationFrame = (id) => globalThis.clearTimeout(id);
}

// ---- node builtin shims (so turbo-dom's loader reaches its native .node) ----
globalThis.__fileURLToPath = (u) => String(u).replace(/^file:\/\//, '');
globalThis.__dirnameOf = (p) => { const s = String(p).replace(/\/+$/, ''); const i = s.lastIndexOf('/'); return i <= 0 ? '/' : s.slice(0, i); };
const __os = {
  platform: () => globalThis.__ttPlatform || 'darwin', arch: () => globalThis.__ttArch || 'arm64', type: () => globalThis.__ttOsType || 'Darwin', release: () => '23.0.0',
  version: () => 'Darwin Kernel', EOL: '\n', tmpdir: () => globalThis.__ttTmpdir || '/tmp', homedir: () => globalThis.__ttHomedir || '/', hostname: () => 'localhost',
  cpus: () => [{ model: 'cpu', speed: 1, times: { user: 0, nice: 0, sys: 0, idle: 0, irq: 0 } }],
  totalmem: () => 16 * 1024 * 1024 * 1024, freemem: () => 8 * 1024 * 1024 * 1024, uptime: () => 0,
  loadavg: () => [0, 0, 0], endianness: () => 'LE', networkInterfaces: () => ({}),
  userInfo: () => ({ username: 'user', uid: -1, gid: -1, shell: null, homedir: '/' }), availableParallelism: () => 4,
  constants: { signals: {}, errno: {} },
};
const __path = {
  sep: '/',
  join: (...a) => a.filter((x) => x != null && x !== '').join('/').replace(/\/+/g, '/'),
  resolve: (...a) => { let p = a.filter(Boolean).join('/').replace(/\/+/g, '/'); return p.startsWith('/') ? p : '/' + p; },
  dirname: (p) => globalThis.__dirnameOf(p),
  basename: (p, ext) => { let b = String(p).split('/').pop(); if (ext && b.endsWith(ext)) b = b.slice(0, -ext.length); return b; },
  extname: (p) => { const m = /\.[^./]+$/.exec(String(p)); return m ? m[0] : ''; },
  isAbsolute: (p) => String(p).startsWith('/'),
  relative: (_f, t) => String(t),
  normalize: (p) => String(p).replace(/\/+/g, '/'),
  parse: (p) => { const s = String(p); const dir = globalThis.__dirnameOf(s); const base = s.split('/').pop() || ''; const m = /\.[^./]+$/.exec(base); const ext = m ? m[0] : ''; return { root: s.startsWith('/') ? '/' : '', dir, base, ext, name: ext ? base.slice(0, -ext.length) : base }; },
  format: (o) => { o = o || {}; if (o.dir) return (o.dir + '/' + (o.base || ((o.name || '') + (o.ext || '')))).replace(/\/+/g, '/'); return o.base || ((o.name || '') + (o.ext || '')); },
  delimiter: ':',
};
// glob-parent and others reach for path.posix / path.win32; expose both (posix is our default).
__path.posix = __path;
__path.win32 = Object.assign({}, __path, { sep: '\\', delimiter: ';' });
const __fs = {
  existsSync: (p) => globalThis.__fs_existsSync(String(p)),
  readFileSync: (p, enc) => globalThis.__fs_readFileSync(String(p), enc ? String(typeof enc === 'string' ? enc : enc.encoding || 'utf8') : ''),
  writeFileSync: (p, data) => globalThis.__fs_writeFileSync(String(p), data == null ? '' : (typeof data === 'string' ? data : String(data))),
  // a real append (O_APPEND on the Rust side), not read + rewrite: parallel workers appending to
  // one file would otherwise overwrite each other's lines.
  appendFileSync: (p, data) => globalThis.__fs_appendFileSync(String(p), data == null ? '' : (typeof data === 'string' ? data : String(data))),
  mkdirSync: (p) => globalThis.__fs_mkdirSync(String(p)),
  rmSync: (p) => globalThis.__fs_rmSync(String(p)),
  rmdirSync: (p) => globalThis.__fs_rmSync(String(p)),
  unlinkSync: (p) => globalThis.__fs_rmSync(String(p)),
  statSync: (p) => { const isDir = globalThis.__fs_existsSync(String(p)) && globalThis.__fs_readdirSync(String(p)).length >= 0 && !globalThis.__fs_existsSync(String(p) + '/.'); return { isFile: () => true, isDirectory: () => false, size: 0, mtimeMs: 0 }; },
  readdirSync: (p) => globalThis.__fs_readdirSync(String(p)),
  symlinkSync: (target, p) => globalThis.__fs_symlinkSync(String(target), String(p)),
  promises: {
    symlink: (target, p) => { try { __fs.symlinkSync(target, p); return Promise.resolve(); } catch (e) { return Promise.reject(e); } },
    readFile: (p, enc) => Promise.resolve(globalThis.__fs_readFileSync(String(p), enc ? 'utf8' : '')),
    writeFile: (p, data) => { globalThis.__fs_writeFileSync(String(p), data == null ? '' : String(data)); return Promise.resolve(); },
    appendFile: (p, data) => { try { __fs.appendFileSync(p, data); return Promise.resolve(); } catch (e) { return Promise.reject(e); } },
    mkdir: (p) => { globalThis.__fs_mkdirSync(String(p)); return Promise.resolve(); },
    rm: (p) => { globalThis.__fs_rmSync(String(p)); return Promise.resolve(); },
    readdir: (p) => Promise.resolve(globalThis.__fs_readdirSync(String(p))),
  },
};
const __module = {
  createRequire: (url) => globalThis.__mkRequire(globalThis.__dirnameOf(globalThis.__fileURLToPath(url))),
  Module: function () {},
  builtinModules: [],
};
let __seedCtr = 1;
function __pseudoRandomBytes(n) {
  const b = new Uint8Array(n);
  for (let i = 0; i < n; i++) { __seedCtr = (__seedCtr * 1103515245 + 12345) & 0x7fffffff; b[i] = (__seedCtr >> 16) & 255; }
  return b;
}
const __crypto = {
  randomUUID: () => {
    const b = __pseudoRandomBytes(16);
    b[6] = (b[6] & 0x0f) | 0x40; b[8] = (b[8] & 0x3f) | 0x80;
    const h = Array.from(b, (x) => x.toString(16).padStart(2, '0'));
    return `${h[0]}${h[1]}${h[2]}${h[3]}-${h[4]}${h[5]}-${h[6]}${h[7]}-${h[8]}${h[9]}-${h[10]}${h[11]}${h[12]}${h[13]}${h[14]}${h[15]}`;
  },
  randomBytes: (n) => globalThis.Buffer ? globalThis.Buffer.from(__pseudoRandomBytes(n)) : __pseudoRandomBytes(n),
  getRandomValues: (arr) => { const b = __pseudoRandomBytes(arr.length); for (let i = 0; i < arr.length; i++) arr[i] = b[i]; return arr; },
  randomInt: (min, max) => { if (max === undefined) { max = min; min = 0; } return min + (__pseudoRandomBytes(4).reduce((a, x) => a * 256 + x, 0) % (max - min)); },
  createHash: () => { let data = ''; const h = { update: (d) => { data += String(d); return h; }, digest: (enc) => { let n = 0; for (let i = 0; i < data.length; i++) { n = (n * 31 + data.charCodeAt(i)) & 0x7fffffff; } const hex = n.toString(16).padStart(8, '0'); return enc === 'hex' || !enc ? hex.repeat(8).slice(0, 64) : hex; } }; return h; },
  createHmac: () => { let data = ''; const h = { update: (d) => { data += String(d); return h; }, digest: () => 'hmac' }; return h; },
  // Node crypto constants — many packages (e.g. @propelauth/node) read these at module load to
  // pick a padding/algorithm. turbo-test runs in bare V8 (no real OpenSSL), so the values just
  // need to EXIST so modules load; tests that exercise actual crypto mock it or are out of scope.
  constants: {
    RSA_PKCS1_PADDING: 1, RSA_NO_PADDING: 3, RSA_PKCS1_OAEP_PADDING: 4, RSA_X931_PADDING: 5,
    RSA_PKCS1_PSS_PADDING: 6, RSA_PSS_SALTLEN_DIGEST: -1, RSA_PSS_SALTLEN_MAX_SIGN: -2,
    RSA_PSS_SALTLEN_AUTO: -2, RSA_SSLV23_PADDING: 2,
    SSL_OP_ALL: 0x80000bff, ENGINE_METHOD_ALL: 0xffff,
    DH_CHECK_P_NOT_SAFE_PRIME: 2, DH_NOT_SUITABLE_GENERATOR: 8,
    POINT_CONVERSION_COMPRESSED: 2, POINT_CONVERSION_UNCOMPRESSED: 4, POINT_CONVERSION_HYBRID: 6,
    defaultCoreCipherList: 'TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256:TLS_AES_128_GCM_SHA256',
  },
  // Cipher/sign stubs: bare V8 has no OpenSSL, so these can't do real AES/RSA. They exist so a
  // module that merely references them at load doesn't crash; actually invoking them throws a
  // clear, catchable error rather than producing wrong ciphertext.
  createCipheriv: () => { throw new Error('turbo-test: crypto.createCipheriv is not available in the V8 runtime (no OpenSSL) — mock it in the test'); },
  createDecipheriv: () => { throw new Error('turbo-test: crypto.createDecipheriv is not available in the V8 runtime (no OpenSSL) — mock it in the test'); },
  createSign: () => { throw new Error('turbo-test: crypto.createSign is not available in the V8 runtime'); },
  createVerify: () => { throw new Error('turbo-test: crypto.createVerify is not available in the V8 runtime'); },
  pbkdf2Sync: () => { throw new Error('turbo-test: crypto.pbkdf2Sync is not available in the V8 runtime'); },
  scryptSync: () => { throw new Error('turbo-test: crypto.scryptSync is not available in the V8 runtime'); },
  timingSafeEqual: (a, b) => { if (a.length !== b.length) return false; let r = 0; for (let i = 0; i < a.length; i++) r |= a[i] ^ b[i]; return r === 0; },
};
if (typeof globalThis.crypto === 'undefined' || !globalThis.crypto.randomUUID) {
  globalThis.crypto = Object.assign(globalThis.crypto || {}, { randomUUID: __crypto.randomUUID, getRandomValues: __crypto.getRandomValues });
}
class __EventEmitter {
  constructor() { this._ev = {}; }
  on(e, f) { (this._ev[e] || (this._ev[e] = [])).push(f); return this; }
  addListener(e, f) { return this.on(e, f); }
  once(e, f) { const g = (...a) => { this.off(e, g); f(...a); }; return this.on(e, g); }
  off(e, f) { if (this._ev[e]) this._ev[e] = this._ev[e].filter((x) => x !== f); return this; }
  removeListener(e, f) { return this.off(e, f); }
  removeAllListeners(e) { if (e) delete this._ev[e]; else this._ev = {}; return this; }
  emit(e, ...a) { (this._ev[e] || []).slice().forEach((f) => f(...a)); return (this._ev[e] || []).length > 0; }
  listeners(e) { return (this._ev[e] || []).slice(); }
  listenerCount(e) { return (this._ev[e] || []).length; }
  setMaxListeners() { return this; }
  prependListener(e, f) { (this._ev[e] || (this._ev[e] = [])).unshift(f); return this; }
}
const __events = Object.assign(__EventEmitter, { EventEmitter: __EventEmitter, default: __EventEmitter, once: (em, e) => new Promise((res) => em.once(e, (...a) => res(a))) });
class __Readable extends __EventEmitter { pipe(d) { return d; } read() { return null; } push() { return true; } destroy() { return this; } setEncoding() { return this; } resume() { return this; } pause() { return this; } }
class __Writable extends __EventEmitter { write() { return true; } end() { return this; } destroy() { return this; } }
class __Duplex extends __Readable { write() { return true; } end() { return this; } }
const __stream = { Readable: __Readable, Writable: __Writable, Duplex: __Duplex, Transform: __Duplex, PassThrough: __Readable, Stream: __EventEmitter, pipeline: (...a) => { const cb = a[a.length - 1]; if (typeof cb === 'function') cb(); }, finished: (_s, cb) => { if (typeof cb === 'function') cb(); } };
// http/https/net/tls: enough surface that connection-agent libs (agent-base, https-proxy-agent —
// pulled in transitively by e.g. @playwright/test) load. agent-base does `class X extends
// http.Agent`, so http.Agent MUST be a real (extendable) class. No real networking happens in
// unit tests (request contexts are mocked); these just need to construct + be subclassed.
class __Agent extends __EventEmitter { constructor(opts) { super(); this.options = opts || {}; this.maxSockets = Infinity; this.sockets = {}; this.requests = {}; } destroy() {} getName() { return 'localhost::'; } addRequest() {} createConnection() { return new __EventEmitter(); } }
class __ClientRequest extends __EventEmitter { setHeader() {} getHeader() {} end() { return this; } write() { return true; } abort() {} destroy() { return this; } setTimeout() { return this; } }
class __IncomingMessage extends __Readable { constructor() { super(); this.headers = {}; this.statusCode = 200; } }
const __mkServer = () => { const s = new __EventEmitter(); s.listen = (..._a) => { const cb = _a[_a.length - 1]; if (typeof cb === 'function') cb(); return s; }; s.close = (cb) => { if (typeof cb === 'function') cb(); return s; }; s.address = () => ({ port: 0, address: '127.0.0.1' }); return s; };
const __http = { Agent: __Agent, globalAgent: new __Agent(), ClientRequest: __ClientRequest, IncomingMessage: __IncomingMessage, ServerResponse: class extends __EventEmitter { setHeader() {} end() { return this; } write() { return true; } writeHead() { return this; } }, Server: class extends __EventEmitter {}, METHODS: ['GET', 'POST', 'PUT', 'DELETE', 'PATCH', 'HEAD', 'OPTIONS'], STATUS_CODES: {}, createServer: __mkServer, request: () => new __ClientRequest(), get: () => new __ClientRequest() };
__http.default = __http;
const __https = Object.assign({}, __http, { Agent: __Agent, globalAgent: new __Agent() }); __https.default = __https;
const __net = { Socket: class extends __EventEmitter { connect() { return this; } write() { return true; } end() { return this; } destroy() { return this; } setTimeout() { return this; } setNoDelay() { return this; } setKeepAlive() { return this; } ref() { return this; } unref() { return this; } }, Server: class extends __EventEmitter { listen(..._a) { const cb = _a[_a.length - 1]; if (typeof cb === 'function') cb(); return this; } close() { return this; } address() { return { port: 0 }; } }, createConnection: () => new __EventEmitter(), connect: () => new __EventEmitter(), createServer: __mkServer, isIP: () => 0, isIPv4: () => false, isIPv6: () => false }; __net.default = __net;
const __tls = { TLSSocket: class extends __EventEmitter { connect() { return this; } }, connect: () => new __EventEmitter(), createSecureContext: () => ({}), createServer: __mkServer, checkServerIdentity: () => undefined, rootCertificates: [] }; __tls.default = __tls;
const __zlib = { createGzip: () => new __Duplex(), createGunzip: () => new __Duplex(), createDeflate: () => new __Duplex(), createInflate: () => new __Duplex(), createBrotliCompress: () => new __Duplex(), createBrotliDecompress: () => new __Duplex(), gzip: (_b, cb) => cb && cb(null, _b), gunzip: (_b, cb) => cb && cb(null, _b), gzipSync: (b) => b, gunzipSync: (b) => b, deflateSync: (b) => b, inflateSync: (b) => b }; __zlib.default = __zlib;
const __tty = { isatty: () => false, ReadStream: __EventEmitter, WriteStream: __EventEmitter }; __tty.default = __tty;
const __dns = { lookup: (_h, _o, cb) => { const c = typeof _o === 'function' ? _o : cb; if (c) c(null, '127.0.0.1', 4); }, resolve: (_h, cb) => cb && cb(null, []), promises: { lookup: () => Promise.resolve({ address: '127.0.0.1', family: 4 }), resolve: () => Promise.resolve([]) } }; __dns.default = __dns;
const __http2 = { connect: () => new __EventEmitter(), createServer: __mkServer, createSecureServer: __mkServer, constants: {}, getDefaultSettings: () => ({}), Http2ServerRequest: __EventEmitter, Http2ServerResponse: __EventEmitter }; __http2.default = __http2;
const __dgram = { createSocket: () => Object.assign(new __EventEmitter(), { bind() { return this; }, send() {}, close() {}, address: () => ({ port: 0 }) }) }; __dgram.default = __dgram;
const __readline = { createInterface: () => Object.assign(new __EventEmitter(), { question: (_q, cb) => cb && cb(''), close() {}, prompt() {}, write() {} }), clearLine: () => {}, cursorTo: () => {}, moveCursor: () => {} }; __readline.default = __readline;
const __v8 = { serialize: (x) => { try { return JSON.stringify(x); } catch (e) { return '{}'; } }, deserialize: () => ({}), getHeapStatistics: () => ({ total_heap_size: 0, used_heap_size: 0 }), setFlagsFromString: () => {} }; __v8.default = __v8;
const __dc = { channel: () => ({ publish: () => {}, subscribe: () => {}, unsubscribe: () => {}, hasSubscribers: false }), hasSubscribers: () => false, subscribe: () => {}, unsubscribe: () => {} }; __dc.default = __dc;
class __AsyncLocalStorage { constructor() { this._store = undefined; } run(store, cb, ...a) { const p = this._store; this._store = store; try { return cb(...a); } finally { this._store = p; } } getStore() { return this._store; } enterWith(s) { this._store = s; } disable() {} exit(cb, ...a) { return cb(...a); } }
const __async_hooks = { AsyncLocalStorage: __AsyncLocalStorage, AsyncResource: class { runInAsyncScope(fn, thisArg, ...a) { return fn.apply(thisArg, a); } emitDestroy() { return this; } bind(fn) { return fn; } }, createHook: () => ({ enable() { return this; }, disable() { return this; } }), executionAsyncId: () => 0, triggerAsyncId: () => 0 }; __async_hooks.default = __async_hooks;
const __util = {
  inherits(ctor, superCtor) { if (ctor && ctor.prototype && superCtor && superCtor.prototype) { ctor.super_ = superCtor; Object.setPrototypeOf(ctor.prototype, superCtor.prototype); } },
  promisify: (f) => (...args) => new Promise((res, rej) => { try { f(...args, (e, v) => e ? rej(e) : res(v)); } catch (e) { rej(e); } }),
  callbackify: (f) => (...args) => { const cb = args.pop(); Promise.resolve(f(...args)).then((v) => cb(null, v), (e) => cb(e)); },
  inspect: (x) => { try { return typeof x === 'string' ? x : JSON.stringify(x); } catch (e) { return String(x); } },
  format: (...a) => a.map((x) => typeof x === 'string' ? x : (() => { try { return JSON.stringify(x); } catch (e) { return String(x); } })()).join(' '),
  formatWithOptions: (_o, ...a) => a.join(' '),
  deprecate: (fn, _msg) => fn,
  debuglog: () => (() => {}),
  isDeepStrictEqual: (a, b) => { try { return JSON.stringify(a) === JSON.stringify(b); } catch (e) { return a === b; } },
  types: { isPromise: (x) => x && typeof x.then === 'function', isDate: (x) => x instanceof Date, isRegExp: (x) => x instanceof RegExp, isNativeError: (x) => x instanceof Error, isArrayBuffer: (x) => x instanceof ArrayBuffer, isTypedArray: (x) => ArrayBuffer.isView(x) },
  TextEncoder: globalThis.TextEncoder,
  TextDecoder: globalThis.TextDecoder,
};
// ---- querystring (Node's legacy API, per its docs; verified against Node's results) ----
const __qsUnescape = (s) => {
  try { return decodeURIComponent(s); } catch (e) {
    // malformed %-sequences: decode the valid %XX bytes, keep the rest, then UTF-8 decode
    const bytes = [];
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if (c === 37 && /^[0-9a-fA-F]{2}$/.test(s.substr(i + 1, 2))) { bytes.push(parseInt(s.substr(i + 1, 2), 16)); i += 2; }
      else if (c < 0x80) bytes.push(c);
      else for (const b of new globalThis.TextEncoder().encode(s[i])) bytes.push(b);
    }
    return __utf8Decode(bytes);
  }
};
const __qsPrim = (v) => {
  if (typeof v === 'string') return v;
  if (typeof v === 'number' && isFinite(v)) return '' + v;
  if (typeof v === 'bigint') return '' + v;
  if (typeof v === 'boolean') return v ? 'true' : 'false';
  return '';
};
const __querystring = {
  unescape: __qsUnescape,
  escape: (s) => encodeURIComponent(typeof s === 'string' ? s : String(s)),
  stringify(obj, sep, eq, options) {
    sep = sep || '&'; eq = eq || '=';
    const enc = (options && typeof options.encodeURIComponent === 'function') ? options.encodeURIComponent : __querystring.escape;
    if (obj === null || typeof obj !== 'object') return '';
    const out = [];
    for (const k of Object.keys(obj)) {
      const v = obj[k];
      const ks = enc(__qsPrim(k)) + eq;
      if (Array.isArray(v)) { for (const x of v) out.push(ks + enc(__qsPrim(x))); }
      else out.push(ks + enc(__qsPrim(v)));
    }
    return out.join(sep);
  },
  parse(qs, sep, eq, options) {
    const obj = Object.create(null);
    if (typeof qs !== 'string' || qs.length === 0) return obj;
    sep = sep || '&'; eq = eq || '=';
    const maxKeys = options && typeof options.maxKeys === 'number' ? options.maxKeys : 1000;
    const dec = (options && typeof options.decodeURIComponent === 'function') ? options.decodeURIComponent : __querystring.unescape;
    let parts = qs.split(sep);
    if (maxKeys > 0 && parts.length > maxKeys) parts = parts.slice(0, maxKeys);
    for (const part of parts) {
      if (part.length === 0) continue;
      const i = part.indexOf(eq);
      const k = dec((i >= 0 ? part.slice(0, i) : part).replace(/\+/g, ' '));
      const v = i >= 0 ? dec(part.slice(i + eq.length).replace(/\+/g, ' ')) : '';
      if (!(k in obj)) obj[k] = v;
      else if (Array.isArray(obj[k])) obj[k].push(v);
      else obj[k] = [obj[k], v];
    }
    return obj;
  },
};
__querystring.decode = __querystring.parse;
__querystring.encode = __querystring.stringify;
// WHATWG UTF-8 decode (one U+FFFD per maximal invalid subpart, like Node's Buffer#toString)
function __utf8Decode(b, start, end) {
  let s = '';
  let i = start || 0;
  end = end === undefined ? b.length : end;
  while (i < end) {
    const c = b[i];
    if (c < 0x80) { s += String.fromCharCode(c); i++; continue; }
    let need = 0, cp = 0, lo = 0x80, hi = 0xbf;
    if (c >= 0xc2 && c <= 0xdf) { need = 1; cp = c & 0x1f; }
    else if (c >= 0xe0 && c <= 0xef) { need = 2; cp = c & 0xf; if (c === 0xe0) lo = 0xa0; if (c === 0xed) hi = 0x9f; }
    else if (c >= 0xf0 && c <= 0xf4) { need = 3; cp = c & 0x7; if (c === 0xf0) lo = 0x90; if (c === 0xf4) hi = 0x8f; }
    else { s += '\ufffd'; i++; continue; }
    let j = i + 1, k = 0;
    for (; k < need && j < end; k++, j++) {
      const x = b[j];
      if (x < lo || x > hi) break;
      lo = 0x80; hi = 0xbf;
      cp = (cp << 6) | (x & 0x3f);
    }
    s += k < need ? '\ufffd' : String.fromCodePoint(cp);
    i = j;
  }
  return s;
}
// Length of the incomplete-but-valid UTF-8 sequence at the end of b (0 if none).
function __utf8Tail(b) {
  for (let back = 1; back <= 3 && back <= b.length; back++) {
    const c = b[b.length - back];
    if ((c & 0xc0) === 0x80) continue; // continuation byte: keep looking for the lead
    const need = c >= 0xc2 && c <= 0xdf ? 1 : c >= 0xe0 && c <= 0xef ? 2 : c >= 0xf0 && c <= 0xf4 ? 3 : 0;
    return need >= back ? back : 0;
  }
  return 0;
}
const __sdEncodings = { utf8: 'utf8', 'utf-8': 'utf8', utf16le: 'utf16le', 'utf-16le': 'utf16le', ucs2: 'utf16le', 'ucs-2': 'utf16le', latin1: 'latin1', binary: 'latin1', ascii: 'ascii', hex: 'hex', base64: 'base64', base64url: 'base64url' };
class __StringDecoder {
  constructor(encoding) {
    const enc = encoding == null ? 'utf8' : __sdEncodings[String(encoding).toLowerCase()];
    if (!enc) { const e = new TypeError('Unknown encoding: ' + encoding); e.code = 'ERR_UNKNOWN_ENCODING'; throw e; }
    this.encoding = enc;
    this._rest = new Uint8Array(0);
  }
  _bytes(buf) {
    const b = typeof buf === 'string' ? new globalThis.TextEncoder().encode(buf) : buf instanceof Uint8Array ? buf : ArrayBuffer.isView(buf) ? new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength) : new Uint8Array(0);
    if (!this._rest.length) return b;
    const all = new Uint8Array(this._rest.length + b.length);
    all.set(this._rest); all.set(b, this._rest.length);
    return all;
  }
  _decode(b, final) {
    switch (this.encoding) {
      case 'utf8': {
        const keep = final ? 0 : __utf8Tail(b);
        this._rest = b.slice(b.length - keep);
        return __utf8Decode(b, 0, b.length - keep);
      }
      case 'utf16le': {
        let n = b.length - (b.length % 2);
        if (!final && n >= 2) { const last = b[n - 2] | (b[n - 1] << 8); if (last >= 0xd800 && last <= 0xdbff) n -= 2; }
        this._rest = final ? new Uint8Array(0) : b.slice(n);
        let s = '';
        for (let i = 0; i < n; i += 2) s += String.fromCharCode(b[i] | (b[i + 1] << 8));
        return s;
      }
      case 'base64':
      case 'base64url': {
        const n = final ? b.length : b.length - (b.length % 3);
        this._rest = b.slice(n);
        let bin = '';
        for (let i = 0; i < n; i++) bin += String.fromCharCode(b[i]);
        const s = n ? globalThis.btoa(bin) : '';
        return this.encoding === 'base64' ? s : s.replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
      }
      default: {
        this._rest = new Uint8Array(0);
        let s = '';
        for (const c of b) s += this.encoding === 'hex' ? c.toString(16).padStart(2, '0') : String.fromCharCode(this.encoding === 'ascii' ? c & 0x7f : c);
        return s;
      }
    }
  }
  write(buf) { return this._decode(this._bytes(buf), false); }
  end(buf) { const s = this._decode(this._bytes(buf === undefined ? new Uint8Array(0) : buf), true); this._rest = new Uint8Array(0); return s; }
}
// ---- assert / assert/strict ----
function __fmtv(v) { try { return typeof v === 'string' ? JSON.stringify(v) : typeof v === 'bigint' ? v + 'n' : (v && typeof v === 'object') ? JSON.stringify(v) : String(v); } catch (e) { return String(v); } }
function __deepEq(a, b, strict, seen) {
  if (a === b) return a !== 0 || !strict || Object.is(a, b);
  if (a === null || typeof a !== 'object' || b === null || typeof b !== 'object') {
    if ((a === null || typeof a !== 'object') && (b === null || typeof b !== 'object')) return strict ? Object.is(a, b) : (a == b || (a !== a && b !== b));
    return false;
  }
  if (strict && Object.getPrototypeOf(a) !== Object.getPrototypeOf(b)) return false;
  const tag = Object.prototype.toString.call(a);
  if (tag !== Object.prototype.toString.call(b)) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  seen = seen || new Map();
  const memo = seen.get(a);
  if (memo && memo.has(b)) return true;
  if (memo) memo.add(b); else seen.set(a, new Set([b]));
  if (a instanceof Date) { if (a.getTime() !== b.getTime()) return false; }
  else if (a instanceof RegExp) { if (a.source !== b.source || a.flags !== b.flags || a.lastIndex !== b.lastIndex) return false; }
  else if (a instanceof Error) { if (a.message !== b.message || a.name !== b.name) return false; }
  else if (ArrayBuffer.isView(a)) {
    if (a.length !== b.length || a.byteLength !== b.byteLength) return false;
    for (let i = 0; i < a.length; i++) if (strict ? !Object.is(a[i], b[i]) : a[i] != b[i]) return false;
  } else if (tag === '[object Number]' || tag === '[object String]' || tag === '[object Boolean]' || tag === '[object BigInt]' || tag === '[object Symbol]') {
    if (!Object.is(a.valueOf(), b.valueOf())) return false;
  }
  if (a instanceof Map) {
    if (a.size !== b.size) return false;
    for (const [k, v] of a) {
      if (b.has(k)) { if (!__deepEq(v, b.get(k), strict, seen)) return false; continue; }
      // an object key (or, loosely, a primitive one) can still match a deep-equal key
      if (strict && (k === null || typeof k !== 'object')) return false;
      let hit = false;
      for (const [k2, v2] of b) if (__deepEq(k, k2, strict, seen) && __deepEq(v, v2, strict, seen)) { hit = true; break; }
      if (!hit) return false;
    }
  } else if (a instanceof Set) {
    if (a.size !== b.size) return false;
    for (const v of a) {
      if (b.has(v)) continue;
      if (strict && (v === null || typeof v !== 'object')) return false;
      let hit = false;
      for (const v2 of b) if (__deepEq(v, v2, strict, seen)) { hit = true; break; }
      if (!hit) return false;
    }
  }
  const own = (o) => { const ks = Object.keys(o); if (strict) for (const s of Object.getOwnPropertySymbols(o)) if (Object.prototype.propertyIsEnumerable.call(o, s)) ks.push(s); return ks; };
  const ka = own(a), kb = own(b);
  if (ka.length !== kb.length) return false;
  for (const k of ka) {
    if (!Object.prototype.propertyIsEnumerable.call(b, k)) return false;
    if (!__deepEq(a[k], b[k], strict, seen)) return false;
  }
  return true;
}
class __AssertionError extends Error {
  constructor(options) {
    options = options || {};
    super(options.message != null ? String(options.message) : 'Failed');
    this.name = 'AssertionError';
    this.code = 'ERR_ASSERTION';
    this.actual = options.actual;
    this.expected = options.expected;
    this.operator = options.operator;
    this.generatedMessage = options.message == null;
  }
}
function __mkAssert(strict) {
  const fail = (actual, expected, message, operator, generated) => {
    if (message instanceof Error) throw message;
    throw new __AssertionError({ actual, expected, operator, message: message != null ? message : generated });
  };
  const okFn = (...args) => {
    if (args.length === 0) throw new __AssertionError({ message: 'No value argument passed to `assert.ok()`', operator: '==' });
    if (!args[0]) fail(args[0], true, args[1], '==', 'The expression evaluated to a falsy value:\n\n  assert.ok(' + __fmtv(args[0]) + ')\n');
  };
  const looseEq = (a, b) => a == b || (a !== a && b !== b);
  // error validation for throws/rejects: a class, a RegExp, a validation function or an object
  const matches = (err, expected, message, operator) => {
    if (expected === undefined) return;
    if (typeof expected === 'function') {
      if (expected.prototype !== undefined && err instanceof expected) return;
      if (Error.isPrototypeOf(expected) || expected === Error) fail(err, expected, message, operator, 'The error is expected to be an instance of "' + expected.name + '". Received "' + (err && err.constructor && err.constructor.name) + '"');
      const res = expected.call({}, err);
      if (res !== true) fail(err, expected, message, operator, 'The validation function is expected to return "true". Received ' + __fmtv(res));
      return;
    }
    if (expected instanceof RegExp) { if (expected.test(String(err))) return; fail(err, expected, message, operator, 'The input did not match the regular expression ' + String(expected) + '. Input:\n\n' + __fmtv(String(err)) + '\n'); }
    if (expected && typeof expected === 'object') {
      for (const k of Object.keys(expected)) {
        const want = expected[k], got = err == null ? undefined : err[k];
        const ok = want instanceof RegExp && typeof got === 'string' ? want.test(got) : __deepEq(got, want, true);
        if (!ok) fail(err, expected, message, operator, 'Expected values to be strictly deep-equal: ' + k + ' ' + __fmtv(got) + ' !== ' + __fmtv(want));
      }
    }
  };
  // doesNotThrow / doesNotReject: an error that does not match `expected` propagates as is
  const unwanted = (e, expected, message, operator, what) => {
    if (expected !== undefined) {
      if (typeof expected !== 'function' && !(expected instanceof RegExp)) { const t = new TypeError('The "expected" argument must be of type function or an instance of RegExp'); t.code = 'ERR_INVALID_ARG_TYPE'; throw t; }
      const hit = expected instanceof RegExp ? expected.test(String(e)) : (expected.prototype !== undefined && e instanceof expected) || (!Error.isPrototypeOf(expected) && expected !== Error && expected.call({}, e) === true);
      if (!hit) throw e;
    }
    fail(e, expected, message, operator, 'Got unwanted ' + what + (message ? ': ' + message : '.') + '\nActual message: "' + (e && e.message) + '"');
  };
  const split = (expected, message) => (typeof expected === 'string' && message === undefined ? [undefined, expected] : [expected, message]);
  const a = function assert(...args) { okFn(...args); };
  Object.assign(a, {
    ok: okFn,
    fail: (message) => { if (message instanceof Error) throw message; throw new __AssertionError({ message: message === undefined ? 'Failed' : message, operator: 'fail' }); },
    equal: (x, y, m) => { if (!(strict ? Object.is(x, y) : looseEq(x, y))) fail(x, y, m, strict ? 'strictEqual' : '==', __fmtv(x) + (strict ? ' !== ' : ' == ') + __fmtv(y)); },
    notEqual: (x, y, m) => { if (strict ? Object.is(x, y) : looseEq(x, y)) fail(x, y, m, strict ? 'notStrictEqual' : '!=', __fmtv(x) + (strict ? ' === ' : ' != ') + __fmtv(y)); },
    strictEqual: (x, y, m) => { if (!Object.is(x, y)) fail(x, y, m, 'strictEqual', 'Expected values to be strictly equal: ' + __fmtv(x) + ' !== ' + __fmtv(y)); },
    notStrictEqual: (x, y, m) => { if (Object.is(x, y)) fail(x, y, m, 'notStrictEqual', 'Expected "actual" to be strictly unequal to: ' + __fmtv(y)); },
    deepEqual: (x, y, m) => { if (!__deepEq(x, y, strict)) fail(x, y, m, strict ? 'deepStrictEqual' : 'deepEqual', 'Expected values to be ' + (strict ? 'strictly ' : 'loosely ') + 'deep-equal: ' + __fmtv(x) + ' vs ' + __fmtv(y)); },
    notDeepEqual: (x, y, m) => { if (__deepEq(x, y, strict)) fail(x, y, m, strict ? 'notDeepStrictEqual' : 'notDeepEqual', 'Expected "actual" not to be ' + (strict ? 'strictly ' : 'loosely ') + 'deep-equal to: ' + __fmtv(y)); },
    deepStrictEqual: (x, y, m) => { if (!__deepEq(x, y, true)) fail(x, y, m, 'deepStrictEqual', 'Expected values to be strictly deep-equal: ' + __fmtv(x) + ' vs ' + __fmtv(y)); },
    notDeepStrictEqual: (x, y, m) => { if (__deepEq(x, y, true)) fail(x, y, m, 'notDeepStrictEqual', 'Expected "actual" not to be strictly deep-equal to: ' + __fmtv(y)); },
    match: (s, re, m) => { if (typeof s !== 'string' || !re.test(s)) fail(s, re, m, 'match', 'The input did not match the regular expression ' + String(re) + '. Input:\n\n' + __fmtv(s) + '\n'); },
    doesNotMatch: (s, re, m) => { if (typeof s !== 'string' || re.test(s)) fail(s, re, m, 'doesNotMatch', 'The input was expected to not match the regular expression ' + String(re) + '. Input:\n\n' + __fmtv(s) + '\n'); },
    ifError: (v) => { if (v !== null && v !== undefined) throw new __AssertionError({ actual: v, expected: null, operator: 'ifError', message: 'ifError got unwanted exception: ' + (v instanceof Error ? v.message : __fmtv(v)) }); },
    throws: (fn, expected, message) => {
      [expected, message] = split(expected, message);
      let threw = false, err;
      try { fn(); } catch (e) { threw = true; err = e; }
      if (!threw) fail(undefined, expected, message, 'throws', 'Missing expected exception' + (message ? ': ' + message : '.'));
      matches(err, expected, message, 'throws');
    },
    doesNotThrow: (fn, expected, message) => {
      [expected, message] = split(expected, message);
      try { fn(); } catch (e) { unwanted(e, expected, message, 'doesNotThrow', 'exception'); }
    },
    rejects: async (p, expected, message) => {
      [expected, message] = split(expected, message);
      let threw = false, err;
      try { await (typeof p === 'function' ? p() : p); } catch (e) { threw = true; err = e; }
      if (!threw) fail(undefined, expected, message, 'rejects', 'Missing expected rejection' + (message ? ': ' + message : '.'));
      matches(err, expected, message, 'rejects');
    },
    doesNotReject: async (p, expected, message) => {
      [expected, message] = split(expected, message);
      try { await (typeof p === 'function' ? p() : p); } catch (e) { unwanted(e, expected, message, 'doesNotReject', 'rejection'); }
    },
    AssertionError: __AssertionError,
  });
  return a;
}
const __assert = __mkAssert(false);
const __assertStrict = __mkAssert(true);
__assert.strict = __assertStrict;
__assertStrict.strict = __assertStrict;
// ---- buffer / fs/promises / process / timers ----
const __buffer = {
  Buffer: globalThis.Buffer,
  atob: (s) => globalThis.atob(s),
  btoa: (s) => globalThis.btoa(s),
  get Blob() { return globalThis.Blob; },
  get File() { return globalThis.File; },
};
// fs.constants: the values that are the same on every platform turbo-test ships for (no O_CREAT
// etc., which differ between Linux and macOS)
const __fsConstants = {
  F_OK: 0, R_OK: 4, W_OK: 2, X_OK: 1, O_RDONLY: 0, O_WRONLY: 1, O_RDWR: 2,
  S_IFMT: 0o170000, S_IFREG: 0o100000, S_IFDIR: 0o040000, S_IFLNK: 0o120000,
  COPYFILE_EXCL: 1, COPYFILE_FICLONE: 2, COPYFILE_FICLONE_FORCE: 4,
};
__fs.constants = __fsConstants;
__fs.promises.constants = __fsConstants;
// os.constants: libuv's priorities everywhere; POSIX signal / errno numbers that Linux and macOS
// share (left empty on Windows, whose numbering differs)
__os.constants = {
  priority: { PRIORITY_LOW: 19, PRIORITY_BELOW_NORMAL: 10, PRIORITY_NORMAL: 0, PRIORITY_ABOVE_NORMAL: -7, PRIORITY_HIGH: -14, PRIORITY_HIGHEST: -20 },
  signals: {},
  errno: {},
};
if ((globalThis.__ttPlatform || 'darwin') !== 'win32') {
  __os.constants.signals = { SIGHUP: 1, SIGINT: 2, SIGQUIT: 3, SIGILL: 4, SIGTRAP: 5, SIGABRT: 6, SIGIOT: 6, SIGFPE: 8, SIGKILL: 9, SIGSEGV: 11, SIGPIPE: 13, SIGALRM: 14, SIGTERM: 15 };
  const names = ['EPERM', 'ENOENT', 'ESRCH', 'EINTR', 'EIO', 'ENXIO', 'E2BIG', 'ENOEXEC', 'EBADF', 'ECHILD', null, 'ENOMEM', 'EACCES', 'EFAULT', 'ENOTBLK', 'EBUSY', 'EEXIST', 'EXDEV', 'ENODEV', 'ENOTDIR', 'EISDIR', 'EINVAL', 'ENFILE', 'EMFILE', 'ENOTTY', 'ETXTBSY', 'EFBIG', 'ENOSPC', 'ESPIPE', 'EROFS', 'EMLINK', 'EPIPE', 'EDOM', 'ERANGE'];
  names.forEach((n, i) => { if (n) __os.constants.errno[n] = i + 1; });
}
const __constants = Object.assign({}, __os.constants.errno, __os.constants.priority, __os.constants.signals, __fsConstants, __crypto.constants);
// the global timers (vi.useFakeTimers drives these same functions)
const __abortError = (signal) => { const e = new Error('The operation was aborted'); e.name = 'AbortError'; e.code = 'ABORT_ERR'; if (signal && signal.reason !== undefined) e.cause = signal.reason; return e; };
const __timerPromise = (schedule, clear, value, options) => new Promise((resolve, reject) => {
  const signal = options && options.signal;
  if (signal && signal.aborted) { reject(__abortError(signal)); return; }
  const onAbort = () => { clear(id); reject(__abortError(signal)); };
  const id = schedule(() => { if (signal) signal.removeEventListener('abort', onAbort); resolve(value); });
  if (signal) signal.addEventListener('abort', onAbort);
});
const __timersPromises = {
  setTimeout: (delay, value, options) => __timerPromise((cb) => globalThis.setTimeout(cb, delay), (id) => globalThis.clearTimeout(id), value, options),
  setImmediate: (value, options) => __timerPromise((cb) => globalThis.setImmediate(cb), (id) => globalThis.clearImmediate(id), value, options),
  // async iterator yielding `value` once per tick (ticks the consumer hasn't taken yet queue up)
  setInterval: (delay, value, options) => {
    const signal = options && options.signal;
    let ticks = 0, wake = null, done = false, error = null;
    const notify = () => { if (wake) { const w = wake; wake = null; w(); } };
    const id = globalThis.setInterval(() => { ticks++; notify(); }, delay);
    const finish = () => { done = true; globalThis.clearInterval(id); if (signal) signal.removeEventListener('abort', onAbort); };
    const onAbort = () => { error = __abortError(signal); finish(); notify(); };
    if (signal) { if (signal.aborted) onAbort(); else signal.addEventListener('abort', onAbort); }
    return {
      [Symbol.asyncIterator]() { return this; },
      async next() {
        while (!ticks && !done) await new Promise((r) => { wake = r; });
        if (error) { const e = error; error = null; throw e; }
        if (done) return { done: true, value: undefined };
        ticks--;
        return { done: false, value };
      },
      async return() { finish(); return { done: true, value: undefined }; },
    };
  },
  scheduler: {
    wait: (delay, options) => __timersPromises.setTimeout(delay, undefined, options),
    yield: () => __timersPromises.setImmediate(),
  },
};
const __timers = {
  get setTimeout() { return globalThis.setTimeout; },
  get clearTimeout() { return globalThis.clearTimeout; },
  get setInterval() { return globalThis.setInterval; },
  get clearInterval() { return globalThis.clearInterval; },
  get setImmediate() { return globalThis.setImmediate; },
  get clearImmediate() { return globalThis.clearImmediate; },
  promises: __timersPromises,
};
// ---- vm / worker_threads / inspector: what one V8 context in one thread can truthfully offer;
// the rest throws a clear error instead of pretending ----
const __unavailable = (what, why) => () => { throw new Error('turbo-test: ' + what + ' is not available (' + why + ')'); };
const __oneContext = 'tests run in a single V8 context';
class __VmScript {
  constructor(code) { this._code = String(code); }
  runInThisContext() { return (0, eval)(this._code); }
}
__VmScript.prototype.runInContext = __unavailable('vm.Script.runInContext', __oneContext);
__VmScript.prototype.runInNewContext = __unavailable('vm.Script.runInNewContext', __oneContext);
const __vm = {
  Script: __VmScript,
  runInThisContext: (code) => (0, eval)(String(code)),
  compileFunction: (code, params) => new Function(...(params || []), String(code)),
  isContext: (o) => {
    if (o === null || typeof o !== 'object') { const e = new TypeError('The "object" argument must be of type object'); e.code = 'ERR_INVALID_ARG_TYPE'; throw e; }
    return false; // no other contexts can exist here
  },
  createContext: __unavailable('vm.createContext', __oneContext),
  runInContext: __unavailable('vm.runInContext', __oneContext),
  runInNewContext: __unavailable('vm.runInNewContext', __oneContext),
};
const __envData = new Map();
const __untransferable = new WeakSet();
const __worker_threads = {
  isMainThread: true,
  parentPort: null,
  threadId: 0,
  workerData: null,
  resourceLimits: {},
  SHARE_ENV: Symbol.for('nodejs.worker_threads.SHARE_ENV'),
  Worker: class Worker { constructor() { __unavailable('worker_threads.Worker', 'tests run on one thread')(); } },
  get MessageChannel() { return globalThis.MessageChannel; },
  get MessagePort() { return globalThis.MessagePort; },
  getEnvironmentData: (k) => __envData.get(k),
  setEnvironmentData: (k, v) => { if (v === undefined) __envData.delete(k); else __envData.set(k, v); },
  markAsUntransferable: (o) => { if (o !== null && (typeof o === 'object' || typeof o === 'function')) __untransferable.add(o); },
  isMarkedAsUntransferable: (o) => o !== null && (typeof o === 'object' || typeof o === 'function') && __untransferable.has(o),
};
const __inspectorError = (code, message) => { const e = new Error(message); e.code = code; return e; };
const __inspector = {
  url: () => undefined, // no inspector is active
  open: __unavailable('inspector.open', 'the test isolate exposes no inspector'),
  close: () => {},
  waitForDebugger: () => { throw __inspectorError('ERR_INSPECTOR_NOT_ACTIVE', 'Inspector is not active'); },
  get console() { return globalThis.console; },
  Session: class Session extends __EventEmitter {
    connect() { __unavailable('inspector.Session.connect', 'the test isolate exposes no inspector')(); }
    connectToMainThread() { __unavailable('inspector.Session.connectToMainThread', 'the test isolate exposes no inspector')(); }
    post() { throw __inspectorError('ERR_INSPECTOR_NOT_CONNECTED', 'Session is not connected'); }
    disconnect() {}
  },
};
globalThis.__nodeBuiltins = {
  fs: __fs,
  'node:fs': __fs,
  path: __path,
  'node:path': __path,
  module: __module,
  'node:module': __module,
  os: __os,
  'node:os': __os,
  child_process: { execSync: () => '', exec: () => {}, spawnSync: () => ({ status: 0 }) },
  'node:child_process': { execSync: () => '' },
  url: { fileURLToPath: globalThis.__fileURLToPath, pathToFileURL: (p) => ({ href: 'file://' + p, toString: () => 'file://' + p }), URL: globalThis.URL },
  'node:url': { fileURLToPath: globalThis.__fileURLToPath, pathToFileURL: (p) => 'file://' + p },
  util: __util,
  'node:util': __util,
  crypto: __crypto,
  'node:crypto': __crypto,
  events: __events,
  'node:events': __events,
  stream: __stream,
  'node:stream': __stream,
  http: __http,
  'node:http': __http,
  https: __https,
  'node:https': __https,
  net: __net,
  'node:net': __net,
  tls: __tls,
  'node:tls': __tls,
  zlib: __zlib,
  'node:zlib': __zlib,
  tty: __tty,
  'node:tty': __tty,
  dns: __dns,
  'node:dns': __dns,
  'dns/promises': __dns.promises,
  'node:dns/promises': __dns.promises,
  http2: __http2,
  'node:http2': __http2,
  dgram: __dgram,
  'node:dgram': __dgram,
  readline: __readline,
  'node:readline': __readline,
  v8: __v8,
  'node:v8': __v8,
  diagnostics_channel: __dc,
  'node:diagnostics_channel': __dc,
  async_hooks: __async_hooks,
  'node:async_hooks': __async_hooks,
  assert: __assert,
  'assert/strict': __assertStrict,
  perf_hooks: { performance: globalThis.performance || { now: () => 0 } },
  buffer: __buffer,
  'fs/promises': __fs.promises,
  get process() { return globalThis.process; },
  timers: __timers,
  'timers/promises': __timersPromises,
  vm: __vm,
  worker_threads: __worker_threads,
  querystring: __querystring,
  string_decoder: { StringDecoder: __StringDecoder },
  constants: __constants,
  inspector: __inspector,
};

// ---- loader helpers (used by the CJS wrapper) ----
globalThis.__keys = (o) =>
  o && (typeof o === 'object' || typeof o === 'function') ? Object.keys(o) : [];
// Node's MODULE_NOT_FOUND error for `spec`: the message lists the require stack (the requiring
// module, the modules loading it, the test file)
globalThis.__ttModuleNotFound = (spec, importer) => {
  const stack = [];
  const add = (p) => { if (p && !stack.includes(p)) stack.push(p); };
  add(importer);
  for (const p of globalThis.__ttRequireStack()) add(p);
  add(globalThis.__ttFile);
  const e = new Error(`Cannot find module '${spec}'` + (stack.length ? `\nRequire stack:\n- ${stack.join('\n- ')}` : ''));
  e.code = 'MODULE_NOT_FOUND';
  e.requireStack = stack;
  return e;
};
// require.cache, shared by every require (Node's Module._cache): a module loaded from a file,
// by its path; deleting one makes the next require() load it again
const __requireCacheEntries = new Map();
function __requireCacheEntry(path) {
  if (globalThis.__ttCache('get', path) === undefined) { __requireCacheEntries.delete(path); return undefined; }
  let e = __requireCacheEntries.get(path);
  if (!e) {
    e = { id: path, filename: path, loaded: true, children: [], paths: [] };
    Object.defineProperty(e, 'exports', {
      get: () => globalThis.__ttCache('get', path),
      set: (v) => globalThis.__ttCache('set', path, v),
      enumerable: true,
      configurable: true,
    });
    __requireCacheEntries.set(path, e);
  }
  return e;
}
const __requireCache = new Proxy({}, {
  get: (_t, k) => (typeof k === 'string' ? __requireCacheEntry(k) : undefined),
  has: (_t, k) => typeof k === 'string' && __requireCacheEntry(k) !== undefined,
  set: (_t, k, v) => { if (typeof k === 'string') globalThis.__ttCache('set', k, v && v.exports); return true; },
  deleteProperty: (_t, k) => {
    if (typeof k === 'string') { globalThis.__ttCache('delete', k); __requireCacheEntries.delete(k); }
    return true;
  },
  ownKeys: () => globalThis.__ttCache('keys'),
  getOwnPropertyDescriptor: (_t, k) => {
    const e = typeof k === 'string' ? __requireCacheEntry(k) : undefined;
    return e && { value: e, writable: true, enumerable: true, configurable: true };
  },
});
globalThis.__mkRequire = (dir, isEsm, importer) => {
  const req = (spec) => globalThis.__nativeRequire(dir, spec, isEsm, importer);
  // as Node: the resolved path (a builtin stays its name); MODULE_NOT_FOUND when nothing resolves
  req.resolve = (spec) => {
    const r = globalThis.__ttRequireResolve(dir, String(spec), isEsm === undefined ? true : !!isEsm);
    if (r === undefined) throw globalThis.__ttModuleNotFound(String(spec), importer);
    return r;
  };
  req.cache = __requireCache;
  req.extensions = {};
  req.main = undefined;
  return req;
};

// ---- asymmetric matchers (expect.objectContaining et al) ----
class Asymmetric {
  constructor(kind, sample, negate) { this.__asymmetric = kind; this.sample = sample; this.__negate = !!negate; }
  matches(actual) { return this.__negate ? !this.__matchesInner(actual) : this.__matchesInner(actual); }
  __matchesInner(actual) {
    switch (this.__asymmetric) {
      case 'any':
        if (this.sample === String) return typeof actual === 'string';
        if (this.sample === Number) return typeof actual === 'number';
        if (this.sample === Boolean) return typeof actual === 'boolean';
        if (this.sample === Function) return typeof actual === 'function';
        if (this.sample === Object) return typeof actual === 'object' && actual !== null;
        if (this.sample === Array) return Array.isArray(actual);
        return actual instanceof this.sample;
      case 'anything': return actual !== null && actual !== undefined;
      case 'objectContaining':
        return actual && typeof actual === 'object' &&
          Object.keys(this.sample).every((k) => deepEqual(actual[k], this.sample[k]));
      case 'arrayContaining':
        return Array.isArray(actual) && this.sample.every((s) => actual.some((a) => deepEqual(a, s)));
      case 'stringContaining': return typeof actual === 'string' && actual.includes(this.sample);
      case 'stringMatching': {
        const r = this.sample instanceof RegExp ? this.sample : new RegExp(this.sample);
        return typeof actual === 'string' && r.test(actual);
      }
      default: return false;
    }
  }
}

// ---- deep equality (asymmetric-aware) ----
// `strict` mirrors jest/vitest: toEqual/toHaveBeenCalledWith (strict=false) treat an own
// property whose value is `undefined` as ABSENT ({a:1,b:undefined} equals {a:1}); toStrictEqual
// (strict=true) keeps undefined-valued keys significant. Array indices are NEVER stripped in
// either mode — a hole/undefined element changes length, so [1,undefined] != [1].
function deepEqual(a, b, strict = false) {
  if (b instanceof Asymmetric) return b.matches(a);
  if (a instanceof Asymmetric) return a.matches(b);
  if (Object.is(a, b)) return true;
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a)) {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) if (!deepEqual(a[i], b[i], strict)) return false;
    return true;
  }
  if (strict) {
    const ka = Object.keys(a), kb = Object.keys(b);
    if (ka.length !== kb.length) return false;
    return ka.every((k) => (k in b) && deepEqual(a[k], b[k], true));
  }
  // non-strict: drop own keys whose value is `undefined` on both sides before comparing.
  const ka = Object.keys(a).filter((k) => a[k] !== undefined);
  const kb = Object.keys(b).filter((k) => b[k] !== undefined);
  if (ka.length !== kb.length) return false;
  return ka.every((k) => deepEqual(a[k], b[k], false));
}

function fmt(v) {
  try { return typeof v === 'string' ? JSON.stringify(v) : (Array.isArray(v) || (v && typeof v === 'object')) ? JSON.stringify(v) : String(v); }
  catch { return String(v); }
}

// ---- snapshots ----
// Per-test state set by runSuite before each test: the full `describe > it` name and a
// per-(test-name) counter so multiple toMatchSnapshot() calls in one test get distinct keys
// (`<full name> 1`, `<full name> 2`, …), matching vitest's keying.
const __snap = {
  testName: '',
  counters: Object.create(null), // testName -> next index
  // assertion enforcement (expect.assertions / hasAssertions); reset per test.
  assertCount: 0,
  expectedAssertions: null, // number | null
  hasAssertions: false,
};
function __snapNextKey(name) {
  const n = (__snap.counters[name] || 0) + 1;
  __snap.counters[name] = n;
  return `${name} ${n}`;
}
function __snapFile() {
  const f = globalThis.__ttFile;
  if (!f) return null;
  const slash = f.lastIndexOf('/');
  const dir = slash >= 0 ? f.slice(0, slash) : '.';
  const base = slash >= 0 ? f.slice(slash + 1) : f;
  return { dir: `${dir}/__snapshots__`, path: `${dir}/__snapshots__/${base}.snap` };
}
// pretty-format-ish serializer (jest-snapshot style): stable, indented, ordered keys.
function __serialize(v, indent = '') {
  const ni = indent + '  ';
  if (v === null) return 'null';
  if (v === undefined) return 'undefined';
  const t = typeof v;
  if (t === 'string') return `"${v.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n')}"`;
  if (t === 'number' || t === 'boolean' || t === 'bigint') return String(v);
  if (t === 'function') return `[Function ${v.name || 'anonymous'}]`;
  if (t === 'symbol') return v.toString();
  if (v instanceof RegExp) return String(v);
  if (v instanceof Date) return v.toISOString();
  if (v instanceof Error) return `[Error: ${v.message}]`;
  if (Array.isArray(v)) {
    if (v.length === 0) return '[]';
    const items = v.map((x) => ni + __serialize(x, ni));
    return `[\n${items.join(',\n')},\n${indent}]`;
  }
  if (v instanceof Map) {
    const items = [...v.entries()].map(([k, val]) => `${ni}${__serialize(k, ni)} => ${__serialize(val, ni)}`);
    return `Map {\n${items.join(',\n')},\n${indent}}`;
  }
  if (v instanceof Set) {
    const items = [...v.values()].map((x) => ni + __serialize(x, ni));
    return `Set {\n${items.join(',\n')},\n${indent}}`;
  }
  if (t === 'object') {
    const keys = Object.keys(v).sort();
    if (keys.length === 0) return '{}';
    const ctor = v.constructor && v.constructor.name;
    const prefix = ctor && ctor !== 'Object' ? `${ctor} ` : '';
    const items = keys.map((k) => `${ni}"${k}": ${__serialize(v[k], ni)}`);
    return `${prefix}{\n${items.join(',\n')},\n${indent}}`;
  }
  return String(v);
}
// Parse a .snap file into a map of key -> serialized body. Format mirrors jest/vitest:
//   exports[`<key>`] = `\n<body>\n`;
function __parseSnapFile(text) {
  const out = Object.create(null);
  const re = /exports\[`((?:[^`\\]|\\.)*)`\]\s*=\s*`((?:[^`\\]|\\.)*)`;/g;
  let m;
  while ((m = re.exec(text))) {
    const key = m[1].replace(/\\`/g, '`').replace(/\\\\/g, '\\');
    const body = m[2].replace(/\\`/g, '`').replace(/\\\\/g, '\\');
    out[key] = body;
  }
  return out;
}
function __serializeSnapFile(map) {
  const header = "// turbo-test snapshot v1\n\n";
  const keys = Object.keys(map).sort();
  const esc = (s) => s.replace(/\\/g, '\\\\').replace(/`/g, '\\`');
  return header + keys.map((k) => `exports[\`${esc(k)}\`] = \`${esc(map[k])}\`;\n`).join('\n');
}
// snapshot body is stored as `\n<serialized>\n` (jest convention); compare on the trimmed form.
function __snapBody(serialized) { return `\n${serialized}\n`; }

// ---- expect ----
function makeExpect(actual, negated = false) {
  // `msg` is a thunk: the failure message (and the formatting of the operands in it) is built
  // only when the assertion fails, as vitest does — formatting can throw (a native class
  // prototype whose members check their receiver) and must not fail a passing assertion.
  const ok = (pass, msg) => {
    if (negated ? pass : !pass) throw new Error(msg());
  };
  const api = {
    get not() { return makeExpect(actual, !negated); },
    toBeTypeOf(t) { ok(typeof actual === t, () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to be typeof ${t}, got ${typeof actual}`); },
    toSatisfy(fn) { ok(!!fn(actual), () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to satisfy predicate`); },
    toHaveBeenCalledExactlyOnceWith(...a) {
      const calls = actual.mock && actual.mock.calls;
      ok(calls && calls.length === 1 && deepEqual(calls[0], a), () => `expected spy called exactly once with ${fmt(a)}`);
    },
    // async matchers: `await expect(promise).resolves.toX(...)`, `.rejects.toX(...)`, and
    // `.resolves.not.toX(...)` / `.rejects.not.toX(...)` (the `.not` returns a negated proxy).
    get resolves() {
      const mk = (neg) => new Proxy({}, { get: (_, m) => {
        if (m === 'not') return mk(!neg);
        return (...args) => Promise.resolve(actual).then(
          (v) => makeExpect(v, neg)[m](...args),
          (e) => { throw new Error('expected promise to resolve, but it rejected with ' + fmt(e && e.message || e)); },
        );
      } });
      return mk(negated);
    },
    get rejects() {
      const mk = (neg) => new Proxy({}, { get: (_, m) => {
        if (m === 'not') return mk(!neg);
        return (...args) => Promise.resolve(actual).then(
          () => { throw new Error('expected promise to reject, but it resolved'); },
          (e) => makeExpect(e, neg)[m](...args),
        );
      } });
      return mk(negated);
    },
    toBe(e) { ok(Object.is(actual, e), () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to be ${fmt(e)}`); },
    toEqual(e) { ok(deepEqual(actual, e), () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to equal ${fmt(e)}`); },
    toStrictEqual(e) { ok(deepEqual(actual, e, true), () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to strictly equal ${fmt(e)}`); },
    toBeTruthy() { ok(!!actual, () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to be truthy`); },
    toBeFalsy() { ok(!actual, () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to be falsy`); },
    toBeNull() { ok(actual === null, () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to be null`); },
    toBeUndefined() { ok(actual === undefined, () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to be undefined`); },
    toBeDefined() { ok(actual !== undefined, () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to be defined`); },
    toBeNaN() { ok(Number.isNaN(actual), () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to be NaN`); },
    toBeGreaterThan(n) { ok(actual > n, () => `expected ${fmt(actual)} > ${fmt(n)}`); },
    toBeGreaterThanOrEqual(n) { ok(actual >= n, () => `expected ${fmt(actual)} >= ${fmt(n)}`); },
    toBeLessThan(n) { ok(actual < n, () => `expected ${fmt(actual)} < ${fmt(n)}`); },
    toBeLessThanOrEqual(n) { ok(actual <= n, () => `expected ${fmt(actual)} <= ${fmt(n)}`); },
    toBeCloseTo(n, d = 2) { ok(Math.abs(actual - n) < Math.pow(10, -d) / 2, () => `expected ${fmt(actual)} close to ${fmt(n)}`); },
    toContain(x) { ok(actual != null && actual.includes && actual.includes(x), () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to contain ${fmt(x)}`); },
    toContainEqual(x) { ok(Array.isArray(actual) && actual.some((a) => deepEqual(a, x)), () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to contain equal ${fmt(x)}`); },
    toBeInstanceOf(ctor) { ok(actual instanceof ctor, () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to be instance of ${ctor && ctor.name}`); },
    toMatchObject(obj) {
      const sub = (a, e) => (e instanceof Asymmetric) ? e.matches(a)
        : e == null || typeof e !== 'object' ? deepEqual(a, e)
        : a != null && typeof a === 'object' && Object.keys(e).every((k) => sub(a[k], e[k]));
      ok(sub(actual, obj), () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to match object ${fmt(obj)}`);
    },
    toThrowError(e) { return api.toThrow(e); },
    // File snapshot: serialize `actual`, key by `<full test name> <counter>` under
    // `__snapshots__/<testfile>.snap`. Missing key or update-mode (-u) → write + pass; else compare.
    toMatchSnapshot(_hint) {
      const sf = __snapFile();
      if (!sf) throw new Error('toMatchSnapshot: no test file path available');
      const key = __snapNextKey(__snap.testName);
      const body = __snapBody(__serialize(actual));
      const exists = globalThis.__fs_existsSync(sf.path);
      const map = exists ? __parseSnapFile(globalThis.__fs_readFileSync(sf.path, 'utf8')) : Object.create(null);
      const update = !!globalThis.__TT_UPDATE_SNAPSHOTS;
      if (!(key in map) || update) {
        map[key] = body;
        globalThis.__fs_mkdirSync(sf.dir);
        globalThis.__fs_writeFileSync(sf.path, __serializeSnapFile(map));
        return;
      }
      ok(map[key] === body, () => `snapshot mismatch for "${key}"\n- Snapshot\n+ Received\n- ${map[key].trim()}\n+ ${body.trim()}`);
    },
    // Inline snapshot: compares against the provided string arg (whitespace-normalized, matching
    // vitest's trimming). AUTO-WRITE back into the source on first run / -u is UNSUPPORTED — pass
    // the expected string explicitly. With no arg and no update, the first run writes nothing and
    // passes (parity gap documented in vitest.compat.md).
    toMatchInlineSnapshot(expected) {
      const body = __serialize(actual);
      if (expected === undefined) return; // no auto-write; treat as accepted (documented gap)
      const norm = (s) => String(s).replace(/^\n/, '').replace(/\n\s*$/, '').split('\n').map((l) => l.trim()).join('\n');
      ok(norm(body) === norm(expected), () => `inline snapshot mismatch\n- ${norm(expected)}\n+ ${norm(body)}`);
    },
    toThrowErrorMatchingSnapshot(_hint) {
      let msg;
      if (typeof actual === 'function') { try { actual(); msg = undefined; } catch (e) { msg = (e && e.message) || String(e); } }
      else if (actual instanceof Error) msg = actual.message;
      makeExpect(msg).toMatchSnapshot();
    },
    toThrowErrorMatchingInlineSnapshot(expected) {
      let msg;
      if (typeof actual === 'function') { try { actual(); msg = undefined; } catch (e) { msg = (e && e.message) || String(e); } }
      else if (actual instanceof Error) msg = actual.message;
      makeExpect(msg).toMatchInlineSnapshot(expected);
    },
    toStrictEqual_alias() {},
    toHaveLength(n) { ok(actual != null && actual.length === n, () => `expected length ${actual && actual.length} ${negated ? 'not ' : ''}to be ${n}`); },
    toHaveProperty(path, value) {
      let cur = actual, present;
      // A literal key (incl. one containing dots, e.g. a CSS selector) takes precedence over
      // dot-path traversal — matches jest/vitest behavior.
      if (!Array.isArray(path) && actual != null && (path in Object(actual))) {
        cur = actual[path]; present = true;
      } else {
        const keys = Array.isArray(path) ? path : String(path).split('.');
        present = true;
        for (const k of keys) {
          if (cur != null && (k in Object(cur))) cur = cur[k]; else { present = false; break; }
        }
      }
      const pass = arguments.length < 2 ? present : (present && deepEqual(cur, value));
      ok(pass, () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to have property ${fmt(path)}${arguments.length < 2 ? '' : ' = ' + fmt(value)}`);
    },
    toMatch(re) { const r = re instanceof RegExp ? re : new RegExp(re); ok(r.test(actual), () => `expected ${fmt(actual)} ${negated ? 'not ' : ''}to match ${r}`); },
    toThrow(expected) {
      let threw = false, err;
      // expect(fn).toThrow() calls the fn; expect(promise).rejects.toThrow() passes the already-
      // rejected error as `actual` (not callable) — treat it as the thrown value directly.
      if (typeof actual === 'function') { try { actual(); } catch (e) { threw = true; err = e; } }
      else if (actual instanceof Error) { threw = true; err = actual; }
      if (!expected) { ok(threw, () => `expected function ${negated ? 'not ' : ''}to throw`); return; }
      const msg = err && (err.message || String(err)) || '';
      let matches;
      if (expected instanceof RegExp) matches = expected.test(msg);
      else if (typeof expected === 'function') matches = err instanceof expected; // toThrow(ErrorClass)
      else if (expected && typeof expected === 'object') matches = err && (expected.message == null || String(err.message || '').includes(expected.message)); // toThrow({message})
      else matches = msg.includes(expected);
      ok(threw && matches, () => `expected to throw matching ${fmt(expected)}, got ${fmt(msg)}`);
    },
    toHaveBeenCalled() { ok(actual.mock && actual.mock.calls.length > 0, () => `expected spy ${negated ? 'not ' : ''}to have been called`); },
    toHaveBeenCalledTimes(n) { ok(actual.mock && actual.mock.calls.length === n, () => `expected spy called ${n}x, got ${actual.mock ? actual.mock.calls.length : 'n/a'}`); },
    toHaveBeenCalledWith(...a) {
      const found = actual.mock && actual.mock.calls.some((c) => deepEqual(c, a));
      const got = () => actual.mock ? `; received ${actual.mock.calls.length} call(s): ${fmt(actual.mock.calls)}` : '; not a spy';
      ok(found, () => `expected spy ${negated ? 'not ' : ''}to have been called with ${fmt(a)}${negated ? '' : got()}`);
    },
    toHaveBeenCalledOnce() { ok(actual.mock && actual.mock.calls.length === 1, () => `expected spy called once, got ${actual.mock ? actual.mock.calls.length : 'n/a'}`); },
    toHaveBeenLastCalledWith(...a) {
      const calls = actual.mock && actual.mock.calls;
      const last = calls && calls.length ? calls[calls.length - 1] : undefined;
      ok(last && deepEqual(last, a), () => `expected spy last called with ${fmt(a)}, got ${fmt(last)}`);
    },
    toHaveBeenNthCalledWith(n, ...a) {
      const calls = actual.mock && actual.mock.calls;
      const c = calls && calls[n - 1];
      ok(c && deepEqual(c, a), () => `expected spy call #${n} with ${fmt(a)}, got ${fmt(c)}`);
    },
    toHaveReturned() { ok(actual.mock && actual.mock.results.some((r) => r.type === 'return'), () => `expected spy to have returned`); },
    toHaveReturnedWith(v) { ok(actual.mock && actual.mock.results.some((r) => r.type === 'return' && deepEqual(r.value, v)), () => `expected spy to have returned ${fmt(v)}`); },
  };
  // custom matchers registered via expect.extend (e.g. @testing-library/jest-dom)
  for (const name of Object.keys(__customMatchers)) {
    api[name] = (...args) => {
      const ctx = {
        isNot: negated,
        promise: '',
        equals: (a, b) => deepEqual(a, b),
        utils: __matcherUtils,
      };
      let res;
      try { res = __customMatchers[name].call(ctx, actual, ...args); }
      catch (e) { throw e; }
      const pass = res && res.pass;
      if (negated ? pass : !pass) {
        let m = res && res.message;
        m = typeof m === 'function' ? m() : m;
        throw new Error(m || `${name} matcher failed`);
      }
      return api;
    };
  }
  return api;
}
const __matcherUtils = {
  printReceived: (x) => fmt(x),
  printExpected: (x) => fmt(x),
  matcherHint: (name) => String(name || ''),
  stringify: (x) => fmt(x),
  diff: (a, b) => `- Expected: ${fmt(a)}\n+ Received: ${fmt(b)}`,
  RECEIVED_COLOR: (s) => s,
  EXPECTED_COLOR: (s) => s,
  BOLD_WEIGHT: (s) => s,
  DIM_COLOR: (s) => s,
  EXPECTED_LABEL: 'Expected',
  RECEIVED_LABEL: 'Received',
  pluralize: (w, n) => `${n} ${w}${n === 1 ? '' : 's'}`,
  highlightTrailingWhitespace: (s) => s,
};
const __customMatchers = {};
globalThis.expect = (actual) => { __snap.assertCount++; return makeExpect(actual); };
globalThis.expect.extend = (matchers) => { Object.assign(__customMatchers, matchers || {}); };
// expect.assertions(n) / expect.hasAssertions(): record the expectation; runSuite verifies the
// count at test end (see __snap.assertCount, reset per test). These calls do NOT count themselves.
globalThis.expect.assertions = (n) => { __snap.expectedAssertions = n; };
globalThis.expect.hasAssertions = () => { __snap.hasAssertions = true; };
globalThis.expect.soft = (actual) => { __snap.assertCount++; return makeExpect(actual); };
globalThis.expect.any = (ctor) => new Asymmetric('any', ctor);
globalThis.expect.anything = () => new Asymmetric('anything');
globalThis.expect.objectContaining = (s) => new Asymmetric('objectContaining', s);
globalThis.expect.arrayContaining = (s) => new Asymmetric('arrayContaining', s);
globalThis.expect.not = {
  arrayContaining: (s) => new Asymmetric('arrayContaining', s, true),
  objectContaining: (s) => new Asymmetric('objectContaining', s, true),
  stringContaining: (s) => new Asymmetric('stringContaining', s, true),
  stringMatching: (s) => new Asymmetric('stringMatching', s, true),
};
globalThis.expect.stringContaining = (s) => new Asymmetric('stringContaining', s);
globalThis.expect.stringMatching = (s) => new Asymmetric('stringMatching', s);

// ---- expectTypeOf / assertType (type-level assertions) ----
// Type assertions are checked by tsc and erased at compile time — they have no runtime behavior in
// vitest either. What has to exist at run time is the CHAIN, so `expectTypeOf<T>().toEqualTypeOf<U>()`
// (and `.not.toHaveProperty(...)`, `.items.toBeString()`, …) loads and runs. A proxy answers every
// property with itself, so any current or future member of the chain works — with `then` excluded so
// an accidental `await` can't see the chain as a thenable and hang forever.
const __typeChain = new Proxy(function () {}, {
  get(_t, p) {
    if (p === 'then' || p === 'catch' || p === 'finally') return undefined;
    if (p === Symbol.toPrimitive) return () => 'expectTypeOf';
    if (p === Symbol.toStringTag) return 'ExpectTypeOf';
    if (p === 'toString') return () => 'expectTypeOf';
    return __typeChain;
  },
  apply() { return __typeChain; },
  construct() { return __typeChain; },
});
globalThis.expectTypeOf = () => __typeChain;
globalThis.assertType = () => undefined;

// ---- vi (mocks/spies) ----
function makeSpy(impl) {
  const onceQueue = [];
  const fn = function (...args) {
    fn.mock.calls.push(args);
    fn.mock.instances.push(this);
    fn.mock.lastCall = args;
    let ret;
    // constructor call (e.g. vi.spyOn(mod, 'default') on a class, then `new spy()`): construct
    // the original with `new` — calling a class via .apply throws "cannot be invoked without new".
    if (new.target) {
      ret = typeof fn.__impl === 'function' ? Reflect.construct(fn.__impl, args, new.target) : this;
    } else if (onceQueue.length) ret = onceQueue.shift().apply(this, args);
    else if (fn.__returnValue !== undefined) ret = fn.__returnValue;
    else if (fn.__impl) ret = fn.__impl.apply(this, args);
    fn.mock.results.push({ type: 'return', value: ret });
    return ret;
  };
  fn.mock = { calls: [], results: [], instances: [], lastCall: undefined };
  fn._isMockFunction = true;
  fn.__impl = impl;
  fn.mockReturnValue = (v) => { fn.__returnValue = v; return fn; };
  fn.mockReturnThis = () => { fn.__impl = function () { return this; }; fn.__returnValue = undefined; return fn; };
  fn.mockImplementation = (f) => { fn.__impl = f; fn.__returnValue = undefined; return fn; };
  fn.mockResolvedValue = (v) => { fn.__impl = () => Promise.resolve(v); fn.__returnValue = undefined; return fn; };
  fn.mockRejectedValue = (v) => { fn.__impl = () => Promise.reject(v); fn.__returnValue = undefined; return fn; };
  fn.mockReturnValueOnce = (v) => { onceQueue.push(() => v); return fn; };
  fn.mockImplementationOnce = (f) => { onceQueue.push(typeof f === 'function' ? f : () => f); return fn; };
  fn.mockResolvedValueOnce = (v) => { onceQueue.push(() => Promise.resolve(v)); return fn; };
  fn.mockRejectedValueOnce = (v) => { onceQueue.push(() => Promise.reject(v)); return fn; };
  fn.mockClear = () => { fn.mock.calls = []; fn.mock.results = []; fn.mock.instances = []; fn.mock.lastCall = undefined; return fn; };
  fn.mockReset = () => { fn.mockClear(); fn.__impl = undefined; fn.__returnValue = undefined; onceQueue.length = 0; return fn; };
  fn.mockRestore = () => {};
  fn.getMockName = () => fn.__name || 'vi.fn()';
  fn.mockName = (n) => { fn.__name = n; return fn; };
  return fn;
}
// vitest automock (factory-less vi.mock, both loader paths): functions become vi.fn() mocks
// returning undefined, classes keep mocked statics and prototype methods (so instances get mock
// methods), objects and class instances are mocked deeply, arrays are emptied, primitives kept.
// A prototype chain is walked up to (not into) the built-in prototypes (Object, Error, ...).
function __isBuiltinProto(p) {
  if (p === Object.prototype || p === Function.prototype) return true;
  try { return typeof p.constructor === 'function' && /\{\s*\[native code\]\s*\}$/.test(Function.prototype.toString.call(p.constructor)); } catch (e) { return true; }
}
function __automockProtoMethods(target, proto) {
  for (let p = proto; p && !__isBuiltinProto(p); p = Object.getPrototypeOf(p)) {
    for (const k of Object.getOwnPropertyNames(p)) {
      if (k === 'constructor' || Object.prototype.hasOwnProperty.call(target, k)) continue;
      const d = Object.getOwnPropertyDescriptor(p, k);
      if (typeof d.value === 'function') { const s = makeSpy(); __spies.push(s); target[k] = s; }
    }
  }
}
function __automock(mod, depth) {
  depth = depth || 0;
  if (typeof mod === 'function') {
    const m = makeSpy(); __spies.push(m);
    if (depth < 3) {
      // own statics (class static methods are non-enumerable), not the function's built-ins
      for (const k of Object.getOwnPropertyNames(mod)) {
        if (k === 'length' || k === 'name' || k === 'prototype' || k === 'caller' || k === 'arguments') continue;
        try { m[k] = __automock(mod[k], depth + 1); } catch (e) {}
      }
      // class / constructor function: `new m()` returns an instance of m.prototype, whose
      // methods (inherited ones included) are mocks
      if (mod.prototype && mod.prototype.constructor === mod) __automockProtoMethods(m.prototype, mod.prototype);
    }
    return m;
  }
  if (Array.isArray(mod)) return [];
  if (mod && typeof mod === 'object') {
    const out = {};
    for (const k of Object.keys(mod)) {
      let v; try { v = mod[k]; } catch (e) { continue; }
      out[k] = depth < 4 ? __automock(v, depth + 1) : v;
    }
    // a class instance: its prototype methods become mocks too
    __automockProtoMethods(out, Object.getPrototypeOf(mod));
    return out;
  }
  return mod;
}
const __spies = [];
// Record (specifier, evaluated-exports) into a queue the native loader drains after the
// module runs, registering the mock keyed by the resolved absolute path. The factory runs
// here, in the calling module's scope (so closures over setup-file vars work). `vitest` is
// false for jest.mock/doMock (jest semantics where they differ).
const __ttMockErrorSpecs = new Set();
// vitest: reading a name the factory did not return throws (an ES import of it fails on access
// rather than reading undefined). Probes that interop/thenable checks make (`then`, `__esModule`),
// symbols and inherited members never throw. Keys (dir + spec) last mocked with jest.mock/doMock
// read undefined instead (jest semantics); checked on access, because the mock prepass registers
// every factory via vi.mock and an isolate reuse keeps that first exports object.
const __ttLenientMockKeys = new Set();
function __ttStrictMock(mod, key, spec) {
  if (mod === null || typeof mod !== 'object' || __ttMockErrors.has(mod)) return mod;
  return new Proxy(mod, {
    get: (t, k, r) => {
      if (typeof k === 'symbol' || k in t || k === 'then' || k === '__esModule' || __ttLenientMockKeys.has(key)) return Reflect.get(t, k, r);
      throw new Error(`[vitest] No "${k}" export is defined on the "${spec}" mock. Did you want to return the actual module?`);
    },
  });
}
// Modules (resolved absolute paths) the entry's mock prepass registered for the current file.
// vitest calls a hoisted factory ONCE; the module's own hoisted vi.mock call for such a module
// keeps the prepass registration instead of running the factory a second time. Consumed by
// that call (a later vi.mock/doMock of the module runs normally) and cleared between files.
const __ttPrepassMocks = new Set();
globalThis.__ttPrepassMocks = __ttPrepassMocks;
function __ttPrepassKey(spec) {
  const dir = globalThis.__ttDir || globalThis.__ttEntryDir;
  return (dir && globalThis.__ttResolveSpec(dir, String(spec))) || '\0' + String(spec);
}
// Pending async vi.doMock / jest.doMock factories, by the module's mock-registry key (its
// resolved path, `?query` kept): each is registered once its factory settles, and an import() of
// the module waits for that first (vitest awaits the factory; turbo-test#42 item 6). A later
// mock / unmock of the module drops its entry, so the stale factory never registers.
const __ttDoMockWaits = new Map();
globalThis.__ttDoMockWaits = __ttDoMockWaits;
function __ttDoMockKey(dir, spec) {
  return (__ttDoMockWaits.size && dir && globalThis.__ttResolveSpec(dir, String(spec))) || undefined;
}
// The pending registration an import() of `spec` from `dir` must wait for, if any (the ESM
// loader's import() asks it) ...
globalThis.__ttImportWait = (dir, spec) => {
  const k = __ttDoMockKey(dir, spec);
  return k && __ttDoMockWaits.get(k);
};
// ... and the promise the module-runner's lowered import() chains its require() on:
// Promise.resolve() when nothing is pending, so the timing is unchanged.
globalThis.__ttImportReady = (dir, spec) => globalThis.__ttImportWait(dir, spec) || Promise.resolve();
// Register `exports` as the mock of `spec` from `dir` now; false when it cannot be (no loader
// registry yet).
function __ttRegisterMockNow(dir, spec, exports, key) {
  // An error stand-in replaces (never merges into) a module already registered under this
  // isolate; a later working factory for the same spec replaces the stand-in the same way.
  if (__ttMockErrors.has(exports) || __ttMockErrorSpecs.has(key)) {
    globalThis.vi.unmock(spec);
    if (__ttMockErrors.has(exports)) __ttMockErrorSpecs.add(key); else __ttMockErrorSpecs.delete(key);
  }
  try { globalThis.__ttRegisterMock(dir, String(spec), exports); return true; } catch (e) { return false; }
}
// An async doMock factory's result: registered once it settles (a rejection registers the
// factory-error stand-in), unless a later mock / unmock of the module came first.
function __ttAwaitDoMock(dir, spec, exports, key, vitest) {
  const k = dir && globalThis.__ttResolveSpec(dir, String(spec));
  if (!k) return false;
  const done = Promise.resolve(exports).then(
    (v) => __ttStrictMock(v, key, String(spec)),
    (e) => __ttFactoryErrorMock(e, vitest),
  ).then((v) => {
    if (__ttDoMockWaits.get(k) !== done) return;
    __ttDoMockWaits.delete(k);
    if (!__ttRegisterMockNow(dir, spec, v, key)) {
      (globalThis.__pendingMocks || (globalThis.__pendingMocks = [])).push({ spec: String(spec), exports: v, vitest, key });
    }
  });
  __ttDoMockWaits.set(k, done);
  return true;
}
function __ttMock(spec, factory, vitest, isDoMock) {
  let exports;
  const key = (globalThis.__ttDir || '') + '\0' + String(spec);
  // The calling module's dir: the module-runner's __ttDir, else (ESM graph, where modules have
  // none) the entry file's dir, so a runtime vi.doMock registers immediately and a later import()
  // of the module sees it.
  const dir = globalThis.__ttDir || globalThis.__ttEntryDir;
  if (!globalThis.__ttMockPrepass) {
    if (vitest) __ttLenientMockKeys.delete(key); else __ttLenientMockKeys.add(key);
    if (__ttPrepassMocks.size && __ttPrepassMocks.delete(__ttPrepassKey(spec))) return;
    // this mock supersedes a still-pending async doMock of the module
    const pending = __ttDoMockKey(dir, spec);
    if (pending) __ttDoMockWaits.delete(pending);
  }
  // vitest passes `importOriginal` (a fn returning the real module, as a Promise) as the
  // factory's first arg — `vi.mock('x', async (importOriginal) => ({...await importOriginal(), y}))`.
  const importOriginal = () => Promise.resolve(globalThis.__ttImportActual(dir, String(spec)));
  if (factory === undefined) {
    // AUTOMOCK: no factory -> auto-generate a mock from the real module (functions -> vi.fn()).
    // A real module that fails to load is left unmocked: importing it reports its own error.
    try { exports = __automock(globalThis.__ttImportActual(dir, String(spec))); }
    catch (e) { return; }
  } else {
    try { exports = typeof factory === 'function' ? factory(importOriginal) : factory; }
    catch (e) {
      // The entry's mock prepass (__ttMockPrepass) runs factories outside the module's scope; a
      // throw there is not registered — the module's own vi.mock call re-runs it in scope.
      if (globalThis.__ttMockPrepass) return;
      exports = __ttFactoryErrorMock(e, vitest);
    }
  }
  // Sync factory: register IMMEDIATELY in the calling module's dir (vi.mock is hoisted above
  // the module's consumer requires, so the mock is in place before they load — and the factory
  // ran in THIS module's scope, so it shares the test's bindings). Async (Promise) factory:
  // queue for __resolvePendingMocks (run_entry_mocks prepass / drain).
  if (globalThis.__ttMockPrepass) __ttPrepassMocks.add(__ttPrepassKey(spec));
  if (exports && typeof exports.then === 'function') {
    // a doMock (not hoisted: it applies to later imports) waits on its own; a hoisted vi.mock is
    // awaited by the loader's resolve + drain
    if (isDoMock && !globalThis.__ttMockPrepass && globalThis.__ttRegisterMock && __ttAwaitDoMock(dir, spec, exports, key, vitest)) return;
    (globalThis.__pendingMocks || (globalThis.__pendingMocks = [])).push({ spec: String(spec), exports, vitest, key });
    return;
  }
  if (factory !== undefined) exports = __ttStrictMock(exports, key, String(spec));
  if (globalThis.__ttRegisterMock && dir && __ttRegisterMockNow(dir, spec, exports, key)) return;
  (globalThis.__pendingMocks || (globalThis.__pendingMocks = [])).push({ spec: String(spec), exports, vitest, key });
}
globalThis.vi = {
  fn: (impl) => { const s = makeSpy(impl); __spies.push(s); return s; },
  spyOn: (obj, key) => {
    const orig = obj[key];
    // Do NOT bind orig to obj — for a prototype-method spy (spyOn(Cls.prototype, 'm')) the
    // original must run with the CALL's `this` (the instance), not the prototype. makeSpy's
    // fn applies the call-time `this` to __impl, preserving it.
    const s = makeSpy(orig);
    // Use defineProperty so it replaces a (configurable) live-export getter, not just a
    // writable data prop — module-runner exports are configurable getters.
    const define = (v) => { try { Object.defineProperty(obj, key, { configurable: true, enumerable: true, writable: true, value: v }); } catch (e) { try { obj[key] = v; } catch (e2) {} } };
    s.mockRestore = () => define(orig);
    define(s);
    __spies.push(s);
    return s;
  },
  // Record (specifier, evaluated-exports) into a queue the native loader drains after the
  // module runs, registering the mock keyed by the resolved absolute path. The factory runs
  // here, in the calling module's scope (so closures over setup-file vars work).
  mock: (spec, factory) => __ttMock(spec, factory, true),
  unmock: (spec) => {
    if (globalThis.__pendingMocks) globalThis.__pendingMocks = globalThis.__pendingMocks.filter((m) => m.spec !== String(spec));
    const pending = __ttDoMockKey(globalThis.__ttDir || globalThis.__ttEntryDir, spec);
    if (pending) __ttDoMockWaits.delete(pending);
    const dir = globalThis.__ttDir || globalThis.__cwd;
    if (globalThis.__ttUnmock && dir) { try { globalThis.__ttUnmock(dir, String(spec)); } catch (e) {} }
  },
  doMock: (spec, factory) => __ttMock(spec, factory, true, true),
  doUnmock: (spec) => globalThis.vi.unmock(spec),
  clearAllMocks: () => { __spies.forEach((s) => s.mockClear()); },
  resetAllMocks: () => { __spies.forEach((s) => s.mockReset()); },
  restoreAllMocks: () => { __spies.forEach((s) => { if (s.mockRestore) s.mockRestore(); }); },
  stubGlobal: (k, v) => {
    const m = globalThis.__stubbedGlobals || (globalThis.__stubbedGlobals = new Map());
    if (!m.has(k)) m.set(k, { had: k in globalThis, val: globalThis[k] });
    globalThis[k] = v;
  },
  stubEnv: (k, v) => {
    const m = globalThis.__stubbedEnvs || (globalThis.__stubbedEnvs = new Map());
    if (!m.has(k)) m.set(k, { had: k in globalThis.process.env, val: globalThis.process.env[k] });
    globalThis.process.env[k] = v;
  },
  isMockFunction: (f) => typeof f === 'function' && f._isMockFunction === true,
  // fake timers
  useFakeTimers: () => {
    __loop.fake = true;
    if (__loop.systemTime === 0) __loop.systemTime = __loop.realNow();
    __installFakeDate();
    // Mark the global timer fns so @testing-library's jestFakeTimersAreEnabled() (checks
    // `setTimeout.clock` / `_isMockFunction`) detects fake timers — otherwise waitFor polls with the
    // frozen real clock and hangs. testing-library then drives jest.advanceTimersByTime() to advance.
    try { globalThis.setTimeout.clock = __loop; globalThis.setInterval.clock = __loop; } catch (e) {}
    return globalThis.vi;
  },
  useRealTimers: () => {
    __loop.fake = false; __restoreDate();
    try { delete globalThis.setTimeout.clock; delete globalThis.setInterval.clock; } catch (e) {}
    return globalThis.vi;
  },
  isFakeTimers: () => __loop.fake,
  setSystemTime: (t) => {
    const ms = t instanceof __loop.RealDate ? t.getTime()
      : typeof t === 'number' ? t : new __loop.RealDate(t).getTime();
    __loop.systemTime = ms;
    __loop.now = 0;
    // vitest fakes Date on setSystemTime even without useFakeTimers — install the fake Date so
    // `new Date()` / Date.now() reflect the set time (timer faking stays separate).
    __installFakeDate();
    return globalThis.vi;
  },
  getMockedSystemTime: () => (__loop.fake ? new __loop.RealDate(__loop.systemTime + __loop.now) : null),
  waitFor: async (fn, opts) => {
    const timeout = (opts && opts.timeout) || 1000;
    const interval = (opts && opts.interval) || 50;
    let last;
    for (let elapsed = 0; elapsed <= timeout; elapsed += interval) {
      try { const r = await fn(); if (r !== false && r !== null && r !== undefined) return r; if (r === undefined) return r; }
      catch (e) { last = e; }
      await new Promise((res) => setTimeout(res, interval));
    }
    throw last || new Error('vi.waitFor timed out');
  },
  waitUntil: async (fn, opts) => {
    const timeout = (opts && opts.timeout) || 1000;
    const interval = (opts && opts.interval) || 50;
    for (let elapsed = 0; elapsed <= timeout; elapsed += interval) {
      try { const r = await fn(); if (r) return r; } catch (e) {}
      await new Promise((res) => setTimeout(res, interval));
    }
    throw new Error('vi.waitUntil timed out');
  },
  advanceTimersByTime: (ms) => { __loop.advance(ms); return globalThis.vi; },
  advanceTimersByTimeAsync: async (ms) => { __loop.advance(ms); return globalThis.vi; },
  advanceTimersToNextTimer: () => {
    const n = __loop.macro.slice().sort((a, b) => a.due - b.due || a.id - b.id)[0];
    if (n) __loop.advance(n.due - __loop.now);
    return globalThis.vi;
  },
  runAllTimers: () => { __loop.runAll(); return globalThis.vi; },
  runAllTimersAsync: async () => { __loop.runAll(); return globalThis.vi; },
  runOnlyPendingTimers: () => { __loop.runOnlyPending(); return globalThis.vi; },
  runOnlyPendingTimersAsync: async () => { __loop.runOnlyPending(); return globalThis.vi; },
  advanceTimersToNextTimerAsync: async () => { const n = __loop.macro.slice().sort((a, b) => a.due - b.due || a.id - b.id)[0]; if (n) __loop.advance(n.due - __loop.now); return globalThis.vi; },
  getTimerCount: () => __loop.macro.length,
  clearAllTimers: () => { __loop.macro = []; return globalThis.vi; },
  mocked: (f) => {
    if (typeof f === 'function' && !f.mock) {
      f.mock = { calls: [], results: [] };
      f.mockClear = () => { f.mock.calls = []; f.mock.results = []; return f; };
      f.mockReset = () => f;
      f.mockRestore = () => {};
      f.mockReturnValue = () => f;
      f.mockReturnValueOnce = () => f;
      f.mockImplementation = () => f;
      f.mockImplementationOnce = () => f;
      f.mockResolvedValue = () => f;
      f.mockRejectedValue = () => f;
    }
    return f;
  },
  // vi.hoisted must return the SAME value in the mock-prepass and the entry module (the mock
  // closure and the test share it). Cache by call-index: the prepass fills the cache, then the
  // entry (index reset to 0) hits it — so `const x = vi.hoisted(...)` is one object everywhere.
  hoisted: (f) => {
    const cache = globalThis.__hoistedCache || (globalThis.__hoistedCache = []);
    const i = globalThis.__hoistedIdx || 0;
    globalThis.__hoistedIdx = i + 1;
    if (i < cache.length) return cache[i];
    const v = f();
    cache[i] = v;
    return v;
  },
  importActual: (p) => { try { return Promise.resolve(globalThis.__ttImportActual(globalThis.__ttDir, String(p))); } catch (e) { return Promise.resolve({}); } },
  importMock: (p) => { try { return Promise.resolve(globalThis.__nativeRequire(globalThis.__ttDir, String(p))); } catch (e) { return Promise.resolve({}); } },
  unstubAllEnvs: () => {
    const m = globalThis.__stubbedEnvs;
    if (m) { m.forEach(({ had, val }, k) => { if (had) globalThis.process.env[k] = val; else delete globalThis.process.env[k]; }); m.clear(); }
  },
  unstubAllGlobals: () => {
    const m = globalThis.__stubbedGlobals;
    if (m) { m.forEach(({ had, val }, k) => { if (had) globalThis[k] = val; else delete globalThis[k]; }); m.clear(); }
  },
  resetModules: () => { if (globalThis.__ttResetModules) globalThis.__ttResetModules(); return globalThis.vi; },
  setConfig: () => {},
  stubEnv: (k, v) => {
    const m = globalThis.__stubbedEnvs || (globalThis.__stubbedEnvs = new Map());
    if (!m.has(k)) m.set(k, { had: k in globalThis.process.env, val: globalThis.process.env[k] });
    globalThis.process.env[k] = v;
  },
};

// ---- legacy decorator + emitDecoratorMetadata helpers ----
// The oxc decorator-metadata transform emits `babelHelpers.decorate / decorateParam /
// decorateMetadata(...)` (External helper mode). Provide them with standard tslib semantics so
// NestJS/Mongoose decorators (`@Injectable`, `@Prop`, `@Controller`) run. No Reflect.metadata
// polyfill: such projects depend on the real `reflect-metadata`, which their import graph
// (`@nestjs/common`) loads before any decorator runs. A partial polyfill would trip
// reflect-metadata's "already installed" guard, leaving it missing methods (getOwnMetadataKeys
// etc.). `decorateMetadata` no-ops cleanly when Reflect.metadata is absent (non-Nest projects).
globalThis.babelHelpers = globalThis.babelHelpers || {};
globalThis.babelHelpers.decorate = function (decorators, target, key, desc) {
  var c = arguments.length,
    r = c < 3 ? target : desc === null ? (desc = Object.getOwnPropertyDescriptor(target, key)) : desc,
    d;
  if (typeof Reflect === 'object' && typeof Reflect.decorate === 'function') {
    r = Reflect.decorate(decorators, target, key, desc);
  } else {
    for (var i = decorators.length - 1; i >= 0; i--) {
      if ((d = decorators[i])) r = (c < 3 ? d(r) : c > 3 ? d(target, key, r) : d(target, key)) || r;
    }
  }
  return c > 3 && r && Object.defineProperty(target, key, r), r;
};
globalThis.babelHelpers.decorateParam = function (paramIndex, decorator) {
  return function (target, key) { decorator(target, key, paramIndex); };
};
globalThis.babelHelpers.decorateMetadata = function (metadataKey, metadataValue) {
  if (typeof Reflect === 'object' && typeof Reflect.metadata === 'function') {
    return Reflect.metadata(metadataKey, metadataValue);
  }
};

// ---- jest compatibility shim ----
// Jest's `jest` global maps onto the same machinery as `vi` (drop-in for jest projects, e.g.
// NestJS backends using ts-jest). Most methods are identical; the jest-only ones (sync
// requireActual/requireMock, isolateModules, setTimeout) are bridged here. Type-only members
// (jest.Mock, jest.Mocked, jest.SpyInstance, ...) are erased by the transform — no runtime needed.
// Defined unconditionally: it doesn't touch vitest behavior (vitest suites never reference `jest`).
globalThis.jest = {
  fn: (impl) => globalThis.vi.fn(impl),
  spyOn: (obj, key) => globalThis.vi.spyOn(obj, key),
  mock: (spec, factory) => __ttMock(spec, factory, false),
  unmock: (spec) => globalThis.vi.unmock(spec),
  doMock: (spec, factory) => __ttMock(spec, factory, false, true),
  dontMock: (spec) => globalThis.vi.unmock(spec),
  mocked: (item) => globalThis.vi.mocked(item),
  clearAllMocks: () => globalThis.vi.clearAllMocks(),
  resetAllMocks: () => globalThis.vi.resetAllMocks(),
  restoreAllMocks: () => globalThis.vi.restoreAllMocks(),
  isMockFunction: (f) => globalThis.vi.isMockFunction(f),
  // jest.requireActual / requireMock are SYNCHRONOUS (return the module, not a Promise) —
  // unlike vi.importActual. Bridge to the native sync loader directly.
  requireActual: (p) => { try { return globalThis.__ttImportActual(globalThis.__ttDir, String(p)); } catch (e) { return {}; } },
  requireMock: (p) => { try { return globalThis.__nativeRequire(globalThis.__ttDir, String(p)); } catch (e) { return {}; } },
  // jest.isolateModules(fn): run `fn` with a fresh module registry. Approximate with a
  // resetModules around the (sync) callback — enough for the common "re-require with new env".
  isolateModules: (fn) => { globalThis.vi.resetModules(); try { fn(); } finally { globalThis.vi.resetModules(); } },
  isolateModulesAsync: async (fn) => { globalThis.vi.resetModules(); try { await fn(); } finally { globalThis.vi.resetModules(); } },
  resetModules: () => { globalThis.vi.resetModules(); return globalThis.jest; },
  // timers
  useFakeTimers: (cfg) => { globalThis.vi.useFakeTimers(cfg); return globalThis.jest; },
  useRealTimers: () => { globalThis.vi.useRealTimers(); return globalThis.jest; },
  setSystemTime: (t) => { globalThis.vi.setSystemTime(t); return globalThis.jest; },
  getRealSystemTime: () => __loop.realNow(),
  now: () => (globalThis.vi.isFakeTimers() ? globalThis.vi.getMockedSystemTime().getTime() : __loop.realNow()),
  advanceTimersByTime: (ms) => { globalThis.vi.advanceTimersByTime(ms); return globalThis.jest; },
  advanceTimersByTimeAsync: async (ms) => { await globalThis.vi.advanceTimersByTimeAsync(ms); return globalThis.jest; },
  advanceTimersToNextTimer: () => { globalThis.vi.advanceTimersToNextTimer(); return globalThis.jest; },
  runAllTimers: () => { globalThis.vi.runAllTimers(); return globalThis.jest; },
  runAllTimersAsync: async () => { await globalThis.vi.runAllTimersAsync(); return globalThis.jest; },
  runOnlyPendingTimers: () => { globalThis.vi.runOnlyPendingTimers(); return globalThis.jest; },
  clearAllTimers: () => { globalThis.vi.clearAllTimers(); return globalThis.jest; },
  getTimerCount: () => globalThis.vi.getTimerCount(),
  // per-test config knobs — no-ops (turbo-test has no per-file timeout/retry gate yet)
  setTimeout: () => globalThis.jest,
  retryTimes: () => globalThis.jest,
  // env stubbing parity (rarely used via jest, but harmless)
  replaceProperty: (obj, key, val) => { const orig = obj[key]; obj[key] = val; return { restore: () => { obj[key] = orig; } }; },
};

// ---- collector: describe / it / hooks ----
const root = { name: '', tests: [], suites: [], hooks: { be: [], ae: [], ba: [], aa: [] }, parent: null };
let current = root;

function describe(name, a, b) {
  const fn = typeof a === 'function' ? a : (typeof b === 'function' ? b : () => {});
  const suite = { name, tests: [], suites: [], hooks: { be: [], ae: [], ba: [], aa: [] }, parent: current };
  current.suites.push(suite);
  const prev = current;
  current = suite;
  fn();
  current = prev;
}
// A skipped/todo suite registers no tests, but it IS a collected task for vitest's "No test suite
// found in file" check (see __tt.run) — count it so such a file is not reported as empty.
describe.skip = (name, _fn) => { __tt.skippedSuites++; };
describe.only = (name, fn) => { __tt.hasOnly = true; describe(name, fn); };
describe.todo = (name) => { __tt.skippedSuites++; /* todo: collected as nothing to run */ };
// describe.skipIf/runIf: when the condition skips the block, register NO tests (the whole suite
// is absent) — mirrors it.skipIf/runIf semantics applied at the suite level.
describe.skipIf = (cond) => (name, a, b) => { if (cond) { __tt.skippedSuites++; return; } describe(name, a, b); };
describe.runIf = (cond) => (name, a, b) => { if (!cond) { __tt.skippedSuites++; return; } describe(name, a, b); };
describe.concurrent = describe; // accepted; a file's tests still run sequentially
describe.each = (rows) => (name, fn) => rows.forEach((row, i) => {
  const args = Array.isArray(row) ? row : [row];
  describe(typeof name === 'string' ? name : name(row), () => fn(...args));
});
// Supports both vitest signatures: it(name, fn, opts) and it(name, opts, fn).
function __normTest(a, b) {
  let fn, o;
  if (typeof a === 'function') { fn = a; o = (typeof b === 'object' && b ? b : (typeof b === 'number' ? { timeout: b } : {})); }
  else if (typeof a === 'object' && a) { o = a; fn = (typeof b === 'function' ? b : () => {}); }
  else { fn = (typeof b === 'function' ? b : () => {}); o = {}; }
  return { fn, o };
}
// Default per-test timeout (ms): `--testTimeout` (host → globalThis.__TT_DEFAULT_TIMEOUT) else
// vitest's 5000ms default. A test's own `{ timeout }` overrides this in runSuite.
function __defaultTimeout() {
  const n = Number(globalThis.__TT_DEFAULT_TIMEOUT);
  return Number.isFinite(n) && n > 0 ? n : 5000;
}
function it(name, a, b) {
  const { fn, o } = __normTest(a, b);
  current.tests.push({ name, fn, skip: false, only: false, retry: o.retry || 0, timeout: o.timeout });
}
it.skip = (name, a, b) => current.tests.push({ name, fn: __normTest(a, b).fn, skip: true });
it.todo = (name) => current.tests.push({ name, fn: () => {}, skip: true });
it.only = (name, a, b) => { __tt.hasOnly = true; const { fn, o } = __normTest(a, b); current.tests.push({ name, fn, skip: false, only: true, retry: o.retry || 0, timeout: o.timeout }); };
it.concurrent = it; // accepted; this runner executes a file's tests sequentially
it.each = (rows) => (name, fn) => {
  rows.forEach((row, i) => {
    const args = Array.isArray(row) ? row : [row];
    current.tests.push({ name: `${name} [${i}]`, fn: () => fn(...args), skip: false, only: false, retry: 0 });
  });
};
it.skipIf = (cond) => (name, fn) => current.tests.push({ name, fn, skip: !!cond, only: false, retry: 0 });
it.runIf = (cond) => (name, fn) => current.tests.push({ name, fn, skip: !cond, only: false, retry: 0 });
// it.fails: the test PASSES iff its body throws (runSuite inverts the outcome via `fails`).
it.fails = (name, a, b) => { const { fn, o } = __normTest(a, b); current.tests.push({ name, fn, skip: false, only: false, retry: o.retry || 0, fails: true }); };
// it.extend({...}) — test-context fixtures (best-effort). vitest passes a merged context object as
// the test fn's first arg; fixtures defined as plain values or `async ({}, use) => use(value)`
// functions are resolved here and provided. Returns a new test fn with these fixtures baked in.
it.extend = (fixtures) => {
  const resolveCtx = async () => {
    const ctx = {};
    for (const k of Object.keys(fixtures || {})) {
      const f = fixtures[k];
      if (typeof f === 'function') {
        // vitest fixture fn: ({...deps}, use) => { ... await use(value) }. Capture the used value.
        let captured;
        const use = (v) => { captured = v; return Promise.resolve(); };
        const r = f(ctx, use);
        if (r && typeof r.then === 'function') await r;
        ctx[k] = captured;
      } else {
        ctx[k] = f;
      }
    }
    return ctx;
  };
  const wrap = (userFn) => async () => { const ctx = await resolveCtx(); return userFn(ctx); };
  const ext = (name, a, b) => { const { fn, o } = __normTest(a, b); current.tests.push({ name, fn: wrap(fn), skip: false, only: false, retry: o.retry || 0 }); };
  ext.skip = (name, a, b) => current.tests.push({ name, fn: wrap(__normTest(a, b).fn), skip: true });
  ext.only = (name, a, b) => { __tt.hasOnly = true; const { fn, o } = __normTest(a, b); current.tests.push({ name, fn: wrap(fn), skip: false, only: true, retry: o.retry || 0 }); };
  ext.each = (rows) => (name, fn) => rows.forEach((row, i) => {
    const args = Array.isArray(row) ? row : [row];
    current.tests.push({ name: `${name} [${i}]`, fn: wrap((ctx) => fn(...args, ctx)), skip: false, only: false, retry: 0 });
  });
  ext.extend = (more) => it.extend({ ...fixtures, ...more });
  return ext;
};
globalThis.describe = describe;
globalThis.it = it;
globalThis.test = it;
globalThis.beforeEach = (fn) => current.hooks.be.push(fn);
globalThis.afterEach = (fn) => current.hooks.ae.push(fn);
globalThis.beforeAll = (fn) => current.hooks.ba.push(fn);
globalThis.afterAll = (fn) => current.hooks.aa.push(fn);

// ---- runner ----
async function runSuite(suite, ancestors, summary) {
  // A throwing beforeAll/afterAll must NOT reject the whole run (vitest fails the suite's tests
  // but settles the run). Record it as a failure and continue so the file still reports.
  for (const h of suite.hooks.ba) {
    try { await h(); } catch (e) { summary.failed++; summary.failures.push(`${suite.name || '<root>'} beforeAll: ${(e && e.message) || String(e)}`); }
  }
  const chain = [...ancestors, suite];
  for (const t of suite.tests) {
    const label = chain.map((s) => s.name).filter(Boolean).concat(t.name).join(' > ');
    // -t/--testNamePattern: skip tests whose full `describe > it` name does not match the regex
    // (vitest: unanchored, case-sensitive, tested against the joined name).
    if (__tt.namePattern && !__tt.namePattern.test(label)) { summary.skipped++; continue; }
    if (t.skip || (__tt.hasOnly && !t.only)) { summary.skipped++; summary.tests.push({ name: label, status: 'skipped', duration_ms: 0 }); continue; }
    // retry: per-test `{ retry }` wins; else the global default (--retry → __TT_DEFAULT_RETRY).
    const retry = (t.retry != null && t.retry > 0)
      ? t.retry
      : (Number(globalThis.__TT_DEFAULT_RETRY) > 0 ? Number(globalThis.__TT_DEFAULT_RETRY) : 0);
    const attempts = retry + 1;
    // timeout (ms): per-test `{ timeout }` wins; else the --testTimeout/5000 default.
    const timeoutMs = (typeof t.timeout === 'number' && t.timeout > 0) ? t.timeout : __defaultTimeout();
    let lastErr;
    let ok = false;
    // Real wall time for the per-test duration reported by junit/tap/verbose. NOT performance.now()
    // — that's stubbed to a constant 0 in this runtime (see the perf stub near top of file).
    const __t0 = Date.now();
    for (let a = 0; a < attempts && !ok; a++) {
      // Reset per-test snapshot + assertion-count state (snapshot counter is keyed by test name,
      // so distinct multi-snapshot tests don't collide; assertion enforcement resets each attempt).
      __snap.testName = label;
      __snap.counters[label] = 0;
      __snap.assertCount = 0;
      __snap.expectedAssertions = null;
      __snap.hasAssertions = false;
      try {
        for (const s of chain) for (const h of s.hooks.be) await h();
        // Race t.fn() against a timeout. The timeout is an INTERNAL one-shot timer (separate
        // from the user/fake-timer queue), so the Rust drive loop advances the virtual clock to
        // it and rejects even a test that never resolves (e.g. `await new Promise(() => {})`) —
        // otherwise that hangs the whole worker — while staying invisible to vi.runAllTimers etc.
        let timer;
        const timed = new Promise((_, rej) => {
          timer = __scheduleInternal(() => rej(new Error(`test timed out in ${timeoutMs}ms`)), timeoutMs);
        });
        try {
          await Promise.race([Promise.resolve().then(() => t.fn()), timed]);
        } finally {
          __clearInternal(timer);
        }
        // expect.assertions(n) / hasAssertions() enforcement (vitest: checked after the test body).
        if (__snap.expectedAssertions != null && __snap.assertCount !== __snap.expectedAssertions) {
          throw new Error(`expected ${__snap.expectedAssertions} assertion(s) but got ${__snap.assertCount}`);
        }
        if (__snap.hasAssertions && __snap.assertCount === 0) {
          throw new Error('expected at least one assertion but got none');
        }
        ok = true;
      } catch (e) {
        lastErr = e;
      }
      // it.fails: invert the outcome — a thrown error is the success condition, a clean pass fails.
      if (t.fails) {
        if (lastErr) { ok = true; lastErr = undefined; }
        else { ok = false; lastErr = new Error('expected test to fail, but it passed'); }
      }
      // afterEach (incl. @testing-library cleanup) runs even when the test failed — otherwise a
      // failed test's mounted DOM leaks into the next test. Hook errors here don't fail a test
      // that already passed (matches the prior behavior; avoids mass cleanup-throw failures).
      for (const s of [...chain].reverse()) for (const h of s.hooks.ae) {
        try { await h(); } catch (e) {}
      }
    }
    const __dur = Date.now() - __t0;
    if (ok) { summary.passed++; summary.tests.push({ name: label, status: 'passed', duration_ms: __dur }); }
    else {
      summary.failed++;
      let msg = lastErr ? (lastErr.message || String(lastErr)) : String(lastErr);
      if (lastErr && lastErr.errors && lastErr.errors.length) {
        msg += ' [' + lastErr.errors.map((x) => (x && x.message) || String(x)).join('; ') + ']';
      }
      if ((!msg || msg === 'AggregateError' || msg === 'Error') && lastErr && lastErr.stack) {
        msg = String(lastErr.stack).split('\n').slice(0, 3).join(' | ');
      }
      if (globalThis.__TT_STACK && lastErr) {
        const e0 = lastErr.errors && lastErr.errors[0] ? lastErr.errors[0] : lastErr;
        if (e0 && e0.stack) msg += '\n          @ ' + String(e0.stack).split('\n').slice(0, 5).join('\n          @ ');
      }
      summary.failures.push(`${label}: ${msg}`);
      summary.tests.push({ name: label, status: 'failed', duration_ms: __dur, message: msg });
    }
  }
  for (const child of suite.suites) await runSuite(child, chain, summary);
  for (const h of suite.hooks.aa) {
    try { await h(); } catch (e) { summary.failed++; summary.failures.push(`${suite.name || '<root>'} afterAll: ${(e && e.message) || String(e)}`); }
  }
}

globalThis.__tt = {
  hasOnly: false,
  // Skipped/todo describe blocks seen in this file (they register nothing in `root`).
  skippedSuites: 0,
  // Compiled lazily from globalThis.__TT_NAME_PATTERN (set by the host from -t/--testNamePattern).
  get namePattern() {
    if (this._np !== undefined) return this._np;
    const src = globalThis.__TT_NAME_PATTERN;
    this._np = src ? (() => { try { return new RegExp(src); } catch { return null; } })() : null;
    return this._np;
  },
  async run() {
    const summary = { passed: 0, failed: 0, skipped: 0, failures: [], tests: [] };
    // vitest (runner `runFiles`): a file that collected NO task at all — no test or suite, skipped
    // and todo ones included — fails with "No test suite found in file <path>" unless
    // passWithNoTests. Reported as the file's only failure entry with no failed test; the CLI
    // turns that into a file-level error (cli.rs `file_result`).
    if (!root.tests.length && !root.suites.length && !this.skippedSuites && !globalThis.__TT_PASS_WITH_NO_TESTS) {
      summary.failures.push(`No test suite found in file ${globalThis.__ttFile}`);
      return summary;
    }
    // --no-allowOnly (host → __TT_FORBID_ONLY): a stray `.only` must fail the run (vitest CI
    // default). Record it as a failure so it surfaces in the report and flips the exit code.
    if (globalThis.__TT_FORBID_ONLY && this.hasOnly) {
      summary.failed++;
      summary.failures.push('found `.only` test(s) but --allowOnly is disabled (--no-allowOnly)');
    }
    await runSuite(root, [], summary);
    return summary;
  },
};

// Isolate-reuse (TURBO_REUSE_ISOLATE): when a worker reuses one isolate+context across files,
// the runner calls this between files to wipe per-file framework state — collected tests, spies,
// timers, fake clock, pending mocks, vi.hoisted cache, and env/global stubs — so file N+1 starts
// as clean as a fresh isolate would. Module caches (node_modules) are kept on the Rust side.
// Snapshot the hooks registered by setup files (run once per worker). The per-file reset
// restores root.hooks to this baseline so the setup file's afterEach(cleanup) etc. survive
// across files, while each test file's own top-level hooks are dropped.
globalThis.__ttCaptureHookBaseline = () => {
  globalThis.__ttHookBaseline = {
    be: root.hooks.be.slice(),
    ae: root.hooks.ae.slice(),
    ba: root.hooks.ba.slice(),
    aa: root.hooks.aa.slice(),
  };
  // Spies created during setup (e.g. the analytics trackEvent vi.fn()) are the first entries in
  // __spies and must persist; everything appended by a test file is per-file and must be dropped
  // on reset. Record the setup count as the truncation point.
  globalThis.__ttSpyBaseline = __spies.length;
};

// Per-file reset, modeled on vitest's isolate:false loop (vitest/dist .../base*.js): with
// isolation OFF, vitest does NOT reset modules, the DOM, spies, timers or globals between files
// — it only calls `vi.resetConfig()` + `vi.restoreAllMocks()`, and its runner scopes test
// collection per file. We mirror that minimally. Over-resetting (clearing DOM, truncating the
// spy registry, unstubbing globals, poking React internals) was what broke interaction tests
// ~100 files into a shared isolate — vitest proves none of that is needed.
globalThis.__ttResetForNextFile = () => {
  // turbo-test collects tests into a single global `root` (vitest scopes per file via
  // startTests([file])) — so we must drop the prior file's collected tests/suites. Setup files
  // run once per worker, so root.hooks only holds setup hooks; leave them in place.
  root.tests = [];
  root.suites = [];
  // Reset snapshot counters between files (keyed by test name; cleared to avoid cross-file drift).
  __snap.counters = Object.create(null);
  // NOTE: root.hooks is intentionally LEFT in place. Restoring it to a post-global-setup baseline
  // drops @testing-library's auto-cleanup afterEach for most files (it registers during a test
  // file's first render, not during global setup) → DOM accumulates → mass failure. The downside
  // (a test-file-imported setup module's root afterEach leaking, e.g. marketing analytics-test-
  // setup) is the lesser evil and handled elsewhere.
  current = root;
  globalThis.__tt.hasOnly = false;
  globalThis.__tt.skippedSuites = 0;
  // mock-prepass scratch (turbo-test specific) — must be empty before the next file hoists.
  globalThis.__pendingMocks = [];
  __ttPrepassMocks.clear();
  __ttDoMockWaits.clear();
  globalThis.__hoistedCache = [];
  globalThis.__hoistedIdx = 0;
  // Bump the timer generation: the prior file's leaked timers (now gen < __loop.gen) get dropped,
  // unrun, the moment the loop next reaches them (see __dropStaleTimers) — without touching this
  // file's own pending one-shots. Do NOT clear macro/nextTicks or reset the clock here (that
  // broke ~40 async tests).
  __loop.gen++;
  try { globalThis.vi && globalThis.vi.resetConfig && globalThis.vi.resetConfig(); } catch (e) {}
  // NOTE: we do NOT call restoreAllMocks() here. A spyOn spy's mockRestore() redefines a
  // (possibly shared, cached) module export back to the `orig` captured at spy time. This
  // suite's own setup.ts afterEach already restores per test; calling it again between files
  // re-applies a now-stale orig onto the shared analytics module — corrupting trackEvent for
  // every later file (asserts then see the wrong/old spy). Per-file restore is the test's job.
};
