#!/usr/bin/env python3
"""Account-aware task sessions. Standard library only; shipped in cc-hub.

A task has one worker at a time. `start` for a task that already has a live
worker in another role, or in another directory, is a hand-over: the new
worker is launched and the old one is stopped. A session cannot change its
own cwd, so moving a task to its repository is a hand-over like any other. The broker owns which account a worker runs on and its tmux
lifetime. When an account runs dry the worker is replaced on another account
and continues from its own transcript: a Claude transcript on another Claude
home is resumed as the same session, every other pairing starts a fresh
session that reads it. No LLM call is needed to notice, stop, select or
restart. Provider credentials stay in their own homes.

A session may also need a resource: a repository checkout, a phone. The config
says which exist and on which host; it does not say who needs one, because only
the session doing the work knows that. A session claims what it needs while it
runs, and one resource is held by one session at a time. A session the hub did
not launch claims as a guest, under the name it gives itself. Holding is derived
from the live sessions, so one that forgets to release holds nothing once it
ends: there is no lock to leak.
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
    hosts = cfg.setdefault('hosts', {})
    for name, resource in cfg.setdefault('resources', {}).items():
        if resource.get('host') not in hosts:
            raise ValueError(f'{name}: resource must sit on a configured host')
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
    if not 0 < value.get('start_percent', 1) < cfg['settings']['stop_percent']:
        raise ValueError(f'{kind}/{role}: start_percent must be above 0 and below stop_percent')
    return value


def ceiling(cfg, rules, account):
    """How full an account may be and still take this work.

    Ordinarily the account decides — its own `start_percent` is the reserve it
    keeps for the user. A policy may raise that for work that cannot wait for
    the window to reset: a paged production alert answered five hours late is
    an alert nobody answered. Nothing lowers it; a policy that asks for less
    room than the account already leaves changes nothing.
    """
    return max(account.get('start_percent', cfg['settings']['start_percent']),
               rules.get('start_percent', 0))


# ─── account probes ──────────────────────────────────────────────────────

# The cheapest model a wake-up prompt can run on; see `wake`.
WAKE_MODEL = 'claude-haiku-4-5-20251001'


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
            if token_expired(report):
                return wake(account)
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


def token_expired(report):
    """The store found a login whose access token has lapsed. Claude Code
    rotates it on its next real request and the store never does, so an
    account nothing has run on for a few hours reads as logged out until
    something runs there."""
    expires_at = report.get('token_expires_at')
    return expires_at is not None and expires_at <= time.time()


def wake(account):
    """One trivial prompt on the account, so Claude Code refreshes its own
    token. The store re-asks the endpoint within its retry window; until then
    the account is unknown rather than logged out, and `retry_at` keeps the
    broker from repeating the wake every tick."""
    run([account.get('executable', 'claude'), '-p', 'ok', '--model', WAKE_MODEL, '--max-turns', '1'],
        env=account_env(account), timeout=90)
    return {'health': 'unknown', 'windows': [], 'retry_at': time.time() + 300,
            'error': 'access token expired; asked Claude Code to refresh it'}


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
# A claim outlives the generation that made it, and a session on its way out
# may still be using the phone, so `stopping` still holds.
USING = ('waiting_for_capacity', 'starting', 'running', 'replacing', 'stopping')


def state():
    db = read_json(root() / 'state.json', {'workers': {}, 'cooldowns': {}})
    # A guest is remembered only while its process is: reading the state is
    # where one that ended stops being anybody, and holds nothing.
    db['guests'] = {key: guest for key, guest in db.get('guests', {}).items()
                    if process_stamp(guest['pid']) == guest['pid_stamp']}
    return db


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
        worker = next((w for w in workers if w['status'] in CARD_STAGES or w.get('wants')), None)
        if worker is None:
            path.unlink(missing_ok=True)
            continue
        stage = 'resource_wait' if worker.get('wants') else CARD_STAGES[worker['status']]
        activity = {'stage': stage, 'detail': worker['role'] + ' / ' + resource_text(worker)}
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
            room = ceiling(cfg, rules, account)
            if used >= room:
                continue
            active = sum(w.get('pool') == pool and w['status'] in LIVE for w in db['workers'].values())
            if active >= account.get('max_workers', 2):
                continue
            # Percentages are a heuristic, not equivalent token budgets. A
            # configured weight and in-flight penalty make this explicit.
            score = (room - used) * account.get('weight', 1) - active * 10
            candidates.append((score, -order, account_id, name, pool))
    if not candidates:
        return None
    _, _, account_id, name, pool = max(candidates)
    return {'account': account_id, 'profile': name, 'pool': pool}


def allocate(cfg, usage, db, worker):
    """The one thing a worker needs to launch: an account with room. Resources
    are not part of this — a session claims those itself, once it knows what
    the work needs."""
    choice = select(cfg, usage, db, worker['kind'], worker['role'])
    if choice is None:
        worker.update(waiting_for='account')
    return choice


# ─── resources ───────────────────────────────────────────────────────────
#
# A resource is a thing only one session may use at a time: a repository
# checkout, a phone. The config says which exist and on which host; nothing
# says who needs one, because only the session doing the work knows that.
#
# Holding is *derived* from live sessions, never stored as a lock, so a
# session that forgets to release — or dies mid-claim — holds nothing the
# moment it is no longer live. There is no lease to expire and no lock to
# leak. Waiting is a `wants` set on the session plus the time it asked, which
# is the whole queue: first to ask, first served.
#
# Two kinds of session ask. A *worker* the hub launched, which it knows is
# live because it supervises it, and a *guest* it did not, which is live as
# long as the process that claimed still runs. Everything below treats them
# alike: the queue is one queue, and a guest waits behind a worker and a
# worker behind a guest.


def sessions(db):
    return [w for w in db['workers'].values() if w['status'] in USING] + list(db.get('guests', {}).values())


def holder_name(session):
    """Who a resource is out to, as the other side of the queue reads it: a
    worker answers with its task, a guest with the name it gave itself."""
    return session.get('task') or session['name']


def resource_holders(db, except_worker=None):
    return {name: session for session in sessions(db) if session['id'] != except_worker
            for name in session.get('holds', [])}


def waiting_for_resources(db):
    """Sessions with an ungranted claim, in the order they asked."""
    return sorted((s for s in sessions(db) if s.get('wants')), key=lambda s: s['wants_at'])


def grant(db, worker):
    """Give the worker its claim if every resource in it is free and nobody
    asked earlier for any of them. All of it or none: a half-granted claim is
    how two sessions deadlock, each holding what the other waits for."""
    wants = worker.get('wants')
    if not wants:
        return False
    holders = resource_holders(db, except_worker=worker['id'])
    if any(name in holders for name in wants):
        return False
    ahead = [w for w in waiting_for_resources(db)
             if w['wants_at'] < worker['wants_at'] and set(w['wants']) & set(wants)]
    if ahead:
        return False
    worker['holds'] = sorted(wants)
    worker.pop('wants', None)
    worker.pop('wants_at', None)
    record(worker, 'claimed', resources=worker['holds'])
    return True


def grant_waiting(db):
    """Offer every queued claim, oldest first. Called wherever a resource can
    have come free: a release, a worker's exit, a supervise tick."""
    return [w for w in waiting_for_resources(db) if grant(db, w)]


