import { describe, test, expect } from 'vitest';

// full.node (tests/napi_support/full.c) is compiled by tests/napi_host.rs next to this file.
const a = require('./full.node');

// napi_status codes as src/napi_host.rs defines them.
const OK = 0;
const INVALID_ARG = 1;
const GENERIC_FAILURE = 9;
const PENDING_EXCEPTION = 10;

describe('module init', () => {
  test('an init returning NULL hands back the exports object it was given', () => {
    expect(typeof a.basics).toBe('function');
    expect(typeof a.Cls).toBe('function');
    // cached: a second require() returns the same exports
    expect(require('./full.node')).toBe(a);
  });
});

describe('values', () => {
  test('getters and creators', () => {
    const [u, n, g, t, f, max, d, utf, cut, obj, arr, empty, acc] = a.basics();
    expect(u).toBe(undefined);
    expect(n).toBe(null);
    expect(g).toBe(globalThis);
    expect(t).toBe(true);
    expect(f).toBe(false);
    expect(max).toBe(4294967295);
    expect(d).toBe(-1.5);
    expect(utf).toBe('héllo wörld €');
    expect(cut).toBe('abc');
    expect(obj).toEqual({});
    expect(Object.getPrototypeOf(obj)).toBe(Object.prototype);
    expect(Array.isArray(arr)).toBe(true);
    expect(arr.length).toBe(3);
    expect(empty).toBe('');
    expect(acc).toBe(OK);
  });

  test('napi_typeof', () => {
    expect(a.typeOf(undefined)).toBe(0);
    expect(a.typeOf(null)).toBe(1);
    expect(a.typeOf(false)).toBe(2);
    expect(a.typeOf(3.5)).toBe(3);
    expect(a.typeOf('s')).toBe(4);
    expect(a.typeOf({})).toBe(6);
    expect(a.typeOf([1])).toBe(6);
    expect(a.typeOf(() => 1)).toBe(7);
    expect(a.typeOf(a.basics)).toBe(7);
    expect(a.status()).toBe(OK);
    // Symbol / BigInt: Node reports 5 / 9; the host has no branch for them (reported
    // separately), so only assert the call succeeds with a non-primitive-kind code.
    for (const v of [Symbol('s'), BigInt(10)]) {
      expect(a.typeOf(v)).toBeGreaterThanOrEqual(5);
      expect(a.status()).toBe(OK);
    }
  });

  test('napi_get_value_string_utf8: length query, full copy, truncation, NULL result', () => {
    expect(a.strInfo('hello world')).toEqual([11, 'hello world', 'hel', 3, 'hello w', OK]);
    expect(a.strInfo('héllo')).toEqual([6, 'héllo', 'hé', 3, 'héllo', OK]);
    expect(a.strInfo('')).toEqual([0, '', '', 0, '', OK]);
  });

  test('napi_coerce_to_string', () => {
    expect(a.coerce(42)).toBe('42');
    expect(a.coerce(true)).toBe('true');
    expect(a.coerce({ toString: () => 'custom' })).toBe('custom');
    expect(a.status()).toBe(OK);
    expect(() =>
      a.coerce({
        toString() {
          throw new Error('toString threw');
        },
      }),
    ).toThrow('toString threw');
    expect(a.status()).toBe(GENERIC_FAILURE);
    expect(() => a.coerce(Symbol('x'))).toThrow(TypeError);
    expect(a.status()).toBe(GENERIC_FAILURE);
  });

  test('number / bool getters', () => {
    expect(a.num(7.9)).toEqual([7, 7.9, 7, true, OK]);
    expect(a.num(0)).toEqual([0, 0, 0, false, OK]);
    expect(a.num(4294967295)).toEqual([4294967295, 4294967295, 4294967295, true, OK]);
    expect(a.num(NaN)).toEqual([0, NaN, 0, false, OK]);
    expect(a.num(-3.5).slice(1)).toEqual([-3.5, -3, true, OK]);
    expect(a.num(1e10).slice(1)).toEqual([1e10, 1e10, true, OK]);
  });
});

