export type RoutePlan = { agent_plan_id: string; head: { head_revision: number } };
export type RouteDraft = { draft_id: string; plan_id?: string; base_head_revision?: number };

export type RouteEditorEntry<P extends RoutePlan, D extends RouteDraft> = {
  key: string;
  plan?: P;
  draft?: D;
  staleDraft?: boolean;
};

/** Keep a current draft in its plan's slot, but never let an obsolete draft hide the live plan. */
export function routingEditorEntries<P extends RoutePlan, D extends RouteDraft>(plans: P[], drafts: D[]): RouteEditorEntry<P, D>[] {
  const used = new Set<string>();
  const entries: RouteEditorEntry<P, D>[] = plans.map(plan => {
    const current = drafts.find(draft => draft.plan_id === plan.agent_plan_id
      && draft.base_head_revision === plan.head.head_revision);
    if (current) {
      used.add(current.draft_id);
      return { key: current.draft_id, plan, draft: current };
    }
    return { key: plan.agent_plan_id, plan };
  });
  for (const draft of drafts) {
    if (used.has(draft.draft_id)) continue;
    const plan = plans.find(candidate => candidate.agent_plan_id === draft.plan_id);
    entries.push({ key: draft.draft_id, draft, ...(plan ? { plan, staleDraft: true } : {}) });
  }
  return entries;
}
