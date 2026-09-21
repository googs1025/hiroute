import assert from 'node:assert/strict';
import test from 'node:test';

import {
  listenerApplyRequest,
  listenerScopeFromAddress,
} from '../src/product/gateway-listener-scope.ts';

test('persisted IPv4 listeners project to the two Desktop choices', () => {
  assert.equal(listenerScopeFromAddress('127.0.0.1'), 'local');
  assert.equal(listenerScopeFromAddress('127.0.0.2'), 'local');
  assert.equal(listenerScopeFromAddress('0.0.0.0'), 'external');
  assert.equal(listenerScopeFromAddress('192.0.2.10'), 'external');
});

test('the external choice explicitly acknowledges exposure without an IP input', () => {
  assert.deepEqual(listenerApplyRequest('local'), {
    scope: 'local',
    accept_remote_risk: false,
  });
  assert.deepEqual(listenerApplyRequest('external'), {
    scope: 'external',
    accept_remote_risk: true,
  });
});
