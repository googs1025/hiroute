import assert from 'node:assert/strict';
import test from 'node:test';
import {
  recommendedWorkerDependencies,
  workerDependencySelectionMatches,
  workerDependencySelectionState,
  workerEnvelopeData,
} from '../src/features/worker-dependencies-state.ts';

function view(overrides = {}) {
  return {
    schema: 'hiroute.worker-dependencies-view/v1',
    selection_revisions: [{ harness: 'codex_cli', revision: 4 }],
    candidates: [
      { harness: 'codex_cli', component: 'cli', path: '/bin/codex', source: 'path', state: 'found' },
      { harness: 'codex_cli', component: 'adapter', path: '/bin/codex-acp', source: 'path', state: 'found' },
      { harness: 'codex_cli', component: 'node', path: '/bin/node', source: 'path', state: 'found' },
    ],
    selected: [],
    install_hints: [],
    ...overrides,
  };
}

test('detection recommends but never invents a persisted selection', () => {
  const detected = view();
  assert.deepEqual(recommendedWorkerDependencies(detected, 'codex_cli'), {
    harness: 'codex_cli',
    cli_path: '/bin/codex',
    adapter_path: '/bin/codex-acp',
    node_path: '/bin/node',
    expected_selection_revision: 4,
  });
  assert.equal(workerDependencySelectionState(detected, 'codex_cli'), 'found');
  assert.deepEqual(detected.selected, []);
});

test('an explicit selected path remains authoritative even when another candidate is found', () => {
  const selected = {
    harness: 'codex_cli',
    cli_path: '/old/codex',
    adapter_path: '/old/codex-acp',
    node_path: '/old/node',
  };
  const detected = view({
    selected: [selected],
    candidates: [
      { harness: 'codex_cli', component: 'cli', path: '/old/codex', source: 'selected', state: 'missing' },
      { harness: 'codex_cli', component: 'adapter', path: '/old/codex-acp', source: 'selected', state: 'missing' },
      { harness: 'codex_cli', component: 'node', path: '/old/node', source: 'selected', state: 'missing' },
      ...view().candidates,
    ],
  });
  assert.equal(recommendedWorkerDependencies(detected, 'codex_cli').cli_path, '/old/codex');
  assert.equal(workerDependencySelectionState(detected, 'codex_cli'), 'incomplete');
});

test('only a healthy persisted selection is configured', () => {
  const selected = {
    harness: 'codex_cli',
    cli_path: '/bin/codex',
    adapter_path: '/bin/codex-acp',
    node_path: '/bin/node',
  };
  assert.equal(workerDependencySelectionState(view({ selected: [selected] }), 'codex_cli'), 'configured');
  const detected = view({ selected: [selected] });
  assert.equal(workerDependencySelectionMatches(detected, {
    ...selected,
    expected_selection_revision: 4,
  }), true);
  assert.equal(workerDependencySelectionMatches(detected, {
    ...selected,
    adapter_path: '/replacement/codex-acp',
    expected_selection_revision: 4,
  }), false);
});

test('a newly discovered script adapter remains incomplete when required Node is missing', () => {
  const detected = view({
    candidates: view().candidates.filter(candidate => candidate.component !== 'node'),
    install_hints: [{
      harness: 'codex_cli',
      component: 'node',
      platform: 'macos',
      command: null,
      reason_code: 'worker.dependencies.install_required',
    }],
  });
  assert.equal(workerDependencySelectionState(detected, 'codex_cli'), 'incomplete');
});

test('backend failures stay failures instead of becoming an empty dependency view', () => {
  assert.throws(() => workerEnvelopeData({
    status: 'conflict',
    data: null,
    next_actions: [],
    error: { code: 'revision_conflict', message_key: 'worker.selection_stale' },
  }), error => error.code === 'revision_conflict');
});