def claim(cfg, db, worker, names):
    """"These are the resources I need." A claim is the complete set: asking
    for a different one releases what the worker holds until the new set can
    be granted in full, so a worker never sits on one resource waiting for
    another."""
    unknown = [name for name in names if name not in cfg['resources']]
    if unknown:
        raise ValueError('unknown resource ' + ', '.join(unknown) + '; see `cc-hub resource list`')
    if sorted(names) == worker.get('holds'):
        return worker
    release(db, worker)
    worker.update(wants=sorted(set(names)), wants_at=time.time())
    if not grant(db, worker):
        record(worker, 'queued', resources=worker['wants'])
    return worker


def release(db, worker, names=None):
    """Hand back some or all of what the worker holds, withdraw the same from
    what it waits for, and offer the queue what came free. A queued claim
    stands ahead of every later one, so withdrawing it frees them as surely
    as handing back a held resource does."""
    def kept(key):
        return [n for n in worker.get(key, []) if names is not None and n not in names]
    freed = [n for n in worker.get('holds', []) if n not in kept('holds')]
    withdrawn = [n for n in worker.get('wants', []) if n not in kept('wants')]
    if withdrawn:
        worker['wants'] = kept('wants')
        if not worker['wants']:
            worker.pop('wants')
            worker.pop('wants_at', None)
        record(worker, 'withdrawn', resources=withdrawn)
    if freed:
        worker['holds'] = kept('holds')
        record(worker, 'released', resources=freed)
    if freed or withdrawn:
        grant_waiting(db)
    return freed


