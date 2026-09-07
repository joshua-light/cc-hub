#!/usr/bin/env python3
"""Account-aware task sessions. Standard library only; shipped in cc-hub.

A task has one worker at a time. `start` for a task that already has a live
worker in another role is a hand-over: the new worker is launched and the old
one is stopped. The broker owns which account a worker runs on and its tmux
lifetime. When an account runs dry the worker is replaced on another account
and continues from its own transcript; no LLM call is needed to notice, stop,
select or restart. Provider credentials stay in their own homes.
"""
import argparse
import contextlib
import datetime
import fcntl
import hashlib
import json
import os
import re
from pathlib import Path
import selectors
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
import uuid


def root():
    return Path(os.environ.get('CC_HUB_RESOURCE_DIR', Path.home() / '.cc-hub/resources'))


def config_path():
    return Path(os.environ.get('CC_HUB_RESOURCE_CONFIG', Path.home() / '.cc-hub/resources.toml'))


def read_json(path, default=None):
    try:
        return json.loads(path.read_text())
    except FileNotFoundError:
        return default


def atomic_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, delete=False) as stream:
        tmp = Path(stream.name)
        json.dump(value, stream)
        stream.flush()
        os.fsync(stream.fileno())
    try:
        tmp.replace(path)
    finally:
        tmp.unlink(missing_ok=True)


@contextlib.contextmanager
def lock(name='state'):
    root().mkdir(parents=True, exist_ok=True, mode=0o700)
    with (root() / (name + '.lock')).open('a') as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        yield


# ─── configuration ───────────────────────────────────────────────────────


def load_config():
    with config_path().open('rb') as stream:
        cfg = tomllib.load(stream)
    settings = cfg.setdefault('settings', {})
    settings.setdefault('warn_percent', 80)
    settings.setdefault('start_percent', 85)
    settings.setdefault('stop_percent', 95)
    settings.setdefault('refresh_seconds', 60)
    settings.setdefault('max_attempts', 20)
    settings.setdefault('stale_seconds', 900)
    if not 0 < settings['warn_percent'] < settings['start_percent'] < settings['stop_percent'] <= 100:
        raise ValueError('require 0 < warn_percent < start_percent < stop_percent <= 100')
    if settings['refresh_seconds'] < 10 or settings['max_attempts'] < 1:
        raise ValueError('refresh_seconds must be >= 10; max_attempts must be positive')
    if settings['stale_seconds'] < settings['refresh_seconds']:
        raise ValueError('stale_seconds must be >= refresh_seconds')
    for name, account in cfg.get('accounts', {}).items():
        if account.get('provider') not in ('claude', 'codex'):
            raise ValueError(f'{name}: provider must be claude or codex')
        if not account.get('home') and account.get('home_mode') != 'default':
            raise ValueError(f'{name}: set home or home_mode="default"')
        if not 0 < account.get('start_percent', settings['start_percent']) <= settings['start_percent']:
            raise ValueError(f'{name}: invalid start_percent reserve')
    for name, profile in cfg.get('profiles', {}).items():
        if not profile.get('model') or not profile.get('effort') or not profile.get('accounts'):
            raise ValueError(f'{name}: model, effort and accounts are required')
        window = profile.get('autocompact')
        if window is not None and not (profile.get('provider') == 'claude' and 100_000 <= window <= 1_000_000):
            raise ValueError(f'{name}: autocompact is Claude-only and must be 100000..1000000 tokens')
        for account in profile['accounts']:
            if account not in cfg.get('accounts', {}):
                raise ValueError(f'{name}: unknown account {account}')
            if cfg['accounts'][account]['provider'] != profile.get('provider'):
                raise ValueError(f'{name}: account/provider mismatch')
    return cfg


def account_home(account):
    return Path(account.get('home', '~/.claude' if account['provider'] == 'claude' else '~/.codex')).expanduser().resolve()


def account_env(account):
    env = dict(os.environ)
    # A subscription selection must not accidentally pick up API billing or
    # another account from a parent shell/tmux server.
    for name in ('CLAUDE_CONFIG_DIR', 'CODEX_HOME', 'ANTHROPIC_API_KEY',
                 'ANTHROPIC_AUTH_TOKEN', 'CLAUDE_CODE_OAUTH_TOKEN', 'OPENAI_API_KEY',
                 'CODEX_API_KEY', 'CODEX_ACCESS_TOKEN', 'OPENAI_BASE_URL',
                 'ANTHROPIC_BASE_URL', 'CLAUDE_CODE_USE_BEDROCK',
                 'CLAUDE_CODE_USE_VERTEX', 'CLAUDE_CODE_USE_FOUNDRY'):
        env.pop(name, None)
    if account['provider'] == 'codex':
        env['CODEX_HOME'] = str(account_home(account))
    elif account.get('home_mode') != 'default':
        env['CLAUDE_CONFIG_DIR'] = str(account_home(account))
    return env


