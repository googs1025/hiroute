import assert from 'node:assert/strict';
import test from 'node:test';

import { firstRealUserText, isInjectedEnvironmentContext } from '../src/features/session-title.ts';

test('only a complete standalone injected environment block is skipped', () => {
  assert.equal(isInjectedEnvironmentContext('  <environment_context>\nrepo facts\n</environment_context>\n'), true);
  assert.equal(isInjectedEnvironmentContext('<environment_context>unfinished'), false);
  assert.equal(isInjectedEnvironmentContext('Please explain <environment_context> in docs'), false);
  assert.equal(isInjectedEnvironmentContext('<environment_context>x</environment_context> keep this'), false);
  assert.equal(isInjectedEnvironmentContext('<environment_context>x</environment_context><environment_context>y</environment_context>'), false);
});

test('title selection skips injected setup and later adopts the first real user input', () => {
  assert.equal(firstRealUserText(['', ' <environment_context>setup</environment_context> ', '\n真实问题\n']), '\n真实问题\n');
  assert.equal(firstRealUserText(['<environment_context>setup</environment_context>']), null);
  assert.equal(firstRealUserText(['How do I write <environment_context>?']), 'How do I write <environment_context>?');
});