def standing(cfg, db, worker):
    """What the session is told after asking: what it holds, or what it is
    waiting for and how many asked before it."""
    if not worker.get('wants'):
        return {'ok': True, 'holds': worker.get('holds', [])}
    ahead = {}
    for name in worker['wants']:
        earlier = [w for w in waiting_for_resources(db)
                   if name in w['wants'] and w['wants_at'] < worker['wants_at']]
        holder = resource_holders(db, except_worker=worker['id']).get(name)
        ahead[name] = {'held_by': holder_name(holder) if holder else None, 'ahead': len(earlier)}
    return {'ok': False, 'holds': [], 'waiting_for': worker['wants'], 'queue': ahead}


def claim_and_wait(cfg, args):
    """Ask, then keep asking until `--wait` runs out. The lock is released
    between looks so the worker that must release first can."""
    deadline = time.monotonic() + max(0.0, args.wait)
    with lock():
        db = state()
        session = current_session(db, args)
        if session is None:
            raise ValueError('this session is not a managed worker; claim as a guest by naming yourself: --as "<who you are>"')
        session = claim(cfg, db, session, args.names)
        answer = standing(cfg, db, session)
        save(db)
    while not answer['ok'] and time.monotonic() < deadline:
        time.sleep(min(2.0, max(0.1, deadline - time.monotonic())))
        with lock():
            db = state()
            session = current_session(db, args)
            grant(db, session)
            answer = standing(cfg, db, session)
            save(db)
    return answer


def resource_text(worker):
    """What the worker holds or waits for, as the card shows it."""
    if worker.get('wants'):
        return 'waiting for ' + ' + '.join(worker['wants'])
    if worker.get('holds'):
        return 'holding ' + ' + '.join(worker['holds'])
    return worker.get('account', 'eligible accounts')


def inventory(cfg):
    """The resources as the session is told about them: what each one is and
    where to reach it. The description is the only thing that tells a session
    whether it needs the thing, so a resource without one is a name to guess
    at."""
    lines = []
    for name, resource in cfg['resources'].items():
        ssh = cfg['hosts'][resource['host']].get('ssh')
        where = '`ssh ' + ssh + '`' if ssh else 'this machine'
        lines.append('  {} — {} (on {})'.format(
            name, resource.get('description', 'no description'), where))
    return '\n'.join(lines)


def resource_list(cfg, db):
    holders = resource_holders(db)
    queue = {}
    for session in waiting_for_resources(db):
        for name in session['wants']:
            queue.setdefault(name, []).append(holder_name(session))
    return {name: dict(resource,
                       holder=holder_name(holders[name]) if name in holders else None,
                       queue=queue.get(name, []),
                       reach=cfg['hosts'][resource['host']].get('ssh'))
            for name, resource in cfg['resources'].items()}


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


# Shells, and the broker itself, are how a session runs a command; they are
# not the session. A guest's hold belongs to whoever is behind them.
SHELLS = ('sh', 'bash', 'zsh', 'fish', 'dash', 'ksh', 'csh', 'tcsh', 'env', 'login', 'python', 'python3')


def passthrough(command):
    """Whether a process is somebody running a command rather than the session
    that wanted it: a shell, or the `cc-hub` that runs this file. The broker is
    always the first parent and it lives no longer than the answer it prints,
    so a guest anchored to it would be gone the moment it was granted."""
    return command in SHELLS or command == Path(os.environ.get('CC_HUB_BINARY', 'cc-hub')).name


