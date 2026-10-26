import { test, expect } from 'vitest';

// Addons compiled by tests/napi_host.rs next to this file (sources in tests/napi_support/).
test('an init returning a value replaces the exports object', () => {
  const m = require('./init_custom.node');
  expect(m).toEqual({ kind: 'custom' });
  expect(m.onExports).toBe(undefined);
});

test('a JS exception thrown during init is a catchable require() error', () => {
  expect(() => require('./init_throw.node')).toThrow(
    /^native addon \/.*\/init_throw\.node: native addon threw during module init: Error: init exploded$/,
  );
  // not cached as a success: requiring again fails the same way
  expect(() => require('./init_throw.node')).toThrow(/init exploded/);
});

test('an unsupported entrypoint called during init is a catchable require() error', () => {
  expect(() => require('./init_wrap.node')).toThrow(
    'native addon threw during module init: Error: turbo-test: N-API napi_wrap is not implemented (native addon called an unsupported entrypoint)',
  );
});

test('a shared library without napi_register_module_v1 is rejected', () => {
  expect(() => require('./noreg.node')).toThrow(
    /^native addon \/.*\/noreg\.node: no napi_register_module_v1: /,
  );
});

test('a file that fails to dlopen is rejected', () => {
  expect(() => require('./garbage.node')).toThrow(/^native addon \/.*\/garbage\.node: dlopen /);
});
