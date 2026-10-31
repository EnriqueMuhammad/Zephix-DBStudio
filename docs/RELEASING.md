# Building a Windows release

```powershell
.\scripts\release.ps1             # tests, release build, signing (if configured), MSI, checksums
.\scripts\release.ps1 -SkipTests
```

Output in `dist\` (ignored by git):

| File | Purpose |
| --- | --- |
| `Zephix-<version>-x64.msi` | Per-user installer: `%LOCALAPPDATA%\Programs\Zephix`, Start menu entry, uninstall via Settings > Apps, no administrator rights |
| `Zephix-<version>-x64.exe` | Portable executable |
| `SHA256SUMS.txt` | SHA-256 checksums |

Installing a newer MSI upgrades the old version in place; installing an older one over a newer
one is refused. Profiles, settings and saved passwords are never touched by installing, upgrading
or uninstalling.

Requirements: Rust (MSVC toolchain) and the .NET SDK; `dotnet tool restore` installs the pinned
WiX 5.0.2 from `dotnet-tools.json`.

## Version

Bump `version` in `[workspace.package]` of `Cargo.toml` (x.y.z), then run `cargo update -w` so
`Cargo.lock` matches (release builds use `--locked`). Date the changelog section for that version.

## Draft release from a tag

After that commit is on `main`, tag it and push the tag. This does not publish the release.

```powershell
git checkout main
git pull
git tag vX.Y.Z
git push origin vX.Y.Z
```

`X.Y.Z` is the `[workspace.package]` version (`v0.3.2` for this release).

`.github/workflows/release.yml` runs on a `v*` tag (and can be started by hand for an existing
tag). On `windows-latest` it runs `scripts\release.ps1`: tests, a release build, the per-user MSI,
the portable exe and `SHA256SUMS.txt`. There is no certificate, so the files are not signed.
The workflow then opens a **draft** GitHub release for that tag and attaches those three files.
The body is the version's section of `CHANGELOG.md`. Publish the draft from the Releases page
when it looks right. Pushing the tag again updates the draft; it does not publish it.

## Code signing

Not configured yet; Windows SmartScreen warns on first start of unsigned builds.
`scripts\sign.ps1` signs the exe and the MSI (SHA-256, RFC 3161 timestamp) as soon as one of these
environment variables is set:

| Variable | Meaning |
| --- | --- |
| `ZEPHIX_SIGN_PFX` + `ZEPHIX_SIGN_PFX_PASSWORD` | Path to a .pfx code-signing certificate and its password |
| `ZEPHIX_SIGN_THUMBPRINT` | Thumbprint of a certificate in `CurrentUser\My` (also hardware tokens) |
| `ZEPHIX_SIGN_TIMESTAMP` | Timestamp server (default `http://timestamp.digicert.com`) |
| `SIGNTOOL` | Path to `signtool.exe` (default: newest Windows SDK) |
