// Documentation-only browser checks. Never calls a real daemon or model provider.
// node tests/decision-docs-capture.mjs CDP_PORT FORWARDED_VITE_URL [--capture]
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { connectPage, navigate, evaluate, waitFor } from './v3/visual/cdp-client.mjs';

const [port, base] = process.argv.slice(2);
assert(port && base, 'Provide an isolated Chrome CDP port and forwarded Vite URL');
const capture = process.argv.includes('--capture');
const assets = new URL('../../../decision-extensions/assets/', import.meta.url);
const client = await connectPage(Number(port));
const keepAlive = setInterval(() => {}, 1000);
const results = [];
let clockScript;
try {
  await client.send('Page.enable');
  await client.send('Emulation.setTimezoneOverride', { timezoneId: 'Asia/Shanghai' });
  // Stable, visibly illustrative dates; also keeps the real seven-day filter repeatable.
  clockScript = await client.send('Page.addScriptToEvaluateOnNewDocument', { source: `
    Date.now = () => Date.UTC(2026, 8, 20, 9, 40);
  ` });
  for (const language of ['zh', 'en']) {
    for (const view of ['quality', 'session', 'config']) {
      const width = view === 'quality' ? 1600 : 1200;
      const height = view === 'quality' ? 880 : view === 'session' ? 400 : language === 'zh' ? 1050 : 980;
      await navigate(client, `${base}/decision-docs.html?lang=${language}&view=${view}`, { width, height });
      await waitFor(client, `location.search.includes('lang=${language}&view=${view}') && !!window.__DECISION_DOCS__`);
      if (view === 'config') {
        await waitFor(client, `!!document.querySelector('input[value="http://127.0.0.1:8080/v1/decisions"]') && window.__DECISION_DOCS__.captureCalls.includes('plan_editor_options') && !document.body.innerText.includes('Loading model') && !document.body.innerText.includes('正在读取模型')`);
      } else {
        await waitFor(client, `document.querySelectorAll('.quality-row').length === ${view === 'quality' ? 4 : 1} && document.querySelector('.quality-row').innerText.includes('${view === 'quality' ? 'Claude Sonnet 5' : 'DeepSeek V4.1 Flash'}')`);
        assert.equal(await evaluate(client, `document.querySelectorAll('.quality-evidence-actions button:not(:disabled)').length`), 0);
        assert.equal(await evaluate(client, `document.querySelectorAll('.quality-reason').length`), 0);
        if (view === 'quality') {
          for (const [filter, count] of [['low', 1], ['high', 2], ['all', 4]]) {
            await evaluate(client, `(() => { const s=document.querySelector('.quality-filters select'); s.value='${filter}';s.dispatchEvent(new Event('change',{bubbles:true})); })()`);
            await waitFor(client, `document.querySelectorAll('.quality-row').length === ${count}`);
          }
          await evaluate(client, `(() => {const s=document.querySelector('.plan-quality').closest('.editor-section'); const pane=document.querySelector('.detail-pane');pane.scrollTop += s.getBoundingClientRect().top - pane.getBoundingClientRect().top - 20;})()`);
          assert(await evaluate(client, `document.querySelector('.quality-list').getBoundingClientRect().bottom <= innerHeight`), 'All score rows fit in the capture');
        }
      }
      await evaluate(client, `document.activeElement?.blur();document.fonts.ready`);
      const state = await evaluate(client, `({overflow:document.documentElement.scrollWidth>innerWidth, errors:[...document.querySelectorAll('[role=alert]')].map(e=>e.innerText), calls:window.__DECISION_DOCS__.captureCalls, label:document.querySelector('.docs-label').innerText})`);
      assert.equal(state.overflow, false);
      assert.deepEqual(state.errors, []);
      assert(state.label.includes(language === 'zh' ? '非真实模型评测' : 'Not a model benchmark'));
      assert(state.calls.every(c => ['observation_read', 'compute_management_snapshot', 'plan_editor_options'].includes(c)));
      if (capture) {
        await client.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 2, mobile: false });
        await evaluate(client, `new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))`);
        const shot = await client.send('Page.captureScreenshot', { format: 'png' });
        fs.writeFileSync(new URL(`${view}-${language === 'zh' ? 'zh-CN' : 'en'}.png`, assets), Buffer.from(shot.data, 'base64'));
      }
      results.push({language, view, status: 'green', readOnlyCalls: state.calls});
    }
  }
  console.log(JSON.stringify({browser:'macOS isolated Chromium; not native Desktop', results}, null, 2));
} finally {
  if (clockScript) await client.send('Page.removeScriptToEvaluateOnNewDocument', {identifier:clockScript.identifier});
  client.close();
  clearInterval(keepAlive);
}
