import { Component, type ErrorInfo, type PropsWithChildren } from 'react';
import {
  loadPresentationPreferences,
  resolveLanguage,
  resolveTheme,
  type Language,
  type ResolvedTheme,
  type TextScale,
} from './preferences';
import { PresentationRoot } from './PresentationRoot';
import { UiIcon } from './UiIcon';

type State = {
  failed: boolean;
};

type FallbackPresentation = {
  language: Language;
  theme: ResolvedTheme;
  textScale: TextScale;
};

function fallbackPresentation(): FallbackPresentation {
  let storage: Storage | undefined;
  try {
    storage = window.localStorage;
  } catch {
    storage = undefined;
  }
  const preferences = loadPresentationPreferences(storage);
  return {
    language: resolveLanguage(preferences.language, navigator.languages?.find(Boolean) ?? navigator.language),
    theme: resolveTheme(preferences.theme, window.matchMedia('(prefers-color-scheme: dark)').matches),
    textScale: preferences.textScale,
  };
}

export class RenderFailureBoundary extends Component<PropsWithChildren, State> {
  state: State = { failed: false };

  static getDerivedStateFromError(): State {
    return { failed: true };
  }

  componentDidCatch(error: unknown, info: ErrorInfo) {
    const name = error instanceof Error ? error.name : 'UnknownError';
    console.error('[HiRoute] UI_RENDER_FAILED', name, info.componentStack);
  }

  render() {
    if (!this.state.failed) return this.props.children;

    const presentation = fallbackPresentation();
    const zh = presentation.language === 'zh';
    return <PresentationRoot {...presentation}>
      <main className="fatal-recovery empty-state" role="alert">
        <div>
          <span className="empty-icon"><UiIcon name="warning" /></span>
          <h3>{zh ? '界面暂时无法显示' : 'This page cannot be displayed'}</h3>
          <p>{zh
            ? '当前页面遇到异常。返回首页后可以继续使用。'
            : 'The current page encountered an error. Return home to continue.'}</p>
          <button className="btn btn-primary" type="button" onClick={() => this.setState({ failed: false })}>
            <UiIcon name="home" />
            {zh ? '返回首页' : 'Return home'}
          </button>
        </div>
      </main>
    </PresentationRoot>;
  }
}
