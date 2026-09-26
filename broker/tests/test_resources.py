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

    def test_a_lapsed_token_wakes_claude_code_and_a_missing_login_does_not(self):
        account = self.cfg['accounts']['cc-2']
        calls = []

        def store(argv, **kwargs):
            calls.append(argv)
            body = {'ok': True, 'health': 'login_required', 'token_expires_at': expires_at}
            return type('done', (), {'returncode': 0, 'stdout': json.dumps(body)})()

        expires_at = time.time() - 60
        with patch.object(broker, 'run', side_effect=store):
            result = broker.probe(account)
        self.assertEqual(result['health'], 'unknown')
        self.assertGreater(result['retry_at'], time.time())
        self.assertEqual(calls[1][:3], ['claude', '-p', 'ok'])
        self.assertIn(broker.WAKE_MODEL, calls[1])

        expires_at = None
        calls.clear()
        with patch.object(broker, 'run', side_effect=store):
            result = broker.probe(account)
        self.assertEqual(result['health'], 'login_required')
        self.assertEqual(len(calls), 1)

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

    def test_a_policy_may_spend_the_reserve_the_others_leave_alone(self):
        rules = self.cfg['routing']['project']['implementation']
        for snapshot in self.usage.values():
            snapshot['windows'][0]['used'] = 88
        self.assertIsNone(self.choose())
        rules['start_percent'] = 94
        self.assertIsNotNone(self.choose())
        # It raises a ceiling, never lowers one: cc-1 keeps its own reserve.
        rules['start_percent'] = 50
        self.usage['cc-1']['windows'][0]['used'] = 60
        self.assertEqual(self.choose(exclude='cc-2')['account'], 'cc-1')

    def test_a_policy_ceiling_stops_below_the_stop_percent(self):
        self.cfg['routing']['project']['implementation']['start_percent'] = \
            self.cfg['settings']['stop_percent']
        with self.assertRaises(ValueError):
            broker.policy(self.cfg, 'project', 'implementation')

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

    # ─── resources ───────────────────────────────────────────────────────

    def claim(self, worker, *names, wait=0):
        args = broker.parser().parse_args(['claim', *names, '--wait', str(wait)])
        with self.owner(worker):
            return broker.claim_and_wait(self.cfg, args)

    def release(self, worker, *names):
        args = broker.parser().parse_args(['release', *names])
        with self.owner(worker), broker.lock():
            db = broker.state()
            live = broker.current_worker(db, require_owner=True)
            freed = broker.release(db, live, args.names or None)
            broker.save(db)
            return freed

    def alive(self, *pids):
        """Which processes exist, as the broker sees them: a guest is alive
        exactly as long as the process it claimed from."""
        stamps = {pid: 'started-%d' % pid for pid in pids}
        return patch.object(broker, 'process_stamp', side_effect=lambda pid: stamps.get(int(pid)) if pid else None)

    def guest_claim(self, pid, *names, alias='by hand', wait=0):
        argv = ['claim', *names, '--wait', str(wait), '--pid', str(pid)]
        return broker.claim_and_wait(self.cfg, broker.parser().parse_args(
            argv + (['--as', alias] if alias else [])))

    def guest_release(self, pid, *names):
        args = broker.parser().parse_args(['release', *names, '--pid', str(pid)])
        with broker.lock():
            db = broker.state()
            session = broker.current_session(db, args)
            freed = broker.release(db, session, args.names or None) if session else []
            broker.save(db)
            return freed

    def test_a_worker_starts_holding_nothing(self):
        for task in ('tk-one', 'tk-two'):
            self.assertEqual(self.start('verification', task=task)['status'], 'starting')
        self.assertEqual(broker.resource_holders(broker.state()), {})

    def test_a_claim_is_granted_once_and_queued_after_that(self):
        first, second = self.start(task='tk-one'), self.start(task='tk-two')
        self.assertEqual(self.claim(first, 'android'), {'ok': True, 'holds': ['android']})

        queued = self.claim(second, 'android')
        self.assertEqual((queued['ok'], queued['waiting_for']), (False, ['android']))
        self.assertEqual(queued['queue']['android'], {'held_by': 'tk-one', 'ahead': 0})

        listing = broker.resource_list(self.cfg, broker.state())
        self.assertEqual(listing['android']['holder'], 'tk-one')
        self.assertEqual(listing['android']['queue'], ['tk-two'])
        # The phone is taken; the checkout beside it is not.
        self.assertIsNone(listing['wh-tps']['holder'])

    def test_releasing_hands_the_resource_to_the_first_in_the_queue(self):
        first, second, third = (self.start(task=t) for t in ('tk-one', 'tk-two', 'tk-three'))
        self.claim(first, 'android')
        self.claim(second, 'android')
        self.claim(third, 'android')
        self.assertEqual(self.claim(third, 'android')['queue']['android']['ahead'], 1)

        self.assertEqual(self.release(first, 'android'), ['android'])
        db = broker.state()
        self.assertEqual(db['workers'][second['id']]['holds'], ['android'])
        self.assertEqual(db['workers'][third['id']]['wants'], ['android'])

    def test_a_session_that_ends_holds_nothing(self):
        first, second = self.start(task='tk-one'), self.start(task='tk-two')
        self.claim(first, 'android')
        self.claim(second, 'android')
        db = broker.state()
        broker.stop(db['workers'][first['id']], 'stopped by operator')
        broker.save(db)
        with patch.object(broker.time, 'time', return_value=time.time() + 10):
            after = self.supervise()
        self.assertEqual(after[first['id']]['status'], 'stopped')
        self.assertEqual(after[second['id']]['holds'], ['android'])

    def test_a_claim_is_the_whole_set_so_nothing_is_held_while_waiting(self):
        first, second = self.start(task='tk-one'), self.start(task='tk-two')
        self.claim(first, 'wh-tps')
        self.assertEqual(self.claim(second, 'android'), {'ok': True, 'holds': ['android']})
        # Asking for both hands back the phone rather than waiting on it,
        # which is what keeps two half-served claims from deadlocking.
        both = self.claim(second, 'android', 'wh-tps')
        self.assertEqual((both['ok'], both['waiting_for']), (False, ['android', 'wh-tps']))
        self.assertIsNone(broker.resource_list(self.cfg, broker.state())['android']['holder'])

        self.release(first)
        db = broker.state()
        self.assertEqual(db['workers'][second['id']]['holds'], ['android', 'wh-tps'])

    def test_reclaiming_the_same_set_changes_nothing(self):
        worker = self.start()
        self.claim(worker, 'android')
        at = broker.state()['workers'][worker['id']]
        self.assertEqual(self.claim(worker, 'android'), {'ok': True, 'holds': ['android']})
        self.assertEqual(broker.state()['workers'][worker['id']]['events'], at['events'])

    def test_an_unknown_resource_is_refused(self):
        worker = self.start()
        with self.assertRaises(ValueError):
            self.claim(worker, 'nowhere')

    def test_a_replaced_worker_keeps_what_it_holds(self):
        worker = self.start()
        self.claim(worker, 'main-tps')
        self.usage[worker['account']]['windows'][0]['used'] = 96
        self.supervise(tmux_alive=True)
        with patch.object(broker.time, 'time', return_value=time.time() + 10):
            after = self.supervise()
        self.assertEqual(after[worker['id']]['generation'], 2)
        self.assertEqual(after[worker['id']]['holds'], ['main-tps'])

    def test_a_resource_must_sit_on_a_configured_host(self):
        text = self.config_file.read_text()
        self.config_file.write_text(text.replace('host = "wh"', 'host = "nowhere"'))
        with self.assertRaises(ValueError):
            broker.load_config()

    def test_the_session_is_told_what_exists_and_how_to_ask(self):
        worker = self.start('verification')
        command, env = broker.execution(worker, self.cfg)
        self.assertIn('cc-hub resource claim', command[-1])
        self.assertIn('android — Android phone', command[-1])
        self.assertIn('ssh workhorse', command[-1])
        # Nothing is claimed for it, so nothing is announced as held.
        self.assertNotIn('already holds', command[-1])
        self.claim(worker, 'android')
        _, _ = broker.execution(broker.state()['workers'][worker['id']], self.cfg)
        self.assertIn('already holds android',
                      broker.execution(broker.state()['workers'][worker['id']], self.cfg)[0][-1])

    # ─── guests ──────────────────────────────────────────────────────────

    def test_a_guest_joins_the_one_queue_the_workers_wait_in(self):
        worker = self.start(task='tk-one')
        with self.alive(4242):
            self.assertEqual(self.guest_claim(4242, 'main-tps', alias='reviving TPS-21146'),
                             {'ok': True, 'holds': ['main-tps']})
            queued = self.claim(worker, 'main-tps')
            self.assertEqual(queued['queue']['main-tps'], {'held_by': 'reviving TPS-21146', 'ahead': 0})
            listing = broker.resource_list(self.cfg, broker.state())
            self.assertEqual((listing['main-tps']['holder'], listing['main-tps']['queue']),
                             ('reviving TPS-21146', ['tk-one']))

            # Its process is its name from here on: no second `--as`.
            self.assertEqual(self.guest_release(4242), ['main-tps'])
            self.assertEqual(broker.state()['workers'][worker['id']]['holds'], ['main-tps'])

    def test_a_worker_asked_first_so_the_guest_waits(self):
        worker = self.start(task='tk-one')
        self.claim(worker, 'android', 'main-tps')
        with self.alive(4242):
            waiting = self.guest_claim(4242, 'main-tps')
            self.assertEqual((waiting['ok'], waiting['holds']), (False, []))
            self.assertEqual(waiting['queue']['main-tps']['held_by'], 'tk-one')
            self.release(worker, 'main-tps')
            self.assertEqual(broker.resource_list(self.cfg, broker.state())['main-tps']['holder'], 'by hand')

    def test_a_guest_that_ended_holds_nothing(self):
        worker = self.start(task='tk-one')
        with self.alive(4242):
            self.guest_claim(4242, 'main-tps')
            self.claim(worker, 'main-tps')
        # Nothing released it; the process is simply gone.
        with self.alive():
            self.assertEqual(broker.state()['guests'], {})
            after = self.supervise()
        self.assertEqual(after[worker['id']]['holds'], ['main-tps'])

    def test_a_guest_names_itself_before_it_may_hold_anything(self):
        with self.alive(4242):
            with self.assertRaises(ValueError):
                self.guest_claim(4242, 'main-tps', alias=None)
            self.assertEqual(self.guest_release(4242), [])
            self.assertIsNone(broker.resource_list(self.cfg, broker.state())['main-tps']['holder'])

    def test_a_guest_claims_for_the_session_not_the_shell_it_typed_in(self):
        # As the broker is really reached: a `cc-hub` run from a shell, which
        # runs this file. Anchoring on either of the two would end the hold
        # with the command that made it.
        tree = {'900': '1 /usr/local/bin/claude', '901': '900 -zsh',
                '902': '901 /opt/cc-hub/bin/cc-hub', '903': '902 python3'}
        with patch.object(broker, 'run', side_effect=lambda argv, **kw: type(
                'done', (), {'returncode': 0, 'stdout': tree.get(argv[2], '')})()):
            self.assertEqual(broker.session_pid(start=903), 900)
        # An installed hub may answer to another name; it says which it is.
        tree['902'] = '901 /opt/hub/libexec/hubd'
        with patch.object(broker, 'run', side_effect=lambda argv, **kw: type(
                'done', (), {'returncode': 0, 'stdout': tree.get(argv[2], '')})()):
            with patch.dict(os.environ, CC_HUB_BINARY='/opt/hub/libexec/hubd'):
                self.assertEqual(broker.session_pid(start=903), 900)

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

    def test_a_claude_session_is_named_before_it_exists(self):
        args = broker.parser().parse_args(['start', '--task', 'tk-test', '--kind', 'project', '--role', 'implementation',
                                           '--cwd', str(self.directory), '--prompt', 'p', '--title', 'Polish: The PR'])
        with patch.object(broker, 'launch'), patch.object(broker, 'bind_board'):
            worker = broker.start_worker(args, self.cfg, self.usage)
        self.assertEqual(worker['title'], 'Polish: The PR')
        order = []
        done = type('done', (), {'returncode': 0, 'stdout': ''})()
        with patch.dict(os.environ, CC_HUB_BINARY='/opt/hub'), patch.object(broker, 'tmux_exists', return_value=False), \
                patch.object(broker, 'run', side_effect=lambda argv, **_: order.append(argv) or done):
            broker.launch(worker, self.cfg)
        provider = self.cfg['accounts'][worker['account']]['provider']
        verbs = [argv[1] if argv[0] == 'tmux' else argv[2] for argv in order]
        self.assertEqual(verbs, ['_name', 'new-session'] if provider == 'claude' else ['new-session'])
        if provider == 'claude':
            self.assertEqual(json.loads(order[0][3])['session_id'], worker['session_id'])

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
        self.assertIn(worker['pool'], broker.state()['cooldowns'])
        successor = self.cfg['accounts'][second['account']]
        if successor['provider'] == 'claude':
            self.assertEqual(second['session_id'], worker['session_id'], 'Claude→Claude is the same session')
            successor['home'] = str(self.directory / 'homes' / second['account'])
            command, _ = broker.execution(second, self.cfg)
            self.assertEqual(command[command.index('--resume') + 1], worker['session_id'])
            carried = Path(successor['home']) / 'projects' / broker.encoded_cwd(worker['cwd']) / (worker['session_id'] + '.jsonl')
            self.assertEqual(carried.read_text(), transcript.read_text())
        else:
            self.assertNotEqual(second['session_id'], worker['session_id'], 'a hand-off is a new session')
            command, _ = broker.execution(second, self.cfg)
            self.assertIn(str(transcript), command[-1])
        self.assertEqual(broker.state()['workers'][worker['id']]['generation'], 2)
        self.assertEqual(account['provider'] in ('claude', 'codex'), True)

    def test_a_codex_transcript_is_handed_to_claude_not_resumed(self):
        # The successor is Claude; the generation that ran dry was Codex. Its
        # rollout is not a Claude session, so `--resume` on it would only die.
        worker = self.start()
        rollout = self.directory / 'rollout.jsonl'
        rollout.write_text('{"type":"session_meta"}\n')
        codex = next(name for name, a in self.cfg['accounts'].items() if a['provider'] == 'codex')
        db = broker.state()
        db['workers'][worker['id']].update(status='running', account=codex, pool=self.usage[codex]['pool'],
                                           profile='luna-medium', model='gpt-5.6-luna', transcript_path=str(rollout))
        broker.save(db)
        self.usage[codex]['windows'][0]['used'] = 96
        first = self.supervise(tmux_alive=True)[worker['id']]
        self.assertEqual(first['previous_provider'], 'codex')
        second = self.supervise()[worker['id']]
        successor = self.cfg['accounts'][second['account']]
        self.assertEqual(successor['provider'], 'claude')
        self.assertNotEqual(second['session_id'], worker['session_id'], 'a hand-off is a new session')
        self.assertNotIn('transcript_path', second, 'the rollout belongs to the generation that wrote it')
        successor['home'] = str(self.directory / 'homes' / second['account'])
        command, _ = broker.execution(second, self.cfg)
        self.assertNotIn('--resume', command)
        self.assertEqual(command[command.index('--session-id') + 1], second['session_id'])
        self.assertIn(str(rollout), command[-1])
        self.assertFalse((Path(successor['home']) / 'projects').exists(), 'nothing is copied into the Claude home')

    def test_a_generation_that_wrote_nothing_passes_on_the_transcript_it_was_handed(self):
        worker = self.start()
        rollout = self.directory / 'rollout.jsonl'
        rollout.write_text('{"type":"session_meta"}\n')
        db = broker.state()
        db['workers'][worker['id']].update(status='running', generation=2, previous_transcript=str(rollout),
                                           previous_provider='codex')
        broker.save(db)
        blocked = self.supervise()[worker['id']]
        self.assertEqual(blocked['status'], 'blocked')
        with patch.object(broker, 'tmux_exists', return_value=False), patch('sys.stdout', io.StringIO()):
            self.assertEqual(broker.main(['retry', '--worker', worker['id']]), 0)
        retried = broker.state()['workers'][worker['id']]
        self.assertEqual(retried['previous_transcript'], str(rollout))
        self.assertEqual(retried['previous_provider'], 'codex')

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

    def test_stop_retires_a_blocked_worker_at_once(self):
        worker = self.start()
        worker['status'] = 'blocked'
        broker.stop(worker, 'card finished')
        self.assertEqual(worker['status'], 'stopped')
        self.assertEqual(worker['events'][-1]['event'], 'stopped')
        self.assertEqual(worker['events'][-1]['reason'], 'card finished')

    def test_stop_retires_a_worker_that_never_got_a_session_at_once(self):
        self.usage.clear()  # No capacity: the worker waits without a tmux session.
        worker = self.start()
        self.assertEqual((worker['status'], worker.get('tmux')), ('waiting_for_capacity', None))
        broker.stop(worker, 'verified elsewhere')
        self.assertEqual(worker['status'], 'stopped')
        self.assertEqual(worker['events'][-1]['reason'], 'verified elsewhere')

    def test_supervise_survives_a_stopping_worker_without_a_session(self):
        self.usage.clear()
        worker = self.start()
        db = broker.state()
        db['workers'][worker['id']].update(status='stopping', stop_at=0)  # Recorded before stop() retired these itself.
        broker.save(db)
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
        for snapshot in self.usage.values():
            snapshot['windows'][0]['used'] = 20
        (self.board / 'tk-held').mkdir()
        holder = self.start('verification', task='tk-held')
        self.claim(holder, 'android')
        (self.board / 'tk-next').mkdir()
        waiter = self.start('verification', task='tk-next')
        self.claim(waiter, 'android')
        label = json.loads((self.board / 'tk-next/resources.json').read_text())
        self.assertEqual((label['stage'], label['detail']),
                         ('resource_wait', 'verification / waiting for android'))
        db = broker.state()
        db['workers'][worker['id']]['status'] = 'stopped'
        broker.save(db)
        self.assertFalse((self.board / 'tk-test/resources.json').exists())


if __name__ == '__main__':
    unittest.main()
