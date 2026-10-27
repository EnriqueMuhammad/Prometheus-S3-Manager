<#
.SYNOPSIS
  Builds a release of Prometheus S3Manager: tests, release exe, signing, per-user MSI, checksums.

.DESCRIPTION
  Output in dist\ (ignored by git):
    Prometheus-S3Manager-<version>-x64.msi   installer, used by the in-app updater
    Prometheus-S3Manager-<version>-x64.exe   portable exe (no install, no self-update)
    SHA256SUMS.txt                           checksums the updater verifies

  S3M_UPDATE_REPO (owner/name on GitHub) is compiled in so the app can check that repository's
  releases; without it the build has no auto-update. CI sets it to the repository being built.
  Signing uses scripts\sign.ps1 (see there for the environment variables); unsigned otherwise.

.EXAMPLE
  $env:S3M_UPDATE_REPO = 'owner/prometheus-s3manager'; .\scripts\release.ps1
  .\scripts\release.ps1 -SkipTests -Version x.y.z
#>
param(
    [string]$Version,
    [switch]$SkipTests,
    [string]$Out = (Join-Path $PSScriptRoot '..\dist')
)
$ErrorActionPreference = 'Stop'

# Windows Installer shows the license as RTF: convert LICENSE.md (headings bold, paragraphs
# joined, non-ASCII as \u escapes).
function ConvertTo-LicenseRtf([string]$Markdown) {
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.Append('{\rtf1\ansi\deff0{\fonttbl{\f0 Segoe UI;}}\fs17 ')
    foreach ($para in (($Markdown -replace "`r", '') -split "`n\s*`n")) {
        $text = $para.Trim()
        if (-not $text) { continue }
        $bold = $text -match '^#+\s'
        $text = ($text -replace '^#+\s*', '') -replace '\s*\n\s*', ' '
        # Markdown links, emphasis and <url> brackets read as plain text.
        $text = (($text -replace '\[([^\]]+)\]\([^)]*\)', '$1') -replace '\*', '') -replace '<(https?://[^>]+)>', '$1'
        $text = $text.Replace('`', '').Replace('\', '\\').Replace('{', '\{').Replace('}', '\}')
        $escaped = New-Object System.Text.StringBuilder
        foreach ($ch in $text.ToCharArray()) {
            if ([int]$ch -gt 127) { [void]$escaped.Append('\u' + [int]$ch + '?') } else { [void]$escaped.Append($ch) }
        }
        if ($bold) { [void]$sb.Append('\b ' + $escaped + '\b0\par\par ') } else { [void]$sb.Append($escaped.ToString() + '\par\par ') }
    }
    [void]$sb.Append('}')
    $sb.ToString()
}

$root = Resolve-Path (Join-Path $PSScriptRoot '..')
Push-Location $root
try {
    $meta = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
    $crateVersion = ($meta.packages | Where-Object name -eq 'prometheus-s3manager').version
    if (-not $Version) { $Version = $crateVersion }
    if ($Version -ne $crateVersion) {
        throw "Version $Version does not match Cargo.toml ($crateVersion). Bump [workspace.package] version first."
    }
    if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "MSI versions must be x.y.z (got $Version)." }

    if (-not $env:S3M_UPDATE_REPO) {
        Write-Warning 'S3M_UPDATE_REPO is not set: this build will not check for updates.'
    }

    if (-not $SkipTests) {
        cargo test --workspace --locked
        if ($LASTEXITCODE -ne 0) { throw 'Tests failed; no release built.' }
    }
    cargo build --release --locked -p prometheus-s3manager
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed. After a version bump run "cargo update -w" so Cargo.lock matches.' }

    $Out = [System.IO.Path]::GetFullPath($Out)
    $stage = Join-Path $Out 'stage'
    New-Item -ItemType Directory -Force $stage | Out-Null
    Get-ChildItem $Out -File | Remove-Item -Force
    $exe = Join-Path $stage 's3manager.exe'
    Copy-Item 'target\release\s3manager.exe' $exe -Force
    & (Join-Path $PSScriptRoot 'sign.ps1') $exe

    $base = "Prometheus-S3Manager-$Version-x64"
    $msi = Join-Path $Out "$base.msi"
    dotnet tool restore | Out-Null
    dotnet tool run wix extension add -g WixToolset.UI.wixext/5.0.2 | Out-Null
    $rtf = Join-Path $stage 'License.rtf'
    [System.IO.File]::WriteAllText($rtf, (ConvertTo-LicenseRtf (Get-Content -Raw 'LICENSE.md')))
    dotnet tool run wix build installer\Prometheus.wxs -arch x64 -ext WixToolset.UI.wixext `
        -d "LicenseRtf=$rtf" `
        -d "Version=$Version" -d "ExePath=$exe" -d "IconPath=apps\prometheus\assets\app.ico" `
        -o $msi
    if ($LASTEXITCODE -ne 0) { throw 'MSI build failed.' }
    Remove-Item (Join-Path $Out "$base.wixpdb") -ErrorAction SilentlyContinue
    & (Join-Path $PSScriptRoot 'sign.ps1') $msi

    Copy-Item $exe (Join-Path $Out "$base.exe") -Force
    $lines = Get-ChildItem -Path (Join-Path $Out '*') -File -Include *.msi, *.exe |
        Sort-Object Name |
        ForEach-Object { '{0}  {1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.Name }
    # LF line endings, no BOM: the format sha256sum and the updater read.
    [System.IO.File]::WriteAllText((Join-Path $Out 'SHA256SUMS.txt'), (($lines -join "`n") + "`n"))
    Remove-Item $stage -Recurse -Force

    Write-Host "Release $Version in $Out"
    Get-ChildItem $Out -File | ForEach-Object { Write-Host ('  {0,-45} {1,8:N1} MB' -f $_.Name, ($_.Length / 1MB)) }
} finally {
    Pop-Location
}
