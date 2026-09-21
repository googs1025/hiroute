import test from 'node:test';
import assert from 'node:assert/strict';
import { reconcileSelection } from '../src/ui/selection-order.ts';

test('filtering and reselecting cannot reorder the active route', () => {
  assert.deepEqual(reconcileSelection(['source-b/model', 'source-a/model'], ['source-a/model', 'source-b/model', 'source-c/model']), ['source-b/model', 'source-a/model', 'source-c/model']);
});
test('unavailable identities survive a catalog refresh until explicitly removed', () => {
  assert.deepEqual(reconcileSelection(['removed-binding', 'live-binding'], ['removed-binding', 'live-binding', 'new-binding']), ['removed-binding', 'live-binding', 'new-binding']);
  assert.deepEqual(reconcileSelection(['removed-binding', 'live-binding'], ['live-binding']), ['live-binding']);
});
test('same model names from different sources remain distinct and duplicates cannot enter the route', () => {
  assert.deepEqual(reconcileSelection(['source-a/model'], ['source-b/model', 'source-a/model', 'source-b/model']), ['source-a/model', 'source-b/model']);
});