def run(argv, **kwargs):
    return subprocess.run(argv, capture_output=True, text=True, timeout=kwargs.pop('timeout', 20), **kwargs)


def policy(cfg, kind, role):
    policies = cfg.get('routing', {})
    value = policies.get(kind, {}).get(role, policies.get('default', {}).get(role))
    if not value:
        raise ValueError(f'no routing policy for {kind}/{role}')
    if value.get('account') and value['account'] not in cfg.get('accounts', {}):
        raise ValueError('policy pins an unknown account')
    for name in value.get('profiles', []):
        if name not in cfg.get('profiles', {}):
            raise ValueError(f'unknown profile {name}')
    return value


# ─── account probes ──────────────────────────────────────────────────────


def rpc(account, requests):
    """Finite app-server session; never expose raw account/auth payloads."""
    process = subprocess.Popen([account.get('executable', 'codex'), 'app-server'],
                               env=account_env(account), stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    buffer = b''
    next_id = 0

    def send(value):
        process.stdin.write((json.dumps(value) + '\n').encode())
        process.stdin.flush()

    def request(method, params):
        nonlocal next_id, buffer
        next_id += 1
        send({'id': next_id, 'method': method, 'params': params})
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            while b'\n' in buffer:
                line, buffer = buffer.split(b'\n', 1)
                value = json.loads(line)
                if value.get('id') == next_id:
                    if 'error' in value:
                        raise ValueError(f'Codex {method} unavailable')
                    return value.get('result', {})
            if selector.select(max(0, deadline - time.monotonic())):
                data = os.read(process.stdout.fileno(), 65536)
                if not data:
                    raise ValueError('Codex app-server closed before responding')
                buffer += data
        raise TimeoutError(f'Codex {method} timed out')

    try:
        request('initialize', {'clientInfo': {'name': 'cc_hub', 'version': '1.0'}, 'capabilities': {}})
        send({'method': 'initialized'})
        return [request(method, params) for method, params in requests]
    finally:
        selector.close()
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        process.stdin.close()
        process.stdout.close()


def epoch(value):
    if value is None:
        return None
    if isinstance(value, (int, float)):
        return float(value)
    return datetime.datetime.fromisoformat(value.replace('Z', '+00:00')).timestamp()


def fingerprint(provider, identity):
    return provider + ':' + hashlib.sha256(identity.encode()).hexdigest()[:24]


def toml_value(value):
    if isinstance(value, dict):
        return '{' + ','.join(json.dumps(k) + '=' + toml_value(v) for k, v in value.items()) + '}'
    if isinstance(value, list):
        return '[' + ','.join(toml_value(v) for v in value) + ']'
    return json.dumps(value)


def probe(account, stale_seconds=900):
    if not account.get('enabled', True):
        return {'health': 'disabled', 'windows': []}
    try:
        if account['provider'] == 'codex':
            auth, limits, models = rpc(account, [
                ('account/read', {'refreshToken': False}),
                ('account/rateLimits/read', {}),
                ('model/list', {'limit': 100, 'includeHidden': False})])
            identity = auth.get('account') or {}
            if identity.get('type') != 'chatgpt':
                return {'health': 'login_required', 'windows': []}
            windows = []
            buckets = limits.get('rateLimitsByLimitId') or {'codex': limits.get('rateLimits', {})}
            for bucket_id, bucket in buckets.items():
                if not bucket:
                    continue
                for name in ('primary', 'secondary'):
                    window = bucket.get(name)
                    if window and window.get('usedPercent') is not None:
                        windows.append({'name': f'{bucket_id}:{name}', 'used': float(window['usedPercent']),
                                        'resets_at': epoch(window.get('resetsAt'))})
            key = identity.get('id') or identity.get('email')
            if not key:
                raise ValueError('Codex did not return an account identity')
            return {'health': 'ready' if windows else 'unknown', 'windows': windows,
                    'pool': fingerprint('codex', key),
                    'models': {m['model']: [e['reasoningEffort'] for e in m.get('supportedReasoningEfforts', [])]
                               for m in models.get('data', [])}}
        home = account_home(account)
        # One store for every consumer on this machine; see `cc-hub usage`.
        answer = run([os.environ.get('CC_HUB_BINARY', 'cc-hub'), 'usage', '--home', str(home)])
        report = json.loads(answer.stdout) if answer.returncode == 0 else {}
        if not report.get('ok'):
            return {'health': 'unknown', 'windows': [], 'error': 'usage store unavailable'}
        if report['health'] == 'login_required':
            return {'health': 'login_required', 'windows': []}
        identity = run([account.get('executable', 'claude'), 'auth', 'status', '--json'], env=account_env(account))
        auth = json.loads(identity.stdout) if identity.returncode == 0 else {}
        if not auth.get('loggedIn') or auth.get('authMethod') not in ('claude.ai', 'oauth'):
            return {'health': 'login_required', 'windows': []}
        key = ':'.join(str(auth.get(k) or '') for k in ('orgId', 'email'))
        if key == ':':
            raise ValueError('Claude did not return an account identity')
        windows = [{'name': name, 'used': float(report[name]['utilization']), 'resets_at': epoch(report[name].get('resets_at'))}
                   for name in ('five_hour', 'seven_day') if report.get(name)]
        # A throttled endpoint is not an exhausted account: a reading younger
        # than stale_seconds still says how much room there is.
        usable = windows and report.get('age_s', float('inf')) <= stale_seconds
        if report['health'] == 'ready' or usable:
            return {'health': 'ready', 'windows': windows, 'error': report.get('error'),
                    'pool': fingerprint('claude', str(key)), 'models': account.get('models', {})}
        if report['health'] == 'throttled':
            return {'health': 'throttled', 'windows': [], 'error': 'usage HTTP 429',
                    'retry_at': report.get('retry_at') or time.time() + 60}
        return {'health': 'unknown', 'windows': [], 'error': report.get('error') or 'usage unavailable'}
    except (OSError, ValueError, KeyError, TypeError, TimeoutError, subprocess.SubprocessError):
        # Raw exceptions may contain sensitive response bodies or CLI output.
        return {'health': 'unknown', 'windows': [], 'error': 'account probe unavailable; check profile login/tools'}


def refresh(cfg, force=False):
    with lock('usage'):
        cache = read_json(root() / 'usage.json', {})
        for name, account in cfg.get('accounts', {}).items():
            value = cache.get(name, {})
            now = time.time()
            if value.get('retry_at', 0) > now:
                continue
            if force or now - value.get('last_probe_at', value.get('observed_at', 0)) >= cfg['settings']['refresh_seconds']:
                result = probe(account, cfg['settings']['stale_seconds'])
                if result['health'] in ('unknown', 'throttled'):
                    failures = value.get('probe_failures', 0) + 1
                    retry_at = max(result.get('retry_at', 0), time.time() + min(600, 60 * 2 ** min(failures - 1, 4)))
                    if value.get('health') == 'ready' and now - value.get('observed_at', 0) <= cfg['settings']['refresh_seconds'] * 2:
                        # Preserve the observation timestamp: a failed refresh
                        # never makes old quota data appear fresh.
                        result = dict(value, error=result.get('error'))
                    result.update(retry_at=retry_at, probe_failures=failures)
                else:
                    result['probe_failures'] = 0
                if result.get('health') != 'ready' or value.get('health') != 'ready' or not result.get('retry_at'):
                    result['observed_at'] = time.time()
                result['last_probe_at'] = time.time()
                cache[name] = result
        atomic_json(root() / 'usage.json', cache)
        return cache


def applicable_windows(snapshot, model):
    # Claude publishes model-family subwindows as well as account-wide ones.
    return [w for w in snapshot.get('windows', [])
            if not any(family in w['name'] and family not in model for family in ('sonnet', 'opus'))]


def usage_of(worker, usage, cfg):
    """(used percent, fresh) for the worker's account on the worker's model."""
    snapshot = usage.get(worker['account'], {})
    model = worker.get('model') or cfg['profiles'][worker['profile']]['model']
    windows = applicable_windows(snapshot, model)
    used = max((w['used'] for w in windows), default=0)
    fresh = time.time() - snapshot.get('observed_at', 0) <= cfg['settings']['refresh_seconds'] * 2
    return used, fresh


# ─── state ───────────────────────────────────────────────────────────────

LIVE = ('starting', 'running', 'replacing', 'stopping')
# A worker that still holds its task; one on its way out no longer does.
HOLDING = ('waiting_for_capacity', 'starting', 'running', 'replacing')
CARD_STAGES = {'waiting_for_capacity': 'capacity_wait', 'replacing': 'resource_handoff', 'blocked': 'resource_blocked'}


def state():
    return read_json(root() / 'state.json', {'workers': {}, 'cooldowns': {}})


def save(value):
    atomic_json(root() / 'state.json', value)
    board = Path(os.environ.get('TASK_BOARD_DIR', Path.home() / '.cc-hub/tasks'))
    by_task = {}
    for worker in value['workers'].values():
        if re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]{0,127}', worker['task']):
            by_task.setdefault(worker['task'], []).append(worker)
    for task, workers in by_task.items():
        path = board / task / 'resources.json'
        if not path.parent.is_dir():
            continue
        worker = next((w for w in workers if w['status'] in CARD_STAGES), None)
        if worker is None:
            path.unlink(missing_ok=True)
            continue
        activity = {'stage': CARD_STAGES[worker['status']],
                    'detail': worker['role'] + ' / ' + worker.get('account', 'eligible accounts')}
        if read_json(path) != activity:
            atomic_json(path, activity)


