const c = () => window.agentTrust;
const tick = () => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const visible = element => Boolean(element && element.getClientRects().length > 0);
const text = () => document.body.innerText;
const calls = name => c().commands.filter(item => item.command === name);
const facet = kind => document.querySelector(`[data-agent-facet="${kind}"]`);
const executableCallout = root => (root ?? document).querySelector('[data-agent-executable-state]');
const dialog = () => document.querySelector('[role="dialog"]');
const submit = () => document.querySelector('[role="dialog"] button[type="submit"]');
async function until(predicate, label) {
  const deadline = Date.now() + 5000;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error(`Timed out: ${label}`);
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  await tick();
}
async function fresh(kind) {
  c().reset();
  c().agents = c()[kind]();
  await tick();
  await until(() => [...document.querySelectorAll('.native-list .list-row')].some(row => row.textContent.includes('Claude')), 'agents loaded');
  document.querySelectorAll('.native-list .list-row').forEach(row => { if (row.textContent.includes('Claude')) row.click(); });
  await until(() => document.querySelector('[data-agent-id="agent_claude_default"] .detail-hero'), 'Claude detail');
}
async function freshCodex(kind) {
  c().reset();
  c().agents = c()[kind]();
  await tick();
  await until(() => [...document.querySelectorAll('.native-list .list-row')].some(row => row.textContent.includes('Codex')), 'agents loaded');
  document.querySelectorAll('.native-list .list-row').forEach(row => { if (row.textContent.includes('Codex')) row.click(); });
  await until(() => document.querySelector('[data-agent-id="agent_codex_default"] .detail-hero'), 'Codex detail');
}
async function openEditor(kind = 'model') {
  const trigger = facet(kind);
  assert(trigger && !trigger.disabled, `Facet button unavailable: ${kind}`);
  trigger.click();
  await until(() => visible(dialog()), 'editor open');
}
async function save() {
  const control = submit();
  assert(control && !control.disabled, 'Save button unavailable');
  control.click();
  await until(() => text().includes('Agent 配置已保存') || text().includes('模型接入已应用'), 'save confirmed');
}
function changeSelect(select, value) {
  Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(select, value);
  select.dispatchEvent(new Event('change', { bubbles: true }));
}
const configureSpecs = () => calls('preview_agent_settings').map(item => item.payload.input.spec);