def session_pid(start=None):
    """The process a guest hold belongs to: the nearest ancestor of this
    command that is a session rather than a shell it was typed into."""
    pid = start or os.getppid()
    for _ in range(16):
        fields = run(['ps', '-p', str(pid), '-o', 'ppid=', '-o', 'comm='], timeout=3).stdout.split(None, 1)
        if len(fields) != 2:
            return pid
        # A login shell answers with a leading dash: `-zsh` is still a shell.
        parent, command = int(fields[0]), Path(fields[1].strip()).name.lstrip('-')
        if parent <= 1 or not passthrough(command):
            return pid
        pid = parent
    return pid


def guest(db, name=None, pid=None):
    """A session the hub did not launch, claiming on its own behalf. It is not
    supervised and never replaced: the hub lends it a resource and finds out it
    is gone when its process is, which is the same rule its own workers live
    under. A guest names itself once; after that its process is its name."""
    pid = pid or session_pid()
    stamp = process_stamp(pid)
    if not stamp:
        raise ValueError('nothing alive to hold for at pid ' + str(pid))
    existing = db['guests'].get(str(pid))
    if existing:
        return existing
    if not name:
        return None
    entry = {'id': 'guest-' + str(pid), 'name': name[:64], 'pid': pid, 'pid_stamp': stamp,
             'since': time.time(), 'events': []}
    db['guests'][str(pid)] = entry
    return entry


def current_session(db, args):
    """Who is asking. A managed worker names itself through the environment
    the hub launched it with; anybody else asks as a guest."""
    if args.worker or os.environ.get('CC_HUB_RESOURCE_WORKER'):
        return current_worker(db, args.worker, require_owner=True)
    return guest(db, getattr(args, 'name', None), getattr(args, 'pid', None))


def releasing(db, args):
    """Who is handing back. A worker, or a guest by its process, as for a
    claim; failing that, the guests that claimed under the `--as` name. An
    agent does not always run its release from the process it claimed from,
    and a release must never register a guest, so the name finds and does
    not create."""
    if args.worker or os.environ.get('CC_HUB_RESOURCE_WORKER'):
        return [current_worker(db, args.worker, require_owner=True)]
    found = guest(db, pid=args.pid)
    if found:
        return [found]
    return [g for g in db['guests'].values() if args.name and g['name'] == args.name[:64]]


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
    """Ask the supervisor to end this worker's session: hand-over or operator stop.

    A worker without a session — blocked, or still waiting for capacity or a
    seat — has nothing to end; stopping it is the operator closing the case
    instead of retrying it, so it is retired on the spot."""
    if worker['status'] == 'stopped':
        return
    if worker['status'] == 'blocked' or not worker.get('tmux'):
        worker.update(status='stopped', stop_reason=reason, stop_at=time.time())
        record(worker, 'stopped', generation=worker['generation'], reason=reason)
        return
    worker.update(status='stopping', stop_reason=reason, stop_at=time.time())
    record(worker, 'stop_requested', reason=reason)


# ─── sessions ────────────────────────────────────────────────────────────

INSTRUCTIONS = '''You are running as a cc-hub managed task session. The account you run on is
the hub's choice; never log out or change auth. If it runs dry, the hub stops
this session and continues it on another account from this transcript; you do
not need to prepare for that. Read ~/.claude/skills/task/SKILL.md and follow it.'''

RESOURCES = '''The hub lends these out, one session at a time:

{inventory}

Claim what the work needs, and only that — a claim you don't need is a queue
for someone who does:

  cc-hub resource claim <name>... [--wait SECONDS]   the complete set you need
  cc-hub resource release [<name>...]                as soon as you are done
  cc-hub resource list                               who holds what, and the queue

`claim` prints what you now hold, or what you are waiting for and how many
sessions asked before you; call it again to ask again. A claim is the whole
set: claiming a different one hands back what you hold, so ask for everything
in one call. Nothing is claimed for you, and everything you hold is released
when this session ends.'''

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


def continues_natively(worker, account):
    """Only a Claude transcript continues as the same session on another Claude
    home; every other pairing (Codex anywhere, or across providers) starts a
    fresh session that reads the old transcript."""
    return worker.get('previous_provider') == 'claude' and account['provider'] == 'claude'


