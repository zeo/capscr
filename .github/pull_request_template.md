<!-- short PRs land faster. one feature/fix per PR, please. -->

## what

<!-- one line: what changes, behaviour-side -->

## why

<!-- link the issue if one exists, e.g. `closes #42` -->

## how I tested

<!-- exact commands you ran + what you saw -->
- [ ] `cargo clippy --bin capscr --all-targets -- -D warnings` clean
- [ ] `cargo test --bin capscr` passes
- [ ] `npm --prefix frontend run build` clean
- [ ] `cargo tauri dev` — manually exercised the affected path

## screenshots (UI changes only)

<!-- drop them here -->

## checklist

- [ ] commit message starts with `<version>:` if this bumps the version
- [ ] no unrelated formatting churn
- [ ] no new prod dependencies (or: justified in the description)
- [ ] CHANGELOG / release-notes intent captured in the version-bump commit subject
