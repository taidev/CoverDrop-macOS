# Contributing to CoverDrop

Thanks for helping improve CoverDrop.

## Development setup

You need Node.js, Rust, and a working `pdftoppm` command from Poppler.

```sh
npm ci
npm run tauri dev
```

Before submitting a change, run:

```sh
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
```

## Pull requests

- Keep each pull request focused on one change.
- Explain the user-facing impact and how you tested it.
- Add or update tests when behavior changes.
- Do not commit generated builds, signing credentials, PDFs, or platform renderer binaries.
- Confirm that new dependencies have licenses compatible with GPL-3.0-or-later.

By contributing, you agree that your contribution is licensed under the project's GPL-3.0-or-later license.
