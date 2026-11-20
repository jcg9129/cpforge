# cpforge (`cpf`)

cpforge is a Rust command-line workspace for competitive programming. It is
being built to manage problems and tests, build and run solutions under
explicit limits, compare outputs, and support custom checkers, interactive
problems and reproducible stress testing. Fetching samples, preparing
submission files and a terminal interface are planned.

## Current status

Early development: the supervisor/process layer is under construction on
Linux. Linux process ownership, cancellation, deadlines, bounded capture,
measurements and connected runs have implementations and tests. The CLI is
mostly a skeleton; the workflows described in the design documents are not
yet available as commands.

CI runs on Linux, macOS and Windows. On macOS and Windows it currently covers
the test fixture and pipe layer only; the process supervisor is Linux-only so
far, so runtime support there is not yet established. The supervisor runs the user's
own programs; it is not a security sandbox. See the [roadmap](ROADMAP.md) for
completed work, planned milestones and known caveats.

## Build and test

Requires Rust 1.85 or later (edition 2024) and the platform's native linker.
From the repository root:

```sh
cargo build --workspace --locked
cargo test --workspace --all-targets --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

The CLI binary is `target/debug/cpf`. Building/testing the whole workspace
also builds the test fixture binary; see its [harness documentation](crates/cpf-fixture/README.md)
for individual test invocations. CI provisions C++, Python 3.12 and JDK 21
for workflow validation.

The `storage-test-hooks` feature exposes deterministic failure injection for
storage integration tests. It is disabled by default; `--all-features` includes
these hooks even in non-test builds. The crate is unpublished, and production
builds should leave this feature disabled.

## Declared platform floor

| Platform | Targets | Minimum |
|---|---|---|
| Linux | `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` | kernel 5.4, glibc 2.28 |
| macOS | `aarch64-apple-darwin`, `x86_64-apple-darwin` | macOS 13 |
| Windows | `x86_64-pc-windows-msvc` | Windows 10 1809 / Server 2019 |

These are declared minimums, not verified minimum-version environments.
Required facilities are probed at runtime; missing facilities produce a
reported fallback or a clean error as specified by the execution contract.

## Documentation

The designs describe intended behavior; they are not a list of implemented
CLI features. Managed-workspace contract amendments are settled but not yet
implemented.

- [Roadmap and implementation status](ROADMAP.md)
- [Overall design](docs/design.md)
- [Checkers and interactors](docs/design-checkers.md)
- [Managed workspaces](docs/design-workspace.md)
- [Comparator specification](docs/compare-spec.md)
- [Execution contract](docs/contracts/execution.md)
- [Storage contract](docs/contracts/storage.md)
- [Configuration and commands contract](docs/contracts/config.md)

## Test catalogs

For an existing selected problem, `cpf test ls` lists registered tests, then
unregistered `*.in` candidates and stray `*.ans` files. It reads without a
workspace lock or recovery and reports invalid catalogs and unhealthy files.
The output is for people and is not a stable machine format. A named answer
is shown as `present` with a separate health error if it cannot be used;
`empty` requires a healthy zero-byte file, and `absent` means no answer was
registered.

`cpf test register tests/example.in` registers an existing input, using its
sibling `example.ans` if present and reporting that choice on stderr. Use
`--no-answer` to omit it, `--answer <file>` to choose one, or `--id <id>` when
the stem is invalid or already taken. Relative paths are resolved lexically
from the **current directory**, including when `--problem` selects a
different problem; absolute paths are also accepted for the same lexical
destination. There is no fallback to paths relative to that problem:
from the workspace root, for example, use
`cpf --problem A test register problems/A/tests/example.in`. Files can be
links to regular data; registration never copies or changes them.

`cpf test add --input <file> [--answer <file>]` copies a new custom test and
prints its id (`custom-1`, `custom-2`, ...). Omit `--answer` for no reference
answer; an empty answer file is a present, empty answer. Either source may
be `-` for stdin, but both cannot be stdin. Paths are relative to the current
directory and may name streams, including FIFOs and links. Both sources are
captured with bounded memory in private OS temporary files before locking;
read errors publish nothing. Captures are removed on normal exit, including
failure; a crash can leave them for OS cleanup.

Published test files are never overwritten or deleted. A failure after input
publication reports the committed paths, that they are not registered, and
the failing step and cause. `cpf test ls` shows unregistered inputs as
candidates; a retry may allocate the next number. A durability warning means
the publication committed and the transaction continued.

`cpf test enable <id>` and `cpf test disable <id>` change registered entries
by exact id spelling. Discovery does not register or run files. Bare
`cpf test` reports that running tests is not yet available. Freezing inputs
is reserved for a later task.

Catalogs use ordered `[[test]]` tables; omit the tables for an empty catalog.
Discovery recognizes the literal lowercase `.in` and `.ans` suffixes and
pairs siblings by exact stem spelling; `register` infers a sibling the same
way and records every path with its exact on-disk spelling, refusing a name
that a case-insensitive filesystem would only match by case. A sibling already
named by an entry is not offered with a candidate. Collision keys still exclude paths
already reserved by either catalog role and determine candidate ordering
(with exact spelling breaking ties). Invalid portable filenames are reported
as diagnostics instead of offered for registration. These spelling choices
make discovery deterministic even on filesystems that distinguish case.

## License

Licensed under either the [MIT License](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option.

Copyright (c) 2026 José Carlos.
