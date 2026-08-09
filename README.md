# CoverDrop

CoverDrop is a local desktop app that turns the first page of one or more PDF files into website-ready cover images.

![CoverDrop desktop app showing a selected PDF, cover preview, and export settings](docs/coverdrop-screenshot.png)

## Features

- Choose individual PDF files or process folders recursively.
- Preview first pages before export.
- Export JPEG, PNG, lossless WebP, or GIF.
- Resize with pixels or physical units.
- Fit or crop to exact dimensions with nine anchor positions.
- Preserve input subfolders and safely number duplicate filenames.
- Process files locally without intentionally uploading PDF contents.
- Use light and dark themes.

## Supported platforms

The application source supports macOS and Windows. Release packaging requires a complete, native Poppler renderer bundle for the target platform. Renderer binaries are deliberately not committed to this source repository.

## Development

Install Node.js, Rust, and Poppler so that `pdftoppm` is available on your command line.

```sh
npm ci
npm run tauri dev
```

Run the checks with:

```sh
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
```

## Production packaging

CoverDrop ships Poppler as a platform-specific renderer. Release builds refuse to continue when the matching renderer is missing, preventing an installer that cannot generate covers.

Expected renderer locations:

- Apple Silicon macOS: `src-tauri/binaries/macos-aarch64/pdftoppm`
- Intel macOS: `src-tauri/binaries/macos-x86_64/pdftoppm`
- 64-bit Windows: `src-tauri/binaries/windows-x86_64/bin/pdftoppm.exe` plus its required DLLs

Build an Apple Silicon macOS disk image with:

```sh
npm run tauri build -- --bundles dmg
```

Build a 64-bit Windows NSIS installer on Windows with:

```powershell
npm run tauri build -- --bundles nsis --target x86_64-pc-windows-msvc
```

Public macOS releases should be signed and notarized with an Apple Developer ID. Public Windows releases should be Authenticode-signed. Before distributing an installer, document and satisfy the source and notice obligations for Poppler and every library in the renderer bundle; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Privacy

CoverDrop operates on local files. It does not intentionally transmit PDF contents or generated images over the network. Source PDFs are never modified.

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request. Report security concerns according to [SECURITY.md](SECURITY.md).

## License

CoverDrop is free software licensed under the [GNU General Public License v3.0 or later](LICENSE).

Copyright © 2026 TaiDev.
