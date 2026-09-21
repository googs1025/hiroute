#!/usr/bin/env python3
"""Prove phase isolation, exact coverage and shared-slot execution without Rust."""
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


runner = load('remote-rust')
scheduler = load('validation-schedule')
reporting = load('validation-report')
COMMAND = ['cargo', 'test', '--locked', '--workspace', '--exclude', 'hiroute-desktop', '--all-features']
TEST_BINARY = '''import os,sys,time,pathlib
root=pathlib.Path(__file__).resolve().parents[3]
assert sys.argv[1:] == ['default_two_domain_smoke', '--exact']
assert os.environ['HIROUTE_SMOKE_REQUIRE_PREPARED']=='1'
assert os.environ['CARGO_PKG_NAME']=='hiroute-product-e2e'
assert os.environ['CARGO_BIN_EXE_hiroute-smoke']=='/fixture/hiroute-smoke'
assert os.environ['RUST_RECURSION_COUNT']=='1'
assert (root/'prepared').exists()
(root/'smoke.ready').touch()
until=time.monotonic()+5
while not (root/'remainder.ready').exists():
 if time.monotonic()>until: sys.exit(98)
 time.sleep(.01)
bad=os.environ['FIXTURE_MODE']=='red-smoke'
print('running 1 test')
print('test default_two_domain_smoke ... '+('FAILED' if bad else 'ok'))
print('test result: '+('FAILED' if bad else 'ok')+f'. {int(not bad)} passed; {int(bad)} failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s')
sys.exit(101 if bad else 0)
'''