def select(cfg, usage, db, kind, role, exclude=None):
    rules = policy(cfg, kind, role)
    candidates = []
    now = time.time()
    for order, name in enumerate(rules.get('profiles', [])):
        profile = cfg['profiles'][name]
        for account_id in profile['accounts']:
            account = cfg['accounts'][account_id]
            if not account.get('enabled', True) or account_id == exclude:
                continue
            if rules.get('account') and rules['account'] != account_id:
                continue
            if not set(rules.get('requires', [])).issubset(account.get('capabilities', [])):
                continue
            snapshot = usage.get(account_id, {})
            if snapshot.get('health') != 'ready' or now - snapshot.get('observed_at', 0) > cfg['settings']['refresh_seconds'] * 2:
                continue
            pool = snapshot.get('pool', account_id)
            if db.get('cooldowns', {}).get(pool, 0) > now:
                continue
            supported = snapshot.get('models', {})
            if profile['model'] not in supported or profile['effort'] not in supported[profile['model']]:
                continue
            windows = applicable_windows(snapshot, profile['model'])
            if not windows:
                continue
            used = max(w['used'] for w in windows)
            ceiling = account.get('start_percent', cfg['settings']['start_percent'])
            if used >= ceiling:
                continue
            active = sum(w.get('pool') == pool and w['status'] in LIVE for w in db['workers'].values())
            if active >= account.get('max_workers', 2):
                continue
            # Percentages are a heuristic, not equivalent token budgets. A
            # configured weight and in-flight penalty make this explicit.
            score = (ceiling - used) * account.get('weight', 1) - active * 10
            candidates.append((score, -order, account_id, name, pool))
    if not candidates:
        return None
    _, _, account_id, name, pool = max(candidates)
    return {'account': account_id, 'profile': name, 'pool': pool}


