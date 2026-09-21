const c = () => window.subscriptionRepair;
const tick = () => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const visible = element => element && element.getClientRects().length > 0;
const button = label => [...document.querySelectorAll('button')].find(element => visible(element) && element.textContent.trim() === label);
const text = () => document.body.innerText;
const calls = name => c().commands.filter(item => item.command === name);
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
  await until(() => button('添加模型'), 'models loaded');
}
async function click(label) {
  const element = button(label);
  assert(element && !element.disabled, `Enabled button missing: ${label}`);
  element.click();
  await tick();
}
async function scan() {
  await click('添加模型');
  const entry = [...document.querySelectorAll('.add-choice')].find(item => item.textContent.includes('扫描本机'));
  assert(entry, 'Scan entry missing');
  entry.click();
  await until(() => text().includes('可复用的订阅'), 'scan completed');
}
async function connect(name = 'Codex 组件测试订阅', twice = false) {
  const row = [...document.querySelectorAll('.oc-scan-list .oc-status-row')].find(item => item.textContent.includes(name));
  const target = row?.querySelector('button');
  assert(target && !target.disabled, `Connect row missing: ${name}`);
  target.click();
  if (twice) target.click();
  await tick();
}
async function checked() {
  await scan();
  await connect();
  await click('检查订阅');
  await until(() => button('保存接入'), 'checked models');
}
function finishCheck() {
  c().subscriptions = [structuredClone(c().checked)];
  c().result = structuredClone(c().verified);
  return c().result;
}

