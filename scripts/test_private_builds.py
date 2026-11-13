import contextlib
import concurrent.futures
import io
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

if os.name == "posix":
    from scripts import private_build_queue as queue
    from scripts import private_build_worker as worker
    from scripts import publish_private_update as publisher
    from scripts import build_linux_bundle as bundle


@unittest.skipUnless(os.name == "posix", "Private build queue uses Unix services")
class QueueTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.work = self.root / "source"
        self.bare = self.root / "bare.git"
        self.state = self.root / "queue"
        self.work.mkdir()
        self.git("init", "-q")
        (self.work / "file").write_text("one")
        self.a = self.commit()
        subprocess.run(["git", "clone", "-q", "--bare", str(self.work), str(self.bare)], check=True)

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.work), *args], stderr=subprocess.DEVNULL, text=True).strip()

    def commit(self):
        self.git("add", ".")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture")
        oid = self.git("rev-parse", "HEAD")
        # Force the named branch in the fixture independently of Git defaults.
        self.git("branch", "-M", "master")
        return oid

    def advance(self):
        (self.work / "file").write_text("two")
        self.b = self.commit()
        subprocess.run(["git", "--git-dir=" + str(self.bare), "fetch", "-q", str(self.work), "master:master"], check=True)
        return self.b

    def test_only_master_pushes_queue_and_rapid_pushes_coalesce(self):
        queue.receive(self.bare, self.state, ["0" * 40 + " " + self.a + " refs/heads/topic"])
        self.assertFalse((self.state / "request.json").exists())
        queue.receive(self.bare, self.state, ["0" * 40 + " " + self.a + " refs/heads/master"])
        self.assertEqual(queue.request(self.bare, self.state)["oid"], self.a)
        b = self.advance()
        queue.receive(self.bare, self.state, [self.a + " " + b + " refs/heads/master"])
        queue.receive(self.bare, self.state, ["0" * 40 + " " + self.a + " refs/heads/master"])
        self.assertEqual(queue.request(self.bare, self.state)["oid"], b)
        self.assertEqual(queue.read(self.state / "request.json")["generation"], 2)

    def test_deleted_other_and_malformed_refs_do_not_start_builds(self):
        queue.receive(self.bare, self.state, [self.a + " " + "0" * 40 + " refs/heads/master"])
        self.assertFalse((self.state / "request.json").exists())
        for bad in ["anything", self.a + " bad refs/heads/master", "-" * 40 + " " + self.a + " refs/heads/master"]:
            with self.assertRaises(ValueError):
                queue.receive(self.bare, self.state, [bad])

    def test_request_recovers_a_later_missed_hook(self):
        queue.enqueue(self.bare, self.state, self.a)
        b = self.advance()
        self.assertEqual(queue.request(self.bare, self.state)["oid"], b)

    def test_build_numbers_are_monotonic_and_exact_retries_reuse_the_reservation(self):
        queue.enqueue(self.bare,self.state,self.a)
        value={"oid":self.a,"generation":1,"context":"a"*64}
        self.assertEqual(queue.reserve_number(self.bare,self.state,value)["build_number"],1)
        self.assertEqual(queue.reserve_number(self.bare,self.state,value)["build_number"],1)
        queue.enqueue(self.bare,self.state,self.a,force=True)
        value["generation"]=2
        self.assertEqual(queue.reserve_number(self.bare,self.state,value)["build_number"],2)
        value["context"]="b"*64
        self.assertEqual(queue.reserve_number(self.bare,self.state,value)["build_number"],3)

    def test_parallel_allocations_are_distinct_and_corruption_never_resets_counter(self):
        queue.enqueue(self.bare,self.state,self.a)
        values=[{"oid":self.a,"generation":1,"context":format(i,"064x")} for i in range(8)]
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            numbers=list(pool.map(lambda v:queue.reserve_number(self.bare,self.state,v)["build_number"],values))
        self.assertEqual(sorted(numbers),list(range(1,9)))
        queue.write(self.state/"build-counter.json",{"version":1,"value":"broken"})
        with self.assertRaises(ValueError):queue.reserve_number(self.bare,self.state,values[0])
        (self.state/"build-counter.json").unlink()
        with self.assertRaises(ValueError):queue.reserve_number(self.bare,self.state,values[0])

    def test_sequential_worker_jobs_do_not_leak_the_previous_build_number(self):
        value=self.fake_worker();observed=[]
        value.ssh=lambda mode,payload=None: {"publication":None} if mode=="status" else {"build_number":payload["generation"]}
        def run(command):
            if command==["just","check"]:self.assertNotIn("HERDR_BUILD_NUMBER",value.env)
            if command==["just","build-linux-bundle"]:observed.append(value.env["HERDR_BUILD_NUMBER"])
        value.run=run
        value.process({"oid":self.a,"generation":1});value.process({"oid":self.a,"generation":2})
        self.assertEqual(observed,["1","2"])

    def fake_worker(self):
        value = worker.Worker.__new__(worker.Worker)
        value.state = self.root / "worker"
        value.state.mkdir()
        value.checkout = self.work
        value.context = "context"
        value.env = {"HERDR_BUILD_VARIANT": "variant"}
        value.config = {"host": "fixture", "identity": "/key", "git_dir": str(self.bare)}
        value.phases = []
        value.commands = []
        value.ssh = lambda mode, payload=None: {"publication": None} if mode == "status" else {"oid": self.a}
        value.phase = lambda oid, phase, **extra: value.phases.append(phase)
        value.prepare = lambda oid: True
        value.exact_checkout = lambda oid: None
        value.current = lambda oid: True
        value.run = lambda command: value.commands.append(command)
        return value

    def test_push_during_checks_never_reaches_build_or_publication(self):
        value = self.fake_worker()
        value.current = lambda oid: False
        value.process({"oid": self.a, "generation": 1})
        self.assertEqual(value.phases, ["fetching", "checking", "superseded"])
        self.assertFalse(any("publish_private_update.py" in str(c) for c in value.commands))

    def test_failed_checks_remain_terminal_until_a_new_request(self):
        value = self.fake_worker()
        def fail(command):
            value.commands.append(command)
            raise worker.Failed("test failure")
        value.run = fail
        request = {"oid": self.a, "generation": 1}
        value.process(request)
        value.process(request)
        self.assertEqual(value.phases, ["fetching", "checking", "failed"])
        self.assertEqual(len(value.commands), 1)

    def test_restart_recognizes_already_published_source_without_duplicate_release(self):
        value = self.fake_worker()
        value.ssh = lambda mode, payload=None: {"publication": {"commit": self.a, "variant": "variant", "build_id": "build"}}
        value.process({"oid": self.a, "generation": 1})
        self.assertEqual(value.phases, ["published"])
        self.assertEqual(value.commands, [])


