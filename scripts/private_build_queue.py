"""Redie-side master push queue. No build, signing, or shell execution from queue data."""
import argparse
import base64
import contextlib
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

OID = re.compile(r"^[0-9a-f]{40}$")
PHASES = {"fetching", "checking", "building", "publishing", "published", "failed", "superseded"}


def read(path, default=None):
    try:
        data = path.read_bytes()
    except FileNotFoundError:
        return default
    if len(data) > 65536:
        raise ValueError("Queue file exceeds size limit")
    return json.loads(data)


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, name = tempfile.mkstemp(dir=path.parent, prefix=".queue-")
    try:
        with os.fdopen(fd, "w") as stream:
            json.dump(value, stream)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(name, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(name):
            os.unlink(name)


@contextlib.contextmanager
def locked(state):
    state.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (state / "queue.lock").open("a") as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        yield


def tip(repo):
    result = subprocess.run(["git", "--git-dir=" + str(repo), "rev-parse", "--verify", "refs/heads/master"], capture_output=True, text=True)
    value = result.stdout.strip()
    return value if result.returncode == 0 and OID.fullmatch(value) else None


def enqueue(repo, state, oid, force=False):
    if not OID.fullmatch(oid):
        raise ValueError("Invalid master commit")
    with locked(state):
        current = tip(repo)
        if current != oid:
            return None
        previous = read(state / "request.json", {})
        if previous.get("oid") == oid and not force:
            return previous
        value = {"version": 1, "oid": oid, "generation": previous.get("generation", 0) + 1, "queued_at": int(time.time())}
        write(state / "request.json", value)
        return value


def receive(repo, state, lines):
    candidates = []
    for line in lines:
        parts = line.split()
        if len(parts) != 3 or not OID.fullmatch(parts[0]) or not OID.fullmatch(parts[1]):
            raise ValueError("Invalid post-receive input")
        if parts[2] == "refs/heads/master" and parts[1] != "0" * 40:
            candidates.append(parts[1])
    if candidates:
        value = enqueue(repo, state, candidates[-1])
        if value:
            print("Herdr: queued signed release build for " + value["oid"][:12] + "; last good release stays active until checks pass.")


def request(repo, state):
    value = read(state / "request.json")
    current = tip(repo)
    # Reconcile a newer tip if a later hook was interrupted. No initial release
    # is synthesized before the first explicitly queued push.
    if value and current and current != value.get("oid"):
        value = enqueue(repo, state, current)
    return value if value and current == value.get("oid") else None


def publication(feed):
    source=read(feed / "published-source.json")
    envelope=read(feed / "current.json")
    if not source or not envelope:
        return None
    manifest=json.loads(base64.b64decode(envelope["manifest"],validate=True))
    return source if all(source.get(key)==manifest.get(key) for key in ["build_id","release","sha256"]) else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["receive", "request", "status", "update", "retry"])
    parser.add_argument("--repo", type=Path, default=Path.home() / "git/herdr.git")
    parser.add_argument("--state", type=Path, default=Path.home() / ".local/state/herdr/private-builds")
    args = parser.parse_args()
    if args.mode == "receive":
        receive(args.repo, args.state, sys.stdin.read(65537).splitlines())
    elif args.mode == "request":
        print(json.dumps(request(args.repo, args.state)))
    elif args.mode == "status":
        print(json.dumps({"request": request(args.repo, args.state), "worker": read(args.state / "status.json", {}), "last_success": read(args.state / "last-success.json"), "publication": publication(Path.home() / ".local/state/herdr/private-update-feed")}))
    elif args.mode == "retry":
        current = tip(args.repo)
        if not current:
            raise ValueError("Master does not exist")
        print(json.dumps(enqueue(args.repo, args.state, current, force=True)))
    elif args.mode == "update":
        data = sys.stdin.buffer.read(65537)
        value = json.loads(data) if len(data) <= 65536 else None
        if not isinstance(value, dict) or value.get("phase") not in PHASES or not OID.fullmatch(value.get("oid", "")):
            raise ValueError("Invalid worker status")
        value["at"] = int(time.time())
        with locked(args.state):
            write(args.state / "status.json", value)
            if value["phase"] == "published":
                write(args.state / "last-success.json", value)


if __name__ == "__main__":
    main()
