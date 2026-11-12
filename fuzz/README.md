# Terminal parser fuzzing

`just fuzz-smoke` runs deterministic chunk-boundary tests and all committed
`seed-*` corpus files on stable Rust. These tests also run in normal CI.

For local AddressSanitizer fuzzing:

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --version 0.13.2 --locked
just fuzz-parser 1800
just fuzz-parser-operations 1800
```

`.github/workflows/parser-fuzz.yml` runs nightly at 05:23 UTC and supports manual
runs through Actions. Each target gets its own Ubuntu job and a 30-minute fuzzing
budget. The jobs keep their evolved corpora in separate caches and never cancel
the other target when one fails.

Failures keep the job red. Even after a fuzzing failure, CI copies crash,
timeout, out-of-memory, and leak inputs into byte-exact
`fuzz/corpus/<target>/seed-regression-<sha256>` files and runs the ordinary Rust
corpus regression test. The artifact includes those regression files, the
original failure names in `regressions.json`, logs, source revision, and nightly
compiler version. Cached regression seeds are included in later artifacts too.

Download the failed job's `parser-fuzz-<target>-<run>-<attempt>` artifact and
extract it into the repository root; its `fuzz/corpus/` paths match the checkout.
Then run:

```sh
just fuzz-smoke
# Reproduce the original sanitizer failure (use its recorded nightly version):
cargo +nightly fuzz run parser_chunking fuzz/corpus/parser_chunking/seed-regression-<sha256>
```

Use `parser_operations` for operation-target failures. To preserve a local crash:

```sh
python3 scripts/fuzz_regressions.py parser_chunking fuzz/artifacts/parser_chunking
just fuzz-smoke
```

Commit the resulting regression seeds with the parser fix. They are picked up
automatically by the corpus test; no new Rust test registration is needed. CI
artifacts last 90 days (subject to repository retention limits), and caches can
be evicted. Commit the seeds to retain them permanently. The workflow has read-only
repository permissions and does not push commits or open pull requests.

A timeout or memory-limit artifact may pass the ordinary test when the failure
depends on sanitizer overhead or accumulated process state. Keep its logs and
reproduce under the recorded fuzzing environment before diagnosing it.
