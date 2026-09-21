import { useMemo, useState } from 'react';

import { canSave, checkStatusText, saveIntent, statusText } from './model';
import type { SubscriptionCandidate, SubscriptionCheckResult, SubscriptionSaveIntent } from './types';

export type SubscriptionSaveSelection = {
  candidate: SubscriptionCandidate['candidate'];
  validation?: SubscriptionCandidate['validation'];
  selected_model_refs: string[];
  intent: SubscriptionSaveIntent;
};

type Props = {
  candidates: SubscriptionCandidate[];
  checkResults?: Readonly<Record<string, SubscriptionCheckResult | undefined>>;
  language: 'zh' | 'en';
  busy?: boolean;
  onCheck: (candidate: SubscriptionCandidate['candidate']) => Promise<void>;
  onSave: (selection: SubscriptionSaveSelection) => Promise<void>;
};

export function Subscriptions({ candidates, checkResults = {}, language, busy = false, onCheck, onSave }: Props) {
  const [selected, setSelected] = useState<Record<string, string[]>>({});
  const labels = useMemo(() => language === 'zh'
    ? { check: '检查订阅', save: '保存接入', disabled: '保存为停用', unknown: '账号可见 · 能力资料待补充，暂不能用于路由' }
    : { check: 'Check subscription', save: 'Save connection', disabled: 'Save disabled', unknown: 'Visible to this account · capability data pending; not yet routable' }, [language]);

  function selection(candidate: SubscriptionCandidate): Set<string> {
    return new Set(selected[candidate.candidate.candidate_ref] ?? []);
  }

  function toggle(candidate: SubscriptionCandidate, modelRef: string) {
    const next = selection(candidate);
    if (next.has(modelRef)) next.delete(modelRef); else next.add(modelRef);
    setSelected(current => ({ ...current, [candidate.candidate.candidate_ref]: [...next] }));
  }

  return <section className="oc-scan-detail" aria-label={language === 'zh' ? '订阅来源' : 'Subscription sources'}>
    {candidates.map(candidate => {
      const check = checkResults[candidate.candidate.candidate_ref];
      const effective = check?.status === 'verified' && check.checked_candidate
        ? check.checked_candidate
        : candidate;
      const chosen = selection(effective);
      const checking = check?.status === 'checking';
      return <article key={`${candidate.candidate.candidate_ref}:${candidate.candidate.candidate_revision}`}>
        <div className="oc-status-row"><span className="agent-avatar" aria-hidden="true">C</span><div className="row-main"><strong>{effective.display_name}</strong><p>{check ? checkStatusText(check, language) : statusText(effective, language)}</p></div>{check?.status === 'verified' && <span className="badge good">{language === 'zh' ? '可用' : 'Ready'}</span>}{checking && <span className="oc-spinner" />}</div>
        {!!effective.models.length && <p className="oc-meta">{language === 'zh' ? '选择要通过这项订阅接入的模型。' : 'Choose models to connect through this subscription.'}</p>}
        <div className="native-list v3-catalog">{effective.models.map(model => <label className="check-row" key={model.model_ref}>
          <input type="checkbox" disabled={busy || checking || !model.selectable} checked={chosen.has(model.model_ref)} onChange={() => toggle(effective, model.model_ref)} />
          <div><strong>{model.display_name}</strong>{!model.selectable && <span>{labels.unknown}</span>}</div>
        </label>)}</div>
        <div className="task-actions">
          {check?.status !== 'retained' && effective.fact_state === 'pending_approval' && <button className="btn btn-primary" disabled={busy || checking} onClick={() => void onCheck(candidate.candidate)}>{labels.check}</button>}
          {check?.status !== 'retained' && effective.fact_state !== 'pending_approval' && <>
            <button className="btn btn-primary" disabled={busy || !canSave(effective, true, chosen)} onClick={() => void onSave({ candidate: effective.candidate, validation: effective.validation, selected_model_refs: [...chosen], intent: saveIntent(true) })}>{labels.save}</button>
            {!canSave(effective, true, chosen) && canSave(effective, false, chosen) && <button className="btn" disabled={busy} onClick={() => void onSave({ candidate: effective.candidate, validation: effective.validation, selected_model_refs: [...chosen], intent: saveIntent(false) })}>{labels.disabled}</button>}
          </>}
        </div>
      </article>;
    })}
  </section>;
}
