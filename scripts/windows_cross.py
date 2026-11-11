"""Install and lint the Windows Rust target from Unix hosts."""

import argparse
import subprocess
import sys


TARGET = "x86_64-pc-windows-msvc"


def setup() -> None:
    # Cross-checks compile Rust metadata and need no Windows SDK or linker.
    subprocess.run(["rustup", "target", "add", TARGET], check=True)


def lint() -> None:
    subprocess.run(["rustup", "target", "add", TARGET], check=True)
    subprocess.run(
        # --tests also checks Windows-only test code, which Unix builds never compile.
        [
            "cargo",
            "clippy",
            "--bin",
            "herdr",
            "--tests",
            "--locked",
            "--target",
            TARGET,
            "--",
            "-D",
            "warnings",
        ],
        check=True,
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("setup", help="Install the Windows Rust target")
    commands.add_parser("lint", help="Check the Windows target without linking")
    args = parser.parse_args()
    try:
        if args.command == "setup":
            setup()
        else:
            lint()
    except (ValueError, OSError) as error:
        print(error, file=sys.stderr)
        return 1
    except subprocess.CalledProcessError as error:
        return error.returncode
    return 0


if __name__ == "__main__":
    sys.exit(main())
