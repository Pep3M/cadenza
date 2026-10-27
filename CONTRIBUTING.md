# Contributing to Cadenza

Thanks for your interest! Bug reports, ideas, docs and code are all welcome.

## Before you start

- **Bugs:** open an issue with your OS, the Cadenza version (`cadenza --version`), steps to
  reproduce and, if possible, the project file.
- **Larger changes** (new features, new crates or dependencies, format changes): open an
  issue first so we can agree on the approach before you invest time.
- Security problems: please do not open a public issue; use GitHub's private
  vulnerability reporting on this repository.

## Development

```sh
cargo build --release -p cadenza-cli -p cadenza-app -p cadenza-plugin-host
cargo test --workspace --release
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

On Linux you need `pkg-config` and `libasound2-dev`. CI runs the same checks (plus
`cargo deny check`) on Linux, and on Windows and macOS for pull requests to `main`.

[`CLAUDE.md`](CLAUDE.md) is the project contract: crate layout and the rules every change
follows; each crate's detailed contract is in [`docs/contracts/`](docs/contracts). The most
important rules:

- **The audio thread is sacred:** no allocation, locks, I/O or panics inside
  `Processor::process` or the engine's realtime callback.
- **Every project mutation is a `cadenza_model::Command`** executed by the `Document`
  (undoable). The UI and the AI use the same commands.
- AI-facing APIs validate their input and return actionable error messages.
- New dependencies go through `[workspace.dependencies]` in the root `Cargo.toml` and must
  pass `cargo deny` (permissive licenses or MPL-2.0).
- Rust 2024, hard tabs (`rustfmt.toml`); English for code, comments and commits;
  [conventional commits](https://www.conventionalcommits.org) (`feat:`, `fix:`, `docs:`…).
- Unit tests next to the code, integration tests in `crates/<name>/tests/`.

## Branches and pull requests

Branch from `dev` (`feat/…`, `fix/…`) and open the pull request against `dev`; `main`
holds released code. Keep pull requests focused, with tests for new behavior.

## License

Cadenza is licensed under GPL-3.0-or-later. By submitting a contribution you agree that it
is licensed under the same terms, and that you have the right to submit it.
