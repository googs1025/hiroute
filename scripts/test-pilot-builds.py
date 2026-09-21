#!/usr/bin/env python3
"""Real Git/worktree/file lifecycle with fake build tools, NOT product E2E."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("builds", Path(__file__).with_name("pilot-builds.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class Lifecycle(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        subprocess.run(['git', 'init', '-q', str(self.repo)], check=True)
        for key, value in [('user.name', 'Fixture'), ('user.email', 'fixture@example.invalid'), ('core.hooksPath', '/dev/null')]:
            m.local.git(self.repo, 'config', key, value)
        (self.repo / '.gitignore').write_text('target/\napps/desktop/node_modules/\napps/desktop/dist/\napps/desktop/src-tauri/gen/\n')
        (self.repo / 'rust-toolchain.toml').write_text('[toolchain]\nchannel = "fixture"\n')
        (self.repo / 'scripts').mkdir()
        (self.repo / 'scripts/desktop-pilot.py').write_text('import json,sys\nprint(json.dumps({"build":{"frontendDist":sys.argv[-1]}}))\n')
        m.local.git(self.repo, 'add', '.')
        m.local.git(self.repo, 'commit', '-qm', 'fixture')
        m.local.git(self.repo, 'remote', 'add', 'origin', str(self.repo))
        self.branch = m.local.git(self.repo, 'symbolic-ref', 'HEAD')
        self.sha = m.local.git(self.repo, 'rev-parse', 'HEAD')
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        tools = '''import json,os,pathlib,sys
name=pathlib.Path(sys.argv[0]).name
if '--version' in sys.argv or '-vV' in sys.argv:
 print(name+' fixture-1');sys.exit(0)
root=pathlib.Path.cwd()
if name=='cargo' and 'metadata' in sys.argv:
 print(json.dumps({'target_directory':str(root/'target')}));sys.exit(0)
if name=='cargo':
 p=root/'target/debug';p.mkdir(parents=True)
 for name in ('hiroute-desktop','hirouted','hiroute'):
  (p/name).write_text('#!/bin/sh\\nexit 0\\n');(p/name).chmod(0o700)
 gen=root/'apps/desktop/src-tauri/gen';gen.mkdir(parents=True);(gen/'schema').write_text('generated')
 (root/'target/evidence').mkdir();(root/'target/evidence/result').write_text('unique evidence')
elif name=='npm':
 base=root/'apps/desktop';base.mkdir(parents=True,exist_ok=True)
 p=base/('node_modules' if 'ci' in sys.argv else 'dist');p.mkdir(exist_ok=True)
 (p/('dependency' if 'ci' in sys.argv else 'index.html')).write_text('fixture')
print('fixture build completed')
'''
        for name in ('cargo', 'rustc', 'sccache', 'npm', 'node', 'cc', 'cmake'):
            path = self.bin / name
            path.write_text('#!' + sys.executable + '\n' + tools)
            path.chmod(0o700)
        env = patch.dict(os.environ, {'PATH': str(self.bin) + os.pathsep + os.environ['PATH']})
        env.start()
        self.addCleanup(env.stop)
        stats = patch.object(m.local.remote, 'stats', return_value={'cache_hits': 0})
        stats.start()
        self.addCleanup(stats.stop)
        self.store = m.local.Store(self.root / 'state', self.root / 'host.lock')
        self.manager = m.Builds(self.store)

    def acquire(self, owner='task-one', cases=('model',), jobs=2):
        return self.manager.acquire(types.SimpleNamespace(repo=str(self.repo), ref=self.branch, sha=self.sha,
                                                         owner=owner, case=list(cases), jobs=jobs, timeout=20))

    def start(self, result, case='model'):
        def launch(args):
            root = Path(args.root)
            root.mkdir(mode=0o700)
            self.addCleanup(shutil.rmtree, root)
            data = root / 'data'
            data.mkdir(mode=0o700)
            session = dict(root=str(root), owner_uid=os.getuid(), build_run=args.build_run,
                           source_sha=args.source_sha, data_root=str(data))
            m.pilot.write_private_json(root / 'session.json', session)
            print(json.dumps(dict(session=str(root / 'session.json'), socket=str(root / 'pilot.sock'))))
            return 0
        with patch.object(m.pilot, 'start', side_effect=launch):
            return self.manager.start(types.SimpleNamespace(lease=result['lease'], case=case, timeout=1,
                                                           data_root=None, process_home=None))

    def finish(self, result, verdicts=('model=not_executed',), stop_error=None, debug=True):
        diagnostics = {'roles': {role: {'evidence': {'level_applied': {'level': 'debug' if debug else 'info'}}}
                                 for role in ('desktop', 'daemon')}}
        with patch.object(m.pilot, 'stop_group', side_effect=stop_error, return_value=[]), \
                patch.object(m.pilot, 'diagnostics_index', return_value=diagnostics):
            return self.manager.finish(types.SimpleNamespace(lease=result['lease'], case=list(verdicts), evidence_saved=True))

    def test_same_candidate_builds_once_and_same_owner_is_idempotent(self):
        first = self.acquire()
        second = self.acquire()
        third = self.acquire('task-two')
        self.assertFalse(first['reused'])
        self.assertTrue(second['reused'])
        self.assertEqual(first['lease'], second['lease'])
        self.assertNotEqual(first['lease'], third['lease'])
        self.assertEqual(first['build_run'], third['build_run'])
        self.assertEqual(len(list((self.store.root / 'checkouts').iterdir())), 1)

    def test_frontend_is_same_candidate_and_generated_dependencies_are_discarded(self):
        result = self.acquire()
        row = self.store.load(result['build_run'])
        self.assertEqual(m.local.git(Path(row['checkout']), 'rev-parse', 'HEAD'), self.sha)
        for directory in ('node_modules', 'dist', 'src-tauri/gen'):
            self.assertFalse((Path(row['checkout']) / 'apps/desktop' / directory).exists())
        self.assertEqual(m.tree_digest(Path(row['pilot_frontend']['path'])), row['pilot_frontend']['sha256'])

    def test_changed_build_jobs_cannot_share_bundle(self):
        first = self.acquire(jobs=2)
        second = self.acquire(jobs=3)
        self.assertNotEqual(first['build_run'], second['build_run'])

    def test_changed_build_environment_cannot_share_bundle(self):
        first = self.acquire()
        with patch.dict(os.environ, {'RUSTFLAGS': '-C debuginfo=1'}):
            second = self.acquire('task-two')
        self.assertNotEqual(first['build_run'], second['build_run'])

    def test_failed_build_is_retained_without_creating_duplicate_on_retry(self):
        cargo = self.bin / 'cargo'
        source = cargo.read_text()
        cargo.write_text(source.replace("root=pathlib.Path.cwd()", "\nif name=='cargo' and 'build' in sys.argv: sys.exit(4)\nroot=pathlib.Path.cwd()"))
        with self.assertRaisesRegex(ValueError, 'build failed/unknown'):
            self.acquire()
        with self.assertRaisesRegex(ValueError, 'Previous build failed/interrupted'):
            self.acquire()
        self.assertEqual(len(list((self.store.root / 'checkouts').iterdir())), 1)

    def test_interrupted_acquisition_is_indexed_before_compilation(self):
        recipe = m.build_recipe(self.repo, self.sha, 2)
        key = m.digest(recipe)
        def disconnected(row):
            m.local.remote.save(self.manager.index(key), {'run': row['id'], 'key': key})
            raise RuntimeError('simulated disconnect')
        args = types.SimpleNamespace(repo=str(self.repo), ref=self.branch, sha=self.sha, source='origin',
                                     jobs=2, command=m.COMMAND, keep=True, cargo_only=False, timeout=20)
        with self.assertRaisesRegex(RuntimeError, 'simulated disconnect'):
            m.local.run(self.store, args, on_record=disconnected)
        with self.assertRaisesRegex(ValueError, 'Previous build failed/interrupted'):
            self.acquire()
        self.assertEqual(len(list((self.store.root / 'runs').iterdir())), 1)
        self.assertEqual(len(list((self.store.root / 'checkouts').iterdir())), 0)

    def test_raw_pilot_start_cannot_create_unregistered_session(self):
        result = self.acquire()
        args = m.pilot.build_parser().parse_args(['start', '--build-run', result['build_run'],
                                                '--app', str(Path(result['checkout']) / 'target/debug/hiroute-desktop')])
        with patch.object(m.pilot.subprocess, 'Popen') as launch:
            # Verification itself uses subprocess.check_output, so exercise the
            # validated attestation boundary without mocking Git internals.
            with patch.object(m.pilot, 'verify_managed_pilot_build', return_value={'reuse_managed': True}):
                with self.assertRaisesRegex(ValueError, 'owner lease'):
                    m.pilot.start(args)
            launch.assert_not_called()

    def test_altered_binary_rejects_reuse_without_silent_rebuild(self):
        first = self.acquire()
        (Path(first['checkout']) / 'target/debug/hiroute').write_text('replaced')
        with self.assertRaisesRegex(ValueError, 'executable identity'):
            self.acquire('task-two')
        self.assertEqual(len(list((self.store.root / 'checkouts').iterdir())), 1)

    def test_altered_frontend_rejects_reuse(self):
        first = self.acquire()
        row = self.store.load(first['build_run'])
        (Path(row['pilot_frontend']['path']) / 'index.html').write_text('changed')
        with self.assertRaisesRegex(ValueError, 'frontend content'):
            self.acquire('task-two')

    def test_frontend_links_are_refused(self):
        first = self.acquire()
        row = self.store.load(first['build_run'])
        (Path(row['pilot_frontend']['path']) / 'link').symlink_to(self.repo / '.gitignore')
        with self.assertRaisesRegex(ValueError, 'link'):
            self.acquire('task-two')

    def test_last_owner_cleans_binaries_and_preserves_evidence_and_sessions(self):
        first = self.acquire()
        second = self.acquire('task-two')
        session = self.start(first)
        retained = self.finish(first, ('model=green',))
        self.assertEqual(retained['cleanup'], 'retained')
        self.assertTrue(Path(first['checkout']).exists())
        removed = self.finish(second)
        self.assertEqual(removed['cleanup'], 'removed')
        self.assertEqual(removed['scenario'], 'unassessed')
        self.assertFalse(Path(first['checkout']).exists())
        self.assertTrue(Path(session['session']).exists())
        run = self.store.record_path(first['build_run']).parent
        self.assertTrue((run / 'artifacts/evidence/result').exists())
        self.assertTrue((run / 'frontend-dist/index.html').exists())
        self.assertTrue((run / 'command.log').exists())
        self.assertEqual(self.finish(second)['cleanup'], 'removed')

    def test_all_executed_green_cases_can_finish_green(self):
        result = self.acquire(cases=('model', 'agent'))
        self.start(result, 'model')
        self.start(result, 'agent')
        final = self.finish(result, ('model=green', 'agent=green'))
        self.assertEqual(final['scenario'], 'green')

    def test_missing_unstarted_duplicate_assessments_do_not_release(self):
        result = self.acquire(cases=('model', 'agent'))
        for values in [('model=red',), ('model=green', 'agent=green'), ('model=red', 'model=green')]:
            with self.subTest(values=values), self.assertRaises(ValueError):
                self.finish(result, values)
        self.assertEqual(self.manager.locate(result['lease'])['pilot_leases'][result['lease']]['state'], 'open')

    def test_unknown_process_ownership_retains_open_lease_and_build(self):
        result = self.acquire()
        self.start(result)
        with self.assertRaisesRegex(ValueError, 'PID reused'):
            self.finish(result, ('model=red',), stop_error=ValueError('PID reused'))
        self.assertTrue(Path(result['checkout']).exists())
        self.assertEqual(self.manager.locate(result['lease'])['pilot_leases'][result['lease']]['state'], 'open')

    def test_wrong_diagnostic_level_does_not_claim_green(self):
        result = self.acquire()
        self.start(result)
        with self.assertRaisesRegex(ValueError, 'actual Debug'):
            self.finish(result, ('model=green',), debug=False)
        self.assertTrue(Path(result['checkout']).exists())

    def test_source_changes_refuse_cleanup_and_preserve_original_verdict(self):
        result = self.acquire()
        (Path(result['checkout']) / 'notes').write_text('unique evidence')
        finished = self.finish(result)
        self.assertEqual(finished['cleanup'], 'retained')
        self.assertTrue(Path(result['checkout']).exists())
        with self.assertRaisesRegex(ValueError, 'cannot be rewritten'):
            self.finish(result, ('model=red',))

    def test_raw_release_cannot_cleanup_open_pilot_lease(self):
        result = self.acquire()
        row = self.store.load(result['build_run'])
        row.update(keep=False, evidence_saved=True)
        self.store.save(row)
        self.assertEqual(self.store.preview_one(row)['state'], 'skipped')
        with self.assertRaisesRegex(ValueError, 'Pilot owners'):
            self.store.apply(row['id'], 'not-a-token')

    def test_interrupted_start_never_guesses_that_processes_are_absent(self):
        result = self.acquire()
        with patch.object(m.pilot, 'start', side_effect=RuntimeError('interrupted')):
            with self.assertRaisesRegex(RuntimeError, 'interrupted'):
                self.manager.start(types.SimpleNamespace(lease=result['lease'], case='model', timeout=1,
                                                        data_root=None, process_home=None))
        with self.assertRaises(OSError):
            self.finish(result, ('model=red',))
        self.assertTrue(Path(result['checkout']).exists())

    def test_same_key_in_flight_refuses_second_acquisition(self):
        result = self.acquire()
        row = self.manager.locate(result['lease'])
        with self.manager.locked(row['pilot_reuse']['key']):
            with self.assertRaises(BlockingIOError):
                self.acquire('task-two')


if __name__ == '__main__':
    unittest.main()