def current_worker(db, requested=None, require_owner=False):
    worker_id = requested or os.environ.get('CC_HUB_RESOURCE_WORKER')
    worker = db['workers'].get(worker_id)
    if worker is None:
        raise ValueError('unknown worker; run inside a managed session or supply --worker')
    if require_owner:
        if worker_id != os.environ.get('CC_HUB_RESOURCE_WORKER') or str(worker['generation']) != os.environ.get('CC_HUB_RESOURCE_GENERATION'):
            raise ValueError('stale worker generation; this session was replaced')
        if worker['status'] not in ('starting', 'running'):
            raise ValueError('worker lease is not active')
    return worker


def live_worker(db, task):
    return next((w for w in db['workers'].values() if w['task'] == task and w['status'] in HOLDING), None)


def tmux_exists(name):
    return run(['tmux', 'has-session', '-t', '=' + name]).returncode == 0


def process_stamp(pid):
    if not pid:
        return None
    result = run(['ps', '-p', str(pid), '-o', 'lstart=', '-o', 'stat='], timeout=3)
    fields = result.stdout.strip().rsplit(None, 1)
    return fields[0] if len(fields) == 2 and not fields[1].startswith('Z') else None


def record(worker, event, **fields):
    worker.setdefault('events', []).append(dict(event=event, at=time.time(), **fields))


def stop(worker, reason):
    """Ask the supervisor to end this worker's session: hand-over or operator stop."""
    if worker['status'] in ('stopped', 'blocked'):
        return
    worker.update(status='stopping', stop_reason=reason, stop_at=time.time())
    record(worker, 'stop_requested', reason=reason)


# ─── sessions ────────────────────────────────────────────────────────────

