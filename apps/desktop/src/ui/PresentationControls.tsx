import type { Language, LanguagePreference, TextScale, ThemePreference } from './preferences';

const copy = {
  zh: {
    language: '显示语言', theme: '外观', scale: '文字大小', system: '跟随系统', light: '浅色', dark: '深色',
  },
  en: {
    language: 'Display language', theme: 'Appearance', scale: 'Text size', system: 'System', light: 'Light', dark: 'Dark',
  },
};

export function PresentationControls({
  language,
  languagePreference,
  theme,
  textScale,
  onLanguageChange,
  onThemeChange,
  onTextScaleChange,
}: {
  language: Language;
  languagePreference: LanguagePreference;
  theme: ThemePreference;
  textScale: TextScale;
  onLanguageChange: (language: LanguagePreference) => void;
  onThemeChange: (theme: ThemePreference) => void;
  onTextScaleChange: (scale: TextScale) => void;
}) {
  const text = copy[language];
  return (
    <div className="hr-preferences">
      <label>
        <span>{text.language}</span>
        <select value={languagePreference} onChange={event => onLanguageChange(event.target.value as LanguagePreference)}>
          <option value="system">{text.system}</option>
          <option value="zh">简体中文</option>
          <option value="en">English</option>
        </select>
      </label>
      <fieldset>
        <legend>{text.theme}</legend>
        <div className="hr-segmented">
          {(['system', 'light', 'dark'] as ThemePreference[]).map(value => (
            <button key={value} type="button" aria-pressed={theme === value} onClick={() => onThemeChange(value)}>
              {text[value]}
            </button>
          ))}
        </div>
      </fieldset>
      <label>
        <span>{text.scale}</span>
        <select value={String(textScale)} onChange={event => onTextScaleChange(Number(event.target.value) as TextScale)}>
          <option value="1">100%</option>
          <option value="1.5">150%</option>
          <option value="2">200%</option>
        </select>
      </label>
    </div>
  );
}
