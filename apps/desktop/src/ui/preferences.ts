import { useEffect, useMemo, useState } from 'react';

export type Language = 'zh' | 'en';
export type LanguagePreference = 'system' | Language;
export type ThemePreference = 'system' | 'light' | 'dark';
export type ResolvedTheme = 'light' | 'dark';
export type TextScale = 1 | 1.5 | 2;

export type PresentationPreferences = {
  language: LanguagePreference;
  theme: ThemePreference;
  textScale: TextScale;
};

export type PresentationPreferenceController = Omit<PresentationPreferences, 'language'> & {
  language: Language;
  languagePreference: LanguagePreference;
  resolvedTheme: ResolvedTheme;
  setLanguage: (language: LanguagePreference) => void;
  setTheme: (theme: ThemePreference) => void;
  setTextScale: (scale: TextScale) => void;
};

export type StorageLike = Pick<Storage, 'getItem' | 'setItem'>;

export const LANGUAGE_STORAGE_KEY = 'hiroute.language';
export const THEME_STORAGE_KEY = 'hiroute.theme';
export const TEXT_SCALE_STORAGE_KEY = 'hiroute.text-scale';

export function parseLanguage(value: string | null | undefined, fallback = 'en'): Language {
  const candidate = value ?? fallback;
  return candidate.toLowerCase().startsWith('zh') ? 'zh' : 'en';
}

export function parseLanguagePreference(value: string | null | undefined): LanguagePreference {
  return value === 'zh' || value === 'en' || value === 'system' ? value : 'system';
}

export function resolveLanguage(preference: LanguagePreference, systemLanguage = 'en'): Language {
  return preference === 'system' ? parseLanguage(systemLanguage) : preference;
}

export function parseTheme(value: string | null | undefined): ThemePreference {
  return value === 'light' || value === 'dark' || value === 'system' ? value : 'system';
}

export function parseTextScale(value: string | number | null | undefined): TextScale {
  const parsed = Number(value);
  return parsed === 1.5 || parsed === 2 ? parsed : 1;
}

export function resolveTheme(theme: ThemePreference, prefersDark: boolean): ResolvedTheme {
  return theme === 'system' ? (prefersDark ? 'dark' : 'light') : theme;
}

export function loadPresentationPreferences(
  storage?: StorageLike,
): PresentationPreferences {
  let language: string | null = null;
  let theme: string | null = null;
  let textScale: string | null = null;
  try {
    language = storage?.getItem(LANGUAGE_STORAGE_KEY) ?? null;
    theme = storage?.getItem(THEME_STORAGE_KEY) ?? null;
    textScale = storage?.getItem(TEXT_SCALE_STORAGE_KEY) ?? null;
  } catch {
    // A denied or damaged localStorage must not prevent the UI from starting.
  }
  return {
    language: parseLanguagePreference(language),
    theme: parseTheme(theme),
    textScale: parseTextScale(textScale),
  };
}

export function savePresentationPreferences(
  storage: StorageLike | undefined,
  preferences: PresentationPreferences,
): void {
  if (!storage) return;
  try {
    storage.setItem(LANGUAGE_STORAGE_KEY, preferences.language);
    storage.setItem(THEME_STORAGE_KEY, preferences.theme);
    storage.setItem(TEXT_SCALE_STORAGE_KEY, String(preferences.textScale));
  } catch {
    // Preferences remain effective for this session when persistence is unavailable.
  }
}

function browserStorage(): StorageLike | undefined {
  if (typeof window === 'undefined') return undefined;
  try {
    return window.localStorage;
  } catch {
    return undefined;
  }
}

function browserLanguage(): string {
  if (typeof navigator === 'undefined') return 'en';
  return navigator.languages?.find(Boolean) ?? navigator.language ?? 'en';
}

function browserPrefersDark(): boolean {
  return typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: dark)').matches;
}

export function usePresentationPreferences(
  storage: StorageLike | undefined = browserStorage(),
): PresentationPreferenceController {
  const [preferences, setPreferences] = useState<PresentationPreferences>(() =>
    loadPresentationPreferences(storage),
  );
  const [prefersDark, setPrefersDark] = useState(browserPrefersDark);
  const [systemLanguage, setSystemLanguage] = useState(browserLanguage);

  useEffect(() => {
    savePresentationPreferences(storage, preferences);
  }, [preferences, storage]);

  useEffect(() => {
    if (typeof window === 'undefined') return;
    const query = window.matchMedia('(prefers-color-scheme: dark)');
    const update = (event: MediaQueryListEvent | MediaQueryList) => setPrefersDark(event.matches);
    update(query);
    query.addEventListener('change', update);
    return () => query.removeEventListener('change', update);
  }, []);

  useEffect(() => {
    if (typeof window === 'undefined') return;
    const update = () => setSystemLanguage(browserLanguage());
    window.addEventListener('languagechange', update);
    return () => window.removeEventListener('languagechange', update);
  }, []);

  return useMemo(() => ({
    language: resolveLanguage(preferences.language, systemLanguage),
    languagePreference: preferences.language,
    theme: preferences.theme,
    textScale: preferences.textScale,
    resolvedTheme: resolveTheme(preferences.theme, prefersDark),
    setLanguage: (language: LanguagePreference) => setPreferences(current => ({ ...current, language })),
    setTheme: (theme: ThemePreference) => setPreferences(current => ({ ...current, theme })),
    setTextScale: (textScale: TextScale) => setPreferences(current => ({ ...current, textScale })),
  }), [preferences, prefersDark, systemLanguage]);
}
