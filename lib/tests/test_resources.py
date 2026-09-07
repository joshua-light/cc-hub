import concurrent.futures
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
        self.board = self.directory / 'board'
        self.env = patch.dict(os.environ, CC_HUB_RESOURCE_CONFIG=str(self.config_file),
                              CC_HUB_RESOURCE_DIR=str(self.directory / 'state'),
                              TASK_BOARD_DIR=str(self.board))
        self.env.start()
        self.addCleanup(self.env.stop)
        self.cfg = broker.load_config()
        self.usage = {}
        for name, account in self.cfg['accounts'].items():
            models = account['models'] if account['provider'] == 'claude' else {'gpt-5.6-luna': ['medium']}
            self.usage[name] = {'health': 'ready', 'observed_at': time.time(), 'pool': name,
                                'windows': [{'name': 'primary', 'used': 20, 'resets_at': time.time() + 3600}],
                                'models': models}
        self.db = {'workers': {}, 'cooldowns': {}}

    def choose(self, **kwargs):
        return broker.select(self.cfg, self.usage, self.db, 'project', 'implementation', **kwargs)

    def start(self, role='implementation', task='tk-test', cwd=None):
        args = broker.parser().parse_args(['start', '--task', task, '--kind', 'project', '--role', role,
                                           '--cwd', str(cwd or self.directory), '--prompt', 'Implement fixture'])
        with patch.object(broker, 'launch'), patch.object(broker, 'bind_board'):
            return broker.start_worker(args, self.cfg, self.usage)

    def supervise(self, tmux_alive=False):
        with patch.object(broker, 'launch'), patch.object(broker, 'bind_board'), \
                patch.object(broker, 'tmux_exists', return_value=tmux_alive), \
                patch.object(broker, 'run', return_value=type('done', (), {'returncode': 0, 'stdout': ''})()):
            return {w['id']: w for w in broker.supervise(self.cfg, self.usage)}

    def owner(self, worker, generation=None):
        return patch.dict(os.environ, CC_HUB_RESOURCE_WORKER=worker['id'],
                          CC_HUB_RESOURCE_GENERATION=str(generation or worker['generation']))

    # ─── selection ───────────────────────────────────────────────────────

    def test_exhausted_unknown_and_stale_are_not_allocatable(self):
        self.usage['cc-2']['windows'][0]['used'] = 85
        self.usage['cc-1']['health'] = 'unknown'
        self.usage['codex-1']['observed_at'] = 0
        self.usage['codex-2']['health'] = 'login_required'
        self.assertIsNone(self.choose())

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
        self.cfg['routing']['project']['implementation'].update(account='cc-1', requires=['fathom'])
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

    def test_unknown_role_has_no_policy(self):
        with self.assertRaises(ValueError):
            broker.policy(self.cfg, 'project', 'qa')

    # ─── one worker per task ─────────────────────────────────────────────

    def test_concurrent_starts_share_one_worker(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            results = list(pool.map(lambda _: self.start(), range(4)))
        self.assertEqual(len({w['id'] for w in results}), 1)
        self.assertEqual(sum(1 for w in results if not w.get('reused')), 1)

    def test_start_in_another_role_hands_the_task_over(self):
        first = self.start('implementation')
        second = self.start('verification')
        self.assertNotEqual(first['id'], second['id'])
        self.assertEqual(second['predecessor'], first['id'])
        self.assertEqual(second['status'], 'starting')
        workers = broker.state()['workers']
        self.assertEqual(workers[first['id']]['status'], 'stopping')
        self.assertEqual(workers[first['id']]['stop_reason'], 'handed to verification')
        # The old session is gone by the next tick; only one worker is live.
        with patch.object(broker.time, 'time', return_value=time.time() + 10):
            after = self.supervise()
        self.assertEqual(after[first['id']]['status'], 'stopped')
        self.assertEqual(after[second['id']]['status'], 'starting')
        self.assertEqual(broker.live_worker(broker.state(), 'tk-test')['id'], second['id'])

    def test_start_in_another_directory_hands_the_task_over(self):
        repository = self.directory / 'repository'
        repository.mkdir()
        scratch = self.start()
        moved = self.start(cwd=repository)
        self.assertNotEqual(moved['id'], scratch['id'])
        self.assertEqual(moved['cwd'], str(repository.resolve()))
        self.assertEqual(moved['predecessor'], scratch['id'])
        self.assertFalse(moved.get('reused'))
        workers = broker.state()['workers']
        self.assertEqual(workers[scratch['id']]['status'], 'stopping')
        self.assertEqual(workers[scratch['id']]['stop_reason'], 'moved to ' + str(repository.resolve()))

    def test_start_in_the_same_place_reuses_the_worker(self):
        first = self.start()
        again = self.start()
        self.assertEqual(again['id'], first['id'])
        self.assertTrue(again['reused'])

    def test_a_task_that_changed_hands_can_change_back(self):
        implementation = self.start('implementation')
        verification = self.start('verification')
        again = self.start('implementation')
        self.assertNotEqual(again['id'], implementation['id'])
        self.assertEqual(again['predecessor'], verification['id'])

    def test_no_capacity_persists_queue(self):
        for snapshot in self.usage.values():
            snapshot['windows'][0]['used'] = 90
        worker = self.start()
        self.assertEqual(worker['status'], 'waiting_for_capacity')
        self.assertIn(worker['id'], broker.state()['workers'])

    def test_stale_generation_is_fenced(self):
        worker = self.start()
        with self.owner(worker, generation=worker['generation'] + 1), self.assertRaises(ValueError):
            broker.current_worker(broker.state(), require_owner=True)
        with self.owner(worker):
            broker.current_worker(broker.state(), require_owner=True)

    # ─── the session ─────────────────────────────────────────────────────

    def test_launch_carries_identity_and_no_api_keys(self):
        worker = self.start()
        with patch.dict(os.environ, ANTHROPIC_API_KEY='must-not-leak', OPENAI_API_KEY='must-not-leak'):
            command, env = broker.execution(worker, self.cfg)
        self.assertEqual(env['CC_HUB_RESOURCE_WORKER'], worker['id'])
        self.assertEqual(env['CC_HUB_RESOURCE_ROLE'], 'implementation')
        self.assertNotIn('ANTHROPIC_API_KEY', env)
        self.assertNotIn('OPENAI_API_KEY', env)
        self.assertIn('skills/task/SKILL.md', command[-1])
        self.assertIn('Implement fixture', command[-1])
        for word in ('checkpoint', 'inbox', 'qa'):
            self.assertNotIn(word, command[-1].lower())
        if command[0] == 'codex':
            override = next(v for v in command if v.startswith('hooks.PreToolUse='))
            tomllib.loads(override)
        else:
            profile = self.cfg['profiles'][worker['profile']]
            self.assertEqual(command[command.index('--model') + 1], profile['model'])
            self.assertEqual(command[command.index('--autocompact') + 1], str(profile['autocompact']))
            self.assertEqual(command[command.index('--session-id') + 1], worker['session_id'])

    def test_hook_records_the_transcript_and_blocks_nothing(self):
        worker = self.start()
        broker.atomic_json(broker.root() / 'usage.json', self.usage)
        event = {'session_id': 'native-1', 'transcript_path': str(self.directory / 't.jsonl'),
                 'tool_name': 'Agent', 'tool_input': {}}
        with self.owner(worker), patch.object(broker, 'bind_board'), \
                patch('sys.stdin', io.StringIO(json.dumps(event))):
            result = broker.hook()
        context = json.loads(result['hookSpecificOutput']['additionalContext'])['resource']
        self.assertEqual(context['quota_used_percent'], 20)
        self.assertNotIn('note', context)
        self.assertNotIn('decision', result)
        stored = broker.state()['workers'][worker['id']]
        self.assertEqual(stored['transcript_path'], str(self.directory / 't.jsonl'))
        self.assertEqual(stored['provider_session_id'], 'native-1')
        self.usage[worker['account']]['windows'][0]['used'] = 82
        broker.atomic_json(broker.root() / 'usage.json', self.usage)
        with self.owner(worker), patch('sys.stdin', io.StringIO(json.dumps(event))):
            result = broker.hook()
        self.assertIn('another account', json.loads(result['hookSpecificOutput']['additionalContext'])['resource']['note'])

    def test_a_broken_hook_fails_open(self):
        with patch.dict(os.environ, CC_HUB_RESOURCE_WORKER='nobody', CC_HUB_RESOURCE_GENERATION='1'), \
                patch('sys.stdin', io.StringIO('{}')), patch('sys.stderr', io.StringIO()):
            self.assertEqual(broker.main(['hook']), 1)

    # ─── replacement from the transcript ─────────────────────────────────

    def test_exhaustion_replaces_the_worker_and_resumes_its_transcript(self):
        worker = self.start()
        account = self.cfg['accounts'][worker['account']]
        transcript = self.directory / 'homes' / worker['account'] / 'projects' / broker.encoded_cwd(worker['cwd']) / (worker['session_id'] + '.jsonl')
        transcript.parent.mkdir(parents=True)
        transcript.write_text('{"type":"assistant"}\n')
        db = broker.state()
        db['workers'][worker['id']].update(status='running', transcript_path=str(transcript))
        broker.save(db)
        self.usage[worker['account']]['windows'][0]['used'] = 96
        first = self.supervise(tmux_alive=True)[worker['id']]
        self.assertEqual(first['status'], 'replacing')
        self.assertEqual(first['previous_transcript'], str(transcript))
        second = self.supervise()[worker['id']]
        self.assertEqual(second['generation'], 2)
        self.assertEqual(second['status'], 'starting')
        self.assertNotEqual(second['account'], worker['account'])
        self.assertEqual(second['session_id'], worker['session_id'])
        self.assertIn(worker['pool'], broker.state()['cooldowns'])
        successor = self.cfg['accounts'][second['account']]
        if successor['provider'] == 'claude':
            successor['home'] = str(self.directory / 'homes' / second['account'])
            command, _ = broker.execution(second, self.cfg)
            self.assertEqual(command[command.index('--resume') + 1], worker['session_id'])
            carried = Path(successor['home']) / 'projects' / broker.encoded_cwd(worker['cwd']) / (worker['session_id'] + '.jsonl')
            self.assertEqual(carried.read_text(), transcript.read_text())
        else:
            command, _ = broker.execution(second, self.cfg)
            self.assertIn(str(transcript), command[-1])
        self.assertEqual(broker.state()['workers'][worker['id']]['generation'], 2)
        self.assertEqual(account['provider'] in ('claude', 'codex'), True)

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

    def test_an_unexplained_exit_blocks_and_retry_requeues(self):
        worker = self.start()
        db = broker.state()
        db['workers'][worker['id']]['status'] = 'running'
        broker.save(db)
        blocked = self.supervise()[worker['id']]
        self.assertEqual(blocked['status'], 'blocked')
        with patch.object(broker, 'tmux_exists', return_value=False), patch('sys.stdout', io.StringIO()):
            self.assertEqual(broker.main(['retry', '--worker', worker['id']]), 0)
        self.assertEqual(broker.state()['workers'][worker['id']]['status'], 'waiting_for_capacity')

    def test_attempt_limit_blocks_instead_of_retrying_forever(self):
        worker = self.start()
        db = broker.state()
        db['workers'][worker['id']].update(status='waiting_for_capacity', generation=self.cfg['settings']['max_attempts'])
        broker.save(db)
        self.assertEqual(self.supervise()[worker['id']]['status'], 'blocked')

    def test_stop_ends_a_worker(self):
        worker = self.start()
        with patch('sys.stdout', io.StringIO()):
            self.assertEqual(broker.main(['stop', '--worker', worker['id'], '--reason', 'done']), 0)
        with patch.object(broker.time, 'time', return_value=time.time() + 10):
            self.assertEqual(self.supervise()[worker['id']]['status'], 'stopped')

    # ─── the card ────────────────────────────────────────────────────────

    def test_the_card_shows_the_broker_state_and_nothing_else(self):
        (self.board / 'tk-test').mkdir(parents=True)
        for snapshot in self.usage.values():
            snapshot['windows'][0]['used'] = 90
        worker = self.start()
        label = json.loads((self.board / 'tk-test/resources.json').read_text())
        self.assertEqual(label['stage'], 'capacity_wait')
        self.assertIn('implementation', label['detail'])
        db = broker.state()
        db['workers'][worker['id']]['status'] = 'stopped'
        broker.save(db)
        self.assertFalse((self.board / 'tk-test/resources.json').exists())


if __name__ == '__main__':
    unittest.main()
