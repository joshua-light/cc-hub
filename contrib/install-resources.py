#!/usr/bin/env python3
"""Install resource config and an independent 30-second OS supervisor."""
import argparse
import datetime
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tomllib


def toml_value(value):
    if isinstance(value, dict):
        return '{' + ','.join(json.dumps(k) + '=' + toml_value(v) for k, v in value.items()) + '}'
    if isinstance(value, list):
        return '[' + ','.join(toml_value(v) for v in value) + ']'
    return json.dumps(value)


def share_tools(home, marketplaces=()):
    """Share local task tooling, never subscription or connector credentials."""
    backup = home / '.cc-hub/resources/backups' / datetime.datetime.now().strftime('%Y%m%d-%H%M%S-%f')
    changed = []

    def write(path, text):
        if path.exists() and path.read_text() == text:
            return
        if path.exists():
            saved = backup / path.relative_to(home)
            saved.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, saved)
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_suffix(path.suffix + '.resources-tmp')
        temporary.write_text(text)
        temporary.chmod(0o600)
        temporary.replace(path)
        changed.append(str(path))

    source = home / '.codex/config.toml'
    target = home / '.codex-personal/config.toml'
    if source.exists() and target.exists():
        base = tomllib.loads(source.read_text())
        current = tomllib.loads(target.read_text())
        additions = {}
        for key in ('plugins', 'marketplaces', 'mcp_servers'):
            if key not in current and key in base:
                if key == 'plugins':
                    additions[key] = {k: v for k, v in base[key].items() if k.rsplit('@', 1)[-1] in marketplaces}
                elif key == 'marketplaces':
                    additions[key] = {k: v for k, v in base[key].items() if k in marketplaces}
                else:
                    additions[key] = {}
                    for name, server in base[key].items():
                        if name not in ('unityMCP', 'node_repl', 'computer-use', 'headroom'):
                            continue
                        server = dict(server)
                        for credential in ('http_headers', 'bearer_token'):
                            server.pop(credential, None)
                        if 'env' in server:
                            server['env'] = {k: v for k, v in server['env'].items()
                                             if not any(word in k.upper() for word in ('TOKEN', 'PASSWORD', 'SECRET', 'API_KEY', 'AUTH'))}
                        additions[key][name] = server
        if additions:
            write(target, ''.join(k + ' = ' + toml_value(v) + '\n' for k, v in additions.items()) + target.read_text())
        cache = home / '.codex-personal/plugins/cache'
        if not cache.exists() and not cache.is_symlink() and (home / '.codex/plugins/cache').is_dir():
            cache.parent.mkdir(parents=True, exist_ok=True)
            cache.symlink_to(home / '.codex/plugins/cache', target_is_directory=True)
            changed.append(str(cache))
    source = home / '.claude'
    target = home / '.claude-personal'
    if source.exists() and target.exists():
        # Installed plugin records point to immutable versioned code in the
        # original cache. Existing personal plugin preferences remain intact.
        for relative, key in [('settings.json', 'enabledPlugins'), ('plugins/installed_plugins.json', 'plugins')]:
            src = source / relative
            dst = target / relative
            if not src.exists():
                continue
            base = json.loads(src.read_text())
            current = json.loads(dst.read_text()) if dst.exists() else ({'version': base.get('version', 2)} if key == 'plugins' else {})
            for name, value in base.get(key, {}).items():
                if name.rsplit('@', 1)[-1] in marketplaces:
                    current.setdefault(key, {}).setdefault(name, value)
            write(dst, json.dumps(current, indent=2) + '\n')
        src = source / 'plugins/known_marketplaces.json'
        dst = target / 'plugins/known_marketplaces.json'
        if src.exists():
            base = json.loads(src.read_text())
            current = json.loads(dst.read_text()) if dst.exists() else {}
            for name in marketplaces:
                if name in base:
                    current.setdefault(name, base[name])
                write(dst, json.dumps(current, indent=2) + '\n')
        skill = target / 'skills/task'
        if not skill.exists() and not skill.is_symlink() and (source / 'skills/task').is_dir():
            skill.parent.mkdir(parents=True, exist_ok=True)
            skill.symlink_to(source / 'skills/task', target_is_directory=True)
            changed.append(str(skill))
    return changed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--home', type=Path, default=Path.home())
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--watchdog', action='store_true')
    parser.add_argument('--share-tools', action='store_true', help='Share local plugin code/configuration with the second profiles; do not copy auth')
    parser.add_argument('--marketplace', action='append', default=[], help='Plugin marketplace to share; repeat for multiple marketplaces')
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error('build cc-hub first')
    destination = args.home / '.cc-hub/resources.toml'
    destination.parent.mkdir(parents=True, exist_ok=True)
    if not destination.exists():
        shutil.copy2(Path(__file__).with_name('resources.toml'), destination)
        destination.chmod(0o600)
    shared = share_tools(args.home, args.marketplace) if args.share_tools else []
    if args.watchdog:
        if sys.platform != 'darwin' or args.home.resolve() != Path.home().resolve():
            parser.error('--watchdog requires the current macOS user')
        logs = args.home / '.cc-hub/resources'
        logs.mkdir(parents=True, exist_ok=True, mode=0o700)
        plist = args.home / 'Library/LaunchAgents/local.cc-hub.resources.plist'
        plist.parent.mkdir(parents=True, exist_ok=True)
        if plist.exists():
            stamp = datetime.datetime.now().strftime('%Y%m%d-%H%M%S-%f')
            shutil.copy2(plist, logs / ('supervisor-' + stamp + '.plist.bak'))
        configuration = {
            'Label': 'local.cc-hub.resources',
            'ProgramArguments': [str(binary), 'resource', 'supervise'],
            'StartInterval': 30, 'RunAtLoad': True, 'ProcessType': 'Background',
            'EnvironmentVariables': {'PATH': ':'.join([str(binary.parent), str(args.home / '.local/bin'),
                                                      '/opt/homebrew/bin', '/usr/local/bin', '/usr/bin', '/bin', '/usr/sbin', '/sbin'])},
            'StandardOutPath': str(logs / 'supervisor.log'),
            'StandardErrorPath': str(logs / 'supervisor-errors.log')}
        subprocess.run(['launchctl', 'bootout', f'gui/{os.getuid()}', str(plist)], capture_output=True)
        temporary = plist.with_suffix('.tmp')
        temporary.write_bytes(plistlib.dumps(configuration))
        temporary.replace(plist)
        subprocess.run(['launchctl', 'bootstrap', f'gui/{os.getuid()}', str(plist)], check=True)
    print(json.dumps({'config': str(destination), 'watchdog': args.watchdog, 'shared_tool_files': shared}))


if __name__ == '__main__':
    main()
