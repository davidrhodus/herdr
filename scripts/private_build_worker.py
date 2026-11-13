"""Trusted Mac worker: turn private master pushes into checked signed releases.

Master pushes authorize repository build scripts and tests to execute on this
publishing Mac. The worker never builds unpushed local edits or other branches.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shlex
import shutil
import subprocess
import sys
import time

try:
    from .private_build_queue import OID, read, write
except ImportError:
    from private_build_queue import OID, read, write

HOME = Path.home()
STATE = HOME / ".local/state/herdr/private-build-worker"
LIBEXEC = HOME / ".local/libexec/herdr-private-build"
LABEL = "dev.herdr.private-build-worker"


class Failed(Exception):
    pass


class Worker:
    def __init__(self, config, state=STATE):
        self.config = config
        self.state = state
        self.checkout = state / "checkout"
        self.env = os.environ.copy()
        for name in list(self.env):
            if name.startswith("HERDR_") or name.startswith("CARGO_TARGET_") or name in {"RUSTFLAGS", "RUSTC_WRAPPER", "CARGO_TARGET_DIR"}:
                self.env.pop(name, None)
        self.env["PATH"] = config["path"]
        self.env["CARGO_BUILD_JOBS"] = "4"
        self.env["NEXTEST_TEST_THREADS"] = "4"
        self.env["GIT_SSH_COMMAND"] = shlex.join(config["ssh"][:-1])
        self.context = hashlib.sha256(json.dumps({"checkout": str(self.checkout), "rustc": self.output(["rustc", "-vV"]), "sdk": self.output(["xcrun", "--show-sdk-build-version"]), "pipeline": 1}, sort_keys=True).encode()).hexdigest()
        self.env["HERDR_BUILD_VARIANT"] = "redie-auto-v1-" + self.context
        self.log = None

    def output(self, command, cwd=None):
        return subprocess.check_output(command, cwd=cwd, env=self.env, text=True, timeout=30).strip()

    def ssh(self, mode, payload=None):
        command = ["python3", self.config["helper"], mode, "--repo", self.config["git_dir"]]
        result = subprocess.run([*self.config["ssh"], shlex.join(command)], input=None if payload is None else json.dumps(payload), text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)
        if result.returncode:
            raise Failed(result.stderr.strip() or "Redie is unavailable")
        return json.loads(result.stdout) if result.stdout.strip() else None

    def phase(self, oid, phase, **extra):
        print(time.strftime("%Y-%m-%dT%H:%M:%S"), phase, oid, flush=True)
        self.ssh("update", {"oid": oid, "phase": phase, **extra})

    def current(self, oid):
        request = self.ssh("request")
        return bool(request and request["oid"] == oid)

    def run(self, command):
        self.log.write(("\n$ " + shlex.join(command) + "\n").encode())
        self.log.flush()
        result = subprocess.run(command, cwd=self.checkout, env=self.env, stdout=self.log, stderr=subprocess.STDOUT, timeout=7200)
        if result.returncode:
            raise Failed("Command failed: " + shlex.join(command) + "; see " + str(self.log.name))

    def exact_checkout(self, oid):
        if self.output(["git", "rev-parse", "HEAD"], self.checkout) != oid:
            raise Failed("Checkout no longer matches queued commit")
        if self.output(["git", "status", "--porcelain", "--untracked-files=normal"], self.checkout):
            raise Failed("Build checkout changed; refusing to sign")

    def prepare(self, oid):
        if not self.checkout.exists():
            self.run_outside(["git", "clone", "--no-checkout", self.config["remote"], str(self.checkout)])
        if self.output(["git", "remote", "get-url", "origin"], self.checkout) != self.config["remote"]:
            raise Failed("Unexpected build checkout remote")
        self.run(["git", "fetch", "--no-tags", "origin", "refs/heads/master"])
        if not self.current(oid):
            return False
        self.run(["git", "checkout", "--detach", "--force", oid])
        self.run(["git", "clean", "-fdx", "-e", "target"])
        self.exact_checkout(oid)
        return True

    def run_outside(self, command):
        result = subprocess.run(command, env=self.env, stdout=self.log, stderr=subprocess.STDOUT, timeout=600)
        if result.returncode:
            raise Failed("Unable to prepare build checkout")

    def process(self, request):
        self.env.pop("HERDR_BUILD_NUMBER",None)
        oid = request.get("oid", "")
        if not OID.fullmatch(oid) or not isinstance(request.get("generation"), int):
            raise Failed("Invalid queued commit")
        token = [oid, request["generation"], self.context]
        previous=read(self.state / "attempt.json",{})
        if previous.get("token")==token and previous.get("result")=="published":return
        publication = self.ssh("status").get("publication")
        if publication and publication.get("commit")==oid and publication.get("variant")==self.env["HERDR_BUILD_VARIANT"]:
            self.phase(oid,"published",build_id=publication.get("build_id"),recovered=True,build_number=publication.get("build_number"))
            write(self.state / "attempt.json", {"token":token,"result":"published"})
            return
        previous = read(self.state / "attempt.json", {})
        if previous.get("token") == token and (previous.get("result") in {"published", "failed"} or previous.get("retry_at", 0) > time.time()):
            return
        logs = self.state / "logs"
        logs.mkdir(parents=True, exist_ok=True)
        phase = "fetching"
        with (logs / (oid + ".log")).open("ab", buffering=0) as log:
            self.log = log
            try:
                self.phase(oid, phase)
                if not self.prepare(oid):
                    self.phase(oid, "superseded")
                    return
                phase = "checking"
                self.phase(oid, phase)
                self.run(["just", "check"])
                self.run(["cargo", "nextest", "run", "--workspace", "--all-features", "--locked", "--status-level", "fail", "--final-status-level", "slow"])
                if not self.current(oid):
                    self.phase(oid, "superseded")
                    return
                reservation=self.ssh("reserve",{"oid":oid,"generation":request["generation"],"context":self.context})
                self.env["HERDR_BUILD_NUMBER"]=str(reservation["build_number"])
                phase = "building"
                self.phase(oid, phase,build_number=reservation["build_number"])
                self.run(["just", "build-linux-bundle"])
                self.exact_checkout(oid)
                if not self.current(oid):
                    self.phase(oid, "superseded")
                    return
                phase = "publishing"
                self.phase(oid, phase)
                self.run([sys.executable, str(LIBEXEC / "publish_private_update.py"), str(self.checkout / "target/release/herdr"), "--source-root", str(self.checkout), "--expected-commit", oid, "--host", self.config["host"], "--identity", self.config["identity"], "--git-dir", self.config["git_dir"]])
                self.phase(oid, "published",build_number=int(self.env["HERDR_BUILD_NUMBER"]))
                write(self.state / "attempt.json", {"token": token, "result": "published"})
            except Exception as error:
                if not self.current(oid):
                    self.phase(oid, "superseded")
                    return
                self.phase(oid, "failed", message=str(error)[:4096], failed_stage=phase)
                write(self.state / "attempt.json", {"token": token, "result": "retry" if phase in {"fetching", "publishing"} or isinstance(error,(subprocess.TimeoutExpired,OSError)) else "failed", "retry_at": int(time.time()) + 120})
                print("Build failed:", error, flush=True)
            finally:
                self.log = None


def install(repo, host):
    if sys.platform != "darwin":
        raise Failed("The release worker must run on the publishing Mac")
    identity = json.loads((HOME / ".local/state/herdr/client/ssh-identities.json").read_text())[host]
    ssh = ["ssh", "-i", identity, "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", host]
    git_dir = "/home/david/git/herdr.git"
    helper = "/home/david/.local/libexec/herdr-private-build-queue.py"
    STATE.mkdir(parents=True, exist_ok=True, mode=0o700)
    LIBEXEC.mkdir(parents=True, exist_ok=True, mode=0o700)
    for name in ["private_build_worker.py", "private_build_queue.py", "publish_private_update.py"]:
        stage=LIBEXEC/(name+".stage-"+str(os.getpid()))
        shutil.copy2(repo / "scripts" / name,stage)
        os.replace(stage,LIBEXEC/name)
    config = {"version": 1, "host": host, "identity": identity, "ssh": ssh, "remote": host + ":" + git_dir, "git_dir": git_dir, "helper": helper, "path": str(HOME / ".local/bin") + ":" + str(HOME / ".cargo/bin") + ":/usr/bin:/bin:/usr/sbin:/sbin"}
    write(STATE / "config.json", config)
    for tool in ["just", "bun", "node", "cargo", "rustc", "cargo-nextest"]:
        if not shutil.which(tool, path=config["path"]):
            raise Failed("Missing build dependency: " + tool)
    subprocess.run([shutil.which("python3",path=config["path"]),"-c","import sys,tomllib; assert sys.version_info >= (3,11), 'Python 3.11+ is required for repository checks'"],check=True)
    # Only the tiny queue helper runs on Redie. Preserve an existing hook rather
    # than overwriting unknown administrative behavior.
    helper_bytes = (repo / "scripts/private_build_queue.py").read_bytes()
    remote = '''import pathlib,sys,os
helper=pathlib.Path(HELPER); helper.parent.mkdir(parents=True,exist_ok=True); stage=helper.with_name(helper.name+'.stage-'+str(os.getpid()));stage.write_bytes(sys.stdin.buffer.read());stage.chmod(0o700);os.replace(stage,helper)
repo=pathlib.Path(REPO);hook=repo/'hooks/post-receive';marker='# herdr-private-build-queue-v1'
body='#!/bin/sh\\n'+marker+'\\nexec /usr/bin/python3 '+str(helper)+' receive --repo '+str(repo)+'\\n'
if hook.exists() and hook.read_text()!=body:raise RuntimeError('Existing post-receive hook requires explicit integration; preserved unchanged')
stage=hook.with_name('.post-receive-'+str(os.getpid()));stage.write_text(body);stage.chmod(0o700);os.replace(stage,hook)
'''.replace("HELPER", repr(helper)).replace("REPO", repr(git_dir))
    subprocess.run([*ssh, "python3 -c " + shlex.quote(remote)], input=helper_bytes, check=True, timeout=30)
    plist = HOME / "Library/LaunchAgents" / (LABEL + ".plist")
    plist.parent.mkdir(parents=True, exist_ok=True)
    spec = {"Label": LABEL, "ProgramArguments": [sys.executable, str(LIBEXEC / "private_build_worker.py"), "serve"], "RunAtLoad": True, "KeepAlive": True, "ThrottleInterval": 10, "LowPriorityIO": True, "Nice": 10, "EnvironmentVariables": {"PATH": config["path"]}, "StandardOutPath": str(STATE / "worker.log"), "StandardErrorPath": str(STATE / "worker.log")}
    plist.write_bytes(plistlib.dumps(spec))
    domain = "gui/" + str(os.getuid())
    subprocess.run(["launchctl", "bootout", domain + "/" + LABEL], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(20):
        result = subprocess.run(["launchctl", "bootstrap", domain, str(plist)], capture_output=True)
        if result.returncode == 0:
            break
        time.sleep(.2)
    else:
        raise Failed(result.stderr.decode(errors="replace"))
    print("Installed master-push release queue and Mac build worker")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["install", "serve", "once", "status", "retry"])
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--host", default="david@redie.tail562384.ts.net")
    args = parser.parse_args()
    if args.mode == "install":
        install(args.repo.resolve(), args.host)
        return
    config = read(STATE / "config.json")
    worker = Worker(config)
    if args.mode in {"status", "retry"}:
        print(json.dumps(worker.ssh(args.mode), indent=2))
        return
    STATE.mkdir(parents=True, exist_ok=True)
    with (STATE / "worker.lock").open("a") as lease:
        try:
            fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return
        while True:
            try:
                request = worker.ssh("request")
                if request:
                    worker.process(request)
            except Exception as error:
                print("Worker retrying:", error, flush=True)
            if args.mode == "once":
                return
            time.sleep(15)


if __name__ == "__main__":
    main()
