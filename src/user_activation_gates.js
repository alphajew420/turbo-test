// Activation-gated members: HTMLInputElement/HTMLSelectElement.showPicker(), the fullscreen
// API and its prefixed forms, details toggle events, and the private hook the native close
// watcher manager uses to run dialog cancel/close actions.
//
// The embedder's native activation answers every gate (see user_activation_gates.rs). A
// successful showPicker() consumes activation exactly as Chrome does; no picker UI is
// modeled, so a control never reports an open picker (`:open`).
(function () {
  'use strict';
  const Native = globalThis.__activationNative; delete globalThis.__activationNative;
  const g = globalThis, ns = g[Symbol.for('ts')], make = ns.makeNativeFunction;
  const apply = Reflect.apply, define = Object.defineProperty, ErrorType = TypeError, Exception = g.DOMException;
  const PromiseType = Promise, reject = Promise.reject.bind(Promise), StringValue = String;
  const TRANSIENT = 0, CONSUME = 1, CROSS_ORIGIN = 4, FULLSCREEN = 5;
  const HTML = 'http://www.w3.org/1999/xhtml', SVG = 'http://www.w3.org/2000/svg';
  const accessor = (proto, name) => Object.getOwnPropertyDescriptor(proto, name).get;
  const nodeType = accessor(g.Node.prototype, 'nodeType'), connectedOf = accessor(g.Node.prototype, 'isConnected');
  const namespaceOf = accessor(g.Element.prototype, 'namespaceURI'), localNameOf = accessor(g.Element.prototype, 'localName');
  const getAttribute = g.Element.prototype.getAttribute, hasAttribute = g.Element.prototype.hasAttribute;
  const checkInput = ns.domAccessor('nodeType', false, 'HTML:input'), checkSelect = ns.domAccessor('nodeType', false, 'HTML:select');
  const EventType = g.Event, ToggleEvent = g.ToggleEvent;
  const isElement = value => { try { return apply(nodeType, value, []) === 1; } catch (_) { return false; } };
  const isDocument = value => { try { return apply(nodeType, value, []) === 9; } catch (_) { return false; } };
  const query = (node, kind) => Native('query', node, kind);
  const connected = node => apply(connectedOf, node, []);
  const lower = value => { let result = ''; for (let index = 0; index < value.length; index++) { const code = value.charCodeAt(index); result += code >= 65 && code <= 90 ? StringValue.fromCharCode(code + 32) : value[index]; } return result; };
  const method = (proto, name, body, length) => define(proto, name, { value: make(body, name, length), writable: true, enumerable: true, configurable: true });

  // https://html.spec.whatwg.org/#dom-input-showpicker: Chrome checks the cross-origin
  // frame (except file and color), then mutability, then transient activation.
  const readOnlyTypes = { text: 1, search: 1, url: 1, tel: 1, email: 1, password: 1, date: 1, month: 1, week: 1, time: 1, 'datetime-local': 1, number: 1 };
  const knownTypes = { hidden: 1, text: 1, search: 1, tel: 1, url: 1, email: 1, password: 1, date: 1, month: 1, week: 1, time: 1, 'datetime-local': 1, number: 1, range: 1, color: 1, checkbox: 1, radio: 1, file: 1, submit: 1, image: 1, reset: 1, button: 1 };
  const inputType = node => { const value = lower(apply(getAttribute, node, ['type']) || ''); return knownTypes[value] === 1 ? value : 'text'; };
  const pickerError = (owner, message, name) => new Exception("Failed to execute 'showPicker' on '" + owner + "': " + message, name);
  method(g.HTMLInputElement.prototype, 'showPicker', function () {
    apply(checkInput, this, []);
    const type = inputType(this);
    if (type !== 'file' && type !== 'color' && query(this, CROSS_ORIGIN)) throw pickerError('HTMLInputElement', 'showPicker() called from cross-origin iframe.', 'SecurityError');
    if (Native('disabled', this) || readOnlyTypes[type] === 1 && apply(hasAttribute, this, ['readonly'])) throw pickerError('HTMLInputElement', 'HTMLInputElement::showPicker() cannot be used on immutable controls.', 'InvalidStateError');
    if (!query(this, TRANSIENT)) throw pickerError('HTMLInputElement', 'HTMLInputElement::showPicker() requires user activation.', 'NotAllowedError');
    query(this, CONSUME);
  }, 0);
  method(g.HTMLSelectElement.prototype, 'showPicker', function () {
    apply(checkSelect, this, []);
    if (query(this, CROSS_ORIGIN)) throw pickerError('HTMLSelectElement', 'showPicker() called from cross-origin iframe.', 'SecurityError');
    if (Native('disabled', this)) throw pickerError('HTMLSelectElement', 'showPicker() cannot be used on immutable controls.', 'InvalidStateError');
    if (!query(this, TRANSIENT)) throw pickerError('HTMLSelectElement', 'showPicker() requires user activation.', 'NotAllowedError');
    if (!Native('rendered', this)) throw pickerError('HTMLSelectElement', 'showPicker() requires the select is rendered.', 'NotSupportedError');
    query(this, CONSUME);
  }, 0);

  // ---- Fullscreen: https://fullscreen.spec.whatwg.org/ as Chrome reports it ----
  const fire = (target, type) => Native('dispatch', target, new EventType(type, { __proto__: null, bubbles: true, composed: true }));
  const schedule = (node, callback) => Native('schedule', node, callback);
  const requestPrefix = "Failed to execute 'requestFullscreen' on 'Element': ";
  const navigationUIs = { auto: 1, show: 1, hide: 1 };
  const ScreenType = g.Screen;
  function convertOptions(options) {
    if (options === undefined || options === null) return null;
    if (typeof options !== 'object' && typeof options !== 'function') return "The provided value is not of type 'FullscreenOptions'.";
    const ui = options.navigationUI;
    if (ui !== undefined) {
      if (typeof ui === 'symbol') return "Failed to read the 'navigationUI' property from 'FullscreenOptions': Cannot convert a Symbol value to a string";
      const value = StringValue(ui);
      if (navigationUIs[value] !== 1) return "Failed to read the 'navigationUI' property from 'FullscreenOptions': The provided value '" + value + "' is not a valid enum value of type FullscreenNavigationUI.";
    }
    const screen = options.screen;
    if (screen !== undefined && !(typeof ScreenType === 'function' && screen instanceof ScreenType)) return "Failed to read the 'screen' property from 'FullscreenOptions': Failed to convert value to 'Screen'.";
    return null;
  }
  // https://fullscreen.spec.whatwg.org/#fullscreen-element-ready-check, Chrome's order.
  function readyError(element) {
    if (!connected(element)) return 'Element is not connected';
    const namespace = apply(namespaceOf, element, []), name = apply(localNameOf, element, []);
    if (namespace !== HTML && !(namespace === SVG && name === 'svg')) return 'Element is not an HTML or SVG element';
    if (namespace === HTML && name === 'dialog') return 'Dialog elements are invalid';
    return null;
  }
  function request(element, prefixed, resolve, rejectRequest) {
    let error = readyError(element);
    if (error === null && !query(element, FULLSCREEN)) error = 'Disallowed by permissions policy';
    if (error === null && !query(element, TRANSIENT)) error = 'Permissions check failed';
    const errorType = prefixed ? 'webkitfullscreenerror' : 'fullscreenerror';
    if (error !== null) {
      const target = connected(element) ? element : Native('document', element);
      schedule(element, () => fire(target, errorType));
      rejectRequest(new ErrorType(error));
      return;
    }
    query(element, CONSUME);
    schedule(element, () => {
      const late = readyError(element);
      if (late !== null) {
        const target = connected(element) ? element : Native('document', element);
        schedule(element, () => fire(target, errorType));
        rejectRequest(new ErrorType(late));
        return;
      }
      const changed = Native('fullscreen-push', element, prefixed);
      resolve();
      if (changed) schedule(element, () => fire(element, prefixed ? 'webkitfullscreenchange' : 'fullscreenchange'));
    });
  }
  method(g.Element.prototype, 'requestFullscreen', function (options) {
    if (!isElement(this)) return reject(new ErrorType(requestPrefix + 'Illegal invocation'));
    const invalid = convertOptions(options);
    if (invalid !== null) return reject(new ErrorType(requestPrefix + invalid));
    if (!Native('active', this)) return reject(new ErrorType(requestPrefix + 'Document not active'));
    let settle, refuse;
    const promise = new PromiseType((resolve, rejectPromise) => { settle = resolve; refuse = rejectPromise; });
    request(this, false, () => settle(undefined), refuse);
    return promise;
  }, 0);
  // The prefixed requests report only through webkit-prefixed events and return undefined.
  for (const name of ['webkitRequestFullscreen', 'webkitRequestFullScreen']) method(g.Element.prototype, name, function () {
    if (!isElement(this)) throw new ErrorType('Illegal invocation');
    if (!Native('active', this)) return;
    request(this, true, () => {}, () => {});
  }, 0);

  const documentProto = g.Document.prototype, exitPrefix = "Failed to execute 'exitFullscreen' on 'Document': ";
  const top = document => Native('fullscreen-top', document);
  function exit(document, resolve) {
    schedule(document, () => {
      const exited = Native('fullscreen-pop', document);
      resolve();
      if (exited === null) return;
      const element = exited[0], type = exited[1] ? 'webkitfullscreenchange' : 'fullscreenchange';
      schedule(document, () => fire(connected(element) ? element : document, type));
    });
  }
  method(documentProto, 'exitFullscreen', function () {
    if (!isDocument(this)) return reject(new ErrorType(exitPrefix + 'Illegal invocation'));
    if (!Native('active', this) || top(this) === null) return reject(new ErrorType(exitPrefix + 'Document not active'));
    const document = this;
    return new PromiseType(resolve => exit(document, () => resolve(undefined)));
  }, 0);
  // The prefixed forms run the same exit and swallow its refusal.
  for (const name of ['webkitExitFullscreen', 'webkitCancelFullScreen']) method(documentProto, name, function () {
    if (!isDocument(this)) throw new ErrorType('Illegal invocation');
    if (Native('active', this) && top(this) !== null) exit(this, () => {});
  }, 0);
  // [LegacyLenientSetter]: a branded no-op, so assignment never shadows the getter.
  const lenient = (name, read) => define(documentProto, name, {
    get: make(function () { if (!isDocument(this)) throw new ErrorType('Illegal invocation'); return read(this); }, 'get ' + name, 0),
    set: make(function () { if (!isDocument(this)) throw new ErrorType('Illegal invocation'); }, 'set ' + name, 1),
    enumerable: true, configurable: true,
  });
  // The fullscreen element retargeted against the document (out of shadow trees).
  const rootOf = g.Node.prototype.getRootNode, hostOf = accessor(g.ShadowRoot.prototype, 'host');
  const retarget = element => { for (let root = apply(rootOf, element, []); apply(nodeType, root, []) === 11; root = apply(rootOf, element, [])) element = apply(hostOf, root, []); return element; };
  lenient('fullscreen', document => top(document) !== null);
  lenient('fullscreenElement', document => { const element = top(document); return element === null ? null : retarget(element); });
  ns.fullscreenElementOf = top;

  // HTMLDetailsElement.open reflects the boolean content attribute.
  const checkDetails = ns.domAccessor('nodeType', false, 'HTML:details');
  const setAttributeOf = g.Element.prototype.setAttribute, removeAttributeOf = g.Element.prototype.removeAttribute;
  if (!Object.getOwnPropertyDescriptor(g.HTMLDetailsElement.prototype, 'open')) define(g.HTMLDetailsElement.prototype, 'open', {
    get: make(function () { apply(checkDetails, this, []); return apply(hasAttribute, this, ['open']); }, 'get open', 0),
    set: make(function (value) { apply(checkDetails, this, []); if (value) apply(setAttributeOf, this, ['open', '']); else apply(removeAttributeOf, this, ['open']); }, 'set open', 1),
    enumerable: true, configurable: true,
  });
  // ---- Details toggle event: https://html.spec.whatwg.org/#queue-a-details-toggle-event-task ----
  const toggle = (details, oldOpen, task) => schedule(details, () => {
    if (!Native('details-task', details, task)) return;
    const open = apply(hasAttribute, details, ['open']);
    Native('dispatch', details, new ToggleEvent('toggle', { __proto__: null, oldState: oldOpen ? 'open' : 'closed', newState: open ? 'open' : 'closed' }));
  });

  const dialogWatcher = ns.dialogCloseWatcher, invokerTarget = ns.invokerPopoverTarget;
  delete ns.dialogCloseWatcher; delete ns.invokerPopoverTarget;
  g.__captureActivationHook = function (operation, a, b, c) {
    if (operation === 'details-toggle') return toggle(a, b, c);
    if (operation === 'fullscreen-removed') return schedule(a, () => fire(a, b ? 'webkitfullscreenchange' : 'fullscreenchange'));
    if (operation === 'dialog-cancel') return dialogWatcher(a, 'cancel', b);
    if (operation === 'dialog-close') return dialogWatcher(a, 'close');
    if (operation === 'invoker-target') return invokerTarget(a);
    return undefined;
  };
})();