def leave_transcript(worker, cfg):
    """What the next generation continues from: this one's transcript and the
    provider that wrote it. A generation that never wrote a transcript of its
    own passes on the one it was handed."""
    account = cfg['accounts'][worker['account']]
    own = transcript_of(worker, account)
    if own:
        worker['previous_transcript'] = own
        worker['previous_provider'] = account['provider']


def carry_transcript(worker, previous, account):
    """Put the previous generation's transcript where the new account's Claude
    will look for it, so `--resume` continues the same session. Returns the
    path the new session resumes from, or None when there is nothing to carry."""
    if not previous or not continues_natively(worker, account):
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
    if cfg['resources']:
        prompt += '\n\n' + RESOURCES.format(inventory=inventory(cfg))
    if worker.get('holds'):
        prompt += '\n\nThis session already holds ' + ' + '.join(worker['holds']) + '.'
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


def launch(worker, cfg):
    # Stable generation-specific name makes crash-after-spawn reconciliation idempotent.
    if tmux_exists(worker['tmux']):
        return
    name_session(worker, cfg)
    execute = [sys.executable, str(Path(__file__).resolve()), '_exec', '--worker', worker['id'],
               '--generation', str(worker['generation'])]
    argv = ['/usr/bin/env', 'CC_HUB_RESOURCE_CONFIG=' + str(config_path()),
            'CC_HUB_RESOURCE_DIR=' + str(root()), 'CC_HUB_BINARY=' + os.environ.get('CC_HUB_BINARY', 'cc-hub'),
            os.environ.get('SHELL', '/bin/sh'), '-ic', 'exec ' + shlex.join(execute)]
    result = run(['tmux', 'new-session', '-d', '-s', worker['tmux'], '-c', worker['cwd'], *argv])
    if result.returncode:
        raise ValueError('tmux worker launch failed')


def name_session(worker, cfg):
    """Name the session before it exists, so the hub never sees it nameless.
    Only Claude takes the id it is given; a Codex session mints its own and is
    named when `bind_board` first learns it. A failure costs the name, not the
    launch: the hub asks the user for one instead."""
    binary = os.environ.get('CC_HUB_BINARY')
    if binary and cfg['accounts'][worker['account']]['provider'] == 'claude':
        run([binary, 'resource', '_name', json.dumps(worker)])


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
    # A native resume keeps the session id; a hand-off is a new session.
    if not continues_natively(worker, cfg['accounts'][choice['account']]):
        worker['session_id'] = str(uuid.uuid4())
    # What this generation learns about itself, it learns through the hook.
    for key in ('provider_session_id', 'transcript_path', 'pid', 'pid_stamp'):
        worker.pop(key, None)
    worker['status'] = 'starting'
    worker['started_at'] = time.time()
    # `holds` and `wants` survive: a replacement continues the same work, and
    # handing its checkout to somebody else mid-task is not a quota decision.
    for key in ('warning_at', 'waiting_for'):
        worker.pop(key, None)
    record(worker, 'allocated', generation=worker['generation'], model=worker['model'], effort=worker['effort'], **choice)


