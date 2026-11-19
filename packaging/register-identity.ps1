# Gives an installed Glide its Windows identity, so notifications arrive at once.
#   1. trusts Glide.cer for this PC (one administrator prompt, skipped if already trusted)
#   2. registers Glide.msix for the current user, pointing at the folder with glide.exe
# Every step is written to %APPDATA%\Glide\identity.log.
#
#   -App <folder>   where glide.exe is (default: the folder above this script)
#   -NoLaunch       do not stop and restart Glide (the installer uses this)
#
# To undo:  Get-AppxPackage Glide.Island | Remove-AppxPackage
#           certutil -delstore TrustedPeople "Glide Dev"   (as administrator)
param([string]$App = (Split-Path -Parent $PSScriptRoot), [switch]$NoLaunch)
$ErrorActionPreference = 'Stop'
$logDir = Join-Path $env:APPDATA 'Glide'
New-Item -ItemType Directory -Force $logDir | Out-Null
$log = Join-Path $logDir 'identity.log'
function Note($text) { "$(Get-Date -Format s)  $text" | Add-Content -Path $log -Encoding utf8; $text }

try {
    # The package sits beside this script once installed, or in "identity" in the source tree.
    $from = if (Test-Path (Join-Path $PSScriptRoot 'Glide.msix')) { $PSScriptRoot } else { Join-Path $PSScriptRoot 'identity' }
    $cer = Join-Path $from 'Glide.cer'
    $msix = Join-Path $from 'Glide.msix'
    $App = (Resolve-Path $App).Path
    if (-not (Test-Path $msix)) { throw "Glide.msix was not found next to this script or in its identity folder." }
    if (-not (Test-Path (Join-Path $App 'glide.exe'))) { throw "glide.exe was not found in $App" }
    Note "Registering Glide in $App"

    $thumbprint = (New-Object System.Security.Cryptography.X509Certificates.X509Certificate2 $cer).Thumbprint
    $trusted = { [bool](Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object { $_.Thumbprint -eq $thumbprint }) }
    if (-not (& $trusted)) {
        Note 'Asking for administrator approval to trust the certificate'
        try {
            Start-Process certutil.exe -ArgumentList '-addstore', 'TrustedPeople', "`"$cer`"" -Verb RunAs -Wait -WindowStyle Hidden
        } catch {
            throw 'Administrator approval was declined, so the certificate is not trusted.'
        }
        if (-not (& $trusted)) { throw 'The certificate could not be trusted.' }
    }
    Note 'Certificate is trusted'

    if (-not $NoLaunch) { Get-Process glide -ErrorAction SilentlyContinue | Stop-Process -Force }
    # One identity per user: an earlier registration (another folder, or the same
    # version) must go first, or Windows refuses the new one.
    Get-AppxPackage Glide.Island | ForEach-Object {
        Note "Removing earlier registration ($($_.PackageFullName))"
        Remove-AppxPackage $_.PackageFullName
    }
    Add-AppxPackage -Path $msix -ExternalLocation $App
    if (-not (Get-AppxPackage Glide.Island)) { throw 'Windows did not keep the registration.' }
    Note 'Done. Notifications will arrive as they happen.'
    if (-not $NoLaunch) { Start-Process (Join-Path $App 'glide.exe') }
} catch {
    Note "FAILED: $($_.Exception.Message)"
    exit 1
}
