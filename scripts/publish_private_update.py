"""Publish a tested, signed Mac manager to the private Redie feed.

Signing keys stay on the publishing Mac, outside the repository. Clients trust
only the public key in their explicitly enabled bootstrap policy. Publishing is
an explicit action; source sync and git push do not publish executable updates.
"""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import shlex
import re
import sys
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
NAMESPACE = "herdr-private-update-v1"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--host", default="david@redie.tail562384.ts.net")
    parser.add_argument("--identity", type=Path)
    parser.add_argument("--signing-key", type=Path, default=Path.home() / ".local/state/herdr/private-update-publisher/signing_ed25519")
    parser.add_argument("--target", default="aarch64-apple-darwin", choices=["aarch64-apple-darwin", "x86_64-apple-darwin"])
    args = parser.parse_args()
    binary = args.binary.resolve()
    policy = json.loads((ROOT / "assets/private-update-channel.json").read_text())
    public = subprocess.check_output(["ssh-keygen", "-y", "-f", str(args.signing_key)], text=True).split()
    if public[:2] != policy["signer"].split()[:2]:
        raise ValueError("Signing key does not match this channel's pinned public key")
    version = subprocess.check_output([str(binary), "--version"], text=True).strip()
    if "-local." not in version:
        raise ValueError("Publish a build from just build-linux-bundle with a local source identity")
    build_id = version.split("-local.", 1)[1]
    if len(build_id) != 24 or not all(c in "0123456789abcdef" for c in build_id):
        raise ValueError("Unexpected local build identity")
    sys.path.insert(0, str(ROOT))
    from scripts.build_linux_bundle import source_id
    if source_id(ROOT) != build_id:
        raise ValueError("Source differs from the tested binary; rebuild before publishing")
    protocol = int(re.search(r"pub const PROTOCOL_VERSION: u32 = (\d+)", (ROOT / "src/protocol/wire.rs").read_text()).group(1))
    generation = int(re.search(r"pub const ENDPOINT_PROTOCOL_GENERATION: u32 = (\d+)", (ROOT / "src/protocol/endpoint.rs").read_text()).group(1))
    ssh = ["ssh", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes"]
    identity = args.identity
    if identity is None:
        identities = Path.home() / ".local/state/herdr/client/ssh-identities.json"
        if identities.exists():
            value = json.loads(identities.read_text()).get(args.host)
            if value:
                identity = Path(value)
    if identity:
        ssh += ["-i", str(identity)]
    ssh += [args.host]
    current = "import pathlib,json,base64; p=pathlib.Path.home()/'.local/state/herdr/private-update-feed/current.json'; print(json.loads(base64.b64decode(json.loads(p.read_text())['manifest']))['release'] if p.exists() else 0)"
    previous = int(subprocess.check_output(ssh + ["python3 -c " + shlex.quote(current)], text=True, timeout=20).strip())
    release = max(int(time.time()), previous + 1)
    data = binary.read_bytes()
    manifest = {"version": 1, "channel": policy["channel"], "release": release, "target": args.target, "build_id": build_id, "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data), "protocol": protocol, "endpoint_generation": generation}
    if not 4096 <= len(data) <= 512 * 1024 * 1024:
        raise ValueError("Invalid release binary size")
    with tempfile.TemporaryDirectory(prefix="herdr-release-") as temp:
        path = Path(temp) / "manifest"
        path.write_bytes(json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode())
        subprocess.run(["ssh-keygen", "-Y", "sign", "-f", str(args.signing_key), "-n", NAMESPACE, str(path)], check=True)
        envelope = json.dumps({"manifest": base64.b64encode(path.read_bytes()).decode(), "signature": base64.b64encode(path.with_suffix(".sig").read_bytes()).decode()}).encode()
    # Immutable content-addressed artifact is complete before current changes.
    upload = '''import pathlib,sys,hashlib,os,tempfile
root=pathlib.Path.home()/'.local/state/herdr/private-update-feed'; artifacts=root/'artifacts'; artifacts.mkdir(parents=True,exist_ok=True)
data=sys.stdin.buffer.read(); digest=hashlib.sha256(data).hexdigest(); assert digest==EXPECTED
fd,name=tempfile.mkstemp(dir=artifacts,prefix='.upload-')
with os.fdopen(fd,'wb') as f: f.write(data); f.flush(); os.fsync(f.fileno())
os.chmod(name,0o644); os.replace(name,artifacts/digest)
'''.replace("EXPECTED", repr(manifest["sha256"]))
    subprocess.run(ssh + ["python3 -c " + shlex.quote(upload)], input=data, check=True, timeout=120)
    activate = '''import pathlib,sys,os,json,base64,tempfile,fcntl
root=pathlib.Path.home()/'.local/state/herdr/private-update-feed'; lock=open(root/'publish.lock','a'); fcntl.flock(lock,fcntl.LOCK_EX)
data=sys.stdin.buffer.read(); new=json.loads(base64.b64decode(json.loads(data)['manifest'])); old=root/'current.json'
assert not old.exists() or json.loads(base64.b64decode(json.loads(old.read_bytes())['manifest']))['release']<new['release'], 'A newer publication won the race; retry'
artifact=root/'artifacts'/new['sha256']; assert artifact.is_file() and artifact.stat().st_size==new['bytes']
bindings=root/'builds'; bindings.mkdir(exist_ok=True); binding=bindings/new['build_id']
assert not binding.exists() or binding.read_text()==new['sha256'], 'Build IDs are immutable: change source/build identity before publishing different bytes'
if not binding.exists():
 with open(binding,'x') as f: f.write(new['sha256']); f.flush(); os.fsync(f.fileno())
fd,name=tempfile.mkstemp(dir=root,prefix='.current-')
with os.fdopen(fd,'wb') as f: f.write(data); f.flush(); os.fsync(f.fileno())
os.chmod(name,0o644); os.replace(name,old)
fd=os.open(root,os.O_RDONLY); os.fsync(fd); os.close(fd)
print('Published release',new['release'],new['build_id'])
'''
    subprocess.run(ssh + ["python3 -c " + shlex.quote(activate)], input=envelope, check=True, timeout=20)


if __name__ == "__main__":
    main()
