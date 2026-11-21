"""Reload active user T3 LaunchAgents with subs' managed Claude hook.

Embedded by push.rs and run from a private temporary file, never with a token
in arguments, plist data, or output. Labels and executable paths come from the
receiving user's existing LaunchAgents, not a built-in machine inventory.
"""
import os
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tempfile
import time
import uuid


HOOK = ('if [ -f "${XDG_CONFIG_HOME:-$HOME/.config}/subs/claude-shell.sh" ]; '
        'then . "${XDG_CONFIG_HOME:-$HOME/.config}/subs/claude-shell.sh"; fi; exec "$@"')


def launch(*args):
    return subprocess.run(['launchctl', *args], capture_output=True, timeout=10)


def running(service):
    result = launch('print', service)
    return result.returncode == 0 and re.search(rb'\bpid = [1-9][0-9]*', result.stdout)


def agents():
    for path in sorted((Path.home() / 'Library/LaunchAgents').glob('*.plist')):
        if path.is_symlink() or path.stat().st_uid != os.getuid():
            continue
        try:
            data = plistlib.loads(path.read_bytes())
        except (ValueError, plistlib.InvalidFileException):
            continue
        args = data.get('ProgramArguments', [])
        program = data.get('Program')
        if program:
            args = [program, *args[1:]]
        if not (isinstance(args, list) and all(isinstance(a, str) for a in args)):
            continue
        if not any(Path(a).name == 't3' and b == 'serve' for a, b in zip(args, args[1:])):
            continue
        label = data.get('Label', '')
        if not isinstance(label, str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,256}', label):
            raise ValueError('invalid T3 label')
        service = f'gui/{os.getuid()}/{label}'
        if running(service):
            if path.stat().st_mode & 0o022:
                raise ValueError('unsafe T3 plist')
            yield path, data, args, service


def atomic_plist(path, data):
    fd, temporary = tempfile.mkstemp(dir=path.parent, prefix='.subs-t3-')
    try:
        with os.fdopen(fd, 'wb') as stream:
            plistlib.dump(data, stream)
        os.chmod(temporary, path.stat().st_mode & 0o777)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def bootstrap(path):
    # bootout returns before launchd has finished removing the old job.
    for _ in range(50):
        if launch('bootstrap', f'gui/{os.getuid()}', str(path)).returncode == 0:
            return
        time.sleep(0.2)
    raise RuntimeError('bootstrap failed')


def restart(path, data, args, service):
    config = Path(os.environ.get('XDG_CONFIG_HOME') or Path.home() / '.config')
    hook = config / 'subs/claude-shell.sh'
    if (hook.is_symlink() or not hook.is_file() or hook.stat().st_uid != os.getuid()
            or hook.stat().st_mode & 0o022):
        raise ValueError('missing or unsafe managed hook')
    if HOOK not in args:
        original = dict(data)
        data = dict(data)
        data.pop('Program', None)
        data['ProgramArguments'] = ['/bin/sh', '-c', HOOK, 'subs-t3', *args]
        atomic_plist(path, data)
        if launch('bootout', service).returncode:
            atomic_plist(path, original)
            raise RuntimeError('bootout failed')
        try:
            bootstrap(path)
        except Exception:
            atomic_plist(path, original)
            bootstrap(path)
            raise
    elif launch('kickstart', '-k', service).returncode:
        raise RuntimeError('kickstart failed')
    for _ in range(50):
        if running(service):
            return
        time.sleep(0.2)
    raise RuntimeError('T3 did not start')


def main():
    jobs = list(agents())
    if not jobs:
        print('subs-t3: inactive')
        return False
    if sys.argv[1] == 'defer':
        label = 'subs-t3-restart-' + uuid.uuid4().hex
        result = launch('submit', '-l', label, '-o', '/dev/null', '-e', '/dev/null',
                        '--', '/bin/sh', '-c', 'sleep 5; exec "$@"', 'subs-t3',
                        sys.executable, str(Path(__file__).resolve()), 'apply', label)
        if result.returncode:
            raise RuntimeError('scheduling failed')
        print('subs-t3: scheduled')
        return True
    for job in jobs:
        restart(*job)
    print('subs-t3: restarted')
    return False


if __name__ == '__main__':
    keep = False
    try:
        keep = main()
    except Exception:
        # launchctl errors and existing plist/environment data may contain secrets.
        print('subs-t3: failed')
        sys.exit(1)
    finally:
        if not keep:
            script = Path(__file__)
            if script.name == 'restart.py' and script.parent.name.startswith('subs-t3.'):
                script.unlink(missing_ok=True)
                script.parent.rmdir()
            if len(sys.argv) > 2:
                launch('remove', sys.argv[2])
