<#
.SYNOPSIS
Run the integration suite against a native Windows NSClient++ taken from the
official release zip (no Docker involved).

Downloads NSCP-<version>-<platform>.zip from the nscp GitHub release, extracts
it, enables the REST API the same way tests/integration/Dockerfile does, starts
`nscp test` in the background, runs `cargo test --test integration` against it
and stops the server again.

.EXAMPLE
tests\integration\run-windows.ps1
$env:NSCP_VERSION = "0.17.0"; tests\integration\run-windows.ps1
tests\integration\run-windows.ps1 -- --nocapture     # extra args go to `cargo test`

Environment (all optional):
  NSCP_VERSION   release to test (default: .nscp_version)
  NSCP_PLATFORM  x64 (default) or Win32
  NSCP_PASSWORD  REST password (default: it-password)
  NSCP_PORT      port for the web server (default: 8443)
  NSCP_DIR       where to extract (default: <repo>\target\nscp-<version>-<platform>)
#>
[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CargoArgs = @()
)
$ErrorActionPreference = "Stop"

$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$version = if ($env:NSCP_VERSION) { $env:NSCP_VERSION } else { (Get-Content (Join-Path $root ".nscp_version") -Raw).Trim() }
$platform = if ($env:NSCP_PLATFORM) { $env:NSCP_PLATFORM } else { "x64" }
$password = if ($env:NSCP_PASSWORD) { $env:NSCP_PASSWORD } else { "it-password" }
$port = if ($env:NSCP_PORT) { $env:NSCP_PORT } else { "8443" }
$dir = if ($env:NSCP_DIR) { $env:NSCP_DIR } else { Join-Path $root "target\nscp-$version-$platform" }
$zipName = "NSCP-$version-$platform.zip"
$zip = Join-Path (Split-Path $dir -Parent) $zipName
$nscp = Join-Path $dir "nscp.exe"

New-Item -ItemType Directory -Force (Split-Path $dir -Parent) | Out-Null
if (-not (Test-Path $zip)) {
    $url = "https://github.com/mickem/nscp/releases/download/$version/$zipName"
    Write-Host "==> Downloading $url"
    Invoke-WebRequest -Uri $url -OutFile $zip
}

# Always start from a clean extraction so leftover settings from a previous run
# (a different port or password) cannot leak into this one.
if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
Write-Host "==> Extracting $zipName to $dir"
Expand-Archive -Path $zip -DestinationPath $dir

Write-Host "==> Configuring NSClient++ $version on port $port"
Push-Location $dir
try {
    & $nscp settings --load-all --list --add-defaults | Out-Null
    & $nscp settings --activate-module CheckHelpers CheckSystem CheckDisk CheckExternalScripts LUAScript
    if ($LASTEXITCODE -ne 0) { throw "nscp settings --activate-module failed" }
    & $nscp web install --https --allowed-hosts "127.0.0.1,::1" --password $password --port $port
    if ($LASTEXITCODE -ne 0) { throw "nscp web install failed" }
} finally {
    Pop-Location
}

Write-Host "==> Starting nscp test"
$log = Join-Path $dir "nscp-test.log"
$server = Start-Process -FilePath $nscp -ArgumentList "test" -WorkingDirectory $dir `
    -RedirectStandardOutput $log -RedirectStandardError (Join-Path $dir "nscp-test.err") `
    -PassThru -WindowStyle Hidden
try {
    Write-Host "==> Waiting for https://127.0.0.1:$port"
    $ready = $false
    for ($i = 0; $i -lt 60 -and -not $ready; $i++) {
        if ($server.HasExited) { throw "nscp test exited early with $($server.ExitCode); see $log" }
        try {
            Invoke-WebRequest -Uri "https://127.0.0.1:$port/api/v2/info" -SkipCertificateCheck -TimeoutSec 2 | Out-Null
            $ready = $true
        } catch {
            if ($_.Exception.Response -and [int]$_.Exception.Response.StatusCode -in 401, 403) { $ready = $true }
            else { Start-Sleep -Seconds 1 }
        }
    }
    if (-not $ready) { throw "NSClient++ did not become ready on port $port; see $log" }

    Write-Host "==> Running integration tests against NSClient++ $version ($platform)"
    $env:CHECK_NSCLIENT_IT_URL = "https://127.0.0.1:$port"
    $env:CHECK_NSCLIENT_IT_PASSWORD = $password
    $env:CHECK_NSCLIENT_IT_USERNAME = if ($env:NSCP_USERNAME) { $env:NSCP_USERNAME } else { "admin" }
    Push-Location $root
    try {
        cargo test --test integration -- @CargoArgs
        if ($LASTEXITCODE -ne 0) { throw "integration tests failed" }
    } finally {
        Pop-Location
    }
} finally {
    Write-Host "==> Stopping nscp test"
    if (-not $server.HasExited) { Stop-Process -Id $server.Id -Force }
    if (Test-Path $log) { Get-Content $log -Tail 20 }
}
