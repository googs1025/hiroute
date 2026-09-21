import type { Language, ModelReference, NativeConfiguration, Rating, RatingValue } from './types';
import { Disclosure } from '../../ui';
import './model-reference.css';
function configuration(value: NativeConfiguration, language: Language): string {
  switch (value.kind) {
    case 'fixed': case 'profile': return value.profile;
    case 'budget': return `${value.tokens} tokens`;
    case 'toggle': return language === 'zh' ? (value.enabled ? '思考开启' : '思考关闭') : (value.enabled ? 'Thinking on' : 'Thinking off');
  }
}
function Score({ value, language }: { value: RatingValue; language: Language }) {
  return <span>{value.state === 'unknown' ? '—' : (value.score_tenths / 10).toFixed(1)}
    {value.state === 'estimated' && <small className="mr-badge">{language === 'zh' ? '暂估' : 'Estimated'}</small>}
    {value.state === 'unknown' && <small className="mr-badge">{language === 'zh' ? '未知' : 'Unknown'}</small>}</span>;
}
/** A fragment for the model page and Plan editor; it does not choose or reorder models. */
export function ReferenceDetails({ model, rating, language }: { model: ModelReference; rating: Rating | null; language: Language }) {
  const zh = language === 'zh';
  const state = { known_supported: zh ? '参考支持' : 'Catalog supported', known_unsupported: zh ? '参考不支持' : 'Catalog unsupported', unknown: zh ? '未知' : 'Unknown' };
  return <section className="model-reference" aria-label={zh ? '模型参考资料' : 'Model reference'}>
    <h3>{model.display_name}</h3><p>{model.publisher_id}</p>
    {rating && <><p>{zh ? '原生配置' : 'Native configuration'}: <strong>{configuration(rating.requested_configuration, language)}</strong></p>
      <dl className="mr-grid">{(['overall', 'coding', 'tool'] as const).map((dimension, index) => <div key={dimension}>
        <dt>{(zh ? ['综合', '编程', '工具'] : ['Overall', 'Coding', 'Tools'])[index]}</dt><dd><Score value={rating[dimension]} language={language}/></dd>
      </div>)}</dl></>}
    <dl className="mr-grid">{(['tool', 'vision', 'streaming'] as const).map((key, i) => <div key={key}><dt>{(zh ? ['工具调用', '视觉', '流式输出'] : ['Tools', 'Vision', 'Streaming'])[i]}</dt><dd>{state[model[key].state]}</dd></div>)}
      <div><dt>Context</dt><dd>{model.context_tokens ?? '—'} tokens</dd></div><div><dt>Max output</dt><dd>{model.max_output_tokens ?? '—'} tokens</dd></div>
    </dl>
    <p>{zh ? '参考资料不代表当前来源已验证支持或已就绪。' : 'Catalog reference does not establish source support or readiness.'}</p>
    <Disclosure label={zh ? '版本与依据' : 'Versions and evidence'} language={language}><dl>
      <dt>{zh ? '模型身份' : 'Model identity'}</dt><dd>{model.model_configuration_id} · {model.model_revision}</dd>
      <dt>{zh ? '资料版本' : 'Data version'}</dt><dd>{model.data_version}</dd>
      {model.rating_snapshots.map(s => <div key={s.digest}><dt>{s.version} · {s.scale_version}</dt><dd>{s.digest}</dd></div>)}
      {rating && (['overall', 'coding', 'tool'] as const).map(key => { const v = rating[key]; return <div key={key}><dt>{key}</dt><dd>{v.state === 'unknown' ? v.reason : `${v.evidence_ref} · ${v.method_revision}`}</dd></div>; })}
      {(['tool', 'vision', 'streaming'] as const).map(key => <div key={key}><dt>{key}</dt><dd>{model[key].evidence_kind}</dd></div>)}
    </dl></Disclosure>
  </section>;
}
