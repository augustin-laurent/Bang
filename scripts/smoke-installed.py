#!/usr/bin/env python3
"""Exercise installed binaries with fictional data under Xvfb and isolated XDG dirs."""
import json
import os
from pathlib import Path
import selectors
import subprocess
import tempfile
import time


def stop(process):
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def main():
    assert Path('/usr/lib/bang/app').is_file()
    assert Path('/usr/lib/bang/bang-compose').is_file()
    assert os.access('/usr/bin/bang', os.X_OK)
    subprocess.run(['desktop-file-validate', '/usr/share/applications/bang.desktop'], check=True)
    for binary in ['app', 'bang-compose']:
        linked = subprocess.run(['ldd', f'/usr/lib/bang/{binary}'], check=True,
                                capture_output=True, text=True).stdout
        assert 'not found' not in linked, linked
    subprocess.run(['bang', '--check-oauth-release'], check=True)
    with tempfile.TemporaryDirectory(prefix='bang-package-smoke-') as directory:
        env = os.environ.copy()
        for variable, folder in [('XDG_CONFIG_HOME', 'config'), ('XDG_DATA_HOME', 'data'),
                                 ('XDG_CACHE_HOME', 'cache')]:
            env[variable] = str(Path(directory) / folder)
        env['GDK_BACKEND'] = 'x11'
        with tempfile.TemporaryFile(mode='w+') as errors:
            app = subprocess.Popen(['bang', '--preview'], env=env, stderr=errors)
            try:
                time.sleep(5)
                errors.seek(0)
                assert app.poll() is None, errors.read()
            finally:
                stop(app)
            composer = subprocess.Popen(['/usr/lib/bang/bang-compose'], env=env,
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=errors, text=True)
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
