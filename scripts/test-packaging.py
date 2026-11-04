#!/usr/bin/env python3
"""Check staging and version rejection without root or distribution tools."""
import json
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import sys
import time
sys.dont_write_bytecode = True

repo = Path(__file__).resolve().parent.parent
with tempfile.TemporaryDirectory(prefix='bang-package-test-') as directory:
    root = Path(directory)
    (root / 'scripts').mkdir()
    shutil.copy(repo / 'scripts/package.sh', root / 'scripts')
    shutil.copytree(repo / 'packaging', root / 'packaging')
    for name in ['LICENSE', 'README.md', 'assets/fonts/OFL.txt']:
        target = root / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(repo / name, target)
    (root / 'Cargo.toml').write_text('[workspace.package]\nversion="0.1.0"\n')
    editor = root / 'crates/composer/web'
    editor.mkdir(parents=True)
    for name in ['package.json', 'package-lock.json']:
        (editor / name).write_text(json.dumps({'version': '0.1.0'}))
    binaries = root / 'target/release'
    binaries.mkdir(parents=True)
    for name in ['app', 'bang-compose']:
        (binaries / name).write_text('#!/bin/sh\nexit 0\n')
    commands = root / 'commands'
    commands.mkdir()
    # Replace distribution tools only. Inspect their complete staged input below.
    (commands / 'dpkg-shlibdeps').write_text('#!/bin/sh\necho "shlibs:Depends=libc6 (>= 2.39)"\n')
    (commands / 'dpkg-deb').write_text('#!/bin/sh\n[ "$1" = --root-owner-group ] && [ "$2" = --build ] || exit 1\ntar -C "$3" -cf "$4" .\n')
    for command in commands.iterdir():
        command.chmod(0o755)
    env = dict(os.environ, PATH=f'{commands}:{os.environ["PATH"]}')
    subprocess.run(['bash', str(root / 'scripts/package.sh'), 'deb'], env=env, check=True)
    artifact = root / 'dist/bang_0.1.0-1_amd64.deb'
    with tarfile.open(artifact) as package:
        for name in ['usr/lib/bang/app', 'usr/lib/bang/bang-compose', 'usr/bin/bang']:
            assert package.getmember('./' + name).mode == 0o755
        control = package.extractfile('./DEBIAN/control').read().decode()
        assert 'libc6 (>= 2.39)' in control and 'gnome-keyring' in control
        launcher = package.extractfile('./usr/bin/bang').read().decode()
        assert 'exec /usr/lib/bang/app "$@"' in launcher
        assert package.getmember('./usr/share/licenses/bang/OFL.txt').size > 0
    (editor / 'package.json').write_text('{"version":"9.0.0"}')
    rejected = subprocess.run(['bash', str(root / 'scripts/package.sh'), 'deb'],
                              env=env, capture_output=True)
    assert rejected.returncode != 0, 'Mismatched editor version was accepted'
subprocess.run(['bash', '-n', str(repo / 'packaging/arch/PKGBUILD')], check=True)
print('Packaging staging, executable modes, dependencies and version rejection passed')

# A native host may leave a WebKit child behind after the parent exits.
spec = importlib.util.spec_from_file_location('bang_smoke', repo / 'scripts/smoke-installed.py')
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)
child_program = "import time; time.sleep(60)"
parent_program = (
    "import signal, subprocess, sys, time; "
    "child = subprocess.Popen([sys.executable, '-c', " + repr(child_program) + "]); "
    "signal.signal(signal.SIGTERM, lambda *_: (child.wait(timeout=3), sys.exit(0))); "
    "print(child.pid, flush=True); time.sleep(60)"
)
process = subprocess.Popen([sys.executable, '-c', parent_program],
                           stdout=subprocess.PIPE, text=True, start_new_session=True)
child_pid = int(process.stdout.readline())
smoke.stop(process)
assert process.poll() is not None
try:
    os.kill(child_pid, 0)
except ProcessLookupError:
    pass
else:
    raise AssertionError('Native child process survived cleanup')
print('Native smoke parent and child process-group cleanup passed')
