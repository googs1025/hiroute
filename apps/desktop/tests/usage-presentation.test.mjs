import assert from 'node:assert/strict';
import test from 'node:test';

import { formatCacheHit, formatCacheHitCoverage, formatTokenCount, usageValue } from '../src/features/usage-presentation.ts';

const cacheHit = (overrides = {}) => ({
  state: 'available',
  ratio_basis_points: 1727,
  cache_read_tokens: 190,
  total_input_tokens: 1100,
  eligible_attempt_count: 2,
  total_attempt_count: 2,
  zero_input_attempt_count: 0,
  missing_attempt_count: 0,
  invalid_attempt_count: 0,
  arithmetic_overflow: false,
  archive_coverage_partial: false,
  coverage: 'complete',
  ...overrides,
});

test('usage presentation preserves zero and missing as different values', () => {
  const metrics = [
    { metric: 'input', known_sum: 0, coverage: 'complete' },
    { metric: 'output', known_sum: null, coverage: 'unknown' },
  ];
  assert.equal(usageValue(metrics, 'input'), 0);
  assert.equal(usageValue(metrics, 'output'), null);
  assert.equal(usageValue(metrics, 'cache_read'), null);
  assert.equal(formatTokenCount(0, 'en'), '0');
  assert.equal(formatTokenCount(null, 'en'), 'Unknown');
});

test('cache-hit presentation uses basis points and explicit unavailable states', () => {
  assert.equal(formatCacheHit(cacheHit(), 'en'), '17.27%');
  assert.equal(formatCacheHitCoverage(cacheHit(), 'en'), '2 / 2 attempts eligible');
  assert.equal(formatCacheHit(cacheHit({ state: 'not_applicable', ratio_basis_points: null }), 'en'), 'N/A (zero input)');
  assert.equal(formatCacheHit(cacheHit({ state: 'unknown', ratio_basis_points: null }), 'en'), 'Unknown');
});
