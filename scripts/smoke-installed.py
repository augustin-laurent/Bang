#!/usr/bin/env python3
"""Exercise installed binaries with fictional data under Xvfb and isolated XDG dirs."""
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import tempfile
import time


def stop(process):
    # WebKit creates child processes; stop the complete isolated session before
    # removing XDG directories, otherwise its shader cache races with rmtree.
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=5)
    deadline = time.monotonic() + 5
    while True:
        try:
            os.killpg(process.pid, 0)
        except ProcessLookupError:
            return
        assert time.monotonic() < deadline, 'Graphical process group did not exit'
        time.sleep(0.05)


def main():
    assert Path('/usr/lib/bang/app').is_file()
    assert Path('/usr/lib/bang/bang-compose').is_file()
    assert os.access('/usr/bin/bang', os.X_OK)
    subprocess.run(['desktop-file-validate', '/usr/share/applications/bang.desktop'], check=True)
    for binary in ['app', 'bang-compose']:
        linked = subprocess.run(['ldd', f'/usr/lib/bang/{binary}'], check=True,
                                capture_output=True, text=True).stdout
        assert 'not found' not in linked, linked
    oauth = subprocess.run(['bang', '--check-oauth-release'], check=True,
                           capture_output=True, text=True).stdout
    assert 'Google Desktop registration bundled:' in oauth, oauth
    assert 'client_secret' not in oauth and 'desktop_parameter' not in oauth
    with tempfile.TemporaryDirectory(prefix='bang-package-smoke-') as directory:
        env = os.environ.copy()
        for variable, folder in [('XDG_CONFIG_HOME', 'config'), ('XDG_DATA_HOME', 'data'),
                                 ('XDG_CACHE_HOME', 'cache')]:
            env[variable] = str(Path(directory) / folder)
        env['GDK_BACKEND'] = 'x11'
        with tempfile.TemporaryFile(mode='w+') as errors:
            app = subprocess.Popen(['bang', '--preview'], env=env, stderr=errors, start_new_session=True)
            try:
                time.sleep(5)
                errors.seek(0)
                assert app.poll() is None, errors.read()
            finally:
                stop(app)
            composer = subprocess.Popen(['/usr/lib/bang/bang-compose'], env=env,
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=errors, text=True, start_new_session=True)
            try:
                # A Ready message proves the installed, bundled web editor loaded.
                with selectors.DefaultSelector() as selector:
                    selector.register(composer.stdout, selectors.EVENT_READ)
                    deadline = time.monotonic() + 30
                    while True:
                        remaining = deadline - time.monotonic()
                        assert remaining > 0 and selector.select(remaining), 'Composer Ready timed out'
                        line = composer.stdout.readline()
                        errors.seek(0)
                        assert line, errors.read()
                        if json.loads(line).get('type') == 'ready':
                            break
            finally:
                stop(composer)
    print('Installed package: desktop entry, shared libraries, app and composer passed')


if __name__ == '__main__':
    main()
