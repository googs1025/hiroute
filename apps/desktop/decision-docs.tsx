// Documentation capture only. Real components with synthetic, read-only IPC.
// Not referenced by the production entry or default build.
import React from 'react';
import { createRoot } from 'react-dom/client';
import { PlanQuality } from './src/features/PlanQuality';
import { PlanEditor } from './src/plan-editor';
import { RoutingPage } from './src/product/RoutingPage';
import { HomeNavigation } from './src/features/home/HomeNavigation';
import { PresentationRoot } from './src/ui/PresentationRoot';
import { decisionDocsData, queryDocsSamples, type DocsQualityQuery } from './decision-docs-data';
import './src/occami/styles.css';

const params = new URLSearchParams(location.search);
const language = params.get('lang') === 'en' ? 'en' : 'zh';
const view = params.get('view') ?? 'quality';
const text = (zh: string, english: string) => language === 'en' ? english : zh;
const { models, plan, samples, snapshot } = decisionDocsData(language);
const captureCalls: string[] = [];
Object.assign(window, { __DECISION_DOCS__: { samples, captureCalls }, __TAURI_INTERNALS__: {
  invoke: async (command: string, args?: { request?: { intent: { view: string; query: DocsQualityQuery } } }) => {
    captureCalls.push(command);
    if (command === 'plan_editor_options') return { suggested_alias: null, free_suggestions: null, codex_capabilities: null,
      candidates: models.map(model => ({ ...model, binding_id: model.model_configuration_id,
        reasoning: { kind: 'fixed', profile: 'default' }, billing_class: 'paid', routable: true,
        ingress_protocols: ['responses', 'messages', 'chat_completions'] })) };
    if (command === 'compute_management_snapshot') return { sources: [{ display_name: 'OpenRouter', models: models.map(m => ({ binding_id: m.model_configuration_id })) }] };
    if (command === 'observation_read' && args?.request?.intent.view === 'plan_quality') {
      return queryDocsSamples(samples, args.request.intent.query);
    }
    throw new Error('DOCUMENTATION_DEMO_NO_LIVE_ACTIONS');
  },
} });

const label = text('演示数据 · 非真实模型评测', 'Illustrative data · Not a model benchmark');
const style = document.createElement('style');
// Only the canvas/crop is styled here. Product components retain their real UI.
style.textContent = view === 'quality' ? `
  html,body,#root {height:100%;overflow:hidden}
  .docs-capture {height:100%;padding:16px;background:#e6e9f1}
  .docs-capture .app-window {border:1px solid var(--border);border-radius:12px;overflow:hidden}
  .docs-label {font-size:11px;color:var(--text-muted);font-weight:400}
` : `
  html,body,#root {height:auto;min-height:0;overflow:visible}
  body.hr-ui.occami-root {display:block;padding:28px;background:#f4f5f9} #root {max-width:1120px;margin:auto}
  .docs-label {font:13px -apple-system,sans-serif;color:#64697c;margin:0 0 14px}
  .docs-panel {background:var(--surface,#fff);padding:24px;border-radius:14px}
  .plan-editor {height:auto;overflow:visible}
  ${view === 'config' ? '.plan-editor>header,.plan-editor>fieldset>*:not(:has([data-route-group="classifier"])),.editor-section:has([data-route-group="classifier"])>:nth-child(-n+3),.editor-section:has([data-route-group="classifier"])>.keywords {display:none}' : ''}`;
document.head.append(style);

createRoot(document.getElementById('root')!).render(view === 'quality' ?
  <PresentationRoot language={language} theme="light" textScale={1} className="docs-capture">
    <div className="app-window">
      <header className="titlebar"><div className="window-controls" aria-hidden="true" /><div className="titlebar-main"><div className="window-title">{text('智能路由', 'Smart routing')}</div><span className="docs-label">{label}</span></div></header>
      <HomeNavigation language={language} current="routing" serviceReady serviceLabel={text('本机服务', 'Local service')}
        items={[
          { id: 'home', label: text('首页', 'Home'), icon: 'home' },
          { id: 'models', label: text('模型', 'Models'), icon: 'models' },
          { id: 'routing', label: text('智能路由', 'Smart routing'), icon: 'route' },
          { id: 'agents', label: 'Agent', icon: 'agent' },
          { id: 'sessions', label: text('会话', 'Sessions'), icon: 'sessions' },
        ]} onNavigate={() => {}} onOpenSettings={() => {}} />
      <main className="main"><RoutingPage language={language} snapshot={snapshot} loading={false} busy={false}
        initialEditor={{ key: plan.agent_plan_id, plan }} onRefresh={async () => {}} onOperation={() => {}} /></main>
    </div>
  </PresentationRoot> : <>
    <p className="docs-label">HiRoute · {label}</p>
    <main className="docs-panel">
      {view === 'config' ? <PlanEditor plan={plan} language={language} onDone={async () => {}} onClose={() => {}} /> :
        <section className="editor-section"><div className="editor-section-heading"><div>
          <h3>{text('模型表现', 'Model performance')}</h3>
          <p>{text('订单对账排错 · 阶段评分及其覆盖范围。', 'Order reconciliation debugging · Stage assessment and coverage.')}</p>
        </div></div><PlanQuality sessionId="session-order-reconciliation" currentModels={models} language={language} compact /></section>}
    </main>
  </>);