INSTRUCTIONS = '''You are running as a cc-hub managed task session. The account you run on is
the hub's choice; never log out or change auth. If it runs dry, the hub stops
this session and continues it on another account from this transcript; you do
not need to prepare for that. Read ~/.claude/skills/task/SKILL.md and follow it.'''

RESUMED = '''This session continues an earlier one that stopped when its account ran dry.
Its transcript is at {path}. Re-read the card's notes before acting; do not
repeat an external write (push, build, PR) without checking whether it landed.'''


def encoded_cwd(cwd):
    return re.sub(r'[/\\.:]', '-', cwd)


def transcript_of(worker, account):
    path = worker.get('transcript_path')
    if not path and account['provider'] == 'claude':
        path = str(account_home(account) / 'projects' / encoded_cwd(worker['cwd']) / (worker['session_id'] + '.jsonl'))
    return path if path and Path(path).is_file() else None


def carry_transcript(worker, previous, account):
    """Put the previous generation's transcript where the new account's Claude
    will look for it, so `--resume` continues the same session. Returns the
    path the new session resumes from, or None when there is nothing to carry."""
    if not previous or account['provider'] != 'claude':
        return None
    target = account_home(account) / 'projects' / encoded_cwd(worker['cwd']) / (worker['session_id'] + '.jsonl')
    if target.resolve() != Path(previous).resolve():
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(previous, target)
    return str(target)


def execution(worker, cfg):
    account = cfg['accounts'][worker['account']]
    profile = dict(cfg['profiles'].get(worker['profile'], {}))
    profile.update({key: worker[key] for key in ('model', 'effort') if key in worker})
    env = account_env(account)
    env.update(CC_HUB_RESOURCE_WORKER=worker['id'], CC_HUB_RESOURCE_GENERATION=str(worker['generation']),
               CC_HUB_RESOURCE_TASK=worker['task'], CC_HUB_RESOURCE_KIND=worker['kind'],
               CC_HUB_RESOURCE_ROLE=worker['role'], CC_HUB_RESOURCE_TMUX=worker['tmux'],
               CC_HUB_RESOURCE_CONFIG=str(config_path()), CC_HUB_RESOURCE_DIR=str(root()))
    binary = os.environ.get('CC_HUB_BINARY')
    if binary and Path(binary).is_absolute():
        env['PATH'] = str(Path(binary).parent) + os.pathsep + env.get('PATH', '')
    previous = worker.get('previous_transcript')
    resume = carry_transcript(worker, previous, account) if previous else None
    prompt = INSTRUCTIONS + '\n\n' + worker['prompt']
    if previous and not resume:
        prompt += '\n\n' + RESUMED.format(path=previous)
    # Profile args preserve the user's permission choices, independently of model/effort.
    argv = [account.get('executable', account['provider']), *account.get('args', [])]
    gate = shlex.join([sys.executable, str(Path(__file__).resolve()), 'hook'])
    hooks = {'PreToolUse': [{'matcher': '.*', 'hooks': [{'type': 'command', 'command': gate, 'timeout': 15}]}]}
    if account['provider'] == 'claude':
        window = profile.get('autocompact')
        argv += ['--model', profile['model'], '--effort', profile['effort'],
                 *(['--autocompact', str(window)] if window else []),
                 '--settings', json.dumps({'hooks': hooks})]
        if resume:
            argv += ['--resume', worker['session_id'], RESUMED.format(path=resume)]
        else:
            argv += ['--session-id', worker['session_id'], prompt]
    else:
        # Inline TOML overrides apply only to this worker, leaving user hooks intact.
        existing = []
        settings_path = account_home(account) / 'config.toml'
        if settings_path.is_file():
            existing = tomllib.loads(settings_path.read_text()).get('hooks', {}).get('PreToolUse', [])
        entry = toml_value(existing + hooks['PreToolUse'])
        argv += ['-m', profile['model'], '-c', 'model_reasoning_effort=' + json.dumps(profile['effort']),
                 '-c', 'features.hooks=true', '-c', 'hooks.PreToolUse=' + entry, prompt]
    return argv, env


def launch(worker):
    # Stable generation-specific name makes crash-after-spawn reconciliation idempotent.
    if tmux_exists(worker['tmux']):
        return
    execute = [sys.executable, str(Path(__file__).resolve()), '_exec', '--worker', worker['id'],
               '--generation', str(worker['generation'])]
    argv = ['/usr/bin/env', 'CC_HUB_RESOURCE_CONFIG=' + str(config_path()),
            'CC_HUB_RESOURCE_DIR=' + str(root()), 'CC_HUB_BINARY=' + os.environ.get('CC_HUB_BINARY', 'cc-hub'),
            os.environ.get('SHELL', '/bin/sh'), '-ic', 'exec ' + shlex.join(execute)]
    result = run(['tmux', 'new-session', '-d', '-s', worker['tmux'], '-c', worker['cwd'], *argv])
    if result.returncode:
        raise ValueError('tmux worker launch failed')