describe('objects and arrays', () => {
  test('named properties', () => {
    const o: Record<string, unknown> = {};
    expect(a.setNamed(o, 'k', 5)).toBe(OK);
    expect(o.k).toBe(5);
    expect(a.getNamed(o, 'k')).toBe(5);
    expect(a.getNamed(o, 'missing')).toBe(undefined);
    expect(a.status()).toBe(OK);
    expect(a.setNamed(null, 'k', 1)).toBe(INVALID_ARG);
    expect(a.getNamed(undefined, 'k')).toBe(undefined);
    expect(a.status()).toBe(INVALID_ARG);
    const trap = Object.defineProperty({}, 'boom', {
      get() {
        throw new Error('getter threw');
      },
    });
    expect(() => a.getNamed(trap, 'boom')).toThrow('getter threw');
    expect(a.status()).toBe(GENERIC_FAILURE);
  });

  test('elements, napi_is_array, napi_get_array_length', () => {
    const arr: unknown[] = [];
    expect(a.setEl(arr, 2, 'x')).toBe(OK);
    expect(arr.length).toBe(3);
    expect(arr[2]).toBe('x');
    expect(a.getEl(arr, 2)).toBe('x');
    expect(a.getEl(['a', 'b'], 5)).toBe(undefined);
    expect(a.status()).toBe(OK);
    expect(a.setEl(undefined, 0, 1)).toBe(INVALID_ARG);
    expect(a.getEl(null, 0)).toBe(undefined);
    expect(a.status()).toBe(INVALID_ARG);
    const trap = Object.defineProperty([], 0, {
      get() {
        throw new Error('index getter threw');
      },
    });
    expect(() => a.getEl(trap, 0)).toThrow('index getter threw');
    expect(a.status()).toBe(GENERIC_FAILURE);

    expect(a.isArray([1])).toBe(true);
    expect(a.isArray({ length: 1 })).toBe(false);
    expect(a.arrLen([1, 2, 3])).toBe(3);
    expect(a.arrLen([])).toBe(0);
    expect(a.arrLen({ length: 2 })).toBe(undefined);
    expect(a.status()).toBe(INVALID_ARG);
  });

  test('napi_get_property_names', () => {
    expect(a.keys({ x: 1, y: 2 })).toEqual(['x', 'y']);
    expect(a.status()).toBe(OK);
    expect(a.keys(null)).toBe(undefined);
    expect(a.status()).toBe(INVALID_ARG);
    const p = new Proxy(
      {},
      {
        ownKeys() {
          throw new Error('ownKeys threw');
        },
      },
    );
    expect(() => a.keys(p)).toThrow('ownKeys threw');
    expect(a.status()).toBe(GENERIC_FAILURE);
  });
});

describe('functions', () => {
  test('napi_create_function + napi_get_cb_info (args, this, data)', () => {
    const f = a.makeFn(42);
    expect(a.status()).toBe(OK);
    expect(typeof f).toBe('function');
    const recv = { me: 1 };
    const r = f.call(recv, 'x', 2);
    expect(r[1]).toBe(recv);
    expect(r).toEqual([2, recv, 42, 'x', 2]);
    // more args than the addon's argv capacity (8): argc still reports the real count
    const many = f(1, 2, 3, 4, 5, 6, 7, 8, 9, 10);
    expect(many[0]).toBe(10);
    expect(many.slice(3)).toEqual([1, 2, 3, 4]);
    expect(f()[0]).toBe(0);
    // each created function keeps its own data pointer
    expect(a.makeFn(7)()[2]).toBe(7);
    expect(f()[2]).toBe(42);
    // plain call: V8 hands API callbacks the global receiver
    expect(f()[1]).toBe(globalThis);
  });

  test('napi_get_cb_info with NULL argv / NULL argc', () => {
    expect(a.argcOnly(1, 2, 3)).toBe(3);
    expect(a.argcOnly()).toBe(0);
    const self = {};
    expect(a.thisOnly.call(self)).toBe(self);
    expect(a.status()).toBe(OK);
  });

  test('a callback returning NULL yields undefined', () => {
    expect(a.retNull(1)).toBe(undefined);
  });

  test('napi_define_class: the constructor runs with this + data', () => {
    const inst = new a.Cls('v1');
    expect(inst).toBeInstanceOf(a.Cls);
    expect(inst.value).toBe('v1');
    expect(inst.tag).toBe(7);
  });

  test('napi_call_function', () => {
    expect(a.call((x: number, y: number) => x + y, null, 2, 3)).toBe(5);
    expect(a.status()).toBe(OK);
    const obj = {
      v: 7,
      get(this: { v: number }) {
        return this.v;
      },
    };
    expect(a.call(obj.get, obj)).toBe(7);
    expect(a.call(() => 'no args', undefined)).toBe('no args');
    expect(
      a.callNullRecv(function (this: unknown) {
        'use strict';
        return [this, [...arguments]];
      }, 'p', 'q'),
    ).toEqual([undefined, ['p', 'q']]);
    expect(a.status()).toBe(OK);
    expect(a.call(123, null)).toBe(undefined);
    expect(a.status()).toBe(INVALID_ARG);
    expect(() =>
      a.call(() => {
        throw new RangeError('from js');
      }, null),
    ).toThrow(RangeError);
    expect(a.status()).toBe(PENDING_EXCEPTION);
  });
});

