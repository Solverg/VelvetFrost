param(
    [Parameter(Mandatory = $true)] [string] $Installer,
    [Parameter(Mandatory = $true)] [string] $OutputZip
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$installerPath = (Resolve-Path -LiteralPath $Installer).Path
if ((Split-Path -Leaf $installerPath) -ne 'VelvetFrost-Setup.exe') {
    throw 'The installer must be named VelvetFrost-Setup.exe.'
}
$zipPath = [IO.Path]::GetFullPath($OutputZip)
if ([IO.Path]::GetExtension($zipPath) -ne '.zip' -or (Test-Path -LiteralPath $zipPath)) {
    throw 'Output must be a new .zip file; existing files are not overwritten.'
}
$sysroot = (& rustc --print sysroot).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Unable to locate the Rust toolchain.' }
$rustNotices = Join-Path $root 'notices/RUST-STANDARD-LIBRARY-NOTICES.html'
$toolchainNotices = Join-Path $sysroot 'share/doc/rust/COPYRIGHT-library.html'
if ((Get-FileHash -LiteralPath $rustNotices).Hash -ne (Get-FileHash -LiteralPath $toolchainNotices).Hash) {
    throw 'Rust notices do not match the current toolchain. Refresh and review them first.'
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$stream = [IO.File]::Open($zipPath, [IO.FileMode]::CreateNew)
try {
    $archive = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Create, $false)
    try {
        foreach ($item in @(
            @($installerPath, 'VelvetFrost-Setup.exe'),
            @((Join-Path $root 'LICENSE'), 'LICENSE.txt'),
            @((Join-Path $root 'THIRD-PARTY-NOTICES.txt'), 'THIRD-PARTY-NOTICES.txt'),
            @($rustNotices, 'RUST-STANDARD-LIBRARY-NOTICES.html')
        )) {
            [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $item[0], $item[1]) | Out-Null
        }
    } finally { $archive.Dispose() }
} finally { $stream.Dispose() }
Write-Host "Created $zipPath. Verify the installer itself and the archive before distribution."
