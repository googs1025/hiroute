import type {
  HomeActivity,
  HomeAgents,
  HomeCompute,
  HomePlans,
  HomeRead,
  HomeReads,
  HomeService,
  HomeValue,
} from '../../../src/features/home';

export type ScenarioName = 'fresh' | 'loading' | 'partial' | 'daily' | 'drift';

const ready = <T,>(data: T): HomeRead<T> => ({ status: 'ready', data });

const service: HomeService = { daemon: 'running', gateway: 'ready', recoveryReady: true };
const emptyValue: HomeValue = {
  rangeLabel: 'Today',
  coverage: 'unknown',
  pending: 0,
  provisional: false,
  unknownTrafficRequests: 0,
  excludedRequests: 0,
  usage: {
    input: null,
    output: null,
    cacheRead: null,
    cacheWrite: null,
    inputCacheHit: { state: 'unknown', ratio_basis_points: null, cache_read_tokens: null, total_input_tokens: null, eligible_attempt_count: 0, total_attempt_count: 0, zero_input_attempt_count: 0, missing_attempt_count: 0, invalid_attempt_count: 0, arithmetic_overflow: false, archive_coverage_partial: false, coverage: 'unknown' },
  },
  money: [],
};

const fresh: HomeReads = {
  service: ready(service),
  compute: ready({ candidates: [], sources: [], saveAttempts: [], subscriptionChecks: [] }),
  plans: ready({ plans: [], drafts: [] }),
  agents: ready({ agents: [] }),
  activity: ready({ sessions: [], tasks: [] }),
  value: ready(emptyValue),
};

const partialCompute: HomeCompute = {
  candidates: [{
    candidateRef: 'candidate/native-secondary',
    candidateRevision: '42',
    editRevision: '7',
    checkId: 'check/native-7',
    displayName: 'Secondary Native API',
    inputState: 'missing',
    factState: 'pending_credential',
    selectableModelCount: 1,
    issues: ['CREDENTIAL_REQUIRED'],
  }],
  sources: [{
    sourceId: 'source/user/loopback-lab',
    displayName: 'Loopback Lab · manual-model',
    bindingIds: ['binding/user/loopback-lab/manual-model'],
    origin: 'user_configured',
    evidenceRevision: 'candidate-revision-7',
    authentication: 'header',
    destinationLabel: '127.0.0.1:4141',
    availability: 'available',
    capabilityKnowledge: 'unknown',
    priceKnowledge: 'unknown',
  }],
  saveAttempts: [{
    attemptId: 'save/custom-secondary',
    displayName: 'Secondary API source',
    disposition: 'needs_input',
    managementState: 'needs_credential',
    safeMessage: 'MODEL_CAPABILITIES_REQUIRED',
  }],
  subscriptionChecks: [{
    checkId: 'check/subscription-a',
    state: 'verified',
    checkedCandidateRevision: 'candidate-revision-12',
    validationRef: 'validation/check-a',
  }],
};

const partialPlans: HomePlans = {
  plans: [],
  drafts: [{ draftId: 'draft/daily-coding', displayName: '日常编码 / Daily coding' }],
};

const partialAgents: HomeAgents = {
  agents: [{
    agentId: 'agent_codex_default',
    contextId: 'context/codex/default',
    displayName: 'Codex',
    brand: 'codex',
    model: 'unconfigured',
    collaboration: 'configured',
  }],
};

const partial: HomeReads = {
  service: ready(service),
  compute: ready(partialCompute),
  plans: ready(partialPlans),
  agents: ready(partialAgents),
  activity: ready({ sessions: [], tasks: [] }),
  value: ready(emptyValue),
};

const dailyCompute: HomeCompute = {
  candidates: [],
  sources: [
    {
      sourceId: 'source/openai/api',
      displayName: 'OpenAI API · gpt-5.6-sol',
      bindingIds: ['binding/openai/gpt-5.6-sol'],
      origin: 'user_configured',
      evidenceRevision: 'candidate-revision-21',
      authentication: 'bearer',
      destinationLabel: 'api.openai.com',
      availability: 'cooling',
      capabilityKnowledge: 'known',
      priceKnowledge: 'known',
    },
    {
      sourceId: 'source/catalog/qwen-free',
      displayName: 'Qwen 2.5 7B Instruct',
      bindingIds: ['binding/catalog/qwen-free'],
      origin: 'catalog',
      authentication: 'none',
      availability: 'available',
      capabilityKnowledge: 'known',
      priceKnowledge: 'free',
    },
  ],
  saveAttempts: [],
  subscriptionChecks: [],
};

