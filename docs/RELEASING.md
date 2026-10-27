# Releasing Prometheus S3Manager

Releases are GitHub Releases with three files:

| File | Purpose |
| --- | --- |
| `Prometheus-S3Manager-<version>-x64.msi` | Per-user installer (no administrator rights) into `%LOCALAPPDATA%\Programs\Prometheus S3Manager`, Start menu entry, uninstall via Settings > Apps. Installed copies update themselves with it |
| `Prometheus-S3Manager-<version>-x64.exe` | Portable exe; runs from anywhere but does not update itself (the update dialog links to the release page) |
| `SHA256SUMS.txt` | Checksums; the updater refuses an MSI that does not match |

User data (`%APPDATA%\Prometheus-S3Manager`) and secrets (Windows Credential Manager) are never
touched by installing, updating or uninstalling.

## Cutting a release

Releases are built locally and published by hand.

1. Bump `version` in `[workspace.package]` of `Cargo.toml` (x.y.z; MSI versions have no
   pre-release part), run `cargo update -w` so `Cargo.lock` matches, and date the version in
   `CHANGELOG.md`. Commit and push to `main`.
2. Build with the repository compiled in (without it, the build never offers updates):
   ```powershell
   $env:S3M_UPDATE_REPO = 'EnriqueMuhammad/Prometheus-S3-Manager'
   .\scripts\release.ps1
   ```
   The script runs the tests, signs when a certificate is configured and writes the three files
   to `dist\`.
3. On GitHub, **Releases > Draft a new release**: a new tag `vX.Y.Z` on `main`, a title, the
   release notes (the update dialog shows them), and the three files from `dist\` attached
   with their names unchanged (the updater looks up the MSI's checksum by its file name).
   Leave the release label at **None**: `/releases/latest` skips pre-releases, so installed
   copies would not see it. Publish.

Installed copies check `https://api.github.com/repos/<owner>/<repo>/releases/latest` at start
(at most every 20 hours, Tools > Options can turn it off) and on Help > Check for updates. The
repository must be public: the updater does not send a GitHub token.

## Building locally

```powershell
$env:S3M_UPDATE_REPO = 'owner/prometheus-s3manager'   # omit for a build without auto-update
.\scripts\release.ps1                                 # -SkipTests to skip cargo test
```

Needs the .NET SDK for WiX (`dotnet tool restore` installs the pinned WiX 5.0.2 from
`dotnet-tools.json`). Output goes to `dist\`.

## How an update is installed

1. The app downloads the MSI and `SHA256SUMS.txt` (through the proxy from Tools > Options) into
   `%TEMP%\Prometheus-S3Manager-update` and compares the SHA-256.
2. When the running exe is Authenticode-signed, the MSI must be signed by the same publisher;
   unsigned builds rely on the checksum only.
3. "Install and restart" starts a hidden helper, closes the app (running transfers are paused and
   wait for Resume), runs `msiexec /i <msi> /passive` (log in `logs\update-<version>.log` of the
   data folder) and starts the new version.

For testing against something other than GitHub, set `S3MANAGER_UPDATE_API` to an API base URL
(for example GitHub Enterprise `https://host/api/v3`, or a local fake serving
`/repos/<owner>/<repo>/releases/latest`).

## Code signing

Not configured yet; releases are unsigned and Windows SmartScreen warns on first start.
`scripts\sign.ps1` signs the exe and the MSI (SHA-256, RFC 3161 timestamp) as soon as one of these
is set: `S3M_SIGN_PFX` + `S3M_SIGN_PFX_PASSWORD`, or `S3M_SIGN_THUMBPRINT` for a certificate in
`CurrentUser\My` (also hardware tokens).

Optional: `S3M_SIGN_TIMESTAMP` (default `http://timestamp.digicert.com`), `SIGNTOOL`.
Once releases are signed, every later update must be signed by the same certificate subject, or
installed copies refuse it.

## CI

`.github/workflows/ci.yml` runs on pushes to `main` and on pull requests (Windows runner): format,
clippy with warnings as errors, unit and wire tests, live tests against SeaweedFS's Windows build
(same settings and test keys as `dev/docker-compose.yml`), and an unsigned MSI build uploaded as
an artifact.
