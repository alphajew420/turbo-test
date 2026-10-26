import { test, expect } from 'vitest';

const a = require('./full.node');

// V8 strings are capped at String::kMaxLength (< 2^29): napi_create_string_utf8 and the
// named-property key conversion must report napi_generic_failure instead of crashing.
test('strings / keys over String::kMaxLength fail with napi_generic_failure', () => {
  expect(a.huge()).toEqual([9, 9, 9, true, true]);
});