const scenarios = [
  ['normal inventory, empty selection and duplicate check/save', async () => {
    await fresh();
    await scan();
    await connect();
    const check = button('检查订阅');
    check.click(); check.click();
    await until(() => button('保存接入'), 'check completed');
    const boxes = [...document.querySelectorAll('.v3-catalog input[type="checkbox"]')];
    assert(boxes.length === 3 && boxes.filter(item => item.disabled).length === 2, 'Inventory must remain visible and nonselectable');
    assert(calls('check_subscription').length === 1, 'Duplicate check dispatched');
    boxes[0].click(); await tick();
    assert(button('保存接入').disabled, 'Empty selection can save');
    boxes[0].click(); await tick();
    const save = button('保存接入'); save.click(); save.click();
    await until(() => text().includes('订阅已接入'), 'save completed');
    assert(calls('apply_compute_save').length === 1, 'Duplicate Apply dispatched');
    const selected = calls('preview_compute_save')[0].payload.change.selected_model_refs;
    assert(JSON.stringify(selected) === '["model/matched"]', 'Inventory-only model submitted');
  }],
  ['typed failure persists through refresh and retry', async () => {
    await fresh(control => { control.handlers.check_subscription = () => ({ ...control.checking, status: 'needs_auth', reason: 'SUBSCRIPTION_NEEDS_AUTH' }); });
    await scan(); await connect(); await click('检查订阅');
    await until(() => document.querySelector('[data-error-code="SUBSCRIPTION_NEEDS_AUTH"]'), 'typed failure visible');
    assert(!button('保存接入'), 'Failed check can save');
    delete c().handlers.check_subscription;
    await click('检查订阅');
    await until(() => button('保存接入'), 'retry success');
    assert(!document.querySelector('[role="alert"]'), 'Old failure survived successful retry');
  }],
  ['recovered checking operation resumes observation without another approval', async () => {
    await fresh(control => {
      control.result = control.checking;
      control.handlers.get_subscription_check_result = finishCheck;
    });
    await until(() => calls('get_subscription_check_result').length > 0 && c().result.status === 'verified', 'recovery polling');
    await scan(); await connect();
    await until(() => button('保存接入'), 'recovered candidate');
    assert(calls('check_subscription').length === 0, 'Recovery created a new approval');
  }],
  ['polling failure exposes original-operation retry', async () => {
    await fresh(control => {
      control.handlers.check_subscription = () => { control.result = control.checking; return control.checking; };
      control.handlers.get_subscription_check_result = () => { throw { code: 'CLIENT_DEADLINE' }; };
    });
    await scan(); await connect(); await click('检查订阅');
    await until(() => button('重新查询检查结果'), 'poll retry');
    assert(document.querySelector('[role="alert"]'), 'Polling failure hidden');
    c().handlers.get_subscription_check_result = finishCheck;
    await click('重新查询检查结果');
    await until(() => button('保存接入'), 'poll recovery');
    assert(calls('check_subscription').length === 1, 'Polling retry reapproved');
  }],
  ['cancelled check response cannot overwrite reopened view', async () => {
    const late = deferred();
    await fresh(control => { control.handlers.check_subscription = () => late.promise; });
    await scan(); await connect(); await click('检查订阅'); await click('取消检查');
    assert(calls('close_subscription_check').length > 0, 'Native close was not requested');
    delete c().handlers.check_subscription;
    await checked();
    late.resolve({ ...c().checking, status: 'needs_auth', reason: 'SUBSCRIPTION_NEEDS_AUTH' });
    await tick();
    assert(button('保存接入') && !document.querySelector('[role="alert"]'), 'Late failure replaced new result');
    assert(calls('apply_compute_save').length === 0, 'Cancel caused save');
  }],
  ['cancelled in-flight poll cannot attach an error to the next check', async () => {
    const late = deferred();
    await fresh(control => {
      control.handlers.check_subscription = () => control.checking;
      control.handlers.get_subscription_check_result = () => late.promise;
    });
    await scan(); await connect(); await click('检查订阅');
    await until(() => calls('get_subscription_check_result').length > 0, 'pending poll');
    await click('取消检查');
    delete c().handlers.check_subscription; delete c().handlers.get_subscription_check_result;
    await checked();
    late.resolve({ ...c().checking, status: 'failed', reason: 'OLD_POLL_FAILURE' });
    await tick();
    assert(button('保存接入') && !document.querySelector('[role="alert"]'), 'Late poll contaminated new check');
  }],
  ['latest management request wins over a late empty response', async () => {
    await fresh();
    const late = deferred(); const previousCount = calls('compute_management_snapshot').length;
    c().handlers.compute_management_snapshot = () => late.promise;
    c().refresh();
    await until(() => calls('compute_management_snapshot').length > previousCount, 'old refresh entered');
    const latest = structuredClone(c().management);
    latest.sources[0].models[0].display_name = 'Newest model';
    c().handlers.compute_management_snapshot = () => latest;
    c().refresh();
    await until(() => text().includes('Newest model'), 'new refresh displayed');
    late.resolve({ ...latest, sources: [] }); await tick();
    assert(text().includes('Newest model') && !text().includes('先连接一个模型'), 'Late empty overwrote latest snapshot');
  }],
  ['failed refresh of an empty snapshot is not a successful first-use state', async () => {
    await fresh(control => { control.management.sources = []; });
    const original = structuredClone(c().management);
    c().handlers.compute_management_snapshot = () => { throw { code: 'DAEMON_UNAVAILABLE' }; };
    c().refresh();
    await until(() => document.querySelector('[data-error-code="DAEMON_UNAVAILABLE"]'), 'management error');
    assert(!text().includes('先连接一个模型'), 'Failed query shown as first use');
    c().handlers.compute_management_snapshot = () => original;
    await click('重试');
    await until(() => text().includes('先连接一个模型'), 'empty retry succeeded');
  }],
  ['API filter empty state recovers and connector credentials remain hidden', async () => {
    await fresh(); await click('API');
    assert(text().includes('没有匹配的模型'), 'API filter did not show empty');
    await click('清除筛选');
    assert(document.querySelector('.models-feature .list-row'), 'Model list failed to recover');
    assert(!button('管理凭据'), 'ConnectorOwned exposed manual keys');
  }],
  ['scan empty, runtime unavailable and failed remain distinct', async () => {
    await fresh(control => { control.subscriptions = []; control.handlers.compute_scan = () => ({ items: [] }); });
    await scan();
    assert(text().includes('没有发现可复用的 Codex 订阅'), 'Successful empty scan missing');
    await click('完成');
    c().handlers.compute_subscriptions = () => ({ discovery_state: 'runtime_unavailable', reason_code: 'subscription_runtime_unavailable', candidates: [] });
    await scan();
    assert(text().includes('当前环境暂时无法读取') && !text().includes('没有发现可复用的 Codex 订阅'), 'Runtime absence treated as empty');
    c().handlers.compute_subscriptions = () => { throw { code: 'DAEMON_UNAVAILABLE' }; };
    await click('重新扫描');
    await until(() => text().includes('暂时无法读取本机 Codex 订阅'), 'scan failure');
    delete c().handlers.compute_subscriptions;
    await click('重新扫描');
    await until(() => text().includes('没有发现可复用的 Codex 订阅'), 'empty after retry');
  }],
  ['Prepare duplicate and close discard late candidate without cancellation API', async () => {
    const late = deferred();
    await fresh(control => { control.handlers.prepare_discovered_model_connection = () => late.promise; });
    await scan(); await connect('智谱 Coding Plan', true);
    assert(calls('prepare_discovered_model_connection').length === 1, 'Duplicate Prepare dispatched');
    await click('关闭'); await scan();
    late.resolve(c().prepared); await tick();
    assert(!button('保存接入'), 'Late Prepare opened save');
    assert(calls('apply_compute_save').length === 0 && calls('cancel_model_connection_check').length === 0, 'Prepare close wrote or called cancellation');
  }],
  ['stale discovery Preview requires a new scan and Prepare', async () => {
    await fresh(control => { control.handlers.preview_compute_save = () => { throw { code: 'CHANGE_PREVIEW_STALE' }; }; });
    await scan(); await connect('智谱 Coding Plan');
    await until(() => button('保存接入'), 'discovery prepared');
    await click('保存接入');
    await until(() => button('重新扫描'), 'stale requires rescan');
    assert(!button('保存接入') && calls('apply_compute_save').length === 0, 'Stale candidate still saveable');
    const next = { ...c().discovered, discovery: { discovery_ref: `discovery/${'b'.repeat(64)}`, discovery_revision: '2' } };
    c().handlers.compute_scan = () => ({ items: [next] });
    delete c().handlers.preview_compute_save;
    await click('重新扫描'); await until(() => text().includes('可复用的订阅'), 'rescan complete');
    await connect('智谱 Coding Plan');
    await until(() => button('保存接入'), 'new candidate prepared');
    assert(calls('prepare_discovered_model_connection').at(-1).payload.discovery.discovery_ref === next.discovery.discovery_ref, 'Old discovery reused');
  }],
  ['late admitted save failure does not close a newer scan', async () => {
    const late = deferred();
    await fresh(); await checked();
    c().handlers.apply_compute_save = () => late.promise;
    await click('保存接入');
    await until(() => calls('apply_compute_save').length === 1, 'save admitted');
    c().result = null;
    c().setActive(false); await tick(); c().setActive(true); await tick();
    await until(() => text().includes('可复用的订阅'), 'new active view');
    late.reject({ code: 'CLIENT_DEADLINE' }); await tick();
    assert(visible(document.querySelector('[role="dialog"]')), 'Late save closed new dialog');
    assert(!document.querySelector('[role="alert"]'), 'Unknown save falsely reported failed');
    assert(c().recoveryRefreshes > 0, 'Unknown operation not handed to recovery');
  }],
];

export async function runSubscriptionRepairScenarios(start = 0, end = Infinity) {
  const results = [];
  const batch = scenarios.slice(start, end);
  for (const [name, run] of batch) {
    try { await run(); results.push({ name, state: 'green' }); }
    catch (error) { results.push({ name, state: 'red', error: error.message }); }
  }
  return { evidence: 'React components + mock IPC only; not native/Tauri', tests: results.length, passed: results.filter(item => item.state === 'green').length, failed: results.filter(item => item.state === 'red').length, results };
}
