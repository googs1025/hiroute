import assert from 'node:assert/strict';
import test from 'node:test';
import {
  LANGUAGE_STORAGE_KEY,
  loadPresentationPreferences,
  parseLanguagePreference,
  resolveLanguage,
  savePresentationPreferences,
} from '../src/ui/preferences.ts';

class MemoryStorage {
  values = new Map();
  getItem(key) { return this.values.get(key) ?? null; }
  setItem(key, value) { this.values.set(key, value); }
}

test('missing and unknown language preferences follow the system deterministically', () => {
  assert.equal(parseLanguagePreference(null), 'system');
  assert.equal(parseLanguagePreference('fr'), 'system');
  assert.equal(resolveLanguage('system', 'zh-Hans'), 'zh');
  assert.equal(resolveLanguage('system', 'en-GB'), 'en');
  assert.equal(resolveLanguage('system', 'fr-FR'), 'en');
});

test('explicit language survives a conflicting system language', () => {
  assert.equal(resolveLanguage('en', 'zh-CN'), 'en');
  assert.equal(resolveLanguage('zh', 'en-US'), 'zh');
});

test('system is a persisted user choice rather than a resolved language', () => {
  const storage = new MemoryStorage();
  savePresentationPreferences(storage, { language: 'system', theme: 'system', textScale: 1 });
  assert.equal(storage.getItem(LANGUAGE_STORAGE_KEY), 'system');
  assert.equal(loadPresentationPreferences(storage).language, 'system');
});

test('unavailable storage keeps safe in-session defaults', () => {
  const storage = {
    getItem() { throw new Error('denied'); },
    setItem() { throw new Error('denied'); },
  };
  assert.deepEqual(loadPresentationPreferences(storage), { language: 'system', theme: 'system', textScale: 1 });
  assert.doesNotThrow(() => savePresentationPreferences(storage, { language: 'zh', theme: 'dark', textScale: 2 }));
});
