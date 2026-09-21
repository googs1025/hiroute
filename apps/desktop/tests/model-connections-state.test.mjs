import assert from 'node:assert/strict';
import test from 'node:test';

import {
  buildSaveChange,
  buildCheckDraft,
  clientIdempotencyKey,
  defaultSelectedModelRefs,
  modelCanBeSelected,
  modelSaveCompleted,
  saveEligibility,
  selectedModelRefsForSave,
} from '../src/features/model-connections/state.ts';

test('only a terminal saved Operation can close a model save as successful', () => {
  for (const disposition of ['pending', 'needs_input', 'conflict', 'failed']) {
    assert.equal(modelSaveCompleted({ disposition }), false, disposition);
  }
  assert.equal(modelSaveCompleted({ disposition: 'saved' }), true);
});

test('save idempotency keys satisfy domain and native recovery constraints', () => {
  const key = clientIdempotencyKey('model-save');
  assert.match(key, /^[A-Za-z0-9._:-]{1,200}$/);
  assert.equal(key.length, 64);
  assert.ok(key.startsWith('model-save:'));
  assert.ok(!key.includes('/'));
});

test('native user check maps UI provenance to the exact public wire draft', () => {
  const draft = {
    entry_kind: 'custom_api', candidate_ref: null, lineage_ref: 'lineage/native/test',
    display_name: 'Native API', existing_source_id: null, edit_revision: 7,
    check_id: 'check/7', base_url: 'http://127.0.0.1:51026/v1', base_kind: 'api_root',
    request_path_override: null, inventory_path_override: null, protocol: 'responses',
    protocol_profile_id: 'profile/custom/responses', protocol_profile_revision: 1,
    authentication: { kind: 'none' },
    provenance: { kind: 'user_configured', configuration_revision: 4 },
    qualification: { free_access: null, evidence_ref: null },
    models: [{
      client_id: 'model/client-only', upstream_model_id: 'manual-native-model',
      display_name: 'Manual Native Model', catalog_configuration_id: null,
      membership: 'user_declared', capabilities: {
        tool: { value: false, basis: 'user_declared' },
        vision: { value: false, basis: 'user_declared' },
        streaming: { value: true, basis: 'user_declared' },
        context_tokens: { value: 32768, basis: 'user_declared' },
        max_output_tokens: { value: 4096, basis: 'user_declared' },
        native_reasoning: { value: { kind: 'fixed', profile: 'provider-default' }, basis: 'user_declared' },
      },
    }],
  };
  const wire = buildCheckDraft(draft);
  assert.equal(wire.configuration_revision, 4);
  assert.equal(wire.models[0].upstream_model_id, 'manual-native-model');
  assert.equal('client_id' in wire.models[0], false);
  assert.equal('entry_kind' in wire, false);
  assert.equal('provenance' in wire, false);
  assert.equal('qualification' in wire, false);
});

function resultWith({ factState, reachability, authentication }) {
  const model = {
    model_ref: 'model/user-declared',
    upstream_model_id: 'declared-model',
    display_name: 'Declared model',
    membership: 'user_declared',
    selectable: false,
    reason: 'model_connections.connection_check_required',
  };
  return {
    candidate: {
      candidate: { candidate_ref: 'candidate/native/test', candidate_revision: 3 },
      correlation: {
        candidate_ref: 'candidate/native/test',
        edit_revision: 7,
        check_id: 'check/7',
        input_digest: 'sha256:input',
      },
      producer: 'native',
      provenance: 'user_configured',
      display_name: 'Custom API',
      models: [model],
      input_state: factState === 'pending_credential' ? 'missing' : 'provided',
      fact_state: factState,
      issues: [],
    },
    target: {
      scheme: 'https', authority: 'api.example.test', port: 443,
      request_path: '/v1/responses', upstream_protocol: 'responses',
      protocol_profile_id: 'profile/custom/responses', protocol_profile_revision: 1,
    },
    inventory_path: '/v1/models',
    reachability,
    authentication,
    directory: 'not_run',
    protocol: 'selected',
    inference: 'not_run',
    checked_model_count: 0,
    invalid_model_count: 0,
    pages_read: 0,
    checked_at_unix_ms: 1,
    input_digest: 'sha256:input',
    issues: [],
  };
}

const revisions = { target: 4, dependencies: {} };