class Schedule(unittest.TestCase):
    def test_exec_audit_rejects_runtime_cargo_without_storing_arguments(self):
        trace = ('123 execve("/tmp/bin/test", [...], 0x1) = 0\n'
                 '124 execve("/home/user/.cargo/bin/cargo", [...], 0x1) = 0\n'
                 '125 execveat(AT_FDCWD, "/toolchain/bin/rustc", [...], 0) = 0\n')
        audit = scheduler.dag_exec_audit(trace)
        self.assertEqual(audit['process_starts'], 3)
        self.assertEqual(audit['cargo_rustc_starts'], 2)
        self.assertEqual(audit['blocked_classes'], ['cargo', 'rustc'])
        self.assertNotIn('/home/user', json.dumps(audit))
        with self.assertRaisesRegex(ValueError, 'no started'):
            scheduler.dag_exec_audit('')

    def test_metadata_is_bound_to_both_dependency_gate_targets(self):
        artifact = {'path': '/fixture/dag-cargo-metadata.json', 'sha256': 'digest'}
        targets = [{'name': name, 'package_name': 'hiroute-product-e2e'}
                   for name in ('product_oracle', 'transaction_recovery')]
        scheduler.dag_bind_metadata(targets, artifact)
        for row in targets:
            self.assertEqual(row['environment']['HIROUTE_VALIDATION_METADATA_JSON'], artifact['path'])
            self.assertEqual(row['external_artifacts'], [artifact])
        with self.assertRaisesRegex(ValueError, 'consumer set changed'):
            scheduler.dag_bind_metadata(targets[:1], artifact)

    def test_toolchain_manifest_is_candidate_bound_and_requires_host(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cargo = root / 'cargo'
            rustc = root / 'rustc'
            cargo.write_bytes(b'cargo tool')
            rustc.write_bytes(b'rustc tool')
            class Runner:
                @staticmethod
                def command(args, cwd):
                    self.assertEqual(args, ['rustup', 'which', 'cargo'])
                    return str(cargo)
                @staticmethod
                def save(path, data):
                    Path(path).write_text(json.dumps(data))
            def version(args, **_kwargs):
                return ('cargo 1.97.1\n' if args[0] == str(cargo)
                        else 'rustc 1.97.1\nhost: x86_64-unknown-linux-gnu\n')
            with patch.object(scheduler.subprocess, 'check_output', side_effect=version), \
                 patch.object(scheduler.shutil, 'which', return_value=None):
                artifact = scheduler.dag_toolchain_context(Runner, root, 'a'*40, root)
            record = json.loads(Path(artifact['path']).read_text())
            self.assertEqual(record['sha'], 'a'*40)
            self.assertEqual(record['target_triple'], 'x86_64-unknown-linux-gnu')
            self.assertEqual(scheduler.file_digest(Path(artifact['path'])), artifact['sha256'])
            with patch.object(scheduler.subprocess, 'check_output', return_value='rustc 1.97.1\n'), \
                 patch.object(scheduler.shutil, 'which', return_value=None), \
                 self.assertRaisesRegex(ValueError, 'host target'):
                scheduler.dag_toolchain_context(Runner, root, 'a'*40, root)

    def test_daemon_fixture_survives_gateway_overwrite_and_binds_exact_consumers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'target/debug/hirouted'
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b'daemon with integration hooks')
            metadata = {'packages': [{'id': 'daemon-id', 'name': 'hiroute-daemon'}]}
            event = {'reason': 'compiler-artifact', 'package_id': 'daemon-id',
                     'target': {'name': 'hirouted', 'kind': ['bin']},
                     'profile': {'test': False}, 'features': ['integration-test-hooks'],
                     'executable': str(binary)}
            artifact = scheduler.dag_daemon_fixture(json.dumps(event), root, metadata, 'fixture')
            binary.write_bytes(b'gateway with the same output name')
            self.assertEqual(Path(artifact['path']).read_bytes(), b'daemon with integration hooks')
            self.assertEqual(scheduler.file_digest(Path(artifact['path'])), artifact['sha256'])
            tests = root / 'crates/daemon/tests'
            tests.mkdir(parents=True)
            targets = []
            for name in ('discovered_model_product', 'subscription_management_product'):
                (tests / (name + '.rs')).write_text(
                    '#[path = "support/discovered_model_product_support.rs"] mod product_support;')
                targets.append({'name': name, 'test_source': 'tests/' + name + '.rs',
                                'cwd': str(tests.parent), 'package_name': 'hiroute-daemon'})
            scheduler.dag_bind_daemon_fixture(targets, artifact)
            self.assertTrue(all(row['environment']['HIROUTE_VALIDATION_DAEMON_BIN']
                                == artifact['path'] for row in targets))
            self.assertTrue(all(row['external_artifacts'] == [artifact] for row in targets))
            event['features'] = []
            with self.assertRaisesRegex(ValueError, 'feature-qualified'):
                scheduler.dag_daemon_fixture(json.dumps(event), root, metadata, 'second')
            with self.assertRaisesRegex(ValueError, 'consumer set changed'):
                scheduler.dag_bind_daemon_fixture(targets[:1], artifact)

    def test_dag_phase_records_render_in_validation_report(self):
        phase = {'name': 'compile', 'status': 'completed', 'process_exit': 0,
                 'build_jobs': 64, 'started_at': 1.0, 'finished_at': 2.0}
        record = {'id': 'fixture', 'sha': 'a'*40, 'command': COMMAND,
                  'process_exit': 0, 'status': 'completed',
                  'schedule_result': {'phases': [phase], 'expected_tests': 1,
                                      'complete': True}}
        log = ('     Running tests/fixture.rs (target/debug/deps/fixture-12345678)\n'
               'running 1 test\ntest example ... ok\n'
               'test result: ok. 1 passed; 0 failed; 0 ignored; '
               '0 measured; 0 filtered out; finished in 0.01s\n')
        self.assertIn('compile | 1.0 | 64 | 0 | completed',
                      reporting.markdown(reporting.build_report(record, log)))

    def test_dag_case_set_ignores_child_repeats_but_keeps_parent_names(self):
        log = ('test parent::one ... ok\n'
               'test parent::one ... ok\n'
               'test parent::panic - should panic ... ok\n'
               'test parent::ignored ... ignored, reason\n')
        self.assertEqual(scheduler.dag_observed_cases(log),
                         {'parent::one', 'parent::panic', 'parent::ignored'})

    def test_dag_case_set_survives_concurrent_stderr_inside_result_line(self):
        log = ('restart-window test control::tests::control_capability_is_rejected_on_a_read_operation ... '
               '1ok: open\n'
               'test another::case ... ok\n'
               'test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; '
               '0 filtered out; finished in 0.01s\n')
        self.assertEqual(scheduler.dag_observed_cases(log), {
            'control::tests::control_capability_is_rejected_on_a_read_operation',
            'another::case',
        })

    def test_publication_daemon_requires_feature_and_preserves_roles(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run = root / 'run'
            run.mkdir()
            source = root / 'target/debug/hirouted'
            source.parent.mkdir(parents=True)
            source.write_bytes(b'feature-qualified-daemon')
            normal = root / 'target/validation-schedule/fixture/products'
            normal.mkdir(parents=True)
            (normal / 'hiroute').write_bytes(b'cli')
            metadata = {'packages': [{'id': 'daemon-id', 'name': 'hiroute-daemon'}]}
            event = {'reason': 'compiler-artifact', 'package_id': 'daemon-id',
                     'target': {'name': 'hirouted', 'kind': ['bin']},
                     'features': ['integration-test-hooks'],
                     'executable': str(source), 'fresh': True}
            environment = {'HIROUTE_VALIDATION_PRODUCT_BIN_DIR': str(normal)}
            receipt_dir = scheduler.dag_publication_daemon(
                json.dumps(event), root, metadata, 'fixture', environment, 'a'*40, run)
            self.assertEqual((receipt_dir / 'hirouted').read_bytes(), b'feature-qualified-daemon')
            self.assertEqual((receipt_dir / 'hiroute').read_bytes(), b'cli')
            self.assertEqual(json.loads((run / 'dag-publication-receipt.json').read_text())['features'],
                             ['integration-test-hooks'])
            event['features'] = []
            with self.assertRaisesRegex(ValueError, 'feature-qualified'):
                scheduler.dag_publication_daemon(json.dumps(event), root, metadata,
                                                 'second', environment, 'a'*40, run)

    def test_dag_snapshots_only_relinked_smoke_target(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            compiled = root / 'target/debug/deps'
            compiled.mkdir(parents=True)
            smoke = compiled / 'smoke_cli-1234'
            ordinary = compiled / 'ordinary-1234'
            smoke.write_bytes(b'smoke')
            ordinary.write_bytes(b'ordinary')
            targets = [
                {'name': 'smoke_cli', 'source': str(smoke),
                 'sha256': scheduler.file_digest(smoke)},
                {'name': 'ordinary', 'source': str(ordinary),
                 'sha256': scheduler.file_digest(ordinary)},
            ]
            scheduler.dag_snapshot(targets, root, 'fixture')
            self.assertNotEqual(targets[0]['executable'], targets[0]['source'])
            self.assertEqual(Path(targets[0]['executable']).read_bytes(), b'smoke')
            self.assertEqual(targets[1]['executable'], targets[1]['source'])

    def test_dag_catalog_rejects_escaped_or_ambiguous_compiler_artifacts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            package = root / 'tools/product-e2e'
            package.mkdir(parents=True)
            source = package / 'tests/smoke_cli.rs'
            source.parent.mkdir()
            source.touch()
            executable = root / 'target/debug/deps/smoke_cli-0123456789abcdef'
            executable.parent.mkdir(parents=True)
            executable.write_text('test artifact')
            declared = {'name': 'smoke_cli', 'kind': ['test'], 'test': True,
                        'src_path': str(source)}
            metadata = {'workspace_members': ['product-package'],
                        'packages': [{'id': 'product-package', 'name': 'hiroute-product-e2e',
                                      'manifest_path': str(package / 'Cargo.toml'),
                                      'targets': [declared]}]}
            event = {'reason': 'compiler-artifact', 'package_id': 'product-package',
                     'profile': {'test': True}, 'executable': str(executable),
                     'target': {'name': 'smoke_cli', 'kind': ['test'],
                                'src_path': str(source)}}
            log = '\n'.join((json.dumps(event), json.dumps({'reason': 'build-finished',
                                                            'success': True})))
            rows = scheduler.dag_catalog(log, root, metadata)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]['label'], 'tests/smoke_cli.rs')
            missing_source = source.with_name('omitted.rs')
            missing_source.touch()
            metadata['packages'][0]['targets'].append(
                {'name': 'omitted', 'kind': ['test'], 'test': True,
                 'src_path': str(missing_source)})
            with self.assertRaisesRegex(ValueError, 'missing=1'):
                scheduler.dag_catalog(log, root, metadata)
            metadata['packages'][0]['targets'].pop()
            with self.assertRaisesRegex(ValueError, 'Malformed Cargo JSON'):
                scheduler.dag_catalog(log + '\n{truncated', root, metadata)
            with self.assertRaisesRegex(ValueError, 'build-finished=0'):
                scheduler.dag_catalog(json.dumps(event), root, metadata)
            escaped = root / 'outside'
            escaped.write_text('unexpected')
            event['executable'] = str(escaped)
            with self.assertRaisesRegex(ValueError, 'escapes'):
                scheduler.dag_catalog(json.dumps(event), root, metadata)

    def test_filters_and_feature_changes_cannot_silently_narrow_the_schedule(self):
        for suffix in (['--lib'], ['--', '--ignored'], ['--no-default-features'], ['some_filter']):
            with self.subTest(suffix=suffix), self.assertRaises(ValueError):
                scheduler.commands(COMMAND + suffix)
        for suffix in ([], ['--no-fail-fast']):
            for name, command in scheduler.commands(COMMAND + suffix).items():
                self.assertEqual(command[:len(COMMAND + suffix)], COMMAND + suffix)

    def test_listing_requires_unique_smoke_and_complete_names(self):
        self.assertEqual(scheduler.listed_tests('default_two_domain_smoke: test\nother: test\n2 tests, 0 benchmarks\n'), 2)
        for text in ('0 tests, 0 benchmarks\n',
                     'default_two_domain_smoke: test\n2 tests, 0 benchmarks\n',
                     'default_two_domain_smoke: test\nextra_default_two_domain_smoke: test\n2 tests, 0 benchmarks\n'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                scheduler.listed_tests(text)

    def run_fixture(self, mode='pass'):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run = root / 'run'
            run.mkdir()
            binary = root / 'bin'
            binary.mkdir()
            test_binary = root / 'target/debug/deps/smoke_cli-deadbeef01234567'
            test_binary.parent.mkdir(parents=True)
            test_binary.write_text('#!' + sys.executable + '\n' + TEST_BINARY)
            test_binary.chmod(0o700)
            (root / 'tools/product-e2e').mkdir(parents=True)
            cargo = binary / 'cargo'
            cargo.write_text('#!' + sys.executable + '\n' + '''import os,sys,time,pathlib,json
a=sys.argv[1:]; root=pathlib.Path.cwd(); mode=os.environ['FIXTURE_MODE']
if '--no-run' in a:
 assert os.environ['CARGO_BUILD_JOBS']=='64'
 (root/'compiled').touch()
 print('  Executable tests/smoke_cli.rs (target/debug/deps/smoke_cli-deadbeef01234567)')
 sys.exit(0)
assert (root/'compiled').exists()
assert os.environ['CARGO_BUILD_JOBS']=='8'
if '--list' in a:
 print('default_two_domain_smoke: test\\nregression: test\\nprepare_default_smoke_builds: test\\n3 tests, 0 benchmarks'); sys.exit(0)
if 'prepare_default_smoke_builds' in a:
 (root/'prepared').touch()
 n=0 if mode=='missing-preparation' else 1
 env={'CARGO_BUILD_JOBS':'8','CARGO_PKG_NAME':'hiroute-product-e2e','CARGO_BIN_EXE_hiroute-smoke':'/fixture/hiroute-smoke','RUST_RECURSION_COUNT':'1'}
 artifacts=[]
 for package in ('hiroute-e2e','hiroute-cli','hiroute-daemon'):
  sha='sha256:'+package
  artifacts.append({'package':package,'sha256':sha})
  if mode=='missing-receipt' and package=='hiroute-cli': continue
  receipt=root/'target/smoke/builds/fixture'/package/'receipt.json'
  receipt.parent.mkdir(parents=True)
  receipt.write_text(json.dumps({'artifact':{'sha256':sha},'recipe':{'source_revision':'a'*40,'environment':env}}))
 report=root/'target/smoke/prepare-fixture/preparation.json'
 report.parent.mkdir(parents=True)
 report.write_text(json.dumps({'schema':'hiroute.smoke.preparation/v1','source_revision':'a'*40,'scenarios_executed':0,'artifacts':artifacts}))
 print('build preparation='+str(report))
 print(f'test result: ok. {n} passed; 0 failed; 0 ignored'); sys.exit(0)
assert (root/'prepared').exists()
assert '--skip' in a
(root/'remainder.ready').touch()
until=time.monotonic()+5
while not (root/'smoke.ready').exists():
 if time.monotonic()>until: sys.exit(98)
 time.sleep(.01)
print('Running tests/remainder.rs (target/debug/deps/remainder-abcdef0123456789)')
print('running 2 tests')
print('test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s')
passed=0 if mode=='missing-remainder' else 1
print(f'test result: ok. {passed} passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s')
''')
            cargo.chmod(0o700)
            output = io.StringIO()
            with patch.dict(os.environ, PATH=str(binary) + os.pathsep + os.environ.get('PATH', ''), FIXTURE_MODE=mode), \
                 (root / 'checkout.lock').open('a') as lease:
                result = scheduler.execute(runner, root, run, root, dict(command=COMMAND, timeout=15, sha='a'*40), lease, output)
            if mode == 'missing-preparation':
                self.assertFalse((root / 'smoke.ready').exists())
            return result, output.getvalue()

    def test_preparation_then_concurrent_lanes_cover_every_listed_test_once(self):
        result, log = self.run_fixture()
        self.assertEqual(result['process_exit'], 0, result)
        self.assertTrue(result['complete'])
        self.assertEqual(result['counts'], dict(passed=2, failed=0, ignored=1))
        self.assertEqual(log.count('test result:'), 3, 'Nested summaries are not parent test counts')
        a, b = result['phases'][-2:]
        self.assertLess(max(a['started_at'], b['started_at']), min(a['finished_at'], b['finished_at']))

    def test_missing_preparation_never_starts_runtime(self):
        result, _ = self.run_fixture('missing-preparation')
        self.assertNotEqual(result['process_exit'], 0)
        self.assertIn('exactly once', result['error'])

    def test_unattested_preparation_never_starts_runtime(self):
        result, _ = self.run_fixture('missing-receipt')
        self.assertNotEqual(result['process_exit'], 0)
        self.assertIn('unique matching receipt', result['error'])

    def test_red_or_omitted_tests_cannot_produce_aggregate_green(self):
        for mode in ('red-smoke', 'missing-remainder'):
            with self.subTest(mode=mode):
                result, _ = self.run_fixture(mode)
                self.assertNotEqual(result['process_exit'], 0)
                self.assertFalse(result['complete'])


if __name__ == '__main__':
    unittest.main()