def bind_board(worker, cfg):
    binary = os.environ.get('CC_HUB_BINARY')
    if not binary:
        return
    binding = dict(worker)
    if cfg['accounts'][worker['account']]['provider'] == 'codex':
        binding['session_id'] = worker.get('provider_session_id')
    result = run([binary, 'resource', '_bind', json.dumps(binding)])
    if result.returncode:
        raise ValueError('worker started but board binding failed; supervisor will retry')


def reserve(worker, choice, cfg):
    if worker.get('tmux'):
        worker.setdefault('tmux_history', []).append(worker['tmux'])
    worker.update(choice)
    profile = cfg['profiles'][choice['profile']]
    worker.update(model=profile['model'], effort=profile['effort'])
    worker['generation'] += 1
    worker['tmux'] = 'cchr-' + worker['id'][:12] + '-' + str(worker['generation'])
    # The session id is the worker's for life: a replacement resumes it.
    worker.setdefault('session_id', str(uuid.uuid4()))
    for key in ('provider_session_id', 'pid', 'pid_stamp'):
        worker.pop(key, None)
    worker['status'] = 'starting'
    worker['started_at'] = time.time()
    worker.pop('warning_at', None)
    record(worker, 'allocated', generation=worker['generation'], model=worker['model'], effort=worker['effort'], **choice)


def start_worker(args, cfg, usage):
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]{0,127}', args.task):
        raise ValueError('task ID must be a filesystem-safe identifier')
    policy(cfg, args.kind, args.role)
    cwd = str(Path(args.cwd).expanduser().resolve())
    if not Path(cwd).is_dir():
        raise ValueError('worker cwd does not exist')
    with lock():
        db = state()
        current = live_worker(db, args.task)
        if current and current['role'] == args.role:
            return dict(current, reused=True)
        if current:
            # A hand-over: the task changes hands, and the old session ends.
            stop(current, 'handed to ' + args.role)
        worker = {'id': uuid.uuid4().hex, 'task': args.task, 'kind': args.kind, 'role': args.role, 'cwd': cwd,
                  'prompt': args.prompt, 'generation': 0, 'status': 'waiting_for_capacity', 'events': [],
                  'created_at': time.time(), 'predecessor': current['id'] if current else None}
        db['workers'][worker['id']] = worker
        choice = select(cfg, usage, db, args.kind, args.role)
        if choice:
            reserve(worker, choice, cfg)
        save(db)
    # No lock held during CLI startup. Generation already reserved durably.
    if choice:
        launch(worker)
        bind_board(worker, cfg)
    return worker


def native_quota_error(worker, cfg):
    """Only provider error records count; ordinary messages/tool output do not."""
    account = cfg['accounts'][worker['account']]
    path = transcript_of(worker, account)
    if not path and account['provider'] == 'codex' and worker.get('pid'):
        executable = '/usr/sbin/lsof' if Path('/usr/sbin/lsof').is_file() else 'lsof'
        try:
            files = run([executable, '-a', '-p', str(worker['pid']), '-Fn'], timeout=5)
        except (OSError, subprocess.SubprocessError):
            return False
        home = account_home(account) / 'sessions'
        path = next((line[1:] for line in files.stdout.splitlines() if line.startswith('n')
                     and line.endswith('.jsonl') and Path(line[1:]).is_relative_to(home)), None)
    if not path:
        return False
    worker['transcript_path'] = path
    try:
        with open(path, 'rb') as stream:
            stream.seek(0, 2)
            stream.seek(max(0, stream.tell() - 65536))
            lines = stream.read().decode('utf-8', errors='replace').splitlines()
    except OSError:
        return False
    for line in reversed(lines):
        try:
            event = json.loads(line)
        except ValueError:
            continue
        payload = event.get('payload', {})
        if event.get('type') == 'event_msg' and payload.get('type') == 'error':
            detail = payload.get('codex_error_info', payload.get('codexErrorInfo', ''))
            return re.sub('[^a-z]', '', str(detail).lower()) == 'usagelimitexceeded'
        if event.get('isApiErrorMessage') and event.get('error') == 'rate_limit':
            content = json.dumps(event.get('message', {}).get('content', []))
            return "You've hit your limit" in content or 'usage limit' in content.lower()
        if event.get('type') == 'assistant' or (event.get('type') == 'event_msg' and payload.get('type') in ('agent_message', 'task_started')):
            return False
    return False