const dailyPlans: HomePlans = {
  plans: [{ planId: 'plan/daily-coding', displayName: '日常编码 / Daily coding', publication: 'published', bindingIds: ['binding/openai/gpt-5.6-sol', 'binding/catalog/qwen-free'] }],
  drafts: [],
};

const dailyAgents: HomeAgents = {
  agents: [
    { agentId: 'agent_codex_default', contextId: 'context/codex/default', displayName: 'Codex', brand: 'codex', model: 'verified', collaboration: 'verified' },
    { agentId: 'agent_claude_default', contextId: 'context/claude/default', displayName: 'Claude Code', brand: 'claude-code', model: 'configured', collaboration: 'verified' },
    { agentId: 'agent_unknown_local', displayName: 'Local Agent', brand: 'agent', model: 'unknown', collaboration: 'unconfigured' },
  ],
};

const dailyActivity: HomeActivity = {
  sessions: [
    { sessionId: 'session/gateway-refactor', requestId: 'request/42', title: '重构 Gateway 路由编译边界', agentName: 'Codex', modelLabel: 'gpt-5.6-sol', occurredAtLabel: '14:28', modelSwitch: true },
    { sessionId: 'session/rust-ownership', title: '解释 Rust ownership 错误', agentName: 'Claude Code', modelLabel: 'Qwen3 Coder Plus', occurredAtLabel: '13:54', modelSwitch: false },
    { sessionId: null, title: '关联仍待后端确认的记录', agentName: 'Codex', occurredAtLabel: '11:20', modelSwitch: null },
  ],
  tasks: [
    { taskId: 'task/mvp16-visual', runId: 'run/3', sessionId: 'session/gateway-refactor', title: '完成 Desktop 视觉核对', state: 'running', agentName: 'Codex' },
    { taskId: 'task/rust-review', runId: 'run/1', title: '复核路由发布边界', state: 'succeeded', agentName: 'Claude Code' },
  ],
};

const dailyValue: HomeValue = {
  rangeLabel: 'Today · Asia/Shanghai',
  coverage: 'partial',
  pending: 2,
  provisional: true,
  unknownTrafficRequests: 1,
  excludedRequests: 1,
  usage: {
    input: 38200,
    output: 4600,
    cacheRead: 22920,
    cacheWrite: 810,
    inputCacheHit: { state: 'available', ratio_basis_points: 6000, cache_read_tokens: 22920, total_input_tokens: 38200, eligible_attempt_count: 1, total_attempt_count: 2, zero_input_attempt_count: 0, missing_attempt_count: 1, invalid_attempt_count: 0, arithmetic_overflow: false, archive_coverage_partial: false, coverage: 'partial' },
  },
  requests: 18,
  modelSwitches: 2,
  money: [
    { currency: 'CNY', apiEquivalent: '¥24.68', estimatedSavings: '¥17.98' },
    { currency: 'USD' },
  ],
};

const daily: HomeReads = {
  service: ready(service),
  compute: ready(dailyCompute),
  plans: ready(dailyPlans),
  agents: ready(dailyAgents),
  activity: ready(dailyActivity),
  value: ready(dailyValue),
};

const drift: HomeReads = {
  ...daily,
  agents: ready({
    agents: dailyAgents.agents.map(agent => agent.agentId === 'agent_codex_default'
      ? { ...agent, model: 'degraded', safeIssue: 'CONFIGURATION_DRIFT' }
      : agent),
  }),
  activity: { status: 'error', code: 'ACTIVITY_READ_UNAVAILABLE', message: '会话刷新失败；保留上次读到的记录。', previous: dailyActivity, retryable: true },
};

const loading: HomeReads = {
  service: { status: 'loading' },
  compute: { status: 'loading' },
  plans: { status: 'loading' },
  agents: { status: 'loading' },
  activity: ready({ sessions: [], tasks: [] }),
  value: ready(emptyValue),
};

export function fixture(name: ScenarioName): HomeReads {
  return { fresh, loading, partial, daily, drift }[name];
}
