import assert from 'node:assert/strict';
import test from 'node:test';
import { persistenceTargetWasSuperseded, resolvePersistedEditor } from '../src/plan-editor-persistence.ts';

const published = { agent_plan_id: 'plan/new', model_alias: 'hiroute-new', head: { head_revision: 1 } };
const existing = { agent_plan_id: 'plan/existing', model_alias: 'hiroute-existing', head: { head_revision: 4 } };
const operation = (operation_id, state = 'succeeded') => ({ operation_id, state, sequence: 1, cancellable: false, safe_error_code: null });
const ours = { operationId: 'op/ours', operation: operation('op/ours') };

test('new route publication adopts the persisted plan identity', () => {
  assert.deepEqual(resolvePersistedEditor(
    'publish',
    { plans: [existing, published], drafts: [] },
    { draftId: 'draft/local', modelAlias: 'hiroute-new', targetHeadRevision: 1 },
    ours,
  ), { key: 'plan/new', plan: published });
});

test('draft save adopts the exact durable draft revision and linked plan', () => {
  const draft = { draft_id: 'draft/durable', plan_id: 'plan/existing', revision: 1 };
  assert.deepEqual(resolvePersistedEditor(
    'save_draft',
    { plans: [existing], drafts: [draft] },
    { draftId: 'draft/durable', planId: 'plan/existing', targetDraftRevision: 1 },
    ours,
  ), { key: 'draft/durable', draft, plan: existing });
});

test('existing route publication prefers its stable plan id', () => {
  assert.deepEqual(resolvePersistedEditor(
    'publish',
    { plans: [existing], drafts: [] },
    { draftId: 'draft/ignored', planId: 'plan/existing', modelAlias: 'hiroute-stale', targetHeadRevision: 4 },
    ours,
  ), { key: 'plan/existing', plan: existing });
});

test('existing route publication requires the exact submitted target revision', () => {
  const stale = { ...existing, head: { head_revision: 4 } };
  const current = { ...existing, head: { head_revision: 5 } };
  const later = { ...existing, head: { head_revision: 6 } };
  const identity = { draftId: 'draft/ignored', planId: 'plan/existing', targetHeadRevision: 5 };
  assert.equal(resolvePersistedEditor('publish', { plans: [stale], drafts: [] }, identity, ours), null);
  assert.deepEqual(resolvePersistedEditor('publish', { plans: [current], drafts: [] }, identity, ours), { key: 'plan/existing', plan: current });
  assert.equal(resolvePersistedEditor('publish', { plans: [later], drafts: [] }, identity, ours), null);
});

test('catalog state cannot confirm a different, failed, or unknown operation', () => {
  const identity = { draftId: 'draft/ignored', planId: 'plan/existing', targetHeadRevision: 4 };
  assert.equal(resolvePersistedEditor('publish', { plans: [existing], drafts: [] }, identity, {
    operationId: 'op/ours', operation: operation('op/other'),
  }), null);
  assert.equal(resolvePersistedEditor('publish', { plans: [existing], drafts: [] }, identity, {
    operationId: 'op/ours', operation: operation('op/ours', 'rolled_back'),
  }), null);
  assert.equal(resolvePersistedEditor('publish', { plans: [existing], drafts: [] }, identity, {
    operationId: null, operation: null,
  }), null);
});

test('a later revision is reported as superseding the submitted target', () => {
  const identity = { draftId: 'draft/ignored', planId: 'plan/existing', targetHeadRevision: 5 };
  assert.equal(persistenceTargetWasSuperseded('publish', {
    plans: [{ ...existing, head: { head_revision: 5 } }], drafts: [],
  }, identity), false);
  assert.equal(persistenceTargetWasSuperseded('publish', {
    plans: [{ ...existing, head: { head_revision: 6 } }], drafts: [],
  }, identity), true);
});
