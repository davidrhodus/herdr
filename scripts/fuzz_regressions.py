"""Preserve libFuzzer failures as byte-exact seeds replayed by normal Rust tests."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil

TARGETS = ("parser_chunking", "parser_operations")
FAILURE_PREFIXES = ("crash-", "timeout-", "oom-", "leak-")
ROOT = Path(__file__).resolve().parent.parent


def preserve(target: str, artifacts: Path, root: Path, archive: Path | None = None) -> list[str]:
    if target not in TARGETS:
        raise ValueError(f"unknown fuzz target: {target}")
    corpus = root / "fuzz" / "corpus" / target
    corpus.mkdir(parents=True, exist_ok=True)
    saved = []
    for source in sorted(artifacts.glob("*")):
        if not source.is_file() or not source.name.startswith(FAILURE_PREFIXES):
            continue
        data = source.read_bytes()
        name = "seed-regression-" + hashlib.sha256(data).hexdigest()
        (corpus / name).write_bytes(data)
        saved.append({"artifact": source.name, "seed": name, "bytes": len(data)})
    if archive is not None:
        destination = archive / "fuzz" / "corpus" / target
        destination.mkdir(parents=True, exist_ok=True)
        # Include previously cached regressions as well as this run's failures.
        for seed in sorted(corpus.glob("seed-regression-*")):
            shutil.copyfile(seed, destination / seed.name)
        (archive / "regressions.json").write_text(
            json.dumps({"target": target, "failures": saved}, indent=2) + "\n"
        )
    return [item["seed"] for item in saved]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=TARGETS)
    parser.add_argument("artifacts", type=Path)
    parser.add_argument("--archive", type=Path)
    args = parser.parse_args()
    names = preserve(args.target, args.artifacts, ROOT, args.archive)
    print(f"Preserved {len(names)} failure artifact(s) for {args.target}")
    for name in names:
        print(f"fuzz/corpus/{args.target}/{name}")


if __name__ == "__main__":
    main()
