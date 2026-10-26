import { describe, test, expect } from 'vitest';

// api.node (tests/napi_support/api.c) is compiled by tests/napi_host.rs next to this file. The
// expected values are Node 26's: the same spec passes against Node's own Node-API.
const api = require('./api.node');

const OK = 0;
const INVALID_ARG = 1;
const STRING_EXPECTED = 3;
const NUMBER_EXPECTED = 6;
const PENDING_EXCEPTION = 10;
const BIGINT_EXPECTED = 17;
const DATE_EXPECTED = 18;

// what `f` throws
function thrown(f: () => unknown): any {
  try {
    f();
  } catch (e) {
    return e;
  }
  throw new Error('did not throw');
}

describe('errors', () => {
  const CLASSES = [Error, TypeError, RangeError, SyntaxError];

  test('napi_throw_error / _type_error / _range_error / node_api_throw_syntax_error', () => {
    CLASSES.forEach((Cls, kind) => {
      const e = thrown(() => api.throwKind(kind, 'E_CODE', `kind ${kind}`));
      expect(e).toBeInstanceOf(Cls);
      expect(e.constructor).toBe(Cls);
      expect(e.message).toBe(`kind ${kind}`);
      expect(e.code).toBe('E_CODE');
      const plain = thrown(() => api.throwKind(kind, undefined, 'no code'));
      expect(plain).toBeInstanceOf(Cls);
      expect('code' in plain).toBe(false);
    });
  });

  test('napi_create_error / _type_error / _range_error / node_api_create_syntax_error', () => {
    CLASSES.forEach((Cls, kind) => {
      const [st, e] = api.createKind(kind, 'E_CODE', 'made');
      expect(st).toBe(OK);
      expect(e.constructor).toBe(Cls);
      expect([e.message, e.code]).toEqual(['made', 'E_CODE']);
      const [st2, e2] = api.createKind(kind, undefined, 'plain');
      expect(st2).toBe(OK);
      expect('code' in e2).toBe(false);
      // the message and the code must be strings
      expect(api.createKind(kind, undefined, 5)).toEqual([STRING_EXPECTED, undefined]);
      expect(api.createKind(kind, 5, 'm')).toEqual([STRING_EXPECTED, undefined]);
    });
  });

  test('a NULL message or result is napi_invalid_arg', () => {
    const [statuses, last] = api.errorChecks();
    expect(statuses).toEqual(Array(12).fill(INVALID_ARG));
    expect(last).toEqual([INVALID_ARG, 'Invalid argument', 0]);
  });

  test('napi_get_last_error_info reports the status of the last call', () => {
    expect(api.lastErrorInfo('not a number')).toEqual([
      [NUMBER_EXPECTED, 'A number was expected', 0],
      [NUMBER_EXPECTED, 'A number was expected', 0],
      [OK, undefined, 0],
      INVALID_ARG,
      [INVALID_ARG, 'Invalid argument', 0],
    ]);
  });

  test('napi_fatal_exception reports the error as uncaught', () => {
    const seen: string[] = [];
    const onUncaught = (e: Error) => seen.push(e.message);
    (globalThis as any).process.on('uncaughtException', onUncaught);
    try {
      expect(api.fatalException(new Error('reported as uncaught'))).toBe(OK);
      expect(api.fatalException(undefined)).toBe(INVALID_ARG);
    } finally {
      (globalThis as any).process.off('uncaughtException', onUncaught);
    }
  });
});

describe('numbers', () => {
  test('napi_create_int32 / napi_create_int64 (a Number, the nearest double)', () => {
    const [i32, i64, st] = api.numbers();
    expect(i32).toEqual([0, -1, -2147483648, 2147483647]);
    expect(i64).toEqual([0, -7, 9007199254740992, -9223372036854775808, 9223372036854775807]);
    expect(st).toEqual([INVALID_ARG, INVALID_ARG]);
  });

  test('napi_get_value_int32 is ToInt32 of a number', () => {
    const cases: [unknown, number[]][] = [
      [7, [OK, 7]],
      [-1.9, [OK, -1]],
      [1.9, [OK, 1]],
      [2 ** 31, [OK, -(2 ** 31)]],
      [2 ** 32 + 5, [OK, 5]],
      [1e20, [OK, 1661992960]],
      [-0, [OK, 0]],
      [NaN, [OK, 0]],
      [Infinity, [OK, 0]],
      ['1', [NUMBER_EXPECTED, -99]],
      [1n, [NUMBER_EXPECTED, -99]],
    ];
    for (const [v, want] of cases) expect(api.getInt32(v)).toEqual(want);
  });
});

