const c = () => window.sessionWindow;
const tick = () => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
const pause = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const text = () => document.body.innerText;
const visible = element => Boolean(element && element.getClientRects().length > 0);
const rows = () => [...document.querySelectorAll('.session-item')];
const button = label => [...document.querySelectorAll('button')].find(element => visible(element) && element.textContent.trim() === label);
const reads = view => c().commands.filter(item => item.command === 'observation_read' && item.payload.request?.intent?.view === view);
const queryOf = call => call.payload.request.intent.query;
const deferred = () => { let resolve; let reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
async function until(predicate, label) {
  const deadline = Date.now() + 5000;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error(`Timed out: ${label}`);
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  await tick();
}
async function fresh(setup = () => {}) {
  c().reset();
  setup(c());
  await tick();
  await until(() => reads('sessions').length > 0, 'initial sessions query');
  await tick();
}
async function type(value) {
  const input = document.querySelector('.session-filters input.input');
  assert(input, 'Search input missing');
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, value);
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await pause(340);
}
function seedSimple(control, at = Date.now() - 60_000, overrides = {}) {
  control.requests['observation-session/simple'] = [control.makeRequest('observation-session/simple', 0, at, overrides)];
}
function seedLater(control, at) {
  control.requests['observation-session/later'] = [control.makeRequest('observation-session/later', 0, at)];
}
function seedUserText(control, request, textValue) {
  const contentId = `${request.request_id}/user`;
  control.catalog[request.request_id] = [{ content_id: contentId, role: 'user', state: 'complete', direction: 'ingress', media_type: 'text/plain', message_occurrence_id: '1', message_ordinal: 1, part_ordinal: 1 }];
  control.texts[contentId] = textValue;
}

