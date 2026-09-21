import type { ManagedSource, ProtectedKeyInput } from './types';

export type DraftKey = {
  draftId: string;
  keyId?: string;
  expectedGeneration: number;
  fingerprintHint: string;
  enabled: boolean;
  removed: boolean;
  replacement: string;
};

export type ModelDraft = {
  selectedModels: string[];
  enabled: boolean;
  keys: DraftKey[];
  addedSecret: string;
};

export function createDraft(source: ManagedSource): ModelDraft {
  return {
    selectedModels: source.models.map(model => model.model_ref),
    enabled: source.state !== 'disabled',
    keys: source.keys.map(key => ({
      draftId: key.key_id,
      keyId: key.key_id,
      expectedGeneration: key.generation,
      fingerprintHint: key.fingerprint_hint,
      enabled: key.enabled,
      removed: false,
      replacement: '',
    })),
    addedSecret: '',
  };
}

export function addDraftKey(draft: ModelDraft): ModelDraft {
  const value = draft.addedSecret;
  if (!value.trim()) return draft;
  return {
    ...draft,
    addedSecret: '',
    keys: [...draft.keys, {
      draftId: crypto.randomUUID(),
      expectedGeneration: 0,
      fingerprintHint: '待保存',
      enabled: true,
      removed: false,
      replacement: value,
    }],
  };
}

export function moveDraftKey(draft: ModelDraft, draftId: string, offset: -1 | 1): ModelDraft {
  const keys = [...draft.keys];
  const from = keys.findIndex(key => key.draftId === draftId);
  const to = from + offset;
  if (from < 0 || to < 0 || to >= keys.length) return draft;
  [keys[from], keys[to]] = [keys[to], keys[from]];
  return { ...draft, keys };
}

export function protectedInputs(draft: ModelDraft): ProtectedKeyInput[] {
  return draft.keys
    .filter(key => !key.removed && key.replacement.length > 0)
    .map(key => ({
      draft_id: key.draftId,
      key_id: key.keyId,
      expected_generation: key.expectedGeneration,
      value: key.replacement,
    }));
}

export function clearSecrets(draft: ModelDraft): ModelDraft {
  return {
    ...draft,
    addedSecret: '',
    keys: draft.keys.map(key => ({ ...key, replacement: '' })),
  };
}
