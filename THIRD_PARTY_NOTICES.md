# Third-Party Notices

CoverDrop uses open-source software, including Tauri, React, Rust crates, npm packages, and Poppler's `pdftoppm` command-line renderer.

Dependency names and versions for source dependencies are recorded in `package-lock.json` and `src-tauri/Cargo.lock`. Their respective licenses continue to apply.

## Poppler renderer

The source repository does not distribute platform-specific Poppler executables or dynamic libraries. Developers provide a local `pdftoppm` installation for development. Release maintainers may place a complete, platform-native renderer bundle under `src-tauri/binaries/<platform>-<architecture>/` before packaging.

Poppler is licensed under GPL version 2 or GPL version 3. Any public installer containing Poppler must include the applicable license notices and provide the exact corresponding source, build information, and notices required by the licenses of Poppler and its bundled libraries.

Do not publish a CoverDrop installer until the provenance and redistribution terms of every bundled renderer file have been documented and verified.
