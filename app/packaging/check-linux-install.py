#!/usr/bin/env python3
"""Exercise the real Linux installer in disposable HOME/XDG directories.
Run after building: python3 packaging/check-linux-install.py [release-binary]
Requires gio and desktop-file-validate; no display is needed.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

source = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/release/ferrous-commander').resolve()

def run(*args, env):
    result = subprocess.run(args, env=env, capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise RuntimeError(f"{args}: {result.stderr}")
    return result

with tempfile.TemporaryDirectory(prefix='ferrous-install-') as root:
    for custom_xdg in (False, True):
        home = Path(root) / ('Żółć space % dollar$ quote" tick` slash\\' + str(custom_xdg))
        home.mkdir()
        env = dict(os.environ, HOME=str(home))
        for key in ('XDG_DATA_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME'):
            env.pop(key, None)
        if custom_xdg:
            env.update(XDG_DATA_HOME=str(home / 'xdg data'), XDG_CONFIG_HOME=str(home / 'xdg config'))
        data = Path(env.get('XDG_DATA_HOME', home / '.local/share'))
        config = Path(env.get('XDG_CONFIG_HOME', home / '.config')) / 'ferrous/settings.lua'
        exe = home / '.local/bin/ferrous-commander'
        desktop = data / 'applications/ferrous-commander.desktop'
        icon = data / 'icons/hicolor/64x64/apps/ferrous-commander.png'
        run(str(source), '--install', env=env)
        assert exe.is_file() and os.access(exe, os.X_OK)
        assert icon.read_bytes().startswith(b'\x89PNG')
        run('desktop-file-validate', str(desktop), env=env)
        run(str(exe), '--install', env=env)  # reinstall from installed location
        # Block icon export on a FIFO, keeping the real installed executable running.
        fifo = home / 'blocked.png'
        os.mkfifo(fifo)
        running = subprocess.Popen([str(exe), '--export-icon', str(fifo)], env=env)
        try:
            time.sleep(0.2)
            assert running.poll() is None
            inode = exe.stat().st_ino
            run(str(source), '--install', env=env)
            assert exe.stat().st_ino != inode
            assert running.poll() is None
        finally:
            running.terminate()
            running.wait(timeout=10)
        # GIO must parse the generated Exec and launch the actual installed binary.
        # Export mode exits by itself; append test arguments to a copy of the launcher.
        launcher = Path(root) / 'verify.desktop'
        shot = Path(root) / f'launch-{custom_xdg}.png'
        text = desktop.read_text().replace(' %F', f' --export-icon {shot}')
        launcher.write_text(text)
        run('gio', 'launch', str(launcher), env=env)
        for _ in range(150):
            if shot.exists():
                break
            time.sleep(0.1)
        assert shot.exists(), 'GIO app-menu launch failed'
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text('return { theme = "Dark" }\n')
        saved = config.read_bytes()
        run(str(exe), '--uninstall', env=env)
        assert not any(p.exists() for p in (exe, desktop, icon))
        assert config.read_bytes() == saved
        run(str(source), '--uninstall', env=env)  # repeat is harmless
        print(f'PASS: install, self-reinstall, running upgrade, GIO launch, uninstall; custom XDG={custom_xdg}')
