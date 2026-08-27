## Code documentation

- Add SUCCINCT docblocks to structs and methods.
- Give an added focus to Domain definitions.
- Do NOT document things that are painfully obvious. For example, a `Rect { w: int64, h: int64 }` has no need for documentation.
- If an implication is particularly complicated, document implementation sections.

## Verifying (MUST BE RUN BEFORE CONSIDERING A TASK COMPLETE)

- `just verify` — runs all three checks below:
    - `cargo fmt --all -- --check`
    - `cargo test --all-features`
    - `cargo clippy --all-targets --all`
- `just build` stages the release binary for the linked plugin (`scripts/build.sh` stays the
  Herdr install entrypoint so plugin consumers never need just).
- Any live, end-to-end testing can be done with the `herdr` CLI and `/herdr` agent skill.
