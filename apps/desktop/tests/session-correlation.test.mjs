import assert from 'node:assert/strict';
import test from 'node:test';

import { sessionCorrelationLabel } from '../src/features/session-correlation.ts';

test('session correlation provenance has distinct user-facing labels', () => {
  assert.deepEqual(
    [
      'agent_supplied',
      'verified_worker',
      'inferred',
      'request_scoped',
      'unknown',
    ].map(sessionCorrelationLabel),
    ['Agent 提供', '执行关联已验证', '推断关联', '请求级', '关联未知'],
  );
});

test('missing legacy provenance is presented as unknown', () => {
  assert.equal(sessionCorrelationLabel(undefined), '关联未知');
  assert.equal(sessionCorrelationLabel(null), '关联未知');
});
