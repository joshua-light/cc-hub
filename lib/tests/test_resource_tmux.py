"""Launch real terminals on a private socket; fake providers consume no quota."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

REPO = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('broker', REPO / 'lib/src/resource_manager.py')
broker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(broker)


@unittest.skipUnless(shutil.which('tmux') and os.name == 'posix', 'requires Unix tmux')
class TmuxIntegrationTests(unittest.TestCase):
    def test_profile_isolation_and_real_process_replacement(self):
        with tempfile.TemporaryDirectory(prefix='chr-', dir='/tmp') as directory:
            base = Path(directory)
            (base / 'socket').mkdir()
            executable = base / 'fake-claude'
            executable.write_text('#!' + sys.executable + '''
import json, os, time
from pathlib import Path
directory=Path(os.environ['CC_HUB_RESOURCE_DIR'])/'workers'/os.environ['CC_HUB_RESOURCE_WORKER']
directory.mkdir(parents=True,exist_ok=True)
(directory/('receipt-'+os.environ['CC_HUB_RESOURCE_GENERATION']+'.json')).write_text(json.dumps({
 'pid':os.getpid(), 'home':os.environ.get('CLAUDE_CONFIG_DIR'),
 'api_key_present':'ANTHROPIC_API_KEY' in os.environ}))
while True: time.sleep(.1)
''')
            executable.chmod(0o700)
            config = base / 'config.toml'
            config.write_text(f'''
[accounts.first]
provider="claude"
home="{base}/first"
executable="{executable}"
[accounts.second]
provider="claude"
home="{base}/second"
executable="{executable}"
[profiles.sonnet]
provider="claude"
model="claude-sonnet-5"
effort="medium"
accounts=["first","second"]
[routing.default.dev]
profiles=["sonnet"]
''')
            env = dict(os.environ, CC_HUB_RESOURCE_DIR=str(base / 'state'),
                       CC_HUB_RESOURCE_CONFIG=str(config), TMUX_TMPDIR=str(base / 'socket'),
                       TASK_JOURNAL_DIR=str(base / 'journal'), ANTHROPIC_API_KEY='fixture-must-not-leak')
            env.pop('TMUX', None)
            env.pop('CC_HUB_BINARY', None)
            with patch.dict(os.environ, env, clear=True):
                try:
                    cfg = broker.load_config()
                    usage = {name: {'health': 'ready', 'pool': name, 'observed_at': time.time(),
                                    'models': {'claude-sonnet-5': ['medium']},
                                    'windows': [{'name': 'primary', 'used': 10, 'resets_at': time.time() + 3600}]}
                             for name in ('first', 'second')}
                    args = broker.parser().parse_args(['start', '--task', 'tk-resource-fixture', '--kind', 'basic',
                                                       '--role', 'dev', '--cwd', str(base), '--prompt', 'No model calls'])
                    worker = broker.start_worker(args, cfg, usage)

                    def receipt(generation):
                        path = broker.worker_dir(worker) / f'receipt-{generation}.json'
                        deadline = time.monotonic() + 10
                        while not path.exists() and time.monotonic() < deadline:
                            time.sleep(.05)
                        self.assertTrue(path.exists(), 'fake provider failed to launch')
                        return json.loads(path.read_text())

                    first = receipt(1)
                    self.assertFalse(first['api_key_present'])
                    checkpoint = base / 'handoff.md'
                    checkpoint.write_text('Keep uncommitted changes; no outstanding tools')
                    db = broker.state()
                    current = db['workers'][worker['id']]
                    broker.checkpoint(current, checkpoint)
                    current.update(status='handoff_requested', handoff_at=0, handoff_reason='quota')
                    broker.save(db)
                    broker.supervise(cfg, usage)
                    second = receipt(2)
                    self.assertNotEqual(first['pid'], second['pid'])
                    self.assertNotEqual(first['home'], second['home'])
                    self.assertFalse(broker.tmux_exists(worker['tmux']))
                    self.assertEqual(broker.state()['workers'][worker['id']]['root_id'], worker['root_id'])
                    self.assertEqual(checkpoint.read_text(), 'Keep uncommitted changes; no outstanding tools')
                finally:
                    # This server was created under the private TMUX_TMPDIR above.
                    subprocess.run(['tmux', 'kill-server'], env=env, capture_output=True)


if __name__ == '__main__':
    unittest.main()
