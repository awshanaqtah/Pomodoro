# Builds a signed installer and publishes it twice: to this private repo's releases, and to the
# public awshanaqtah/Pomodoro-releases repo together with latest.json, the feed installed copies
# check for updates.
#
# No secrets live in this repo. The signing key and its password are read from
# %USERPROFILE%\.tauri; the password file is DPAPI-encrypted, so only this Windows user on this
# PC can unlock it.
#
# Usage:  powershell -File scripts\release.ps1 -Notes "What changed"

param([Parameter(Mandatory = $true)][string]$Notes)

$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$version = (Get-Content "$root\src-tauri\tauri.conf.json" -Raw | ConvertFrom-Json).version
$keyDir = Join-Path $env:USERPROFILE ".tauri"

$env:TAURI_SIGNING_PRIVATE_KEY = (Get-Content (Join-Path $keyDir "pomodoro-updater.key") -Raw).Trim()
$secure = Get-Content (Join-Path $keyDir "pomodoro-updater.password") -Raw | ConvertTo-SecureString
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = [System.Net.NetworkCredential]::new("", $secure).Password
try {
    Push-Location $root
    npm run tauri build
    if ($LASTEXITCODE) { throw "build failed" }
} finally {
    Pop-Location
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY, Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
}

$bundle = "$root\src-tauri\target\release\bundle\nsis"
$setup = "$bundle\Pomodoro_${version}_x64-setup.exe"
if (-not (Test-Path "$setup.sig")) { throw "no signature next to $setup" }
$name = Split-Path $setup -Leaf

$feed = [ordered]@{
    version   = $version
    notes     = $Notes
    pub_date  = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    platforms = @{
        "windows-x86_64" = @{
            signature = (Get-Content "$setup.sig" -Raw).Trim()
            url       = "https://github.com/awshanaqtah/Pomodoro-releases/releases/download/v$version/$name"
        }
    }
}
$feedPath = "$bundle\latest.json"
# UTF-8 without a BOM: the updater parses this as plain JSON.
[System.IO.File]::WriteAllText($feedPath, ($feed | ConvertTo-Json -Depth 5), (New-Object System.Text.UTF8Encoding $false))

$unsigned = "Windows may warn that the app is unsigned: More info, then Run anyway."
gh release create "v$version" $setup --repo awshanaqtah/Pomodoro --title "Pomodoro $version" --notes "$Notes`n`n$unsigned"
if ($LASTEXITCODE) { throw "private release failed" }
gh release create "v$version" $setup $feedPath --repo awshanaqtah/Pomodoro-releases --title "Pomodoro $version" --notes "$Notes`n`n$unsigned"
if ($LASTEXITCODE) { throw "public release failed" }
"Released $version"
