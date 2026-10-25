// Arrange interface objects and interface prototype objects in Chrome's own-key order (Blink's
// generated bindings: attributes, constants, operations, then `constructor`, with runtime
// enabled members after it). V8 lists string keys in creation order and symbols after them, so
// a key is moved by deleting and redefining it with its unchanged descriptor; the longest
// already-ordered prefix stays. Members Chrome lacks keep their place after the member they
// follow here. Web IDL constants are created configurable during install so they can move; the
// constants Chrome has non-configurable are frozen again here, before any guest code runs.
(function () {
  'use strict';
  const g = globalThis;
  const ownKeys = Reflect.ownKeys, describe = Object.getOwnPropertyDescriptor;
  const remove = Reflect.deleteProperty, defineAll = Object.defineProperties, hasOwn = Object.hasOwn, symbolText = Symbol.prototype.toString, apply = Reflect.apply;
  const label = (key) => typeof key === 'symbol' ? apply(symbolText, key, []) : key;

  // Chrome's order for the keys present here, each followed by the extra keys that follow it.
  function target(keys, order) {
    const known = new Map(), extras = new Map(), leading = [], result = [];
    for (let index = 0; index < order.length; index++) known.set(order[index], index);
    let anchor = null;
    for (const key of keys) {
      const name = label(key);
      if (known.has(name)) { anchor = name; continue; }
      if (anchor === null) leading.push(key);
      else { if (!extras.has(anchor)) extras.set(anchor, []); extras.get(anchor).push(key); }
    }
    const present = new Map();
    for (const key of keys) present.set(label(key), key);
    for (const key of leading) result.push(key);
    for (const name of order) {
      if (!present.has(name) || !known.has(name)) continue;
      result.push(present.get(name));
      for (const key of extras.get(name) || []) result.push(key);
    }
    return result;
  }

  // A constant Chrome has non-configurable is re-added frozen: redefining the attribute of an
  // existing property in place would leave V8 retaining the realm, so it moves with the keys
  // that follow it.
  const frozen = (descriptor, name, fixed) => 'value' in descriptor && typeof descriptor.value === 'number' && descriptor.configurable && fixed.indexOf(name) >= 0;
  function arrange(object, order, fixed) {
    const keys = ownKeys(object), wanted = target(keys, order);
    for (const kind of ['string', 'symbol']) {
      const current = keys.filter((key) => typeof key === kind), desired = wanted.filter((key) => typeof key === kind);
      // Moved keys are appended, so the keys left alone must be a prefix of the desired order
      // that already appears in that order here: keep the longest such prefix.
      const position = new Map();
      for (let index = 0; index < current.length; index++) position.set(current[index], index);
      let kept = 0, last = -1;
      while (kept < desired.length && position.get(desired[kept]) > last && !frozen(describe(object, desired[kept]), label(desired[kept]), fixed)) last = position.get(desired[kept++]);
      // Deleting from a prototype V8 already optimized re-optimizes it each time, so collect
      // the moved keys, delete them last-first and define them back in one call.
      const moved = { __proto__: null };
      const movedKeys = [];
      for (let index = kept; index < desired.length; index++) {
        const key = desired[index], descriptor = describe(object, key);
        if (!descriptor.configurable) continue;
        if (frozen(descriptor, label(key), fixed)) descriptor.configurable = false;
        moved[key] = descriptor;
        movedKeys.push(key);
      }
      for (let index = movedKeys.length - 1; index >= 0; index--) remove(object, movedKeys[index]);
      defineAll(object, moved);
    }
  }

  // entries: [name, interfaceKeys, interfaceFixed, prototypeKeys, prototypeFixed]
  g.__captureKeyOrder = (entries) => {
    for (const [name, interfaceKeys, interfaceFixed, prototypeKeys, prototypeFixed] of entries) {
      const descriptor = describe(g, name), constructor = descriptor && descriptor.value;
      if (typeof constructor !== 'function') continue;
      const prototype = constructor.prototype;
      arrange(constructor, interfaceKeys, interfaceFixed);
      if (prototype && (typeof prototype === 'object' || typeof prototype === 'function') && hasOwn(prototype, 'constructor') && prototype.constructor === constructor) {
        arrange(prototype, prototypeKeys, prototypeFixed);
      }
    }
  };
})();
