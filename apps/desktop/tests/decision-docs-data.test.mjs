import assert from 'node:assert/strict';
import test from 'node:test';
import { decisionDocsData, queryDocsSamples } from '../decision-docs-data.ts';

const now = Date.UTC(2026, 8, 20, 12, 0);
test('documentation ranges are plausible and do not invent Jev reasons', () => {
  const { plan, models, samples } = decisionDocsData('zh', now);
  assert.equal(samples.length, 4);
  assert.equal(new Set(samples.map(s => s.session_id)).size, 4);
  for (const sample of samples) {
    assert.equal(sample.plan_id, plan.agent_plan_id);
    assert.equal(sample.plan_revision, plan.agent_plan_revision);
    assert(models.some(m => m.model_configuration_id === sample.model_configuration_id));
    assert(sample.first_at_ms < sample.last_at_ms && sample.last_at_ms < now);
    assert(sample.first_at_ms > now - 7 * 86400_000);
    if (sample.assessment) {
      assert(sample.assessment.target_through_ordinal <= sample.last_observed_turn_ordinal);
      assert(sample.assessment.score >= 0 && sample.assessment.score <= 1);
      assert.equal(sample.assessment.reason, undefined);
    }
  }
  assert(samples.some(s => s.assessment === null));
  assert(samples.some(s => s.history_partial));
  assert(samples.some(s => s.assessment && s.assessment.target_through_ordinal < s.last_observed_turn_ordinal));
});

test('screenshot filters honor score, revision, model, session and time scopes', () => {
  const { samples } = decisionDocsData('en', now);
  assert.equal(queryDocsSamples(samples, { score_lt: 0.5 }).samples.length, 1);
  assert.equal(queryDocsSamples(samples, { score_gt: 0.5 }).samples.length, 2);
  assert.equal(queryDocsSamples(samples, { score_lt: 0.37 }).samples.length, 0);
  assert.equal(queryDocsSamples(samples, { score_gt: 0.91 }).samples.length, 0);
  assert.equal(queryDocsSamples(samples, { plan_revision: 2 }).samples.length, 0);
  assert.equal(queryDocsSamples(samples, { plan_id: 'unrelated' }).samples.length, 0);
  assert.equal(queryDocsSamples(samples, { session_id: samples[0].session_id }).samples.length, 1);
  assert.equal(queryDocsSamples(samples, { model_configuration_id: samples[0].model_configuration_id }).samples.length, 2);
  assert.equal(queryDocsSamples(samples, { from_ms: now }).samples.length, 0);
  assert.equal(queryDocsSamples(samples, { to_ms: samples.at(-1).last_at_ms }).samples.length, 0);
  assert.deepEqual(queryDocsSamples(samples, { limit: 2 }).samples, samples.slice(0, 2));
  assert.throws(() => queryDocsSamples(samples, { cursor: 'unexpected' }), /CURSOR_UNSUPPORTED/);
});

test('both languages describe the same synthetic executions and model labels', () => {
  const zh = decisionDocsData('zh', now), en = decisionDocsData('en', now);
  assert.deepEqual(zh.samples, en.samples);
  assert.deepEqual(zh.models, en.models);
  assert.notEqual(zh.plan.desired.display_name, en.plan.desired.display_name);
});