const scenarios = [
  ['Desktop-only Codex saves one shared fixed-model configuration without a surface selector', async () => {
    await freshCodex('desktopOnly');
    await openEditor('model');
    document.querySelector('#hr-agent-settings [role="switch"]').click();
    await tick();
    assert(!document.querySelector('[data-agent-surface-option]'), 'Legacy surface selector is still rendered');
    assert(visible(document.querySelector('[data-codex-shared-scope]')), 'Shared-scope explanation is missing');
    assert(document.querySelector('[data-agent-surface-fact="codex_desktop"]').getAttribute('data-agent-surface-detected') === 'true', 'Desktop discovery fact missing');
    assert(document.querySelector('[data-agent-surface-fact="codex_cli"]').getAttribute('data-agent-surface-detected') === 'false', 'Missing CLI was not reported as a fact');
    assert(submit().disabled, 'Uncovered native default did not block save');
    const fixed = document.querySelector('[data-client-model-id="gpt-5.6-sol"] select');
    changeSelect(fixed, 'binding/codex/gpt-5.6-sol');
    await tick();
    const effort = [...document.querySelectorAll('[data-client-model-id="gpt-5.6-sol"] select.select')].at(-1);
    assert(effort && effort.value === 'high', 'Fixed model did not carry explicit native effort');
    const defaultChoice = document.querySelector('[data-agent-default]');
    changeSelect(defaultChoice, 'fixed:gpt-5.6-sol');
    await tick();
    await save();
    const spec = configureSpecs().at(-1).model.settings;
    assert(!Object.hasOwn(spec, 'surfaces'), 'Desktop discovery leaked into the persisted configuration');
    assert(spec.fixed_models.length === 1 && spec.allowed_plan_ids.length === 0, 'Fixed-only selection was not preserved');
    assert(spec.fixed_models[0].candidate.reasoning.profile === 'high', 'Native effort was not submitted');
  }],
  ['CLI-only Codex saves one shared plan configuration without selecting Desktop', async () => {
    await freshCodex('cliOnly');
    await openEditor('model');
    document.querySelector('#hr-agent-settings [role="switch"]').click();
    await tick();
    assert(!document.querySelector('[data-agent-surface-option]'), 'Legacy surface selector is still rendered');
    assert(document.querySelector('[data-agent-surface-fact="codex_cli"]').getAttribute('data-agent-surface-detected') === 'true', 'CLI discovery fact missing');
    assert(document.querySelector('[data-agent-surface-fact="codex_desktop"]').getAttribute('data-agent-surface-detected') === 'false', 'Missing Desktop was not reported as a fact');
    const plan = document.querySelector('[data-agent-plan-id]');
    plan.querySelector('input').click();
    await tick();
    const planId = plan.getAttribute('data-agent-plan-id');
    changeSelect(document.querySelector('[data-agent-default]'), `plan:${planId}`);
    await tick();
    await save();
    const spec = configureSpecs().at(-1).model.settings;
    assert(!Object.hasOwn(spec, 'surfaces'), 'CLI discovery leaked into the persisted configuration');
    assert(spec.fixed_models.length === 0 && spec.allowed_plan_ids.length === 1, 'Plan-only selection was not preserved');
  }],
  ['Codex mixed selection keeps fixed and Plan routes while preserving the covered native default', async () => {
    await freshCodex('desktopOnly');
    await openEditor('model');
    document.querySelector('#hr-agent-settings [role="switch"]').click();
    await tick();
    changeSelect(document.querySelector('[data-client-model-id="gpt-5.6-sol"] select'), 'binding/codex/gpt-5.6-sol');
    const plan = document.querySelector('[data-agent-plan-id]');
    plan.querySelector('input').click();
    await tick();
    assert(document.querySelector('[data-agent-default]').value === 'native', 'Native default was changed without user action');
    await save();
    const spec = configureSpecs().at(-1).model.settings;
    assert(spec.fixed_models.length === 1 && spec.allowed_plan_ids.length === 1, 'Mixed selection lost a route family');
    assert(spec.default_selection.kind === 'preserve_native', 'Covered native default was not preserved');
  }],
  ['Codex renders CLI passed and Desktop not verified as independent current-revision states', async () => {
    await freshCodex('splitCodexStatus');
    const cli = document.querySelector('[data-agent-surface="codex_cli"]');
    const desktop = document.querySelector('[data-agent-surface="codex_desktop"]');
    assert(cli.textContent.includes('已验证') && cli.textContent.includes('r19'), 'CLI passed state missing');
    assert(desktop.textContent.includes('尚未验证') && desktop.textContent.includes('r19'), 'Desktop not-verified state missing');
    assert(!text().includes('全部模型可用'), 'Partial evidence was generalized to the full catalog');
  }],
  ['a non-runnable executable reports the real reason without becoming a trust gate', async () => {
    await fresh('notRunnable');
    const callout = executableCallout();
    assert(visible(callout), 'Executable diagnostic missing');
    assert(callout.getAttribute('data-agent-executable-state') === 'executable_not_runnable', 'Wrong executable diagnostic state');
    assert(callout.textContent.includes('不可执行'), 'Diagnostic does not name the executable failure');
    assert(!callout.textContent.includes('不受信任'), 'Diagnostic still makes an installation trust claim');
    assert(!facet('model').disabled && !facet('collaboration').disabled, 'Executable diagnostics still block configuration');
    await openEditor('model');
    assert(visible(dialog()), 'Editor did not open alongside the executable diagnostic');
    assert(calls('preview_agent_settings').length === 0, 'Opening the editor dispatched a mutation request');
  }],
  ['rescan replaces a non-runnable diagnostic with the current installation state', async () => {
    await fresh('notRunnable');
    assert(visible(executableCallout()), 'Executable diagnostic missing');
    c().agents = c().registered();
    executableCallout().querySelector('button').click();
    await until(() => !executableCallout(), 'executable diagnostic cleared after rescan');
    assert(!facet('model').disabled, 'Configure entry changed after rescan');
  }],
  ['a late executable failure remains diagnostic while the shared configuration stays editable', async () => {
    await fresh('registered');
    await openEditor('model');
    assert(!submit().disabled && !executableCallout(dialog()), 'Registered editor unexpectedly restricted');
    const snapshots = calls('agent_snapshot').length;
    c().agents = c().notRunnable();
    c().refresh();
    await until(() => calls('agent_snapshot').length > snapshots, 'late state refetched');
    await until(() => executableCallout(dialog()), 'late executable diagnostic rendered');
    const before = calls('preview_agent_settings').length;
    await save();
    assert(calls('preview_agent_settings').length === before + 1, 'Executable diagnostic prevented a shared configuration save');
  }],
  ['a non-runnable install keeps the managed recovery entry reachable', async () => {
    await fresh('notRunnableWithRestore');
    assert(visible(executableCallout()), 'Executable diagnostic missing');
    const details = document.querySelector('.native-details');
    details.querySelector('summary').click();
    await tick();
    const restore = [...details.querySelectorAll('button')].find(item => item.textContent.trim() === '恢复模型设置');
    assert(restore && !restore.disabled, 'Managed recovery entry blocked by an executable diagnostic');
    restore.click();
    await until(() => calls('preview_agent_settings').length === 1, 'recovery request dispatched');
    assert(configureSpecs()[0].model.intent === 'restore', 'Recovery entry dispatched a configure request');
    assert(!dialog(), 'Recovery reply opened a configuration editor');
  }],
  ['registered installs keep both facets configurable in one step', async () => {
    await fresh('registered');
    await openEditor('model');
    assert(!executableCallout(dialog()), 'Registered install wrongly reported an executable failure');
    await save();
    assert(configureSpecs().length === 1 && configureSpecs()[0].model.intent === 'configure', 'Model route did not save');
    await until(() => !dialog(), 'editor closed after save');
    await openEditor('collaboration');
    assert(!executableCallout(dialog()), 'Registered install wrongly reported an executable failure on the second facet');
    await save();
    const specs = configureSpecs();
    assert(specs.length === 2 && specs[1].collaboration.intent === 'configure', 'Task delegation skill did not save independently');
    assert(specs[1].collaboration.settings.trigger_mode === 'delegate_by_default', 'Task trigger mode was not preserved');
    assert(c().mutations === 2, 'Saves were not reported to the host');
  }],
  ['registered installs still pass the prerequisite check before saving', async () => {
    await fresh('registered');
    let previews = 0;
    c().handlers.preview_agent_settings = () => {
      previews += 1;
      return previews === 1
        ? { preview: { applicable: false, blockers: [{ reason: 'capability_unavailable', capabilities: [{ capability: 'ingress_authentication', reason: 'unverified' }] }] }, mutation: null }
        : { preview: { applicable: true, blockers: [] }, mutation: { state: 'applied', operation: { state: 'succeeded' } } };
    };
    await openEditor('model');
    await save();
    assert(calls('check_agent_authentication').length === 1, 'Prerequisite check was skipped');
    assert(calls('preview_agent_settings').length === 2, 'Save did not re-preview after the prerequisite check');
    assert(!executableCallout() && !executableCallout(dialog()), 'A prerequisite gap was reported as an executable failure');
  }],
  ['non-executable states keep their own diagnostics', async () => {
    await fresh('unregisteredEndpoint');
    assert(!executableCallout(), 'Configuration state was generalized to an executable failure');
    assert(!facet('model').disabled, 'Configure entry wrongly blocked for a configuration state');
    await openEditor('model');
    assert(visible(dialog()), 'Editor did not open for a configuration state');
    assert(calls('preview_agent_settings').length === 0, 'Opening the editor dispatched a mutation request');
  }],
];

export async function runAgentTrustScenarios(start = 0, end = Infinity) {
  const results = [];
  for (const [name, run] of scenarios.slice(start, end)) {
    try { await run(); results.push({ name, state: 'green' }); }
    catch (error) { results.push({ name, state: 'red', error: error.message }); }
  }
  return { evidence: 'React components + mock IPC only; not native/Tauri', tests: results.length, passed: results.filter(item => item.state === 'green').length, failed: results.filter(item => item.state === 'red').length, results };
}
