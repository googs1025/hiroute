export type UsageMetric = { known_sum: number | null; metric: string; coverage: string };
export type CacheHitSummary = {
  state: 'available' | 'not_applicable' | 'unknown';
  ratio_basis_points: number | null;
  cache_read_tokens: number | null;
  total_input_tokens: number | null;
  eligible_attempt_count: number;
  total_attempt_count: number;
  zero_input_attempt_count: number;
  missing_attempt_count: number;
  invalid_attempt_count: number;
  arithmetic_overflow: boolean;
  archive_coverage_partial: boolean;
  coverage: 'complete' | 'partial' | 'unknown';
};

export function usageValue(metrics: readonly UsageMetric[], name: string): number | null {
  return metrics.find(metric => metric.metric === name)?.known_sum ?? null;
}

export function formatTokenCount(value: number | null, language: 'zh' | 'en'): string {
  if (value === null) return language === 'zh' ? '未知' : 'Unknown';
  return value.toLocaleString(language === 'zh' ? 'zh-CN' : 'en-US');
}

export function formatCacheHit(summary: CacheHitSummary, language: 'zh' | 'en'): string {
  if (summary.state === 'not_applicable') return language === 'zh' ? '不适用（输入为 0）' : 'N/A (zero input)';
  if (summary.state !== 'available' || summary.ratio_basis_points === null) return language === 'zh' ? '未知' : 'Unknown';
  return `${(summary.ratio_basis_points / 100).toFixed(2)}%`;
}

export function formatCacheHitCoverage(summary: CacheHitSummary, language: 'zh' | 'en'): string {
  return language === 'zh'
    ? `${summary.eligible_attempt_count} / ${summary.total_attempt_count} 个 Attempt 可计算`
    : `${summary.eligible_attempt_count} / ${summary.total_attempt_count} attempts eligible`;
}
