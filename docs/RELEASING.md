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
`Cargo.lock` matches (release builds use `--locked`).

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
