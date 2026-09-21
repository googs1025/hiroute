"""All three-domain submission orders and a barrier-released real process overlap."""
import concurrent.futures
import itertools
import json
import sys
import threading
from publication_process import (
    bootstrap,
    model_settings_spec_v2,
    plan_change,
    prepare_agent_settings_v2,
    prepare_saved_source_update,
)
from publication_product import Product


def scenario(repository, order, overlap=False):
    product = Product(repository, project_source=True)
    try:
        bootstrap(product)
        create = plan_change(product, 'create', 'interleaving-plan-b',
                             display_name='Interleaving Plan B')
        created = product.preview('routing preview', {'change': create})
        product.apply('routing apply', 'ApplyAgentPlanChange', created,
                      {'change': create}, 'interleaving-plan-b')
        plan_b = created['plan_head']['reference']['plan_id']
        plan = plan_change(product, 'update', display_name='Interleaved Plan')
        agent = model_settings_spec_v2(product, [product.plan_id, plan_b])
        commands = {
            'plan': ('routing preview', 'routing apply', 'ApplyAgentPlanChange', {'change': plan}),
            'agent': ('agents connect preview', 'agents connect apply', 'ApplyAgentConnectionChange', {'spec': agent}),
        }

        def prepare(name, key):
            if name == 'source':
                body, capability = prepare_saved_source_update(product, key)
                return 'control', 'ApplyComputeSave', body, capability
            if name == 'agent':
                _preview, body, capability = prepare_agent_settings_v2(
                    product, agent, key)
                return 'cli', 'agents connect apply', body, capability
            preview_command, apply_command, operation, payload = commands[name]
            preview = product.preview(preview_command, payload)
            body = dict(payload)
            body.update(accept_digest=preview['change_digest'], expected_revisions=preview['expected_revisions'], idempotency_key=key)
            return 'cli', apply_command, body, None

        prepared = {name: prepare(name, name + '-first') for name in order}
        barrier = threading.Barrier(3)

        def submit(name):
            if overlap:
                barrier.wait(timeout=10)
            transport, command, body, cap = prepared[name]
            if transport == 'control':
                result = product.control(command, body, cap, success=False)
                return (0 if result.get('error') is None else 2), result
            return product.cli(command, body, cap, success=False)

        if overlap:
            with concurrent.futures.ThreadPoolExecutor(max_workers=3) as executor:
                results = dict(zip(order, executor.map(submit, order)))
        else:
            results = {name: submit(name) for name in order}
        successes = [name for name, (code, result) in results.items()
                     if code == 0 and result['data']['state'] == 'succeeded']
        assert len(successes) == 1, results
        accepted = {}
        for name in order:
            code, result = results[name]
            if name not in successes:
                assert result['error']['code'] in ('REVISION_CONFLICT', 'CHANGE_PREVIEW_STALE'), (name, result, successes)
                prepared[name] = prepare(name, name + '-retry')
                transport, command, body, cap = prepared[name]
                if transport == 'control':
                    result = product.control(command, body, cap)
                else:
                    _, result = product.cli(command, body, cap)
            assert result['data']['state'] == 'succeeded', result
            accepted[name] = result['data']
        assert len({result['operation_id'] for result in accepted.values()}) == 3
        for name in order:
            transport, command, body, cap = prepared[name]
            if transport == 'control':
                replay = product.control(command, body, cap)
            else:
                _, replay = product.cli(command, body, cap)
            assert replay['data'] == accepted[name], 'cross-domain update changed replay identity'
            state = product.cli('operations get ' + accepted[name]['operation_id'])[1]['data']['state']
            assert state == 'succeeded', state
        # A fresh read must see all three results together: updated saved source,
        # unchanged plan identity at revision 2, and the independently persisted Agent grant.
        source = product.control(
            'ListCompute', {'source_id': product.source_id})['data']['sources'][0]
        assert source['revision'] == 2, source
        product.preview('routing preview', {'change': dict(
            plan, target=dict(plan['target'], expected_head_revision=2))})
        status = product.cli('agents connect status ' + product.agent_context_id)[1]
        assert status['status'] == 'succeeded', status
        product.catalog()
        print(json.dumps({'scenario': 'three-domain-' + ('overlap' if overlap else '-'.join(order)),
                          'state': 'green', 'first_success': successes, 'conflicts': 2,
                          'retained_operations': 3, 'cli_exit': 0}), flush=True)
    finally:
        product.close()


if __name__ == '__main__':
    for order in itertools.permutations(('source', 'plan', 'agent')):
        scenario(sys.argv[1], order)
    scenario(sys.argv[1], ('source', 'plan', 'agent'), overlap=True)
