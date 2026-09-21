import { useState } from 'react';
import { UiIcon } from './UiIcon';
import { connectionTemplate } from './provider-identity';

const assets = import.meta.glob('./assets/providers/*.svg', { eager: true, query: '?url', import: 'default' }) as Record<string, string>;
export function ProviderIcon({ optionId, language }: { optionId?: string | null; language: 'zh' | 'en' }) {
  const provider = connectionTemplate(optionId);
  const [failed, setFailed] = useState('');
  const asset = provider && assets[`./assets/providers/${provider.provider_id}.svg`];
  const label = provider?.name[language] || (language === 'zh' ? 'API 供应商' : 'API provider');
  return <span className="model-avatar provider-icon" role="img" aria-label={label}>
    {asset && failed !== asset ? <img src={asset} alt="" width="28" height="28" onError={() => setFailed(asset)} /> : <UiIcon name="models" />}
  </span>;
}