describe('bigints', () => {
  test('napi_create_bigint_int64 / _uint64 / _words', () => {
    const [i, u, w, st, exc] = api.bigints();
    expect(i).toEqual([0n, -1n, -(2n ** 63n), 2n ** 63n - 1n]);
    expect(u).toEqual([0n, 1n, 2n ** 64n - 1n]);
    expect(w).toEqual([2n * 2n ** 64n + 1n, -(2n * 2n ** 64n + 1n), 0n, 1n]);
    // NULL words / result, a count over INT_MAX, over V8's maximum size (a RangeError), with an
    // exception pending, then a creation that runs no JS
    expect(st).toEqual([INVALID_ARG, INVALID_ARG, INVALID_ARG, INVALID_ARG, INVALID_ARG,
      PENDING_EXCEPTION, PENDING_EXCEPTION, OK]);
    expect(exc).toBeInstanceOf(RangeError);
  });

  test('napi_get_value_bigint_int64 / _uint64 report whether the value fit', () => {
    expect(api.getBigInt64(-5n)).toEqual([OK, -5n, true]);
    expect(api.getBigInt64(2n ** 63n)).toEqual([OK, -(2n ** 63n), false]);
    expect(api.getBigInt64(2n ** 64n + 3n)).toEqual([OK, 3n, false]);
    expect(api.getBigUint64(2n ** 64n - 1n)).toEqual([OK, 2n ** 64n - 1n, true]);
    expect(api.getBigUint64(-1n)).toEqual([OK, 2n ** 64n - 1n, false]);
    expect(api.getBigInt64(5)).toEqual([BIGINT_EXPECTED, 0n, false]);
  });

  test('napi_get_value_bigint_words', () => {
    const big = -(3n * 2n ** 128n + 2n * 2n ** 64n + 1n);
    expect(api.getWords(big, -1)).toEqual([OK, -1, 3, 0n, 0n, 0n, 0n]);
    expect(api.getWords(big, 4)).toEqual([OK, 1, 3, 1n, 2n, 3n, 0n]);
    // a buffer too small takes the low words; the count is the BigInt's
    expect(api.getWords(big, 2)).toEqual([OK, 1, 3, 1n, 2n, 0n, 0n]);
    expect(api.getWords(5n, 4)).toEqual([OK, 0, 1, 5n, 0n, 0n, 0n]);
    expect(api.getWords(0n, 4)).toEqual([OK, 0, 0, 0n, 0n, 0n, 0n]);
    expect(api.getWords(0n, -1)).toEqual([OK, -1, 0, 0n, 0n, 0n, 0n]);
  });

  test('argument checks', () => {
    expect(api.bigintChecks(1n)).toEqual([
      INVALID_ARG, INVALID_ARG, INVALID_ARG, INVALID_ARG, INVALID_ARG, INVALID_ARG,
      INVALID_ARG, INVALID_ARG, INVALID_ARG, BIGINT_EXPECTED, BIGINT_EXPECTED, BIGINT_EXPECTED,
      // the word count is checked before the type
      INVALID_ARG,
    ]);
  });
});

describe('dates', () => {
  test('napi_create_date / napi_is_date / napi_get_date_value', () => {
    const [d, isDate, otherIs, value, st] = api.dates(1700000000123.9, {});
    expect(d).toBeInstanceOf(Date);
    expect(d.getTime()).toBe(1700000000123);
    expect([isDate, otherIs, value]).toEqual([true, false, 1700000000123]);
    // created, value, a non-date, NULL value / result, napi_is_date NULL value / result,
    // napi_create_date NULL result
    expect(st).toEqual([OK, OK, DATE_EXPECTED, INVALID_ARG, INVALID_ARG, INVALID_ARG,
      INVALID_ARG, INVALID_ARG]);
    // TimeClip: out of range is an invalid date
    const [bad, , , badValue] = api.dates(8.64e15 + 1, new Date(0));
    expect(Number.isNaN(bad.getTime())).toBe(true);
    expect(Number.isNaN(badValue)).toBe(true);
    expect(api.dates(0, new Date(0))[2]).toBe(true);
  });
});

describe('a NULL env', () => {
  test('is napi_invalid_arg for every entrypoint', () => {
    const statuses = api.nullEnv();
    expect(statuses.length).toBeGreaterThan(0);
    expect(statuses).toEqual(Array(statuses.length).fill(INVALID_ARG));
  });
});
