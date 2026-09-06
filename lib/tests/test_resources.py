import concurrent.futures
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import time
import tomllib
import unittest
from unittest.mock import patch

REPO = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('resources', REPO / 'lib/src/resource_manager.py')
broker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(broker)


class ResourceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.config_file = self.directory / 'resources.toml'
        self.config_file.write_text((REPO / 'contrib/resources.toml').read_text())
        self.env = patch.dict(os.environ, CC_HUB_RESOURCE_CONFIG=str(self.config_file),
                              CC_HUB_RESOURCE_DIR=str(self.directory / 'state'))
        self.env.start()
        self.addCleanup(self.env.stop)
        self.cfg = broker.load_config()
        self.usage = {}
        for name, account in self.cfg['accounts'].items():
            models = {'claude-sonnet-5': ['medium']} if account['provider'] == 'claude' else {'gpt-5.6-luna': ['medium']}
            self.usage[name] = {'health': 'ready', 'observed_at': time.time(), 'pool': name,
                                'windows': [{'name': 'primary', 'used': 20, 'resets_at': time.time() + 3600}],
                                'models': models}
        self.db = {'workers': {}, 'cooldowns': {}}

    def choose(self, **kwargs):
        return broker.select(self.cfg, self.usage, self.db, 'project', 'dev', **kwargs)

    def start(self, role='dev'):
        args = broker.parser().parse_args(['start', '--task', 'tk-test', '--kind', 'project', '--role', role,
                                           '--cwd', str(self.directory), '--prompt', 'Implement fixture'])
        with patch.object(broker, 'launch'), patch.object(broker, 'bind_board'):
            return broker.start_worker(args, self.cfg, self.usage)

    def owner(self, worker, generation=None):
        return patch.dict(os.environ, CC_HUB_RESOURCE_WORKER=worker['id'],
                          CC_HUB_RESOURCE_GENERATION=str(generation or worker['generation']))

    def test_exhausted_unknown_and_stale_are_not_allocatable(self):
        self.usage['cc-2']['windows'][0]['used'] = 85
        self.usage['cc-1']['health'] = 'unknown'
        self.usage['codex-1']['observed_at'] = 0
        self.usage['codex-2']['health'] = 'login_required'
        self.assertIsNone(self.choose())

    def test_recovery_controls_cannot_smuggle_an_external_write(self):
        self.assertTrue(broker.recovery_command('cc-hub resource handoff --reason quota-reserve'))
        for command in ('cc-hub resource status; gh pr create', 'cc-hub resource status\ngh pr create',
                        'echo "cc-hub resource handoff" && gh pr create',
                        'cc-hub resource handoff --reason "$(gh pr create)"'):
            self.assertFalse(broker.recovery_command(command), command)

    def test_transient_probe_failure_preserves_fresh_timestamp_and_backs_off(self):
        broker.atomic_json(broker.root() / 'usage.json', self.usage)
        observed = self.usage['cc-1']['observed_at']
        with patch.object(broker, 'probe', return_value={'health': 'throttled', 'windows': [], 'error': 'usage HTTP 429'}) as probe:
            first = broker.refresh(self.cfg, force=True)
            self.assertEqual(first['cc-1']['health'], 'ready')
            self.assertEqual(first['cc-1']['observed_at'], observed)
            count = probe.call_count
            broker.refresh(self.cfg, force=True)
            self.assertEqual(probe.call_count, count)
        first['cc-1']['observed_at'] = 0
        self.usage = first
        for name in ('cc-2', 'codex-1', 'codex-2'):
            self.usage[name]['health'] = 'unknown'
        self.assertIsNone(self.choose())

    def test_tool_bootstrap_preserves_auth_and_existing_settings(self):
        spec = importlib.util.spec_from_file_location('installer', REPO / 'contrib/install-resources.py')
        installer = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(installer)
        source = self.directory / '.codex'
        personal = self.directory / '.codex-personal'
        (source / 'plugins/cache').mkdir(parents=True)
        personal.mkdir()
        (personal / 'auth.json').write_text('independent-auth-sentinel')
        (personal / 'config.toml').write_text('model="keep-this-model"\n')
        (source / 'config.toml').write_text('''
[plugins."dev@example-tools"]
enabled=true
[mcp_servers.unityMCP]
url="http://localhost:8080"
http_headers={Authorization="must-not-copy"}
[mcp_servers.unityMCP.env]
API_KEY="must-not-copy"
PROJECT="keep-project"
''')
        installer.share_tools(self.directory, ['example-tools'])
        value = tomllib.loads((personal / 'config.toml').read_text())
        self.assertEqual(value['model'], 'keep-this-model')
        self.assertNotIn('http_headers', value['mcp_servers']['unityMCP'])
        self.assertEqual(value['mcp_servers']['unityMCP']['env'], {'PROJECT': 'keep-project'})
        self.assertEqual((personal / 'auth.json').read_text(), 'independent-auth-sentinel')
        self.assertEqual(installer.share_tools(self.directory, ['example-tools']), [])

    def test_model_and_effort_must_be_available(self):
        for snapshot in self.usage.values():
            snapshot['models'] = {'gpt-6-astra': ['high']}
        self.assertIsNone(self.choose())

    def test_pin_and_capabilities_never_fall_through(self):
        self.cfg['routing']['project']['dev'].update(account='cc-1', requires=['fathom'])
        self.assertIsNone(self.choose())
        self.cfg['accounts']['cc-1']['capabilities'].append('fathom')
        self.assertEqual(self.choose()['account'], 'cc-1')
        self.usage['cc-1']['windows'][0]['used'] = 76
        self.assertIsNone(self.choose())

    def test_subscription_aliases_share_concurrency_and_cooldown(self):
        for snapshot in self.usage.values():
            snapshot['pool'] = 'same-subscription'
        self.db['workers'] = {str(i): {'pool': 'same-subscription', 'status': 'running'} for i in range(2)}
        self.assertIsNone(self.choose())
        self.db['workers'] = {}
        self.db['cooldowns']['same-subscription'] = time.time() + 60
        self.assertIsNone(self.choose())

    def test_no_duplicate_role_under_concurrent_starts(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            workers = list(pool.map(lambda _: self.start(), range(8)))
        self.assertEqual(len({w['id'] for w in workers}), 1)
        self.assertEqual(len(broker.state()['workers']), 1)

    def test_parallel_helpers_share_policy_but_have_independent_leases(self):
        root = self.start()
        first = self.start('worker-review-a')
        second = self.start('worker-review-b')
        self.assertNotEqual(first['id'], second['id'])
        self.assertEqual(first['root_id'], root['root_id'])
        self.assertEqual(second['root_id'], root['root_id'])

    def test_stage_qa_workers_share_policy_with_distinct_identities(self):
        root = self.start()
        editor = self.start('qa-editor')
        build = self.start('qa-build')
        self.assertNotEqual(editor['actor_id'], build['actor_id'])
        self.assertEqual(editor['root_id'], root['root_id'])
        self.assertEqual(build['root_id'], root['root_id'])

    def test_no_capacity_persists_queue(self):
        for snapshot in self.usage.values():
            snapshot['health'] = 'unknown'
        worker = self.start()
        self.assertEqual(worker['status'], 'waiting_for_capacity')
        self.assertEqual(worker['generation'], 0)

    def test_checkpoint_is_immutable_and_stale_owner_is_fenced(self):
        worker = self.start()
        source = self.directory / 'checkpoint.md'
        source.write_text('Uncommitted implementation; build job 12 pending')
        broker.checkpoint(worker, source)
        source.write_text('overwritten')
        self.assertIn('job 12', Path(worker['checkpoint']['path']).read_text())
        with self.owner(worker, generation=worker['generation'] + 1):
            with self.assertRaisesRegex(ValueError, 'stale'):
                broker.current_worker(broker.state(), require_owner=True)

    def test_handoff_stops_old_owner_before_replacement(self):
        worker = self.start()
        db = broker.state()
        db['workers'][worker['id']].update(status='handoff_requested', handoff_at=0, handoff_reason='quota')
        broker.save(db)
        live = {worker['tmux']}
        calls = []

        def run(argv, **kwargs):
            self.assertEqual(argv[:2], ['tmux', 'kill-session'])
            calls.append('stop')
            live.clear()
            return type('Result', (), {'returncode': 0})()

        def launch(replacement):
            self.assertFalse(live)
            calls.append('launch')
            self.assertNotEqual(replacement['account'], worker['account'])
            self.assertEqual(replacement['root_id'], worker['root_id'])

        with patch.object(broker, 'tmux_exists', side_effect=lambda name: name in live), \
             patch.object(broker, 'run', side_effect=run), patch.object(broker, 'launch', side_effect=launch), \
             patch.object(broker, 'bind_board'):
            broker.supervise(self.cfg, self.usage)
        self.assertEqual(calls, ['stop', 'launch'])
        self.assertEqual(broker.state()['workers'][worker['id']]['generation'], 2)

    def test_failed_stop_never_launches_second_owner(self):
        worker = self.start()
        db = broker.state()
        db['workers'][worker['id']]['status'] = 'stopping'
        broker.save(db)
        with patch.object(broker, 'tmux_exists', return_value=True), \
             patch.object(broker, 'run', return_value=type('Result', (), {'returncode': 1})()), \
             patch.object(broker, 'launch') as launch:
            broker.supervise(self.cfg, self.usage)
        launch.assert_not_called()
        self.assertEqual(broker.state()['workers'][worker['id']]['status'], 'stopping')

    def test_emergency_does_not_need_final_model_response(self):
        worker = self.start()
        db = broker.state()
        db['workers'][worker['id']]['status'] = 'running'
        broker.save(db)
        self.usage[worker['account']]['windows'][0]['used'] = 100
        live = {worker['tmux']}

        def run(argv, **kwargs):
            if argv[:2] == ['tmux', 'kill-session']:
                live.clear()
            return type('Result', (), {'returncode': 0, 'stdout': 'pending-job-12'})()

        with patch.object(broker, 'tmux_exists', side_effect=lambda name: name in live), \
             patch.object(broker, 'run', side_effect=run), patch.object(broker, 'launch'), patch.object(broker, 'bind_board'):
            broker.supervise(self.cfg, self.usage)
        current = broker.state()['workers'][worker['id']]
        self.assertEqual(current['generation'], 2)
        self.assertIn('pending-job-12', Path(current['checkpoint']['path']).read_text())

    def test_generation_specific_launch_and_prompt_contract(self):
        worker = self.start()
        command, env = broker.execution(worker, self.cfg)
        self.assertEqual(env['CC_HUB_RESOURCE_ROOT'], worker['root_id'])
        self.assertIn('resource handoff', command[-1])
        self.assertIn('MUST use `cc-hub resource start`', command[-1])
        self.assertNotIn('OPENAI_API_KEY', env)
        if command[0] == 'codex':
            override = next(v for v in command if v.startswith('hooks.PreToolUse='))
            tomllib.loads(override)

    def test_native_quota_records_are_distinct_from_throttles_or_quoted_text(self):
        worker = self.start()
        transcript = self.directory / 'rollout.jsonl'
        worker['transcript_path'] = str(transcript)
        quota = {'type': 'event_msg', 'payload': {'type': 'error', 'codex_error_info': 'UsageLimitExceeded'}}
        transcript.write_text(json.dumps(quota) + '\n')
        self.assertTrue(broker.native_quota_error(worker, self.cfg))
        transcript.write_text(json.dumps({'type': 'response_item', 'payload': {'type': 'function_call_output', 'output': json.dumps(quota)}}) + '\n')
        self.assertFalse(broker.native_quota_error(worker, self.cfg))
        transcript.write_text(json.dumps({'type': 'event_msg', 'payload': {'type': 'error', 'codex_error_info': 'HttpConnectionFailed'}}) + '\n')
        self.assertFalse(broker.native_quota_error(worker, self.cfg))
        transcript.write_text(json.dumps(quota) + '\n' + json.dumps({'type': 'event_msg', 'payload': {'type': 'agent_message', 'message': 'Recovered'}}) + '\n')
        self.assertFalse(broker.native_quota_error(worker, self.cfg))

    def test_hook_warns_before_reserve_and_blocks_native_children(self):
        worker = self.start()
        self.usage[worker['account']]['windows'][0]['used'] = 81
        broker.atomic_json(broker.root() / 'usage.json', self.usage)
        with self.owner(worker), patch('sys.stdin', io.StringIO(json.dumps({'tool_name': 'Agent', 'tool_input': {}}))):
            with self.assertRaisesRegex(ValueError, 'spawn task workers'):
                broker.hook()
        with self.owner(worker), patch('sys.stdin', io.StringIO(json.dumps({'tool_name': 'Bash', 'tool_input': {'command': 'cc-hub resource status'}}))):
            result = broker.hook()
        self.assertIn('checkpoint-and-handoff', result['hookSpecificOutput']['additionalContext'])


if __name__ == '__main__':
    unittest.main()
