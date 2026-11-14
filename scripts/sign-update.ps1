<#
.SYNOPSIS
  Create a signed VelvetFrost update manifest for an already verified installer.
.DESCRIPTION
  The private key must be an encrypted Ed25519 PEM stored outside the product
  tree. Use -GuiPassword for a masked local dialog, or enter the passphrase at
  the OpenSSL console prompt. Never put it in an argument, environment variable,
  build log, or chat. The script compares the supplied public key to the pin.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Installer,
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $PrivateKey,
    [Parameter(Mandatory)] [string] $PublicKey,
    [Parameter(Mandatory)] [string] $OutputDirectory,
    [switch] $GuiPassword,
    # For isolated fixtures only. Artifacts created this way are not releases.
    [string] $TestOnlyTrustedPublicKeyHexFile,
    [switch] $TestOnlyPasswordFromEnvironment,
    [string] $OpenSsl = 'openssl'
)

$ErrorActionPreference = 'Stop'
if ($TestOnlyPasswordFromEnvironment -and (-not $TestOnlyTrustedPublicKeyHexFile -or $GuiPassword)) {
    throw 'Test-only password input requires a test trust anchor and cannot be used with the GUI.'
}
if ($Version -cnotmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
    throw 'Version must be a stable three-component version without leading zeroes.'
}
$parts = @($Version.Split('.') | ForEach-Object { [uint16]::Parse($_) })
$installerPath = (Resolve-Path -LiteralPath $Installer).Path
$privateKeyPath = (Resolve-Path -LiteralPath $PrivateKey).Path
$publicKeyPath = (Resolve-Path -LiteralPath $PublicKey).Path
$trustedKeyPath = if ($TestOnlyTrustedPublicKeyHexFile) {
    Write-Warning 'TEST ONLY: using a test trust anchor. Do not publish this output.'
    (Resolve-Path -LiteralPath $TestOnlyTrustedPublicKeyHexFile).Path
} else {
    (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../windows/update-public-key.hex')).Path
}
$trustedKey = [System.IO.File]::ReadAllText($trustedKeyPath).Trim().ToLowerInvariant()
if ($trustedKey -cnotmatch '^[0-9a-f]{64}$') { throw 'The pinned update public key is invalid.' }
$reader = [System.IO.StreamReader]::new($privateKeyPath)
try { $keyHeader = $reader.ReadLine() } finally { $reader.Dispose() }
if ($keyHeader -cne '-----BEGIN ENCRYPTED PRIVATE KEY-----') {
    throw 'The private key must be an encrypted PKCS#8 PEM.'
}
$outputPath = (Resolve-Path -LiteralPath $OutputDirectory).Path
if (-not (Test-Path -LiteralPath $outputPath -PathType Container)) { throw 'OutputDirectory must be an existing directory.' }
$manifestPath = Join-Path $outputPath 'VelvetFrost-update.txt'
$signaturePath = Join-Path $outputPath 'VelvetFrost-update.sig'
if ((Test-Path -LiteralPath $manifestPath) -or (Test-Path -LiteralPath $signaturePath)) { throw 'Update metadata already exists; use a new output directory.' }

# Hold a read-only handle denying write/delete sharing until the signature and
# self-check are complete, so the digest describes the actual input file.
$installerStream = [System.IO.File]::Open($installerPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::Read)
$staging = Join-Path $outputPath ('.sign-' + [guid]::NewGuid().ToString('N'))
try {
    if ($installerStream.Length -lt 1 -or $installerStream.Length -gt 512MB) { throw 'Installer size is outside the supported range.' }
    $info = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($installerPath)
    if ($info.OriginalFilename -cne 'VelvetFrost-Setup.exe' -or
        $info.FileMajorPart -ne $parts[0] -or $info.FileMinorPart -ne $parts[1] -or
        $info.FileBuildPart -ne $parts[2] -or $info.FilePrivatePart -ne 0 -or
        $info.ProductMajorPart -ne $parts[0] -or $info.ProductMinorPart -ne $parts[1] -or
        $info.ProductBuildPart -ne $parts[2] -or $info.ProductPrivatePart -ne 0) {
        throw 'Installer identity or version does not match the requested version.'
    }
    $hash = (Get-FileHash -InputStream $installerStream -Algorithm SHA256).Hash.ToLowerInvariant()
    $manifest = "VelvetFrost update manifest v1`n$Version`nVelvetFrost-Setup.exe`n$($installerStream.Length)`n$hash`n"
    [void] [System.IO.Directory]::CreateDirectory($staging)
    $stagedManifest = Join-Path $staging 'manifest'
    $stagedSignature = Join-Path $staging 'signature'
    $stagedPublic = Join-Path $staging 'public.der'
    & $OpenSsl pkey -pubin -in $publicKeyPath -outform DER -out $stagedPublic
    if ($LASTEXITCODE -ne 0) { throw 'OpenSSL could not read the public key.' }
    $der = [System.IO.File]::ReadAllBytes($stagedPublic)
    $prefix = '302a300506032b6570032100'
    if ($der.Length -ne 44 -or [Convert]::ToHexString($der[0..11]).ToLowerInvariant() -cne $prefix -or
        [Convert]::ToHexString($der[12..43]).ToLowerInvariant() -cne $trustedKey) {
        throw 'The supplied public key does not match the pinned VelvetFrost key.'
    }
    [System.IO.File]::WriteAllText($stagedManifest, $manifest, [System.Text.UTF8Encoding]::new($false))
    if ($GuiPassword -or $TestOnlyPasswordFromEnvironment) {
        if ($GuiPassword) {
            Add-Type -AssemblyName System.Windows.Forms
            Add-Type -AssemblyName System.Drawing
            $form = [System.Windows.Forms.Form]::new()
            $form.Text = 'VelvetFrost — подпись обновления'
            $form.StartPosition = 'CenterScreen'
            $form.ClientSize = [System.Drawing.Size]::new(460, 150)
            $form.FormBorderStyle = 'FixedDialog'
            $form.MaximizeBox = $false
            $form.MinimizeBox = $false
            $form.TopMost = $true
            $label = [System.Windows.Forms.Label]::new()
            $label.SetBounds(20, 15, 420, 25)
            $label.Text = 'Введите пароль закрытого ключа VelvetFrost:'
            $form.Controls.Add($label)
            $box = [System.Windows.Forms.TextBox]::new()
            $box.SetBounds(20, 45, 420, 25)
            $box.UseSystemPasswordChar = $true
            $form.Controls.Add($box)
            $ok = [System.Windows.Forms.Button]::new()
            $ok.SetBounds(245, 100, 95, 30)
            $ok.Text = 'Подписать'
            $ok.DialogResult = [System.Windows.Forms.DialogResult]::OK
            $form.Controls.Add($ok)
            $cancel = [System.Windows.Forms.Button]::new()
            $cancel.SetBounds(345, 100, 95, 30)
            $cancel.Text = 'Отмена'
            $cancel.DialogResult = [System.Windows.Forms.DialogResult]::Cancel
            $form.Controls.Add($cancel)
            $form.AcceptButton = $ok
            $form.CancelButton = $cancel
            $form.Add_Shown({ $form.Activate(); $box.Focus() })
            $answer = $form.ShowDialog()
            if ($answer -ne [System.Windows.Forms.DialogResult]::OK) { $form.Dispose(); throw 'Signing was cancelled.' }
            $passphrase = $box.Text
            $box.Clear()
            $form.Dispose()
        } else {
            $passphrase = [System.Environment]::GetEnvironmentVariable('VELVETFROST_TEST_KEY_PASSWORD')
            if ([string]::IsNullOrEmpty($passphrase)) { throw 'The test-only password is missing.' }
        }
        try {
            $info = [System.Diagnostics.ProcessStartInfo]::new()
            $info.FileName = $OpenSsl
            $info.UseShellExecute = $false
            $info.CreateNoWindow = $true
            $info.RedirectStandardInput = $true
            $info.RedirectStandardOutput = $true
            $info.RedirectStandardError = $true
            $info.StandardInputEncoding = [System.Text.UTF8Encoding]::new($false)
            foreach ($arg in @('pkeyutl', '-sign', '-rawin', '-inkey', $privateKeyPath, '-in', $stagedManifest, '-out', $stagedSignature, '-passin', 'stdin')) {
                [void] $info.ArgumentList.Add($arg)
            }
            $process = [System.Diagnostics.Process]::new()
            $process.StartInfo = $info
            try {
                if (-not $process.Start()) { throw 'OpenSSL could not start.' }
                $process.StandardInput.WriteLine($passphrase)
                $process.StandardInput.Close()
                [void] $process.StandardOutput.ReadToEnd()
                [void] $process.StandardError.ReadToEnd()
                $process.WaitForExit()
                if ($process.ExitCode -ne 0) { throw 'OpenSSL could not sign the manifest; check the password and retry.' }
            } finally { $process.Dispose() }
        } finally { $passphrase = $null }
    } else {
        & $OpenSsl pkeyutl -sign -rawin -inkey $privateKeyPath -in $stagedManifest -out $stagedSignature
        if ($LASTEXITCODE -ne 0) { throw 'OpenSSL could not sign the manifest.' }
    }
    if ((Get-Item -LiteralPath $stagedSignature).Length -ne 64) { throw 'The signature is not an Ed25519 signature.' }
    & $OpenSsl pkeyutl -verify -rawin -pubin -inkey $publicKeyPath -in $stagedManifest -sigfile $stagedSignature
    if ($LASTEXITCODE -ne 0) { throw 'Signature verification with the supplied public key failed.' }
    [System.IO.File]::Move($stagedSignature, $signaturePath)
    [System.IO.File]::Move($stagedManifest, $manifestPath)
    Write-Output "Signed update metadata created in $outputPath"
}
finally {
    $installerStream.Dispose()
    if (Test-Path -LiteralPath $staging -PathType Container) {
        foreach ($name in @('manifest', 'signature', 'public.der')) {
            $file = Join-Path $staging $name
            if (Test-Path -LiteralPath $file -PathType Leaf) { Remove-Item -LiteralPath $file -Force }
        }
        Remove-Item -LiteralPath $staging -ErrorAction SilentlyContinue
    }
}