def supervise(cfg, usage):
    # Only one reconciler can stop/launch sessions. State is saved before each
    # external effect; recovery replays only idempotent tmux lifecycle operations.
    with lock('supervisor'):
        with lock():
            db = state()
            for worker in list(db['workers'].values()):
                status = worker['status']
                if status in ('stopped', 'blocked'):
                    continue
                if status in ('starting', 'running'):
                    used, fresh = usage_of(worker, usage, cfg)
                    if fresh and used >= cfg['settings']['warn_percent']:
                        worker.setdefault('warning_at', time.time())
                    elif fresh:
                        worker.pop('warning_at', None)
                    exhausted = fresh and used >= cfg['settings']['stop_percent']
                    if status == 'running' and not exhausted:
                        exhausted = native_quota_error(worker, cfg)
                    if exhausted:
                        worker['previous_transcript'] = transcript_of(worker, cfg['accounts'][worker['account']])
                        worker['status'] = 'replacing'
                        record(worker, 'quota_exhausted', generation=worker['generation'])
                    elif status == 'starting':
                        if tmux_exists(worker['tmux']):
                            worker['status'] = 'running'
                            bind_board(worker, cfg)
                        elif time.time() - worker['started_at'] > 30:
                            # Reconcile a crash before launch without manufacturing a new generation.
                            launch(worker)
                    elif not tmux_exists(worker['tmux']):
                        worker['status'] = 'blocked'
                        record(worker, 'process_exited', reason='inspect exit before retrying; not classified as quota')
                if worker['status'] in ('replacing', 'stopping'):
                    # Brief grace lets the requesting command return to the caller.
                    if time.time() - worker.get('stop_at', 0) < 3:
                        continue
                    if tmux_exists(worker['tmux']):
                        result = run(['tmux', 'kill-session', '-t', '=' + worker['tmux']])
                        if result.returncode or tmux_exists(worker['tmux']):
                            continue  # Never grant a second owner while the old session exists.
                    deadline = time.monotonic() + 1
                    while worker.get('pid_stamp') and process_stamp(worker.get('pid')) == worker['pid_stamp'] and time.monotonic() < deadline:
                        time.sleep(.05)
                    if worker.get('pid_stamp') and process_stamp(worker.get('pid')) == worker['pid_stamp']:
                        continue  # CLI still tearing down; reconcile again next tick.
                    if worker['status'] == 'stopping':
                        worker['status'] = 'stopped'
                        record(worker, 'stopped', generation=worker['generation'], reason=worker.get('stop_reason'))
                    else:
                        snapshot = usage.get(worker['account'], {})
                        windows = applicable_windows(snapshot, worker.get('model') or cfg['profiles'][worker['profile']]['model'])
                        reset = max((w['resets_at'] for w in windows if w['used'] >= cfg['settings']['warn_percent']
                                     and w.get('resets_at') and w['resets_at'] > time.time()),
                                    default=time.time() + 300)
                        db['cooldowns'][worker['pool']] = reset
                        record(worker, 'replaced', generation=worker['generation'])
                        worker['status'] = 'waiting_for_capacity'
                    save(db)
                if worker['status'] == 'waiting_for_capacity':
                    if worker['generation'] >= cfg['settings']['max_attempts']:
                        worker['status'] = 'blocked'
                        record(worker, 'attempt_limit')
                        continue
                    try:
                        choice = select(cfg, usage, db, worker['kind'], worker['role'])
                    except ValueError as why:
                        # A role the configuration no longer routes cannot be relaunched.
                        worker['status'] = 'blocked'
                        record(worker, 'no_policy', reason=str(why))
                        continue
                    if choice:
                        reserve(worker, choice, cfg)
                        save(db)
                        launch(worker)
                        bind_board(worker, cfg)
            save(db)
            return list(db['workers'].values())