describe('arraybuffers, typed arrays, buffers', () => {
  test('napi_create_arraybuffer', () => {
    const ab = a.ab(4);
    expect(ab).toBeInstanceOf(ArrayBuffer);
    expect([...new Uint8Array(ab)]).toEqual([1, 2, 3, 4]);
    expect(a.ab(0).byteLength).toBe(0);
    expect(a.abNoData(3).byteLength).toBe(3);
    expect(a.status()).toBe(OK);
  });

  test('napi_create_typedarray: every type code', () => {
    const buf = new ArrayBuffer(32);
    const names = [
      'Int8Array',
      'Uint8Array',
      'Uint8ClampedArray',
      'Int16Array',
      'Uint16Array',
      'Int32Array',
      'Uint32Array',
      'Float32Array',
      'Float64Array',
      'BigInt64Array',
      'BigUint64Array',
    ];
    names.forEach((name, type) => {
      const ta = a.ta(type, 2, buf, 8);
      expect(a.status()).toBe(OK);
      expect(ta.constructor.name).toBe(name);
      expect(ta.length).toBe(2);
      expect(ta.byteOffset).toBe(8);
      expect(ta.buffer).toBe(buf);
    });
    // Host-specific leniency (Node returns napi_invalid_arg): an unknown code builds a Uint8Array.
    expect(a.ta(99, 4, buf, 0).constructor.name).toBe('Uint8Array');
    expect(a.ta(1, 1, {}, 0)).toBe(undefined);
    expect(a.status()).toBe(INVALID_ARG);
  });

  test('napi_create_external_arraybuffer copies the external bytes', () => {
    expect([...new Uint8Array(a.extAb(3))]).toEqual([120, 121, 122]);
    expect(a.extAb(0).byteLength).toBe(0);
    expect(a.status()).toBe(OK);
  });

  test('napi_create_buffer / _copy / _external', () => {
    const b = a.buf(3);
    expect(b).toBeInstanceOf(Uint8Array);
    expect([...b]).toEqual([0, 2, 4]);
    expect(a.buf(0).length).toBe(0);
    expect(a.bufNoData(2).length).toBe(2);

    const [c, hadData] = a.bufCopy('hello', 5);
    expect(c).toBeInstanceOf(Uint8Array);
    // result_data aliases the new buffer: the addon poked 'Z' through it
    expect(String.fromCharCode(...c)).toBe('Zello');
    expect(hadData).toBe(true);
    const [c0, hadData0] = a.bufCopy('', 0);
    expect(c0.length).toBe(0);
    expect(hadData0).toBe(false);
    expect([...a.bufCopyNull(3)]).toEqual([0, 0, 0]);
    expect(String.fromCharCode(...a.bufCopyNoRes('abc'))).toBe('abc');

    expect(String.fromCharCode(...a.extBuf(4))).toBe('exte');
    expect(a.extBuf(0).length).toBe(0);
    expect([...a.extBufNull(2)]).toEqual([0, 0]);
    expect(a.status()).toBe(OK);
  });

  test('napi_get_buffer_info: views (with offset), ArrayBuffers, non-buffers', () => {
    const u = new Uint8Array([9, 8, 7, 6]);
    expect(a.bufInfo(u.subarray(1))).toEqual([3, 8]);
    expect([...u]).toEqual([9, 9, 7, 6]); // data points at the view's first byte
    expect(a.bufInfo(new DataView(u.buffer, 2, 2))).toEqual([2, 7]);
    expect(u[2]).toBe(8);
    const ab = new Uint8Array([5]).buffer;
    expect(a.bufInfo(ab)).toEqual([1, 5]);
    expect(new Uint8Array(ab)[0]).toBe(6);
    expect(a.bufInfo(new Uint8Array(0))).toEqual([0, -1]);
    expect(a.bufInfo(new ArrayBuffer(0))).toEqual([0, -1]);
    expect(a.status()).toBe(OK);
    expect(a.bufInfo('nope')).toBe(undefined);
    expect(a.status()).toBe(INVALID_ARG);
    expect(a.bufInfoNulls(u)).toBe(OK);
    expect(a.bufInfoNulls(new ArrayBuffer(1))).toBe(OK);
  });
});

