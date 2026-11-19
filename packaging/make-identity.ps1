# Builds the identity package that gives glide.exe a Windows app identity.
#
#   packaging\identity\Glide.msix   the signed identity package
#   packaging\identity\Glide.cer    the public certificate that recipients trust
#
# The signing certificate ("CN=Glide Dev") is created once in your personal
# certificate store. Its private key never leaves that store and is never
# written to this folder: anyone holding it could sign packages that PCs
# trusting Glide.cer would accept, so keep it to yourself.

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$identity = Join-Path $here 'identity'
$assets = Join-Path $identity 'Assets'
$subject = 'CN=Glide Dev'

$kit = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Directory |
    Where-Object { Test-Path (Join-Path $_.FullName 'x64\makeappx.exe') } |
    Sort-Object Name -Descending | Select-Object -First 1
if (-not $kit) { throw 'The Windows SDK (makeappx.exe, signtool.exe) is not installed.' }
$makeappx = Join-Path $kit.FullName 'x64\makeappx.exe'
$signtool = Join-Path $kit.FullName 'x64\signtool.exe'

# Logos: a plain dark rounded tile with the island's lip. Windows requires the files.
Add-Type -AssemblyName System.Drawing
New-Item -ItemType Directory -Force $assets | Out-Null
foreach ($logo in @(@{ Name = 'Square150x150Logo.png'; Size = 150 }, @{ Name = 'Square44x44Logo.png'; Size = 44 }, @{ Name = 'StoreLogo.png'; Size = 50 })) {
    $size = $logo.Size
    $bitmap = New-Object System.Drawing.Bitmap $size, $size
    $g = [System.Drawing.Graphics]::FromImage($bitmap)
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::FromArgb(255, 12, 12, 14))
    $pill = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 74, 168, 255))
    $w = [int]($size * 0.56); $h = [int]($size * 0.2)
    $g.FillRectangle($pill, [int](($size - $w) / 2), 0, $w, $h)
    $g.FillEllipse($pill, [int](($size - $w) / 2), 0, $h * 2, $h * 2)
    $g.FillEllipse($pill, [int](($size + $w) / 2) - $h * 2, 0, $h * 2, $h * 2)
    $g.FillRectangle($pill, [int](($size - $w) / 2) + $h, $h, $w - 2 * $h, $h)
    $g.Dispose()
    $bitmap.Save((Join-Path $assets $logo.Name), [System.Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose()
}

# The signing certificate, made once.
$cert = Get-ChildItem Cert:\CurrentUser\My | Where-Object { $_.Subject -eq $subject -and $_.HasPrivateKey } | Select-Object -First 1
if (-not $cert) {
    $cert = New-SelfSignedCertificate -Type Custom -Subject $subject -KeyUsage DigitalSignature `
        -FriendlyName 'Glide package signing' -CertStoreLocation Cert:\CurrentUser\My -NotAfter (Get-Date).AddYears(5) `
        -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
}
Export-Certificate -Cert $cert -FilePath (Join-Path $identity 'Glide.cer') | Out-Null

# Pack only the manifest and logos (no program: /nv skips the check for one), then sign.
$package = Join-Path $identity 'Glide.msix'
$staging = Join-Path $env:TEMP 'glide-identity'
Remove-Item $staging -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $staging | Out-Null
Copy-Item (Join-Path $identity 'AppxManifest.xml') $staging
Copy-Item $assets $staging -Recurse
& $makeappx pack /o /nv /d $staging /p $package | Select-Object -Last 1
if ($LASTEXITCODE -ne 0) { throw 'makeappx failed.' }
& $signtool sign /fd SHA256 /sha1 $cert.Thumbprint $package | Select-Object -Last 1
if ($LASTEXITCODE -ne 0) { throw 'signtool failed.' }
Remove-Item $staging -Recurse -Force
"Identity package: $package"
"Certificate:      $(Join-Path $identity 'Glide.cer')  (thumbprint $($cert.Thumbprint))"
