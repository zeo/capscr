# contributing to capscr

short version: fork, branch, build, PR. read on for the specifics.

## what we accept

- **bug fixes** with a reproduction (manual repro steps or a failing test).
- **performance** improvements with a before/after measurement (RAM, CPU, latency, binary size).
- **plugin format / marketplace** work — see [`README.md`](README.md) for the v0.4 plan.
- **upload destinations** beyond imgur / custom http / ftp. one destination per PR.
- **docs, screenshots, demo gifs.**

## what we'll usually decline

- new big dependencies. capscr ships a small binary by design — adding `serde_yaml` to read a config that's already TOML is a no.
- features that already have a plugin shape. e.g. "auto-OCR every capture" should land as a plugin once the host is ready (v0.4), not in core.
- code style refactors that don't change behaviour. land them with a feature change, not on their own.
- migrations to other languages, build systems, or frontend frameworks. solid + tauri is what we have.

## dev loop

```bash
# Rust
cargo check --bin capscr           # type-check (fast)
cargo clippy --bin capscr --all-targets -- -D warnings
cargo test --bin capscr

# frontend
npm --prefix frontend install
npm --prefix frontend run build    # tsc --noEmit && vite build

# dev run (Rust + Vite + HMR)
cargo tauri dev

# release build (needs signing env loaded — see README)
cargo tauri build
```

## commits + PRs

- **commit messages**: lowercase, present tense, one line. version bumps prefix with the new version, e.g. `0.3.20: event-driven hotkeys, cached selector back buffer`.
- **branches**: `feature/<thing>`, `fix/<thing>`, `docs/<thing>`. no convention enforcement, just don't push directly to `master`.
- **PRs**: link the issue if one exists. include screenshots for UI changes. say what you tested.

## style

- **rust**: rustfmt defaults. clippy-clean (`-D warnings`).
- **typescript**: tsc strict. no eslint config — match the surrounding code.
- **comments**: lowercase, no terminal punctuation, only when the *why* is non-obvious. don't restate what the code does.

```rust
// kept here so older 0.3.0 configs don't fail to deserialize
```

not

```rust
// This is the FTP target struct.
```

## scope of the project

capscr is a screen-capture tool with HDR support. it does *not* aim to be:

- a video editor
- a streaming tool
- a screen-share / RTC app
- a general image annotator (the editor is for capture markup, not full bitmap edits)

if your contribution starts looking like one of those, file an issue first so we can talk scope.

## getting help

- bugs / feature requests: open an issue with the templates in [`.github/ISSUE_TEMPLATE/`](.github/ISSUE_TEMPLATE/).
- security: see [`SECURITY.md`](SECURITY.md) if present, otherwise email the maintainer listed in `Cargo.toml`. don't file public issues for exploitable bugs.

## licensing

by submitting a PR you agree your contribution is licensed under the same MIT terms as the rest of the repo. see [`LICENSE`](LICENSE).
