import React, { useState } from 'react';
import { createRoot } from 'react-dom/client';
import {
  Dialog,
  PresentationControls,
  PresentationRoot,
  UiIcon,
  parseLanguage,
  resolveLanguage,
  parseTextScale,
  parseTheme,
  resolveTheme,
  useDiscardGuard,
  type Language,
  type LanguagePreference,
  type TextScale,
  type ThemePreference,
} from '../../../src/ui';
import { Home, HomeNavigation, type HomeAction, type HomeNavigationItem } from '../../../src/features/home';
import { fixture, type ScenarioName } from './fixtures';
import './test-shell.css';

const parameters = new URLSearchParams(window.location.search);
const initialScenario = (['fresh', 'loading', 'partial', 'daily', 'drift'].includes(parameters.get('scenario') || '') ? parameters.get('scenario') : 'daily') as ScenarioName;
const initialLanguage = parseLanguage(parameters.get('lang'), navigator.language);
const initialTheme = parseTheme(parameters.get('theme'));
const initialScale = parseTextScale(parameters.get('scale'));

function navigation(language: Language): HomeNavigationItem[] {
  return language === 'zh'
    ? [
      { id: 'home', label: '首页', icon: 'home' },
      { id: 'models', label: '我的模型', icon: 'models' },
      { id: 'routing', label: '智能路由', icon: 'route' },
      { id: 'agents', label: 'Agent 接入', icon: 'agent' },
      { id: 'tasks', label: '委派任务', icon: 'tasks' },
      { id: 'sessions', label: '会话', icon: 'sessions' },
    ]
    : [
      { id: 'home', label: 'Home', icon: 'home' },
      { id: 'models', label: 'My models', icon: 'models' },
      { id: 'routing', label: 'Smart routing', icon: 'route' },
      { id: 'agents', label: 'Agent access', icon: 'agent' },
      { id: 'tasks', label: 'Delegated tasks', icon: 'tasks' },
      { id: 'sessions', label: 'Sessions', icon: 'sessions' },
    ];
}

function Harness() {
  const [languagePreference, setLanguagePreference] = useState<LanguagePreference>(initialLanguage);
  const language = resolveLanguage(languagePreference, navigator.language);
  const [theme, setTheme] = useState<ThemePreference>(initialTheme);
  const [textScale, setTextScale] = useState<TextScale>(initialScale);
  const [scenario, setScenario] = useState<ScenarioName>(initialScenario);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [editorOpen, setEditorOpen] = useState(false);
  const [draft, setDraft] = useState('');
  const [lastAction, setLastAction] = useState<HomeAction | { kind: 'navigate'; id: string } | null>(null);
  const discard = useDiscardGuard(draft.trim().length > 0, () => setEditorOpen(false));
  const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
  const resolvedTheme = resolveTheme(theme, prefersDark);
  const text = language === 'zh'
    ? { page: '首页', settings: '显示设置', edit: '测试输入保留', close: '关闭', check: '组件检查', scenario: '场景', saveDraft: '保留输入', discardTitle: '放弃未保存输入？', discardBody: '只有这段非敏感测试输入会丢失。', keep: '继续编辑', discard: '放弃输入', draft: '非敏感草稿', draftHint: '切换语言、主题或字号后，这段输入必须仍然存在。' }
    : { page: 'Home', settings: 'Display settings', edit: 'Test input retention', close: 'Close', check: 'Component check', scenario: 'Scenario', saveDraft: 'Keep input', discardTitle: 'Discard unsaved input?', discardBody: 'Only this non-sensitive test input will be lost.', keep: 'Keep editing', discard: 'Discard input', draft: 'Non-sensitive draft', draftHint: 'This input must remain after language, theme, or text-size changes.' };

  function closeEditor() {
    discard.requestClose();
  }

  return (
    <PresentationRoot language={language} theme={resolvedTheme} textScale={textScale}>
      <div className="hr-app-frame">
        <HomeNavigation language={language} items={navigation(language)} current="home" serviceLabel={language === 'zh' ? '服务正常 · 仅在本机运行' : 'Service ready · Running locally'} onNavigate={id => setLastAction({ kind: 'navigate', id })} />
        <div className="hr-app-main">
          <header className="hr-app-bar">
            <div><strong>{text.page}</strong><span className="hr-test-label">{text.check}</span></div>
            <div>
              <label className="hr-test-select"><span>{text.scenario}</span><select aria-label={text.scenario} value={scenario} onChange={event => setScenario(event.target.value as ScenarioName)}><option value="fresh">Fresh</option><option value="loading">Loading</option><option value="partial">Partial</option><option value="daily">Daily</option><option value="drift">Drift + stale activity</option></select></label>
              <button id="open-draft-demo" className="hr-button" type="button" onClick={() => setEditorOpen(true)}><UiIcon name="sessions" />{text.edit}</button>
              <button id="open-preferences" className="hr-button" type="button" onClick={() => setSettingsOpen(true)}><UiIcon name="settings" />{text.settings}</button>
            </div>
          </header>
          <Home language={language} reads={fixture(scenario)} operation={scenario === 'partial' ? { operationId: 'operation/save-b', sequence: 4, state: 'accepted', label: 'Loopback Lab' } : undefined} onAction={action => setLastAction(action)} />
          {lastAction && <output className="hr-test-output" aria-live="polite">{JSON.stringify(lastAction)}</output>}
        </div>
      </div>
      <Dialog open={settingsOpen} title={text.settings} description={language === 'zh' ? '偏好是本机非秘密数据；切换不会重建页面业务状态。' : 'Preferences are non-secret local data. Changes do not recreate page state.'} closeLabel={text.close} onClose={() => setSettingsOpen(false)}>
        <PresentationControls language={language} languagePreference={languagePreference} theme={theme} textScale={textScale} onLanguageChange={setLanguagePreference} onThemeChange={setTheme} onTextScaleChange={setTextScale} />
      </Dialog>
      <Dialog
        open={editorOpen}
        title={text.edit}
        description={text.draftHint}
        closeLabel={text.close}
        onClose={closeEditor}
        footer={discard.confirmationOpen
          ? <><span className="hr-test-confirm-copy" role="alert"><strong>{text.discardTitle}</strong><small>{text.discardBody}</small></span><button className="hr-button" type="button" autoFocus onClick={() => { discard.keepEditing(); requestAnimationFrame(() => document.getElementById('draft-note')?.focus()); }}>{text.keep}</button><button className="hr-button hr-button--primary" type="button" onClick={() => { setDraft(''); discard.discard(); }}>{text.discard}</button></>
          : <button className="hr-button hr-button--primary" type="button" onClick={() => setEditorOpen(false)}>{text.saveDraft}</button>}
      >
        <label className="hr-test-draft"><span>{text.draft}</span><textarea id="draft-note" data-autofocus value={draft} onChange={event => setDraft(event.target.value)} /></label>
        <div className="hr-test-draft-prefs">
          <PresentationControls language={language} languagePreference={languagePreference} theme={theme} textScale={textScale} onLanguageChange={setLanguagePreference} onThemeChange={setTheme} onTextScaleChange={setTextScale} />
        </div>
      </Dialog>
    </PresentationRoot>
  );
}

createRoot(document.getElementById('root')!).render(<Harness />);
