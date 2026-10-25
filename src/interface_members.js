(function () {
  'use strict';
  // Small Chrome 156 interface members measured in interface_members_chrome156.json:
  // HTMLMediaElement constants and `loading`, HTMLScriptElement.supports().
  const g = globalThis, ns = g[Symbol.for('ts')], make = ns.makeNativeFunction;
  const apply = Reflect.apply, define = Object.defineProperty;
  const getAttribute = g.Element.prototype.getAttribute, setAttribute = g.Element.prototype.setAttribute;
  const lower = String.prototype.toLowerCase;
  const string = value => { if (typeof value === 'symbol') throw new TypeError('Cannot convert a Symbol to a string'); return String(value); };
  const media = g.HTMLMediaElement;
  if (media) {
    const constants = [['NETWORK_EMPTY', 0], ['NETWORK_IDLE', 1], ['NETWORK_LOADING', 2], ['NETWORK_NO_SOURCE', 3], ['HAVE_NOTHING', 0], ['HAVE_METADATA', 1], ['HAVE_CURRENT_DATA', 2], ['HAVE_FUTURE_DATA', 3], ['HAVE_ENOUGH_DATA', 4]];
    for (const [name, value] of constants) for (const object of [media, media.prototype]) define(object, name, { value, writable: false, enumerable: true, configurable: false });
    // loading: an enumerated attribute limited to lazy/eager, missing and invalid default eager.
    const brand = ns.domAccessor('nodeType', false, 'HTML:audio|video');
    define(media.prototype, 'loading', {
      enumerable: true, configurable: true,
      get: make(function () { apply(brand, this, []); const value = apply(getAttribute, this, ['loading']); return value !== null && apply(lower, value, []) === 'lazy' ? 'lazy' : 'eager'; }, 'get loading', 0),
      set: make(function (value) { apply(brand, this, []); apply(setAttribute, this, ['loading', string(value)]); }, 'set loading', 1),
    });
  }
  const script = g.HTMLScriptElement;
  if (script) {
    // Script types this engine prepares. Chrome also reports importmap and webbundle; the engine
    // recognises import maps without applying them and has no web bundle loader, so it says so
    // (a feature-detecting page then takes its polyfill path). Speculation rules are advisory:
    // the engine accepts them and, like any UA may, never acts on a speculation.
    const types = ['classic', 'module', 'speculationrules'];
    define(script, 'supports', { value: make(function supports(type) {
      if (!arguments.length) throw new TypeError("Failed to execute 'supports' on 'HTMLScriptElement': 1 argument required, but only 0 present.");
      return types.indexOf(string(type)) >= 0;
    }, 'supports', 1), writable: true, enumerable: true, configurable: true });
  }
})();