describe('unsupported entrypoints throw catchable errors', () => {
  test('napi_wrap / napi_unwrap / napi_new_instance', () => {
    expect(() => a.wrap({})).toThrow(
      'turbo-test: N-API napi_wrap is not implemented (native addon called an unsupported entrypoint)',
    );
    expect(a.status()).toBe(PENDING_EXCEPTION);
    expect(() => a.unwrap({})).toThrow(/N-API napi_unwrap is not implemented/);
    expect(a.status()).toBe(PENDING_EXCEPTION);
    expect(() => a.newInstance(class {})).toThrow(/N-API napi_new_instance is not implemented/);
    expect(a.status()).toBe(PENDING_EXCEPTION);
    let caught: unknown;
    try {
      a.wrap({});
    } catch (e) {
      caught = e;
    }
    expect(caught).toBeInstanceOf(Error);
  });
});

describe('errors and exceptions', () => {
  test('napi_throw passes the value through unchanged', () => {
    let caught: unknown = 'nothing';
    try {
      a.throwVal(42);
    } catch (e) {
      caught = e;
    }
    expect(caught).toBe(42);
    expect(a.status()).toBe(OK);
    const err = new TypeError('mine');
    expect(() => a.throwVal(err)).toThrow(err);
  });

  test('napi_throw_error throws an Error with the message', () => {
    let caught: any;
    try {
      a.throwErr('native boom');
    } catch (e) {
      caught = e;
    }
    expect(caught).toBeInstanceOf(Error);
    expect(caught.message).toBe('native boom');
    expect(a.status()).toBe(OK);
  });

  test('napi_create_error / napi_is_error', () => {
    const e = a.createErr('made');
    expect(a.status()).toBe(OK);
    expect(e).toBeInstanceOf(Error);
    expect(e.message).toBe('made');
    expect(a.createErr(5)).toBe(undefined);
    expect(a.status()).toBe(INVALID_ARG);
    expect(a.isError(new Error('x'))).toBe(true);
    expect(a.isError(new RangeError('x'))).toBe(true);
    expect(a.isError(e)).toBe(true);
    expect(a.isError({ message: 'x' })).toBe(false);
    expect(a.isError('x')).toBe(false);
  });

  test('no exception pending outside an error', () => {
    expect(a.pending()).toEqual([false, undefined, OK]);
  });
});

describe('references', () => {
  test('create / get / unref / delete', () => {
    const target = { id: 1 };
    const id = a.ref(target);
    expect(a.status()).toBe(OK);
    expect(id).toBeGreaterThan(0);
    expect(a.deref(id)).toBe(target);
    const id2 = a.ref('second');
    expect(id2).not.toBe(id);
    expect(a.deref(id2)).toBe('second');
    expect(a.refUnref(id)).toBe(OK);
    expect(a.deref(id)).toBe(target);
    expect(a.delRef(id)).toBe(OK);
    expect(a.deref(id)).toBe(undefined);
    expect(a.status()).toBe(OK);
    expect(a.deref(id2)).toBe('second');
  });

  test('unknown references', () => {
    expect(a.deref(0)).toBe(undefined);
    expect(a.status()).toBe(INVALID_ARG);
    expect(a.deref(1e6)).toBe(undefined);
    expect(a.status()).toBe(INVALID_ARG);
    expect(a.delRef(0)).toBe(OK);
    expect(a.delRef(1e6)).toBe(OK);
  });
});

describe('stubs', () => {
  test('cleanup hooks, napi_stub, threadsafe functions', () => {
    expect(a.stubs()).toEqual([OK, GENERIC_FAILURE, OK, true, OK, OK]);
  });
});
