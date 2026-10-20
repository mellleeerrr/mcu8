#!/usr/bin/env python3
"""Install only the hash-pinned, pure-Python Pycrate wheel into an isolated venv.

The standard pip/requirements.lock workflow is preferred when ensurepip exists.
This stdlib fallback also works on the VM's minimal Debian Python installation;
it does not need apt, root access, global packages or source build hooks.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import urllib.request
import zipfile

VERSION = '0.8.1'
SHA256 = '65c4dd48d93248a0a4aa2fc78bab10ea64087c6382827035fae9f8bba343db53'


def install(venv, wheel=None):
    venv = Path(venv).expanduser().resolve()
    subprocess.run([sys.executable, '-m', 'venv', '--without-pip', str(venv)], check=True, timeout=90)
    python = venv / ('Scripts/python.exe' if sys.platform == 'win32' else 'bin/python3')
    purelib = Path(subprocess.check_output([str(python), '-c', 'import sysconfig;print(sysconfig.get_paths()["purelib"])'], text=True).strip()).resolve()
    if not purelib.is_relative_to(venv):
        raise RuntimeError('venv site-packages escaped the requested directory')
    if wheel:
        payload = Path(wheel).read_bytes()
    else:
        with urllib.request.urlopen(f'https://pypi.org/pypi/pycrate/{VERSION}/json', timeout=20) as response:
            metadata = json.load(response)
        url = next(item['url'] for item in metadata['urls'] if item['filename'].endswith('.whl') and item['digests']['sha256'] == SHA256)
        with urllib.request.urlopen(url, timeout=25) as response:
            payload = response.read(20 * 1024 * 1024 + 1)
    if len(payload) > 20 * 1024 * 1024 or hashlib.sha256(payload).hexdigest() != SHA256:
        raise RuntimeError('Pycrate wheel SHA256/size verification failed')
    purelib.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(io.BytesIO(payload)) as archive:
        for member in archive.infolist():
            relative = Path(member.filename)
            # Only importable library modules and original license/metadata.
            # CLI launchers in the wheel's .data directory are not needed.
            if relative.parts[0].endswith('.data'):
                continue
            if not (relative.parts[0].startswith('pycrate_') or relative.parts[0] == f'pycrate-{VERSION}.dist-info'):
                raise RuntimeError('Unexpected wheel member')
            destination = (purelib / relative).resolve()
            if not destination.is_relative_to(purelib):
                raise RuntimeError('Unsafe wheel path')
            if member.is_dir():
                destination.mkdir(parents=True, exist_ok=True)
            else:
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(archive.read(member))
    subprocess.run([str(python), '-c', 'import importlib.metadata,pycrate_asn1rt;assert importlib.metadata.version("pycrate")=="0.8.1";print("Pycrate 0.8.1 ready")'], check=True, timeout=30)
    print(python)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--venv', default='~/.local/share/vks-mcu/h323-venv')
    parser.add_argument('--wheel', help='Optional already-downloaded wheel; the same SHA256 is enforced')
    args = parser.parse_args()
    install(args.venv, args.wheel)
