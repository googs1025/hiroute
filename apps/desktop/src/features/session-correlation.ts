export type CorrelationKind =
  | 'agent_supplied'
  | 'verified_worker'
  | 'inferred'
  | 'request_scoped'
  | 'unknown';

const labels: Record<CorrelationKind, [string, string]> = {
  agent_supplied: ['Agent 提供', 'Agent supplied'],
  verified_worker: ['执行关联已验证', 'Execution link verified'],
  inferred: ['推断关联', 'Inferred'],
  request_scoped: ['请求级', 'Request scoped'],
  unknown: ['关联未知', 'Correlation unknown'],
};

export function sessionCorrelationLabel(kind: CorrelationKind | null | undefined, language: 'zh' | 'en' = 'zh'): string {
  return (labels[kind ?? 'unknown'] ?? labels.unknown)[language === 'en' ? 1 : 0];
}
