<#
.SYNOPSIS
  Create the VelvetFrost update key with a local masked-password dialog.
.DESCRIPTION
  The password is sent to OpenSSL over a private stdin pipe, never in an
  argument, environment variable, file, chat or log. The encrypted private key
  remains in the ignored local directory. Store a separate offline backup.
  -ResumeExistingKey re-derives a public key after an interrupted run.
  -HardenExistingKey makes a stronger-encrypted copy without replacing the key.
  -TestOnly is restricted to an isolated non-Git test copy of this script.
#>
[CmdletBinding()]
param(
    [switch] $ResumeExistingKey,
    [switch] $HardenExistingKey,
    [switch] $TestOnly,
    [switch] $TestOnlyUseEnvironmentPassword,
    [string] $OpenSsl = 'C:\Program Files\Git\mingw64\bin\openssl.exe'
)

$ErrorActionPreference = 'Stop'
if ($PSVersionTable.PSEdition -ne 'Core') { throw 'Use PowerShell 7.' }
if ($ResumeExistingKey -and $HardenExistingKey) { throw 'Choose only one recovery operation.' }
if ($TestOnlyUseEnvironmentPassword -and -not $TestOnly) { throw 'Environment password input is restricted to isolated tests.' }
if (-not (Test-Path -LiteralPath $OpenSsl -PathType Leaf)) { throw 'OpenSSL was not found.' }
$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
if ($TestOnly -and (Test-Path -LiteralPath (Join-Path $root '.git'))) { throw 'Test mode cannot run in the product repository.' }
$keyDirectory = Join-Path $root '.local/VelvetFrost-update-signing'
$private = Join-Path $keyDirectory 'VelvetFrost-updater.key'
$hardened = Join-Path $keyDirectory 'VelvetFrost-updater-hardened.key'
$pinned = Join-Path $root 'windows/update-public-key.hex'
if ($HardenExistingKey) {
    if (-not (Test-Path -LiteralPath $private -PathType Leaf) -or -not (Test-Path -LiteralPath $pinned -PathType Leaf)) { throw 'The key or public pin is missing.' }
    if (Test-Path -LiteralPath $hardened) { throw 'A hardened key already exists; it will not be replaced.' }
} elseif (Test-Path -LiteralPath $pinned) {
    throw 'A public key is already pinned; it will not be replaced.'
} elseif ($ResumeExistingKey) {
    if (-not (Test-Path -LiteralPath $private -PathType Leaf)) { throw 'No existing encrypted key was found.' }
} elseif (Test-Path -LiteralPath $keyDirectory) {
    throw 'The key directory already exists; review it before using -ResumeExistingKey.'
}

function Invoke-OpenSsl([string[]] $Arguments, [string] $Secret, [int] $SecretLines = 1) {
    $info = [System.Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $OpenSsl
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardInputEncoding = [System.Text.UTF8Encoding]::new($false)
    foreach ($argument in $Arguments) { [void] $info.ArgumentList.Add($argument) }
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $info
    try {
        if (-not $process.Start()) { throw 'OpenSSL could not start.' }
        if ($null -ne $Secret) { for ($i = 0; $i -lt $SecretLines; $i++) { $process.StandardInput.WriteLine($Secret) } }
        $process.StandardInput.Close()
        [void] $process.StandardOutput.ReadToEnd()
        [void] $process.StandardError.ReadToEnd()
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) { throw 'OpenSSL could not complete the operation.' }
    } finally { $process.Dispose() }
}

if ($TestOnly) {
    Write-Warning 'TEST ONLY: generating a disposable key in an isolated test copy.'
    $passphrase = if ($TestOnlyUseEnvironmentPassword) { [System.Environment]::GetEnvironmentVariable('VELVETFROST_TEST_KEY_PASSWORD') } else { [guid]::NewGuid().ToString('N') + [guid]::NewGuid().ToString('N') }
    if ([string]::IsNullOrEmpty($passphrase)) { throw 'The test-only password is missing.' }
} else {
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
    $form = [System.Windows.Forms.Form]::new()
    $form.Text = 'VelvetFrost — ключ обновлений'
    $form.StartPosition = 'CenterScreen'
    $form.ClientSize = [System.Drawing.Size]::new(480, 250)
    $form.FormBorderStyle = 'FixedDialog'
    $form.MaximizeBox = $false
    $form.MinimizeBox = $false
    $form.TopMost = $true
    $label = [System.Windows.Forms.Label]::new()
    $label.SetBounds(20, 15, 440, 48)
    $label.Text = if ($HardenExistingKey -or $ResumeExistingKey) { 'Введите существующий пароль ключа и повторите его для подтверждения.' } else { 'Придумайте пароль не короче 12 символов: латинские буквы, цифры и знаки. Сохраните его отдельно от ключа.' }
    $form.Controls.Add($label)
    $first = [System.Windows.Forms.TextBox]::new()
    $first.SetBounds(20, 75, 440, 25)
    $first.UseSystemPasswordChar = $true
    $form.Controls.Add($first)
    $secondLabel = [System.Windows.Forms.Label]::new()
    $secondLabel.SetBounds(20, 108, 440, 22)
    $secondLabel.Text = 'Повторите пароль:'
    $form.Controls.Add($secondLabel)
    $second = [System.Windows.Forms.TextBox]::new()
    $second.SetBounds(20, 130, 440, 25)
    $second.UseSystemPasswordChar = $true
    $form.Controls.Add($second)
    $errorLabel = [System.Windows.Forms.Label]::new()
    $errorLabel.SetBounds(20, 160, 440, 30)
    $errorLabel.ForeColor = [System.Drawing.Color]::Firebrick
    $form.Controls.Add($errorLabel)
    $ok = [System.Windows.Forms.Button]::new()
    $ok.SetBounds(265, 205, 95, 30)
    $ok.Text = if ($HardenExistingKey) { 'Защитить' } else { 'Создать' }
    $form.Controls.Add($ok)
    $cancel = [System.Windows.Forms.Button]::new()
    $cancel.SetBounds(365, 205, 95, 30)
    $cancel.Text = 'Отмена'
    $cancel.DialogResult = [System.Windows.Forms.DialogResult]::Cancel
    $form.Controls.Add($cancel)
    $form.AcceptButton = $ok
    $form.CancelButton = $cancel
    $ok.Add_Click({
        if ($first.Text.Length -lt 12 -or $first.Text -cnotmatch '^[\x20-\x7e]+$') {
            $errorLabel.Text = 'Нужно не менее 12 символов: латинские буквы, цифры и знаки.'
        } elseif ($first.Text -cne $second.Text) {
            $errorLabel.Text = 'Пароли не совпадают.'
        } else {
            $form.DialogResult = [System.Windows.Forms.DialogResult]::OK
            $form.Close()
        }
    })
    $form.Add_Shown({ $form.Activate(); $first.Focus() })
    $result = $form.ShowDialog()
    if ($result -ne [System.Windows.Forms.DialogResult]::OK) { $form.Dispose(); return }
    $passphrase = $first.Text
    $first.Clear()
    $second.Clear()
    $form.Dispose()
}

