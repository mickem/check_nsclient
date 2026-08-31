<#
.SYNOPSIS
Build the NSClient++ image, start it, run the integration suite against it and
tear it down again (Docker Desktop with Linux containers).

.EXAMPLE
tests\integration\run.ps1
$env:NSCP_VERSION = "0.17.0"; tests\integration\run.ps1
tests\integration\run.ps1 -- --nocapture     # extra args go to `cargo test`
#>
[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CargoArgs = @()
)
$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$version = if ($env:NSCP_VERSION) { $env:NSCP_VERSION } else { (Get-Content (Join-Path $root ".nscp_version") -Raw).Trim() }
$arch = if ($env:NSCP_ARCH) { $env:NSCP_ARCH } else { "amd64" }
$password = if ($env:NSCP_PASSWORD) { $env:NSCP_PASSWORD } else { "it-password" }
$port = if ($env:NSCP_PORT) { $env:NSCP_PORT } else { "8443" }
$image = "check_nsclient-it:$version-$arch"
$container = "check_nsclient-it-$PID"

Write-Host "==> Building $image"
docker build --build-arg "NSCP_VERSION=$version" --build-arg "NSCP_ARCH=$arch" --build-arg "NSCP_PASSWORD=$password" -t $image (Join-Path $root "tests\integration")
if ($LASTEXITCODE -ne 0) { throw "docker build failed" }

try {
    Write-Host "==> Starting $container"
    docker run -d --name $container -p "${port}:8443" $image | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "docker run failed" }

    Write-Host "==> Waiting for https://127.0.0.1:$port"
    $ready = $false
    for ($i = 0; $i -lt 60 -and -not $ready; $i++) {
        try {
            Invoke-WebRequest -Uri "https://127.0.0.1:$port/api/v2/info" -SkipCertificateCheck -TimeoutSec 2 | Out-Null
            $ready = $true
        } catch {
            if ($_.Exception.Response -and [int]$_.Exception.Response.StatusCode -in 401, 403) { $ready = $true }
            else { Start-Sleep -Seconds 1 }
        }
    }
    if (-not $ready) { throw "NSClient++ did not become ready on port $port" }

    Write-Host "==> Running integration tests against NSClient++ $version"
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
    Write-Host "==> Stopping $container"
    docker logs --tail 50 $container 2>$null
    docker rm -f $container 2>$null | Out-Null
}
