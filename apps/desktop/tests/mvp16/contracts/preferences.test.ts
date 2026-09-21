import assert from 'node:assert/strict';
import test from 'node:test';
import {
  LANGUAGE_STORAGE_KEY,
  TEXT_SCALE_STORAGE_KEY,
  THEME_STORAGE_KEY,
  loadPresentationPreferences,
  parseLanguagePreference,
  parseTextScale,
  resolveLanguage,
  resolveTheme,
  savePresentationPreferences,
  type StorageLike,
} from '../../../src/ui/preferences.ts';

class MemoryStorage implements StorageLike {
  values = new Map<string, string>();
  getItem(key: string) { return this.values.get(key) ?? null; }
  setItem(key: string, value: string) { this.values.set(key, value); }
}

test('preferences reuse existing language and text-scale keys', () => {
  const storage = new MemoryStorage();
  storage.setItem(LANGUAGE_STORAGE_KEY, 'zh');
  storage.setItem(TEXT_SCALE_STORAGE_KEY, '1.5');
  assert.deepEqual(loadPresentationPreferences(storage), { language: 'zh', theme: 'system', textScale: 1.5 });
});

test('language follows the system until an explicit preference is stored', () => {
  assert.equal(parseLanguagePreference(null), 'system');
  assert.equal(parseLanguagePreference('unknown'), 'system');
  assert.equal(resolveLanguage('system', 'zh-Hans-CN'), 'zh');
  assert.equal(resolveLanguage('system', 'fr-FR'), 'en');
  assert.equal(resolveLanguage('en', 'zh-CN'), 'en');
});

test('system theme follows the OS until an explicit choice is stored', () => {
  assert.equal(resolveTheme('system', true), 'dark');
  assert.equal(resolveTheme('system', false), 'light');
  assert.equal(resolveTheme('light', true), 'light');
});

test('invalid scale fails closed to 100 percent', () => {
  assert.equal(parseTextScale('0'), 1);
  assert.equal(parseTextScale('3'), 1);
  assert.equal(parseTextScale('2'), 2);
});

test('explicit preferences persist without business state', () => {
  const storage = new MemoryStorage();
  savePresentationPreferences(storage, { language: 'en', theme: 'dark', textScale: 2 });
  assert.equal(storage.getItem(LANGUAGE_STORAGE_KEY), 'en');
  assert.equal(storage.getItem(THEME_STORAGE_KEY), 'dark');
  assert.equal(storage.getItem(TEXT_SCALE_STORAGE_KEY), '2');
  assert.equal(storage.values.size, 3);
});

test('restoring the system language preference is persisted', () => {
  const storage = new MemoryStorage();
  savePresentationPreferences(storage, { language: 'system', theme: 'system', textScale: 1 });
  assert.equal(storage.getItem(LANGUAGE_STORAGE_KEY), 'system');
});