def handover_reason(current, role, cwd):
    return 'handed to ' + role if current['role'] != role else 'moved to ' + cwd


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
        if current and current['role'] == args.role and current['cwd'] == cwd:
            return dict(current, reused=True)
        if current:
            # A hand-over: the task changes hands or place, and the old session ends.
            stop(current, handover_reason(current, args.role, cwd))
        worker = {'id': uuid.uuid4().hex, 'task': args.task, 'kind': args.kind, 'role': args.role, 'cwd': cwd,
                  'prompt': args.prompt, 'title': args.title, 'generation': 0, 'status': 'waiting_for_capacity', 'events': [],
                  'created_at': time.time(), 'predecessor': current['id'] if current else None}
        db['workers'][worker['id']] = worker
        choice = allocate(cfg, usage, db, worker)
        if choice:
            reserve(worker, choice, cfg)
        save(db)
    # No lock held during CLI startup. Generation already reserved durably.
    if choice:
        launch(worker, cfg)
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
                        leave_transcript(worker, cfg)
                        worker['status'] = 'replacing'
                        record(worker, 'quota_exhausted', generation=worker['generation'])
                    elif status == 'starting':
                        if tmux_exists(worker['tmux']):
                            worker['status'] = 'running'
                            bind_board(worker, cfg)
                        elif time.time() - worker['started_at'] > 30:
                            # Reconcile a crash before launch without manufacturing a new generation.
                            launch(worker, cfg)
                    elif not tmux_exists(worker['tmux']):
                        worker['status'] = 'blocked'
                        record(worker, 'process_exited', reason='inspect exit before retrying; not classified as quota')
                if worker['status'] in ('replacing', 'stopping'):
                    # Brief grace lets the requesting command return to the caller.
                    if time.time() - worker.get('stop_at', 0) < 3:
                        continue
                    if worker.get('tmux') and tmux_exists(worker['tmux']):
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
                        choice = allocate(cfg, usage, db, worker)
                    except ValueError as why:
                        # A role the configuration no longer routes cannot be relaunched.
                        worker['status'] = 'blocked'
                        record(worker, 'no_policy', reason=str(why))
                        continue
                    if choice:
                        reserve(worker, choice, cfg)
                        save(db)
                        launch(worker, cfg)
                        bind_board(worker, cfg)
            # A worker that ended above stopped holding what it held, so this
            # is where a queued claim finds out. No other sweep is needed:
            # `claim` and `release` already offer the queue themselves.
            grant_waiting(db)
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
                            'holds': worker.get('holds', []), 'waiting_for': worker.get('wants', []),
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
    commands.add_parser('list')
    claim_p = commands.add_parser('claim')
    claim_p.add_argument('names', nargs='+')
    claim_p.add_argument('--worker')
    # A session the hub did not launch says who it is; its process says how
    # long it is around to hold anything.
    claim_p.add_argument('--as', dest='name')
    claim_p.add_argument('--pid', type=int)
    # A tool call has its own timeout, so a claim waits for a while and says
    # where it stands rather than blocking until it is granted.
    claim_p.add_argument('--wait', type=float, default=0)
    release_p = commands.add_parser('release')
    release_p.add_argument('names', nargs='*')
    release_p.add_argument('--worker')
    release_p.add_argument('--as', dest='name')
    release_p.add_argument('--pid', type=int)
    select_p = commands.add_parser('select')
    for name in ('kind', 'role'):
        select_p.add_argument('--' + name, required=True)
    start = commands.add_parser('start')
    for name in ('task', 'kind', 'role', 'cwd', 'prompt'):
        start.add_argument('--' + name, required=True)
    # The session's name; without one it takes its card's (see `cc-hub resource _name`).
    start.add_argument('--title')
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
        elif args.verb == 'list':
            with lock():
                result = resource_list(cfg, state())
        elif args.verb == 'claim':
            result = claim_and_wait(cfg, args)
        elif args.verb == 'start':
            # Endpoint latency must not stall the router. The OS supervisor
            # refreshes usage and wakes workers queued on unknown/stale capacity.
            result = start_worker(args, cfg, read_json(root() / 'usage.json', {}))
        elif args.verb == 'supervise':
            workers = supervise(cfg, refresh(cfg))
            result = [{k: w.get(k) for k in ('id', 'task', 'role', 'status', 'account', 'holds', 'wants', 'model', 'effort', 'generation')} for w in workers]
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
                    leave_transcript(worker, cfg)
                    worker['status'] = 'waiting_for_capacity'
                    record(worker, 'retry_requested')
                    result = worker
                elif args.verb == 'stop':
                    worker = current_worker(db, args.worker)
                    stop(worker, args.reason)
                    result = worker
                elif args.verb == 'release':
                    # A guest with nothing to its name held nothing to hand back.
                    found = releasing(db, args)
                    freed = [n for s in found for n in release(db, s, args.names or None)]
                    result = {'released': freed,
                              'holds': [n for s in found for n in s.get('holds', [])],
                              'waiting_for': [n for s in found for n in s.get('wants', [])]}
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
