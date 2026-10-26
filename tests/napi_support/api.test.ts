import { describe, test, expect } from 'vitest';

// api.node (tests/napi_support/api.c) is compiled by tests/napi_host.rs next to this file. The
// expected values are Node 26's: the same spec passes against Node's own Node-API.
const api = require('./api.node');

const OK = 0;
const INVALID_ARG = 1;
const STRING_EXPECTED = 3;
const NUMBER_EXPECTED = 6;
const PENDING_EXCEPTION = 10;

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

describe('a NULL env', () => {
  test('is napi_invalid_arg for every entrypoint', () => {
    const statuses = api.nullEnv();
    expect(statuses.length).toBeGreaterThan(0);
    expect(statuses).toEqual(Array(statuses.length).fill(INVALID_ARG));
  });
});