def hook():
    """PreToolUse: remember where this session's transcript is, and say how
    much of the account is left. Nothing here blocks a tool."""
    event = json.load(sys.stdin)
    if not os.environ.get('CC_HUB_RESOURCE_WORKER'):
        return None
    cfg = load_config()
    with lock():
        db = state()
        worker = current_worker(db, require_owner=True)
        worker['last_tool_at'] = time.time()
        native_id = event.get('session_id')
        binding_changed = native_id and native_id != worker.get('provider_session_id')
        if native_id:
            worker['provider_session_id'] = native_id
        if event.get('transcript_path'):
            worker['transcript_path'] = event['transcript_path']
        used, fresh = usage_of(worker, read_json(root() / 'usage.json', {}), cfg)
        save(db)
    if binding_changed:
        bind_board(worker, cfg)
    context = {'resource': {'worker': worker['id'], 'account': worker['account'], 'generation': worker['generation'],
                            'quota_used_percent': used if fresh else None}}
    if fresh and used >= cfg['settings']['warn_percent']:
        context['resource']['note'] = ('this account is near its limit; the hub will continue you on another '
                                       'account from this transcript, so finish the current step cleanly')
    return {'hookSpecificOutput': {'hookEventName': 'PreToolUse', 'additionalContext': json.dumps(context)}}


# ─── command line ────────────────────────────────────────────────────────


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    commands = p.add_subparsers(dest='verb', required=True)
    commands.add_parser('accounts').add_argument('--refresh', action='store_true')
    select_p = commands.add_parser('select')
    for name in ('kind', 'role'):
        select_p.add_argument('--' + name, required=True)
    start = commands.add_parser('start')
    for name in ('task', 'kind', 'role', 'cwd', 'prompt'):
        start.add_argument('--' + name, required=True)
    commands.add_parser('status').add_argument('--worker')
    commands.add_parser('retry').add_argument('--worker', required=True)
    stop_p = commands.add_parser('stop')
    stop_p.add_argument('--worker', required=True)
    stop_p.add_argument('--reason', default='stopped by operator')
    commands.add_parser('supervise')
    commands.add_parser('hook')
    execute = commands.add_parser('_exec')
    execute.add_argument('--worker', required=True)
    execute.add_argument('--generation', type=int, required=True)
    return p


def main(argv=None):
    args = parser().parse_args(argv)
    try:
        if args.verb == 'hook':
            result = hook()
            if result:
                print(json.dumps(result))
            return 0
        cfg = load_config()
        if args.verb == 'accounts':
            result = refresh(cfg, args.refresh)
        elif args.verb == 'select':
            usage = refresh(cfg)
            with lock():
                result = select(cfg, usage, state(), args.kind, args.role)
        elif args.verb == 'start':
            # Endpoint latency must not stall the router. The OS supervisor
            # refreshes usage and wakes workers queued on unknown/stale capacity.
            result = start_worker(args, cfg, read_json(root() / 'usage.json', {}))
        elif args.verb == 'supervise':
            workers = supervise(cfg, refresh(cfg))
            result = [{k: w.get(k) for k in ('id', 'task', 'role', 'status', 'account', 'model', 'effort', 'generation')} for w in workers]
        elif args.verb == '_exec':
            with lock():
                db = state()
                worker = current_worker(db, args.worker)
                if worker['generation'] != args.generation or worker['status'] not in ('starting', 'running'):
                    raise ValueError('launch lease no longer valid')
                worker['status'] = 'running'
                worker['pid'] = os.getpid()
                worker['pid_stamp'] = process_stamp(worker['pid'])
                save(db)
                command, env = execution(worker, cfg)
            os.chdir(worker['cwd'])
            os.execvpe(command[0], command, env)
        else:
            with lock():
                db = state()
                if args.verb == 'status':
                    result = current_worker(db, args.worker) if args.worker or os.environ.get('CC_HUB_RESOURCE_WORKER') else list(db['workers'].values())
                elif args.verb == 'retry':
                    worker = current_worker(db, args.worker)
                    if worker['status'] != 'blocked' or (worker.get('tmux') and tmux_exists(worker['tmux'])):
                        raise ValueError('retry requires a blocked worker whose previous session has stopped')
                    worker['previous_transcript'] = transcript_of(worker, cfg['accounts'][worker['account']])
                    worker['status'] = 'waiting_for_capacity'
                    record(worker, 'retry_requested')
                    result = worker
                elif args.verb == 'stop':
                    worker = current_worker(db, args.worker)
                    stop(worker, args.reason)
                    result = worker
                save(db)
        print(json.dumps({'ok': True, 'result': result}))
        return 0
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as exc:
        if args.verb == 'hook':
            # A broken hook must never block a tool: exit 1 shows the note, exit 2 would refuse.
            print('resource hook: ' + str(exc), file=sys.stderr)
            return 1
        print(json.dumps({'ok': False, 'error': str(exc)}))
        return 1


if __name__ == '__main__':
    sys.exit(main())
