<#
.SYNOPSIS
  Authenticode-signs files with signtool, when a code-signing certificate is configured.

.DESCRIPTION
  The certificate comes from environment variables, never from the repository:

    ZEPHIX_SIGN_PFX           Path to a .pfx file, with ZEPHIX_SIGN_PFX_PASSWORD
    ZEPHIX_SIGN_THUMBPRINT    SHA-1 thumbprint of a certificate in the CurrentUser\My store
                           (or a hardware token that exposes it there)
    ZEPHIX_SIGN_TIMESTAMP     RFC 3161 timestamp server (default http://timestamp.digicert.com)
    SIGNTOOL               Full path to signtool.exe (default: newest Windows SDK)

  Without ZEPHIX_SIGN_PFX or ZEPHIX_SIGN_THUMBPRINT the files are left unsigned with a warning, so
  development and CI builds keep working before a certificate exists. With -Require, missing
  configuration is an error instead.

.EXAMPLE
  .\scripts\sign.ps1 dist\stage\zephix.exe
  .\scripts\sign.ps1 -Require dist\Zephix-0.2.0-x64.msi
#>
param(
    [Parameter(Mandatory, ValueFromRemainingArguments)][string[]]$Path,
    [switch]$Require
)
$ErrorActionPreference = 'Stop'

if (-not $env:ZEPHIX_SIGN_PFX -and -not $env:ZEPHIX_SIGN_THUMBPRINT) {
    if ($Require) { throw 'No code-signing certificate configured (ZEPHIX_SIGN_PFX or ZEPHIX_SIGN_THUMBPRINT).' }
    Write-Warning "Not signed (no ZEPHIX_SIGN_PFX / ZEPHIX_SIGN_THUMBPRINT): $($Path -join ', ')"
    return
}

$signtool = $env:SIGNTOOL
if (-not $signtool) {
    $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
        Sort-Object { [version]$_.Directory.Parent.Name } -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}
if (-not $signtool -or -not (Test-Path $signtool)) { throw 'signtool.exe not found; install the Windows SDK or set SIGNTOOL.' }

$timestamp = if ($env:ZEPHIX_SIGN_TIMESTAMP) { $env:ZEPHIX_SIGN_TIMESTAMP } else { 'http://timestamp.digicert.com' }
$certArgs = if ($env:ZEPHIX_SIGN_PFX) {
    if (-not (Test-Path $env:ZEPHIX_SIGN_PFX)) { throw "ZEPHIX_SIGN_PFX not found: $env:ZEPHIX_SIGN_PFX" }
    @('/f', $env:ZEPHIX_SIGN_PFX, '/p', $env:ZEPHIX_SIGN_PFX_PASSWORD)
} else {
    @('/sha1', $env:ZEPHIX_SIGN_THUMBPRINT)
}

foreach ($file in $Path) {
    & $signtool sign @certArgs /fd SHA256 /tr $timestamp /td SHA256 /d 'Zephix - Database Management Studio' $file
    if ($LASTEXITCODE -ne 0) { throw "signtool sign failed for $file" }
    & $signtool verify /pa /q $file
    if ($LASTEXITCODE -ne 0) { throw "Signature check failed for $file" }
    Write-Host "Signed: $file"
}