try {
    if (-not $ResumeExistingKey -and -not $HardenExistingKey) {
        [void] [System.IO.Directory]::CreateDirectory($keyDirectory)
        Invoke-OpenSsl @('genpkey', '-algorithm', 'ED25519', '-aes-256-cbc', '-out', $private, '-pass', 'stdin') $passphrase
    }
    $reader = [System.IO.StreamReader]::new($private)
    try { $header = $reader.ReadLine() } finally { $reader.Dispose() }
    if ($header -cne '-----BEGIN ENCRYPTED PRIVATE KEY-----') { throw 'The key is not encrypted PKCS#8.' }
    $id = [guid]::NewGuid().ToString('N')
    if ($HardenExistingKey) {
        $candidate = Join-Path $keyDirectory "hardened-candidate-$id.key"
        $candidatePublic = Join-Path $keyDirectory "hardened-public-$id.der"
        Invoke-OpenSsl @('pkcs8', '-topk8', '-in', $private, '-out', $candidate, '-v2', 'aes-256-cbc', '-iter', '600000', '-passin', 'stdin', '-passout', 'stdin') $passphrase 2
        Invoke-OpenSsl @('pkey', '-in', $candidate, '-pubout', '-outform', 'DER', '-out', $candidatePublic, '-passin', 'stdin') $passphrase
        $candidateBytes = [System.IO.File]::ReadAllBytes($candidatePublic)
        $existingPin = [System.IO.File]::ReadAllText($pinned).Trim().ToLowerInvariant()
        if ($candidateBytes.Length -ne 44 -or [Convert]::ToHexString($candidateBytes[0..11]).ToLowerInvariant() -cne '302a300506032b6570032100' -or
            [Convert]::ToHexString($candidateBytes[12..43]).ToLowerInvariant() -cne $existingPin) { throw 'The hardened key does not match the public pin.' }
        [System.IO.File]::Move($candidate, $hardened)
        if ($TestOnly) { Write-Output 'Test key copy hardened.' }
        else { [void] [System.Windows.Forms.MessageBox]::Show('Более стойко зашифрованная копия создана и проверена. Исходный ключ сохранён.', 'VelvetFrost', 'OK', 'Information') }
        return
    }
    $der = Join-Path $keyDirectory "public-$id.der"
    $public = Join-Path $keyDirectory "public-$id.pem"
    Invoke-OpenSsl @('pkey', '-in', $private, '-pubout', '-outform', 'DER', '-out', $der, '-passin', 'stdin') $passphrase
    $bytes = [System.IO.File]::ReadAllBytes($der)
    if ($bytes.Length -ne 44 -or [Convert]::ToHexString($bytes[0..11]).ToLowerInvariant() -cne '302a300506032b6570032100') {
        throw 'The public key is not Ed25519.'
    }
    Invoke-OpenSsl @('pkey', '-pubin', '-inform', 'DER', '-in', $der, '-out', $public) $null
    $hex = [Convert]::ToHexString($bytes[12..43]).ToLowerInvariant() + "`n"
    $stream = [System.IO.File]::Open($pinned, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
    try {
        $writer = [System.IO.StreamWriter]::new($stream, [System.Text.UTF8Encoding]::new($false))
        try { $writer.Write($hex) } finally { $writer.Dispose() }
    } finally { $stream.Dispose() }
    if ($TestOnly) { Write-Output 'Test key created.' }
    else {
        [void] [System.Windows.Forms.MessageBox]::Show('Ключ создан. Сохраните зашифрованную резервную копию отдельно и сохраните пароль в надёжном месте.', 'VelvetFrost', 'OK', 'Information')
    }
} catch {
    if ($TestOnly) { throw }
    [void] [System.Windows.Forms.MessageBox]::Show('Не удалось создать ключ. Локальная папка сохранена для проверки; не удаляйте её и сообщите об ошибке.', 'VelvetFrost', 'OK', 'Error')
} finally {
    $passphrase = $null
}
