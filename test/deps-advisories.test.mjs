// Dependency advisory guard (turbo-test#36). esbuild <= 0.24.2 is affected by
// GHSA-67mh-4wv8-2f99 (its `serve` dev server answers cross-origin requests). turbo-test never
// starts that server, but consumers' `npm audit` flags the runtime dependency, so both the
// declared range and the locked/installed version must stay above the affected range.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import fs from 'node:fs';
import path from 'node:path';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const readJson = (p) => JSON.parse(fs.readFileSync(path.join(ROOT, p), 'utf8'));

// Lowest esbuild release outside GHSA-67mh-4wv8-2f99's affected range (<= 0.24.2).
const FIXED = [0, 25, 0];

function atLeastFixed(version) {
  const m = /^(\d+)\.(\d+)\.(\d+)/.exec(version);
  assert.ok(m, `parsable version: ${version}`);
  const v = m.slice(1).map(Number);
  for (let i = 0; i < 3; i++) {
    if (v[i] !== FIXED[i]) return v[i] > FIXED[i];
  }
  return true;
}

test('esbuild dependency is outside GHSA-67mh-4wv8-2f99 (declared, locked, installed)', () => {
  const declared = readJson('package.json').dependencies.esbuild;
  assert.match(declared, /^\^\d+\.\d+\.\d+$/, 'esbuild is declared as a caret range');
  assert.ok(atLeastFixed(declared.slice(1)), `declared range ${declared} admits an affected esbuild`);

  const locked = readJson('package-lock.json').packages['node_modules/esbuild'].version;
  assert.ok(atLeastFixed(locked), `package-lock.json pins affected esbuild ${locked}`);
  for (const [key, entry] of Object.entries(readJson('package-lock.json').packages)) {
    if (key.startsWith('node_modules/@esbuild/')) {
      assert.equal(entry.version, locked, `${key} matches the locked esbuild`);
    }
  }

  const installed = path.join(ROOT, 'node_modules', 'esbuild', 'package.json');
  if (fs.existsSync(installed)) {
    const v = JSON.parse(fs.readFileSync(installed, 'utf8')).version;
    assert.equal(v, locked, 'installed esbuild matches the lock (run `npm ci`)');
  }
});
