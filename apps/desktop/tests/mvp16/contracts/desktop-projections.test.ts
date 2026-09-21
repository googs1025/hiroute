import assert from 'node:assert/strict';
import test from 'node:test';
import { deriveHomePrimaryMode } from '../../../src/features/home/state.ts';
import type { HomeReads } from '../../../src/features/home/types.ts';
import {
  projectHomeActivity,
  projectHomeAgents,
  projectHomeCompute,
  projectHomePlans,
  projectHomeService,
  projectHomeValue,
  type DesktopSnapshot,
} from '../../../src/product/home-projections.ts';
import type { AgentSnapshot } from '../../../src/agents.tsx';
import type { ManagementSnapshot, ManagedSource } from '../../../src/features/models/types.ts';

function source(
  sourceId: string,
  provenance: ManagedSource['provenance'],
  authentication: ManagedSource['authentication'],
): ManagedSource {
  return {
    source_id: sourceId,
    revision: 2,
    display_name: sourceId,
    provenance,
    target: {
      scheme: 'http', authority: '127.0.0.1', port: 51026, request_path: '/v1',
      upstream_protocol: 'responses', protocol_profile_id: 'profile/custom/responses',
      protocol_profile_revision: 1,
    },
    authentication,
    state: 'ready',
    models: [{
      model_ref: `model/${sourceId}`, binding_id: `binding/${sourceId}`, revision: 1,
      upstream_model_id: 'native-model', display_name: 'Native Model', membership: 'user_declared',
    }],
    keys: [],
    ready_model_count: 1,
    actions: ['edit'],
  };
}

const management: ManagementSnapshot = {
  schema: 'hiroute.compute-management/v2',
  revisions: { target: 3, dependencies: {} },
  runtime_state: 'complete',
  sources: [
    source('source/user', 'user_configured', { kind: 'none' }),
    source('source/connector', 'connector_owned', { kind: 'bearer' }),
  ],
};

const emptyDesktop: DesktopSnapshot = {
  catalog_error: null,
  service: {
    daemon_role: 'owner', recovery_ready: true, mutation_available: true,
    gateway: 'empty', revisions: { target: 1, dependencies: {} },
  },
  catalog: { plans: [], drafts: [] },
  trusted_authority: true,
  restore_names: [],
  pending: null,
};

test('saved user and connector sources remain distinct without invented pricing', () => {
  const projected = projectHomeCompute(management);
  assert.deepEqual(projected.sources.map(item => item.origin), ['user_configured', 'connector_owned']);
  assert.equal(projected.sources[0].authentication, 'none');
  assert.equal(projected.sources[0].availability, 'available');
  assert.equal(projected.sources[0].priceKnowledge, 'unknown');
  assert.equal(projected.sources[0].capabilityKnowledge, 'unknown');
  assert.deepEqual(projected.candidates, []);
});

test('empty real projections produce first-use only after every required read resolves', () => {
  const agents: AgentSnapshot = { agents: [], plans: { plans: [] }, trusted_authority: true };
  const reads: HomeReads = {
    service: { status: 'ready', data: projectHomeService(emptyDesktop) },
    compute: { status: 'ready', data: projectHomeCompute({ ...management, sources: [] }) },
    plans: { status: 'ready', data: projectHomePlans(emptyDesktop) },
    agents: { status: 'ready', data: projectHomeAgents(agents) },
    activity: { status: 'ready', data: projectHomeActivity({ sessions: [], next_cursor: null }) },
    value: { status: 'ready', data: projectHomeValue({
      from_ms: 0, to_ms: 1, pending_requests: 0, provisional_requests: 0,
      unknown_traffic_requests: 0, excluded_requests: 0, amounts: [], archive_boundary_partial: false, retention_boundary_partial: false, usage: [],
      input_cache_hit: { state: 'unknown', ratio_basis_points: null, cache_read_tokens: null, total_input_tokens: null, eligible_attempt_count: 0, total_attempt_count: 0, zero_input_attempt_count: 0, missing_attempt_count: 0, invalid_attempt_count: 0, arithmetic_overflow: false, archive_coverage_partial: false, coverage: 'unknown' },
    }) },
  };
  assert.equal(deriveHomePrimaryMode(reads), 'first-use');
});