@unittest.skipUnless(os.name == "posix", "Private publisher uses Unix Git and SSH tools")
class PublisherTests(QueueTests):
    def setUp(self):
        super().setUp()
        self.home = self.root / "remote-home"
        self.home.mkdir()
        self.key = self.root / "signer"
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(self.key)], check=True)
        (self.work / "assets").mkdir()
        (self.work / "assets/private-update-channel.json").write_text(json.dumps({"signer": self.key.with_suffix(".pub").read_text().strip(), "channel": "fixture"}))
        (self.work / "src/protocol").mkdir(parents=True)
        (self.work / "src/protocol/wire.rs").write_text("pub const PROTOCOL_VERSION: u32 = 22;")
        (self.work / "src/protocol/endpoint.rs").write_text("pub const ENDPOINT_PROTOCOL_GENERATION: u32 = 1;")
        for name in ["Cargo.toml", "Cargo.lock", "build.rs", "scripts/build_linux_bundle.py"]:
            path = self.work / name
            path.parent.mkdir(exist_ok=True)
            path.write_text("fixture")
        (self.work / ".gitignore").write_text("target/\n")
        self.a = self.commit()
        subprocess.run(["git", "--git-dir=" + str(self.bare), "fetch", "-q", str(self.work), "master:master"], check=True)
        self.binary = self.work / "target/release/herdr"
        self.binary.parent.mkdir(parents=True)
        identity = bundle.source_id(self.work)
        self.binary.write_text("#!/bin/sh\necho herdr 0.9.5-local." + identity + "\n#" + "x" * 4096 + "\n")
        self.binary.chmod(0o755)
        self.real_run = subprocess.run
        self.before_activation = None
        self.competing = None

    def remote(self, command, **kwargs):
        if command[0] != "ssh":
            return self.real_run(command, **kwargs)
        remote = shlex.split(command[-1])
        env = os.environ.copy()
        env["HOME"] = str(self.home)
        if remote[:2] == ["python3", "-c"] and "update-ref" in remote[2] and self.before_activation:
            return self.before_activation(remote, env, kwargs)
        return self.real_run(remote, env=env, **kwargs)

    def publish(self):
        args = ["publish_private_update.py", str(self.binary), "--source-root", str(self.work), "--expected-commit", self.a, "--git-dir", str(self.bare), "--host", "fixture", "--signing-key", str(self.key)]
        with mock.patch.object(sys, "argv", args), mock.patch.object(publisher, "ROOT", self.work), mock.patch.object(subprocess, "run", side_effect=self.remote):
            publisher.main()

    def test_clean_commit_is_recorded_and_dirty_source_is_rejected(self):
        self.publish()
        receipt = queue.read(self.home / ".local/state/herdr/private-update-feed/published-source.json")
        self.assertEqual(receipt["commit"], self.a)
        (self.work / "file").write_text("uncommitted")
        with self.assertRaisesRegex(ValueError, "clean"):
            self.publish()

    def other_object(self):
        (self.work / "file").write_text("next")
        b = self.commit()
        subprocess.run(["git", "--git-dir=" + str(self.bare), "fetch", "-q", str(self.work), b], check=True)
        self.git("checkout", "--detach", self.a)
        return b

    def test_master_advancing_before_activation_preserves_current_release(self):
        self.publish()
        current = self.home / ".local/state/herdr/private-update-feed/current.json"
        before = current.read_bytes()
        b = self.other_object()
        def advance(remote, env, kwargs):
            self.real_run(["git", "--git-dir=" + str(self.bare), "update-ref", "refs/heads/master", b, self.a], check=True)
            return self.real_run(remote, env=env, **kwargs)
        self.before_activation = advance
        with self.assertRaises(subprocess.CalledProcessError):
            self.publish()
        self.assertEqual(current.read_bytes(), before)

    def test_push_during_activation_waits_for_the_prepared_git_transaction(self):
        b = self.other_object()
        def race(remote, env, kwargs):
            process = subprocess.Popen(remote, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                end = time.monotonic() + 5
                while not (self.bare / "refs/heads/master.lock").exists():
                    if process.poll() is not None or time.monotonic() > end:
                        self.fail("Publisher did not prepare its Git reference lock")
                    time.sleep(.01)
                self.competing = subprocess.Popen(["git", "-c", "core.filesRefLockTimeout=3000", "--git-dir=" + str(self.bare), "update-ref", "refs/heads/master", b, self.a])
                time.sleep(.05)
                self.assertIsNone(self.competing.poll())
                stdout, stderr = process.communicate(input=kwargs["input"], timeout=5)
                if process.returncode:
                    raise subprocess.CalledProcessError(process.returncode, remote, stdout, stderr)
                return subprocess.CompletedProcess(remote, 0, stdout, stderr)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()
        self.before_activation = race
        self.publish()
        self.assertEqual(self.competing.wait(timeout=5), 0)
        self.assertEqual(queue.tip(self.bare), b)
        receipt = queue.read(self.home / ".local/state/herdr/private-update-feed/published-source.json")
        self.assertEqual(receipt["commit"], self.a)


if __name__ == "__main__":
    unittest.main()
