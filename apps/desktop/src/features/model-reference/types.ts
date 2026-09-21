export type Language = 'zh' | 'en';
export type RatingValue = { state: 'unknown'; reason: string } | { state: 'reference' | 'estimated'; score_tenths: number; evidence_ref: string; method_revision: string };
export type NativeConfiguration = { kind: 'fixed' | 'profile'; profile: string } | { kind: 'toggle'; enabled: boolean } | { kind: 'budget'; tokens: number };
export type Rating = { requested_configuration: NativeConfiguration; overall: RatingValue; coding: RatingValue; tool: RatingValue };
export type Capability = { state: 'known_supported' | 'known_unsupported' | 'unknown'; evidence_kind: string };
export type ModelReference = {
  model_configuration_id: string; model_revision: number; display_name: string; publisher_id: string; data_version: string;
  tool: Capability; vision: Capability; streaming: Capability; context_tokens: number | null; max_output_tokens: number | null;
  native_render_convention?: 'claude_adaptive_effort_messages';
  rating_snapshots: { version: string; digest: string; scale_version: string }[];
};
export type TargetLocator = { kind: 'binding'; binding_id: string } | { kind: 'source_model'; source_id: string; model_identity: { kind: 'catalog_model' | 'local_model'; id: string } };
export type PriceContext = {
  target_locator: TargetLocator; currency: string; valuation_kind: 'usage_estimate' | 'api_equivalent';
};
export type PriceDisplay = {
  result: { pending_activation: boolean; evaluated_at: number; generation_ref: { id: string; digest: string } | null;
    items: { query_id: string; quote: { origin: string; quote_digest: string; unknown_reasons: string[] };
      edit_context?: { target_locator: TargetLocator; expected_source_revision: number; expected_binding_revision: number; expected_override_revision: number } | null }[] };
  // Render native decimal strings: JSON u64 rate numbers cannot safely pass through JS arithmetic.
  display_rates: [string | null, string | null, string | null, string | null][];
};
export type PriceOutcome = { state: string; operation: { operation_id: string; state: string; sequence: number; cancellable: boolean } | null };
