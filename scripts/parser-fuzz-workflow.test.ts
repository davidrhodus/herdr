import { expect, test } from "bun:test";
import { chmodSync, copyFileSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

const workflow = Bun.YAML.parse(readFileSync(new URL("../.github/workflows/parser-fuzz.yml", import.meta.url), "utf8")) as any;
const job = workflow.jobs.fuzz;
const step = (name: string) => job.steps.find((item: any) => item.name === name);

test("both scheduled targets finish independently and preserve failures after nonzero exits", () => {
  expect(workflow.on.schedule).toHaveLength(1);
  expect(workflow.on.workflow_dispatch).toBeDefined();
  expect(job.strategy.matrix.target).toEqual(["parser_chunking", "parser_operations"]);
  expect(job.strategy["fail-fast"]).toBe(false);
  expect(workflow.concurrency["cancel-in-progress"]).toBe(false);
  expect(workflow.permissions).toEqual({ contents: "read" });
  expect(step("Preserve failures as regression seeds").if).toBe("always()");
  expect(step("Upload logs and regression tests").if).toBe("always()");
  // Explicit status functions allow these steps to run after a failed fuzz step.
  for (const name of ["Replay regression seeds", "Save evolved corpus including regressions"]) {
    expect(step(name).if).toContain("!cancelled()");
    expect(step(name).if).not.toContain("success()");
  }
});

test.skipIf(process.platform === "win32")("a crashing fuzz command stays failed and its raw bytes become a regression artifact", () => {
  for (const target of job.strategy.matrix.target) {
    const root = mkdtempSync(join(tmpdir(), "herdr-fuzz-ci-"));
    try {
      mkdirSync(join(root, "bin"));
      mkdirSync(join(root, "scripts"));
      mkdirSync(join(root, "results"));
      copyFileSync(new URL("./fuzz_regressions.py", import.meta.url), join(root, "scripts/fuzz_regressions.py"));
      const cargo = join(root, "bin/cargo");
      writeFileSync(cargo, `#!/bin/sh
mkdir -p "fuzz/artifacts/$FUZZ_TARGET"
printf '\\000\\377\\302\\223' > "fuzz/artifacts/$FUZZ_TARGET/crash-injected"
echo 'injected parser failure' >&2
exit 77
`);
      chmodSync(cargo, 0o755);
      const env = { ...process.env, PATH: `${join(root, "bin")}:${process.env.PATH}`, FUZZ_TARGET: target, FUZZ_RESULTS: "results" };
      const crashed = spawnSync("bash", ["-e", "-c", step("Fuzz for 30 minutes").run], { cwd: root, env, encoding: "utf8" });
      expect(crashed.status).toBe(77);
      expect(readFileSync(join(root, "results/fuzz.log"), "utf8")).toContain("injected parser failure");
      const preserved = spawnSync("bash", ["-e", "-c", step("Preserve failures as regression seeds").run], { cwd: root, env, encoding: "utf8" });
      expect(preserved.status).toBe(0);
      const manifest = JSON.parse(readFileSync(join(root, "results/regressions.json"), "utf8"));
      expect(manifest.failures).toHaveLength(1);
      const seed = join("fuzz/corpus", target, manifest.failures[0].seed);
      const expected = Buffer.from([0, 255, 194, 147]);
      expect(readFileSync(join(root, seed))).toEqual(expected);
      expect(readFileSync(join(root, "results", seed))).toEqual(expected);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }
});
