import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { guidePages, guideRoute, renderUserGuide } from '../src/lib/user-guides.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

test('every public user guide renders in Chinese and English', async () => {
  for (const slug of Object.keys(guidePages)) {
    for (const language of ['zh', 'en']) {
      const html = await renderUserGuide(slug, language);
      assert.match(html, /<h1[^>]*>/, `${slug}.${language} needs a title`);
      assert.match(html, /<h2[^>]*>/, `${slug}.${language} needs actionable sections`);
    }
  }
  assert.equal(guideRoute('quickstart', 'zh'), '/docs/');
  assert.equal(guideRoute('cli', 'en'), '/en/docs/cli/');
});

test('guides cover Desktop, Linux headless, and the released management CLI', async () => {
  const installZh = await renderUserGuide('install-macos', 'zh');
  const installEn = await renderUserGuide('install-macos', 'en');
  const linuxZh = await renderUserGuide('install-linux', 'zh');
  const linuxEn = await renderUserGuide('install-linux', 'en');
  const routing = await renderUserGuide('model-routing', 'zh');
  const worker = await renderUserGuide('cli', 'en');
  assert.match(installZh, /自签名/);
  assert.match(installZh, /仍要打开/);
  assert.match(installEn, /Open Anyway/);
  assert.match(linuxZh, /curl -fsSL https:\/\/hiroute\.ai\/install\.sh \| sh/);
  assert.match(linuxEn, /hiroute service start --output json/);
  assert.match(linuxEn, /hiroute service run/);
  assert.match(linuxEn, /hiroute compute scan --output json/);
  assert.match(linuxEn, /compute connection options\/test\/preview\/apply/);
  assert.match(linuxEn, /<code>authorize<\/code>/);
  assert.match(routing, /固定模型/);
  assert.match(routing, /智能省钱/);
  assert.match(routing, /免费优先/);
  assert.match(worker, /hiroute worker exec/);
  assert.match(worker, /--submission my-check-001 --operation start/);
  assert.match(worker, /routing options\/list\/show\/preview\/apply/);
  assert.match(worker, /sessions list/);

  const sources = await Promise.all(Object.keys(guidePages).flatMap(slug => ['zh', 'en'].map(language =>
    fs.readFile(path.join(root, 'content/guides', `${slug}.${language}.md`), 'utf8'))));
  const all = sources.join('\n');
  assert.match(all, /standalone/i);
  assert.doesNotMatch(all, /commands for creating model sources, editing routes, or connecting agents are available yet/i);
  assert.doesNotMatch(all, /这些配置入口仍由 Desktop 提供/);
});