for (const scenario of [
  {
    name: 'missing key',
    result: resultWith({ factState: 'pending_credential', reachability: 'not_run', authentication: 'not_run' }),
    readyReason: 'credential_required',
  },
  {
    name: 'connection failure without a selectable declaration',
    result: resultWith({ factState: 'complete', reachability: 'transport_failed', authentication: 'unknown' }),
    readyReason: 'model_required',
  },
]) {
  test(`${scenario.name} keeps declared model refs for a disabled draft`, () => {
    const selected = new Set(defaultSelectedModelRefs(scenario.result));
    const disabledRefs = selectedModelRefsForSave(scenario.result, selected, false);
    const readyRefs = selectedModelRefsForSave(scenario.result, selected, true);

    assert.deepEqual(disabledRefs, ['model/user-declared']);
    assert.deepEqual(readyRefs, []);
    assert.equal(scenario.result.candidate.models[0].selectable, false);
    assert.deepEqual(saveEligibility(scenario.result, false), { allowed: true, reason: null });
    assert.deepEqual(saveEligibility(scenario.result, true), { allowed: false, reason: scenario.readyReason });

    const change = buildSaveChange({
      result: scenario.result,
      expectedRevisions: revisions,
      selectedModelRefs: disabledRefs,
      enable: false,
      protectedInput: null,
    });
    assert.equal(change.intent, 'save_disabled');
    assert.deepEqual(change.selected_model_refs, ['model/user-declared']);
  });
}

test('disabled selection exception rejects capability gaps and CPA candidates', () => {
  for (const candidate of [
    { producer: 'native', reason: 'model_connections.capability_required' },
    { producer: 'cpa', reason: 'model_connections.connection_check_required' },
  ]) {
    const result = resultWith({
      factState: 'complete',
      reachability: 'transport_failed',
      authentication: 'unknown',
    });
    result.candidate.producer = candidate.producer;
    result.candidate.models[0].reason = candidate.reason;
    const model = result.candidate.models[0];

    assert.equal(modelCanBeSelected(result, model, false), false);
    assert.deepEqual(defaultSelectedModelRefs(result), []);
    assert.deepEqual(selectedModelRefsForSave(result, new Set([model.model_ref]), false), []);
  }
});

test('mixed result selects complete model without admitting capability-required model', () => {
  const result = resultWith({
    factState: 'complete',
    reachability: 'reachable',
    authentication: 'verified',
  });
  result.candidate.models[0].reason = 'model_connections.capability_required';
  result.candidate.models.push({
    model_ref: 'model/complete',
    upstream_model_id: 'complete-model',
    display_name: 'Complete model',
    membership: 'user_declared',
    selectable: true,
  });

  assert.deepEqual(defaultSelectedModelRefs(result), ['model/complete']);
  const selected = new Set(result.candidate.models.map(model => model.model_ref));
  assert.deepEqual(selectedModelRefsForSave(result, selected, true), ['model/complete']);
  assert.deepEqual(selectedModelRefsForSave(result, selected, false), ['model/complete']);
});


test('optional directory failure permits valid models but explicit inference and auth failures do not', () => {
  const result = resultWith({ factState: 'complete', reachability: 'transport_failed', authentication: 'unknown' });
  result.candidate.models[0].selectable = true;
  assert.deepEqual(saveEligibility(result, true), { allowed: true, reason: null });
  result.inference = 'failed';
  assert.deepEqual(saveEligibility(result, true), { allowed: false, reason: 'explicit_failure' });
  result.inference = 'not_run';
  result.authentication = 'rejected';
  assert.deepEqual(saveEligibility(result, true), { allowed: false, reason: 'explicit_failure' });
});


test('save after a repeated check uses the current revision of the same protected slot', () => {
  const result = resultWith({ factState: 'complete', reachability: 'not_run', authentication: 'unknown' });
  result.candidate.candidate = { candidate_ref: 'candidate/protected', candidate_revision: 4 };
  const build = protectedInput => buildSaveChange({ result, expectedRevisions: revisions, selectedModelRefs: ['model/user-declared'], enable: true, protectedInput });
  assert.deepEqual(build({ candidate_ref: 'candidate/protected', candidate_revision: 1 }).key_edits[0].input_candidate, result.candidate.candidate);
  const other = { candidate_ref: 'candidate/another-slot', candidate_revision: 2 };
  assert.deepEqual(build(other).key_edits[0].input_candidate, other);
});