test('plans and agent facets project only backend facts', () => {
  const snapshot: DesktopSnapshot = {
    ...emptyDesktop,
    catalog: {
      drafts: [],
      plans: [{
        agent_plan_id: 'plan/native',
        desired: {
          display_name: 'Native route', purpose: '', mode: 'fixed_model',
          strategy: { mode: 'fixed_model', candidates: [{ binding_id: 'binding/source/user' }] },
          delegation_enabled: false,
          requirements: {}, limits: { maximum_attempts: 1, request_timeout_ms: 60000, attempt_timeout_ms: 30000 },
        },
        head: { head_revision: 1, status: 'enabled' },
        agent_plan_revision: 1,
        model_alias: 'hiroute-native',
        publication: { revision: 1, digest: 'sha256:1' },
        execution: 'ready',
      }],
    },
  };
  assert.deepEqual(projectHomePlans(snapshot).plans[0], {
    planId: 'plan/native', displayName: 'Native route', publication: 'published',
    bindingIds: ['binding/source/user'],
  });
  const projectedAgents = projectHomeAgents({
    trusted_authority: true,
    plans: { plans: snapshot.catalog.plans },
    agents: [{
      agent_id: 'agent_codex_default', version: '1', context_id: 'context/1',
      configuration_state: 'configured', status_error: null,
      settings: { state: 'configured', model_verified: true, restore_point_ref: null },
    }],
  });
  assert.equal(projectedAgents.agents[0].brand, 'codex');
  assert.equal(projectedAgents.agents[0].model, 'verified');
  assert.equal(projectedAgents.agents[0].collaboration, 'unconfigured');
});

test('session projection never invents a Desktop task list', () => {
  const activity = projectHomeActivity({
    sessions: [{
      session_id: 'session/real', agent_id: 'agent_codex_default', first_request_at_ms: 1,
      last_request_at_ms: 2, request_count: 1, fallback_request_count: 0,
      unknown_model_request_count: 0, correlation_kind: 'agent_supplied',
    }],
    next_cursor: null,
  });
  assert.equal(activity.sessions[0].sessionId, 'session/real');
  assert.deepEqual(activity.tasks, []);
});

test('value projection exposes returned valuations without fabricating savings or counts', () => {
  const value = projectHomeValue({
    from_ms: 1,
    to_ms: 2,
    pending_requests: 2,
    provisional_requests: 1,
    unknown_traffic_requests: 4,
    excluded_requests: 2,
    archive_boundary_partial: false,
    retention_boundary_partial: false,
    amounts: [
      { currency: 'USD', valuation_kind: 'usage_estimate', known_sum_micros: 1250000, coverage: 'complete', missing_contribution_count: 0 },
      { currency: 'USD', valuation_kind: 'api_equivalent', known_sum_micros: 2500000, coverage: 'partial', missing_contribution_count: 1 },
    ],
    usage: [
      { metric: 'input', known_sum: 1100, coverage: 'complete', missing_attempt_count: 0 },
      { metric: 'output', known_sum: 75, coverage: 'complete', missing_attempt_count: 0 },
      { metric: 'cache_read', known_sum: 190, coverage: 'complete', missing_attempt_count: 0 },
      { metric: 'cache_write', known_sum: 0, coverage: 'complete', missing_attempt_count: 0 },
    ],
    input_cache_hit: { state: 'available', ratio_basis_points: 1727, cache_read_tokens: 190, total_input_tokens: 1100, eligible_attempt_count: 2, total_attempt_count: 2, zero_input_attempt_count: 0, missing_attempt_count: 0, invalid_attempt_count: 0, arithmetic_overflow: false, archive_coverage_partial: false, coverage: 'complete' },
  });
  assert.equal(value.money[0].usageEstimate, '1.25');
  assert.equal(value.money[0].apiEquivalent, '2.50');
  assert.equal(value.money[0].estimatedSavings, undefined);
  assert.equal(value.requests, undefined);
  assert.equal(value.modelSwitches, undefined);
  assert.equal(value.usage.input, 1100);
  assert.equal(value.usage.cacheWrite, 0);
  assert.equal(value.usage.inputCacheHit.ratio_basis_points, 1727);
  assert.equal(value.excludedRequests, 2);
  assert.equal(value.coverage, 'partial');
});

test('retention boundary lowers Home coverage even when retained samples are complete', () => {
  const value = projectHomeValue({
    from_ms: 0, to_ms: 2, pending_requests: 0, provisional_requests: 0,
    unknown_traffic_requests: 0, excluded_requests: 0, amounts: [],
    archive_boundary_partial: false, retention_boundary_partial: true,
    usage: [{ metric: 'input', known_sum: 100, coverage: 'partial', missing_attempt_count: 0 }],
    input_cache_hit: { state: 'available', ratio_basis_points: 4000, cache_read_tokens: 40, total_input_tokens: 100, eligible_attempt_count: 1, total_attempt_count: 1, zero_input_attempt_count: 0, missing_attempt_count: 0, invalid_attempt_count: 0, arithmetic_overflow: false, archive_coverage_partial: false, coverage: 'partial' },
  });
  assert.equal(value.usage.input, 100);
  assert.equal(value.coverage, 'partial');
});
