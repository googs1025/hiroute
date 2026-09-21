export type CandidateRef = { candidate_ref: string; candidate_revision: number };
export type ValidationRef = {
  approval_operation: { operation_id: string; state: string; sequence: number; cancellable: boolean };
  validation_ref: string;
  validation_revision: string;
};
export type SubscriptionCandidate = {
  candidate: CandidateRef;
  correlation: { candidate_ref: string; edit_revision: number; check_id: string; input_digest: string };
  producer: 'native' | 'cpa';
  provenance: 'registered' | 'user_configured' | 'connector_owned';
  display_name: string;
  existing_source_id?: string;
  models: {
    model_ref: string;
    upstream_model_id: string;
    display_name: string;
    membership: 'catalog' | 'observed' | 'user_declared';
    selectable: boolean;
    reason?: string;
  }[];
  input_state: 'not_required' | 'provided' | 'missing' | 'unavailable';
  fact_state: 'pending_credential' | 'pending_approval' | 'complete';
  validation?: ValidationRef;
  issues?: { code: string; message_key: string; retryable: boolean }[];
};
export type SubscriptionCheckResult = {
  candidate: CandidateRef;
  approval_operation: ValidationRef['approval_operation'];
  status: 'checking' | 'verified' | 'source_changed' | 'needs_auth' | 'unavailable' | 'failed' | 'released' | 'retained';
  save_operation?: ValidationRef['approval_operation'];
  validation?: ValidationRef;
  checked_candidate?: SubscriptionCandidate;
  reason?: string;
};
export type SubscriptionSaveIntent = 'save_ready' | 'save_disabled';
