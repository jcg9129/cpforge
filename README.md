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

## License

Licensed under either the [MIT License](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option.

Copyright (c) 2026 José Carlos.