const scenarios = [
  ['a session that starts after mount appears on an explicit re-query', async () => {
    await fresh(control => { seedSimple(control); });
    await until(() => rows().length === 1, 'initial row');
    const mountWindow = queryOf(reads('sessions')[0]);
    seedLater(c(), Date.now() + 30);
    await pause(160);
    button('全部').click();
    await until(() => rows().length === 2, 'post-mount session after re-query');
    const latest = queryOf(reads('sessions').at(-1));
    assert(latest.to_ms > mountWindow.to_ms, 'Re-query reused the mount-time upper bound');
  }],
  ['list pagination keeps the pinned window', async () => {
    await fresh(control => {
      for (let index = 0; index < 60; index += 1) control.requests[`observation-session/bulk-${index}`] = [control.makeRequest(`observation-session/bulk-${index}`, 0, Date.now() - 120_000 + index)];
    });
    await until(() => rows().length === 50 && button('加载更多会话'), 'first list page');
    const first = queryOf(reads('sessions')[0]);
    button('加载更多会话').click();
    await until(() => rows().length === 60, 'second list page');
    const second = queryOf(reads('sessions').at(-1));
    assert(second.from_ms === first.from_ms && second.to_ms === first.to_ms, 'Pagination changed the pinned window');
    assert(!document.querySelector('[data-error-code]'), 'Pagination surfaced an error');
  }],
  ['keyword search starts a new window and keeps it while paging', async () => {
    await fresh(control => {
      seedSimple(control);
      control.texts['content/seed'] = '关于路由编译边界的说明';
      control.hits = Array.from({ length: 51 }, (_, index) => {
        control.texts[`content/hit-${index}`] = '关于路由编译边界的说明';
        return { session_id: 'observation-session/simple', request_id: 'request/simple/0', content_id: `content/hit-${index}`, original_text_offset: 0 };
      });
    });
    await until(() => rows().length === 1, 'initial row');
    const mountWindow = queryOf(reads('sessions')[0]);
    await type('路由');
    await until(() => reads('search').length === 1, 'search query');
    const searchWindow = queryOf(reads('search')[0]);
    assert(searchWindow.to_ms > mountWindow.to_ms, 'Search reused the mount-time upper bound');
    await until(() => button('加载更多结果'), 'search pagination');
    button('加载更多结果').click();
    await until(() => reads('search').length === 2, 'second search page');
    const second = queryOf(reads('search').at(-1));
    assert(second.from_ms === searchWindow.from_ms && second.to_ms === searchWindow.to_ms, 'Search pagination recomputed the window');
    assert(!document.querySelector('[data-error-code]'), 'Search pagination surfaced an error');
  }],
  ['refreshVersion re-queries with a fresh window', async () => {
    await fresh(control => { seedSimple(control); });
    const mountWindow = queryOf(reads('sessions')[0]);
    seedLater(c(), Date.now() + 30);
    await pause(160);
    c().bumpRefresh();
    await until(() => rows().length === 2, 'refresh picked up the later session');
    const latest = queryOf(reads('sessions').at(-1));
    assert(latest.to_ms > mountWindow.to_ms, 'Refresh reused the mount-time window');
  }],
  ['a late response cannot replace a newer window result', async () => {
    await fresh(control => { seedSimple(control); });
    const late = deferred();
    const before = reads('sessions').length;
    c().views.sessions = () => late.promise;
    c().bumpRefresh();
    await until(() => reads('sessions').length > before, 'slow cycle entered');
    c().views.sessions = () => ({ next_cursor: null, sessions: [{ session_id: 'observation-session/new', agent_id: '', request_count: 2, fallback_request_count: 0, first_request_at_ms: Date.now() - 5_000, last_request_at_ms: Date.now() - 4_000, unknown_model_request_count: 0, correlation_kind: 'agent_supplied' }] });
    c().bumpRefresh();
    await until(() => rows().length === 1 && text().includes('2 个请求'), 'newer cycle rendered');
    late.resolve({ next_cursor: null, sessions: [{ session_id: 'observation-session/stale', agent_id: '', request_count: 9, fallback_request_count: 0, first_request_at_ms: Date.now() - 9_000, last_request_at_ms: Date.now() - 8_000, unknown_model_request_count: 0, correlation_kind: 'agent_supplied' }] });
    await pause(60);
    assert(rows().length === 1 && !text().includes('9 个请求'), 'Late response replaced the newer result');
    assert(!document.querySelector('[data-error-code]'), 'Late response produced an error');
  }],
  ['a failed refresh keeps loaded rows and retry uses a fresh window', async () => {
    await fresh(control => { seedSimple(control); });
    const mountWindow = queryOf(reads('sessions')[0]);
    c().views.sessions = () => { throw { code: 'DAEMON_UNAVAILABLE' }; };
    c().bumpRefresh();
    await until(() => document.querySelector('[data-error-code="DAEMON_UNAVAILABLE"]'), 'list error shown');
    assert(rows().length === 1, 'Failed refresh dropped the loaded rows');
    delete c().views.sessions;
    seedLater(c(), Date.now() + 30);
    await pause(160);
    button('重试').click();
    await until(() => rows().length === 2, 'retry picked up the later session');
    const latest = queryOf(reads('sessions').at(-1));
    assert(latest.to_ms > mountWindow.to_ms, 'Retry reused the mount-time window');
  }],
  ['a failed session without captured content still renders and lists its requests', async () => {
    await fresh(control => {
      const at = Date.now() - 60_000;
      control.requests['observation-session/complex'] = Array.from({ length: 30 }, (_, index) => control.makeRequest('observation-session/complex', index, at + index * 1000, { outcome: 'failed', attempted_model_count: 0 }));
    });
    await until(() => rows().length === 1, 'complex session row');
    await until(() => rows()[0].querySelector('.badge')?.textContent.includes('内容不完整'), 'incomplete-content badge');
    rows()[0].click();
    await until(() => document.querySelector('.request-timeline summary')?.textContent.includes('30'), 'full request list');
    assert(document.querySelectorAll('.request-timeline .list-row').length === 30, 'Failed requests were filtered out');
    assert(!document.querySelector('[data-error-code]'), 'Failed session rendered an error');
  }],
  ['timeline pagination reuses the pinned query instead of recomputing now', async () => {
    await fresh(control => {
      const at = Date.now() - 120_000;
      control.requests['observation-session/long'] = Array.from({ length: 60 }, (_, index) => control.makeRequest('observation-session/long', index, at + index * 1000));
    });
    await until(() => rows().length === 1, 'long session row');
    rows()[0].click();
    await until(() => document.querySelector('.request-timeline summary')?.textContent.includes('50'), 'first timeline page');
    const first = queryOf(reads('timeline').filter(call => !queryOf(call).cursor).at(-1));
    const details = document.querySelector('.request-timeline');
    details.open = true;
    await tick();
    const more = [...details.querySelectorAll('button')].find(element => element.textContent.trim() === '加载更多请求');
    assert(more, 'Timeline load-more control missing');
    more.click();
    await until(() => document.querySelector('.request-timeline summary')?.textContent.includes('60') || Boolean(document.querySelector('[data-error-code]')), 'second timeline page or an error');
    const failure = document.querySelector('[data-error-code]')?.getAttribute('data-error-code');
    assert(!failure, `Timeline pagination surfaced ${failure}`);
    const second = queryOf(reads('timeline').at(-1));
    assert(second.from_ms === first.from_ms && second.to_ms === first.to_ms, 'Timeline pagination recomputed the window');
  }],
  ['an empty window shows first use and recovers on refresh', async () => {
    await fresh();
    await until(() => text().includes('还没有会话记录'), 'empty first-use state');
    assert(!document.querySelector('[data-error-code]'), 'Empty state reported an error');
    seedSimple(c(), Date.now() - 30_000);
    c().bumpRefresh();
    await until(() => rows().length === 1, 'row after refresh');
  }],
  ['a failed list re-query cannot offer its previous cursor for pagination', async () => {
    await fresh(control => {
      for (let index = 0; index < 60; index += 1) control.requests[`observation-session/bulk-${index}`] = [control.makeRequest(`observation-session/bulk-${index}`, 0, Date.now() - 120_000 + index)];
    });
    await until(() => rows().length === 50 && button('加载更多会话'), 'first list page');
    c().views.sessions = () => { throw { code: 'DAEMON_UNAVAILABLE' }; };
    c().bumpRefresh();
    await until(() => document.querySelector('[data-error-code="DAEMON_UNAVAILABLE"]'), 'failed re-query error');
    assert(rows().length === 50, 'Failed re-query dropped the loaded rows');
    delete c().views.sessions;
    const stalePager = button('加载更多会话');
    if (stalePager) {
      stalePager.click();
      await pause(150);
      const failure = document.querySelector('[data-error-code]')?.getAttribute('data-error-code');
      assert(failure !== 'CHANGE_PREVIEW_STALE', `A list cursor from before the failed re-query stayed clickable and surfaced ${failure}`);
    }
    button('重试').click();
    await until(() => !document.querySelector('[data-error-code]') && button('加载更多会话'), 'retry recovered');
    const retried = queryOf(reads('sessions').at(-1));
    button('加载更多会话').click();
    await until(() => rows().length === 60, 'second page after retry');
    const paged = queryOf(reads('sessions').at(-1));
    assert(paged.from_ms === retried.from_ms && paged.to_ms === retried.to_ms, 'Pagination after retry did not reuse the recovered window');
    assert(!document.querySelector('[data-error-code]'), 'Recovered pagination surfaced an error');
  }],
  ['a failed search re-query cannot offer its previous cursor for pagination', async () => {
    await fresh(control => {
      seedSimple(control);
      control.texts['content/seed'] = '关于路由编译边界的说明';
      control.hits = Array.from({ length: 51 }, (_, index) => {
        control.texts[`content/hit-${index}`] = '关于路由编译边界的说明';
        return { session_id: 'observation-session/simple', request_id: 'request/simple/0', content_id: `content/hit-${index}`, original_text_offset: 0 };
      });
    });
    await until(() => rows().length === 1, 'initial row');
    await type('路由');
    await until(() => reads('search').length === 1, 'search query');
    await until(() => button('加载更多结果'), 'search pagination');
    c().views.search = () => { throw { code: 'DAEMON_UNAVAILABLE' }; };
    c().bumpRefresh();
    await until(() => document.querySelector('[data-error-code="DAEMON_UNAVAILABLE"]'), 'failed search re-query error');
    assert(rows().length === 50, 'Failed search re-query dropped the loaded hits');
    delete c().views.search;
    const stalePager = button('加载更多结果');
    if (stalePager) {
      stalePager.click();
      await pause(150);
      const failure = document.querySelector('[data-error-code]')?.getAttribute('data-error-code');
      assert(failure !== 'CHANGE_PREVIEW_STALE', `A search cursor from before the failed re-query stayed clickable and surfaced ${failure}`);
    }
    button('重试').click();
    await until(() => !document.querySelector('[data-error-code]') && button('加载更多结果'), 'retry recovered');
    const retried = queryOf(reads('search').at(-1));
    button('加载更多结果').click();
    await until(() => rows().length === 51, 'second search page after retry');
    const paged = queryOf(reads('search').at(-1));
    assert(paged.from_ms === retried.from_ms && paged.to_ms === retried.to_ms, 'Search pagination after retry did not reuse the recovered window');
    assert(!document.querySelector('[data-error-code]'), 'Recovered search pagination surfaced an error');
  }],
  ['the fallback filter is a fresh server-side window', async () => {
    await fresh(control => {
      seedSimple(control, Date.now() - 60_000);
      control.requests['observation-session/fallback'] = [control.makeRequest('observation-session/fallback', 0, Date.now() - 50_000, { within_request_fallback: true })];
    });
    await until(() => rows().length === 2, 'both sessions');
    button('发生过请求内回退').click();
    await until(() => rows().length === 1, 'filtered list');
    const latest = queryOf(reads('sessions').at(-1));
    assert(latest.only_model_switch === true, 'Filter was not applied by the query');
    assert(!text().includes('没有匹配的会话'), 'Filter was reported as an empty search');
  }],
  ['session title skips an injected environment block and uses later real input', async () => {
    await fresh(control => {
      const sessionId = 'observation-session/title';
      const first = control.makeRequest(sessionId, 0, Date.now() - 60_000);
      const second = control.makeRequest(sessionId, 1, Date.now() - 50_000);
      control.requests[sessionId] = [first, second];
      seedUserText(control, first, '  <environment_context>\nworkspace facts\n</environment_context>\n');
      seedUserText(control, second, '解释缓存命中率');
    });
    await until(() => rows().length === 1 && rows()[0].textContent.includes('解释缓存命中率'), 'real title after injected context');
    assert(!rows()[0].textContent.includes('workspace facts'), 'Injected environment block became the title');
  }],
  ['title refresh adopts a real user input that arrives after setup context', async () => {
    const sessionId = 'observation-session/title-refresh';
    await fresh(control => {
      const first = control.makeRequest(sessionId, 0, Date.now() - 60_000);
      control.requests[sessionId] = [first];
      seedUserText(control, first, '<environment_context>setup only</environment_context>');
    });
    await until(() => rows().length === 1 && rows()[0].textContent.includes('会话记录'), 'neutral title before real input');
    const second = c().makeRequest(sessionId, 1, Date.now() - 30_000);
    c().requests[sessionId].push(second);
    seedUserText(c(), second, '后来输入的真实问题');
    c().bumpRefresh();
    await until(() => rows()[0]?.textContent.includes('后来输入的真实问题'), 'title updated after refresh');
  }],
  ['Sessions manual All refresh replaces an existing neutral title', async () => {
    const sessionId = 'observation-session/title-manual-refresh';
    await fresh(control => {
      const first = control.makeRequest(sessionId, 0, Date.now() - 60_000);
      control.requests[sessionId] = [first];
      seedUserText(control, first, '<environment_context>setup only</environment_context>');
    });
    await until(() => rows().length === 1 && rows()[0].textContent.includes('会话记录'), 'neutral title before manual refresh');
    const next = c().makeRequest(sessionId, 1, Date.now() - 30_000);
    c().requests[sessionId].push(next);
    seedUserText(c(), next, '手动刷新后的真实输入');
    button('全部').click();
    await until(() => rows()[0]?.textContent.includes('手动刷新后的真实输入'), 'manual All refresh re-read existing session title');
  }],
  ['title scan keeps the timeline window while finding real input beyond page one', async () => {
    const sessionId = 'observation-session/title-paged';
    await fresh(control => {
      const at = Date.now() - 120_000;
      control.requests[sessionId] = Array.from({ length: 51 }, (_, index) => {
        const request = control.makeRequest(sessionId, index, at + index);
        seedUserText(control, request, index === 50 ? '第 51 个请求里的真实问题' : '<environment_context>setup</environment_context>');
        return request;
      });
      control.views.timeline = async query => {
        const page = control.defaultRead('timeline', query);
        if (!query.cursor) await pause(20);
        return page;
      };
    });
    await until(() => rows().length === 1 && rows()[0].textContent.includes('第 51 个请求里的真实问题'), 'title from second timeline page');
    const pages = reads('timeline').filter(call => queryOf(call).session_id === sessionId);
    const second = pages.findIndex(call => queryOf(call).cursor);
    assert(second > 0, 'Title scan did not request a second page');
    const first = pages.slice(0, second).findLast(call => !queryOf(call).cursor);
    assert(first && queryOf(first).to_ms === queryOf(pages[second]).to_ms, 'Title scan changed the cursor-bound window');
  }],
  ['Home title refreshes after the same session gains a real user request', async () => {
    const sessionId = 'observation-session/home-title-refresh';
    await fresh(control => {
      const first = control.makeRequest(sessionId, 0, Date.now() - 60_000);
      control.requests[sessionId] = [first];
      seedUserText(control, first, '<environment_context>setup only</environment_context>');
    });
    c().showHome();
    await until(() => document.querySelector('.v3-recent .row-title') && reads('timeline').some(call => queryOf(call).session_id === sessionId), 'Home neutral title read');
    assert(!document.querySelector('.v3-recent .row-title').textContent.includes('setup only'), 'Home showed injected context');
    const second = c().makeRequest(sessionId, 1, Date.now() - 30_000);
    c().requests[sessionId].push(second);
    seedUserText(c(), second, 'Home 后续真实输入');
    c().bumpRefresh();
    await until(() => document.querySelector('.v3-recent')?.textContent.includes('Home 后续真实输入'), 'Home title refreshed with activity');
  }],
  ['run record shows tokens and weighted cache hit even when cost is unpriced', async () => {
    await fresh(control => {
      seedSimple(control);
      seedUserText(control, control.requests['observation-session/simple'][0], '查看用量统计');
    });
    await until(() => button('运行记录'), 'run record button');
    button('运行记录').click();
    await until(() => text().includes('输入 Token') && text().includes('1,100') && text().includes('17.27%'), 'usage metrics rendered');
    assert(text().includes('尚未计价'), 'Unpriced state was not shown independently');
    assert(text().includes('190 / 1,100'), 'Cache-hit basis was not disclosed');
    assert(text().includes('2 / 2 个 Attempt 可计算'), 'Cache-hit sample coverage was not disclosed');
    assert(text().includes('已排除 1 个连接探针'), 'Excluded probe count was not disclosed');
  }],
];

export async function runSessionWindowScenarios(start = 0, end = Infinity) {
  const results = [];
  for (const [name, run] of scenarios.slice(start, end)) {
    try { await run(); results.push({ name, state: 'green' }); }
    catch (error) { results.push({ name, state: 'red', error: error.message }); }
  }
  return { evidence: 'React components + mock IPC only; not native/Tauri and not the real daemon', tests: results.length, passed: results.filter(item => item.state === 'green').length, failed: results.filter(item => item.state === 'red').length, results };
}
