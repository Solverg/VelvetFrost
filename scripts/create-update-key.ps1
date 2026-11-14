<#
.SYNOPSIS
  Create the dedicated VelvetFrost update-signing key in an interactive console.
.DESCRIPTION
  OpenSSL asks locally for the passphrase without placing it in an argument or
  environment variable. Save an encrypted offline backup separately and retain
  the passphrase somewhere secure. Never publish or share the private key.
  -ResumeExistingKey derives the public key again after an interrupted run;
  it never replaces a key or an already pinned public key.
#>
[CmdletBinding()]
param(
    [switch] $ResumeExistingKey,
    [string] $OpenSsl = 'C:\Program Files\Git\mingw64\bin\openssl.exe'
)

$ErrorActionPreference = 'Stop'
if ($PSVersionTable.PSEdition -ne 'Core') { throw 'Use PowerShell 7 for this script.' }
if (-not (Test-Path -LiteralPath $OpenSsl -PathType Leaf)) { throw 'OpenSSL was not found. Supply its executable path with -OpenSsl.' }
$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$keyDirectory = Join-Path $root '.local/VelvetFrost-update-signing'
$private = Join-Path $keyDirectory 'VelvetFrost-updater.key'
$pinned = Join-Path $root 'windows/update-public-key.hex'
if (Test-Path -LiteralPath $pinned) { throw 'A public key is already pinned. Do not replace it without a reviewed key transition.' }

if ($ResumeExistingKey) {
    if (-not (Test-Path -LiteralPath $private -PathType Leaf)) { throw 'There is no existing key to resume.' }
} else {
    if (Test-Path -LiteralPath $keyDirectory) { throw 'The key directory already exists. Review it before using -ResumeExistingKey.' }
    [void] [System.IO.Directory]::CreateDirectory($keyDirectory)
    Write-Host 'Create a strong passphrase. OpenSSL will ask for it twice; the input is hidden.'
    & $OpenSsl genpkey -algorithm ED25519 -aes-256-cbc -out $private
    if ($LASTEXITCODE -ne 0) { throw 'Key generation failed or was cancelled. The key directory was preserved for review.' }
}

$reader = [System.IO.StreamReader]::new($private)
try { $header = $reader.ReadLine() } finally { $reader.Dispose() }
if ($header -cne '-----BEGIN ENCRYPTED PRIVATE KEY-----') { throw 'The key is not encrypted PKCS#8. Stop and review the local key directory.' }
$id = [guid]::NewGuid().ToString('N')
$der = Join-Path $keyDirectory "public-$id.der"
$public = Join-Path $keyDirectory "public-$id.pem"
Write-Host 'Enter the same passphrase once more to derive the public key.'
& $OpenSsl pkey -in $private -pubout -outform DER -out $der
if ($LASTEXITCODE -ne 0) { throw 'Public key extraction failed. The encrypted key was preserved; use -ResumeExistingKey to retry.' }
$bytes = [System.IO.File]::ReadAllBytes($der)
if ($bytes.Length -ne 44 -or [Convert]::ToHexString($bytes[0..11]).ToLowerInvariant() -cne '302a300506032b6570032100') {
    throw 'The derived public key is not Ed25519. The encrypted key was preserved for review.'
}
& $OpenSsl pkey -pubin -inform DER -in $der -out $public
if ($LASTEXITCODE -ne 0) { throw 'Public key export failed. The encrypted key was preserved; use -ResumeExistingKey to retry.' }
$hex = [Convert]::ToHexString($bytes[12..43]).ToLowerInvariant() + "`n"
$stream = [System.IO.File]::Open($pinned, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
try {
    $writer = [System.IO.StreamWriter]::new($stream, [System.Text.UTF8Encoding]::new($false))
    try { $writer.Write($hex) } finally { $writer.Dispose() }
} finally { $stream.Dispose() }
Write-Host 'The encrypted private key and public key were created.'
Write-Host "Private key directory: $keyDirectory"
Write-Host "Public PEM: $public"
Write-Host 'Make and verify an encrypted offline backup, and save the passphrase separately.'
