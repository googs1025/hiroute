import assert from 'node:assert/strict';
import test from 'node:test';
import { routingEditorEntries } from '../src/routing-editor-entries.ts';

test('current linked draft occupies the live plan slot', () => {
  const plan = { agent_plan_id: 'plan/1', head: { head_revision: 2 } };
  const draft = { draft_id: 'draft/1', plan_id: 'plan/1', base_head_revision: 2 };
  assert.deepEqual(routingEditorEntries([plan], [draft]), [{ key: 'draft/1', plan, draft }]);
});

test('outdated linked draft cannot hide the active plan', () => {
  const plan = { agent_plan_id: 'plan/1', head: { head_revision: 2 } };
  const draft = { draft_id: 'draft/1', plan_id: 'plan/1', base_head_revision: 1 };
  assert.deepEqual(routingEditorEntries([plan], [draft]), [
    { key: 'plan/1', plan },
    { key: 'draft/1', plan, draft, staleDraft: true },
  ]);
});
