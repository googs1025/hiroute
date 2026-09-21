export type PersistedPlan = {
  agent_plan_id: string;
  model_alias: string;
  head?: { head_revision: number };
};

export type PersistedDraft = {
  draft_id: string;
  plan_id?: string;
};

export type PersistedEditor<P extends PersistedPlan, D extends PersistedDraft> = {
  key: string;
  plan?: P;
  draft?: D;
};

export type PersistenceIdentity = {
  draftId: string;
  planId?: string;
  modelAlias?: string;
  targetHeadRevision?: number;
  targetDraftRevision?: number;
};

export type PlanOperation = {
  operation_id: string;
  state: string;
  sequence: number;
  cancellable: boolean;
  safe_error_code: string | null;
};

export type PersistenceObservation = {
  operationId: string | null;
  operation: PlanOperation | null | undefined;
};

export function resolvePersistedEditor<P extends PersistedPlan, D extends PersistedDraft>(
  action: 'save_draft' | 'publish',
  catalog: { plans: P[]; drafts: D[] },
  identity: PersistenceIdentity,
  observation: PersistenceObservation,
): PersistedEditor<P, D> | null {
  if (!observation.operationId
    || observation.operation?.operation_id !== observation.operationId
    || observation.operation.state !== 'succeeded') return null;
  if (action === 'save_draft') {
    const draft = catalog.drafts.find(candidate => candidate.draft_id === identity.draftId);
    if (!draft || identity.targetDraftRevision == null
      || !('revision' in draft) || draft.revision !== identity.targetDraftRevision
      || draft.plan_id !== identity.planId) return null;
    const linkedPlanId = draft.plan_id ?? identity.planId;
    const plan = linkedPlanId
      ? catalog.plans.find(candidate => candidate.agent_plan_id === linkedPlanId)
      : undefined;
    return { key: draft.draft_id, draft, ...(plan ? { plan } : {}) };
  }

  const plan = catalog.plans.find(candidate =>
    candidate.agent_plan_id === identity.planId
      || (Boolean(identity.modelAlias) && candidate.model_alias === identity.modelAlias),
  );
  if (!plan || identity.targetHeadRevision == null
    || !plan.head || plan.head.head_revision !== identity.targetHeadRevision) return null;
  return plan ? { key: plan.agent_plan_id, plan } : null;
}

export function persistenceTargetWasSuperseded<P extends PersistedPlan, D extends PersistedDraft>(
  action: 'save_draft' | 'publish',
  catalog: { plans: P[]; drafts: D[] },
  identity: PersistenceIdentity,
): boolean {
  if (action === 'save_draft') {
    const draft = catalog.drafts.find(candidate => candidate.draft_id === identity.draftId);
    return Boolean(draft && identity.targetDraftRevision != null
      && 'revision' in draft && typeof draft.revision === 'number'
      && draft.revision > identity.targetDraftRevision);
  }
  const plan = catalog.plans.find(candidate =>
    candidate.agent_plan_id === identity.planId
      || (Boolean(identity.modelAlias) && candidate.model_alias === identity.modelAlias),
  );
  return Boolean(plan?.head && identity.targetHeadRevision != null
    && plan.head.head_revision > identity.targetHeadRevision);
}
