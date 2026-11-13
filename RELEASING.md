# Preparing a Windows release

VelvetFrost is a Windows-only fork of [Snowhush](https://github.com/mbezruchko/Snowhush). The fork repository is `Solverg/VelvetFrost`; verify the destination before every push and never push to upstream Snowhush. Building locally does not publish anything.

## Before the first release

- Verify the destination of every push and release. The new repository has no imported history or upstream tags. Review the files and attribution before publication.
- Choose the release version, repository description and release notes. `windows/Cargo.toml` is the Windows version source, and its lockfile must match. The temporary migration installer was distributed as 0.17.0; use a later version for the next build.
- Review [LICENSE](LICENSE) and [THIRD-PARTY-NOTICES.txt](THIRD-PARTY-NOTICES.txt), including the exact upstream `Required Notice`. The installer embeds and displays the license and notices offline and installs them beside the application. Verify each release artifact and installation, including silent installation, before publishing. Supply all notices alongside any separately distributed app binary or source snapshot.
- Review the locked dependency graph and refresh `THIRD-PARTY-NOTICES.txt` when it changes. The Rust standard-library notice snapshot in `notices/RUST-STANDARD-LIBRARY-NOTICES.html` comes from Rust 1.99.0 (b940084d7 2026-09-28); review and replace it with `share/doc/rust/COPYRIGHT-library.html` from the actual release compiler when changing toolchains. Release builds check that the files match; install the Rust documentation component if needed. Review its component licenses and verify the installed and displayed notices offline.
- Record the exact release compiler, MSVC and Windows SDK versions, and verify the actual linked CRT components and applicable Microsoft redistribution terms. The static CRT setting does not by itself establish the complete EXE contents or settle these terms. Audit inherited artwork separately when there is a specific reason.
- The installer identifies the publisher as VelvetFrost; upstream copyright remains in the executable and [LICENSE](LICENSE). `.github/FUNDING.yml` points to the upstream author's GitHub Sponsors account; README and the app distinguish this from VelvetFrost's Telegram link.
- Test the independent installation and coexistence behavior below before distribution.
- The default update source is `Solverg/VelvetFrost` GitHub Releases. The repository and a suitable release must be publicly accessible to the unauthenticated update check; test this after publication is authorized. `VELVETFROST_UPDATE_REPOSITORY` can override the source at build time; never point it to upstream Snowhush releases.
- Test installation, launch, update and uninstall in an isolated Windows account or virtual machine. Check cancel, silent uninstall, and uninstall with and without the data-deletion checkbox, including reinstall and collection restoration. The installer is unsigned; check the resulting SmartScreen experience. Decide whether to sign it.
- Check the release artifact, version, included notices and release notes before publishing. Do not use the development-only `*-test-20s` or `inventory-preview` features for a release.

## Build on Windows

Use the Rust MSVC toolchain and Microsoft C++ build tools. Set the intended version in `windows/Cargo.toml` and update its lockfile if needed. Start at the repository root and run Windows Cargo commands from `windows/` so Cargo loads `windows/.cargo/config.toml`:

```powershell
cargo test --locked --manifest-path core/Cargo.toml
Push-Location windows
cargo update --workspace --offline
cargo test --locked
```

Still in `windows/`, build the app first so the installer embeds that exact binary:

```powershell
cargo build --locked --release --bin VelvetFrost
$env:SNOWHUSH_APP = (Resolve-Path target/release/VelvetFrost.exe).Path
cargo build --locked --release --features setup --bin VelvetFrost-Setup
Remove-Item Env:SNOWHUSH_APP
Pop-Location
```

The outputs are `windows/target/release/VelvetFrost.exe` and `windows/target/release/VelvetFrost-Setup.exe`. The setup executable carries the app and license and installs per user without administrator rights. To view the license without installing, use its license link or run `VelvetFrost-Setup.exe --license`. Building it does not install it, create a tag, push or publish. If setting `VELVETFROST_UPDATE_REPOSITORY`, set it before building the app and rebuild when changing it.

The installer viewer also displays third-party notices, including a text rendering of the Rust notice document. The installed folder contains `LICENSE.txt`, `THIRD-PARTY-NOTICES.txt` and the original `RUST-STANDARD-LIBRARY-NOTICES.html`. Check the actual installer and its embedded documents independently, including a standalone download: the packaging script does not verify that an existing installer was built from these sources.

After building and verifying the intended installer, an additional ZIP can be prepared with:

```powershell
./scripts/package-installer.ps1 -Installer windows/target/release/VelvetFrost-Setup.exe -OutputZip dist/VelvetFrost-windows.zip
```

The script creates a new ZIP without overwriting an existing file; it does not build or publish. Keep `VelvetFrost-Setup.exe` as a separate release asset for the update checker. If distributing `VelvetFrost.exe` separately, accompany it with the same three notice files. Inspect every archive and source snapshot, including any automatically generated by the hosting service. `site/engine.wasm` is not part of the current product tree.

## Installation and coexistence

VelvetFrost stores settings and the collection in `HKCU\Software\VelvetFrost` and installs `VelvetFrost\VelvetFrost.exe` in the per-user Programs folder, with its own uninstall and startup entries. Updates preserve existing VelvetFrost settings. Uninstall removes its registration, startup entry and log, but keeps settings and the collection unless the user selects the optional deletion checkbox. Silent uninstall always keeps them.

The two apps retain the shared `Local\Snowhush` mutex so they cannot run together. The installer acquires it while installing and reports that the app must be closed if it is held by another process.

## Automated checks

The GitHub Actions workflow checks formatting and core tests, then builds and tests the Windows app. GPU, interactive-desktop and registry-writing tests are ignored by default; run relevant ones separately in a suitable isolated environment. See [Windows architecture](docs/windows-architecture.md) for test and preview details.
