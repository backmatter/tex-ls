# Install a prebuilt tex-ls release for the current user.
$ErrorActionPreference = 'Stop'
$tag = '@TAG@'
$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
switch ($architecture) {
    'X64' { $target = 'x86_64-pc-windows-msvc' }
    'Arm64' { $target = 'aarch64-pc-windows-msvc' }
    default { throw "Unsupported CPU architecture: $architecture" }
}
$archive = "tex-ls-$target.zip"
$base = "https://github.com/backmatter/tex-ls/releases/download/$tag"
$work = Join-Path ([System.IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $work | Out-Null
try {
    Invoke-WebRequest -UseBasicParsing "$base/$archive" -OutFile (Join-Path $work $archive)
    Invoke-WebRequest -UseBasicParsing "$base/SHA256SUMS" -OutFile (Join-Path $work 'SHA256SUMS')
    $row = @(Get-Content (Join-Path $work 'SHA256SUMS') | Where-Object { ($_ -split '\s+')[1] -eq $archive })
    if ($row.Count -ne 1) { throw 'Missing or ambiguous download checksum.' }
    $expected = ($row[0] -split '\s+')[0]
    $actual = (Get-FileHash (Join-Path $work $archive) -Algorithm SHA256).Hash
    if ($actual -ne $expected) { throw 'Download checksum verification failed.' }
    Expand-Archive (Join-Path $work $archive) -DestinationPath $work
    & (Join-Path $work 'tex-ls.exe') --version
    if ($LASTEXITCODE -ne 0) { throw 'Downloaded executable could not run.' }
    $bin = if ($env:TEX_LS_INSTALL_DIR) { $env:TEX_LS_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'tex-ls\bin' }
    New-Item -ItemType Directory -Force -Path $bin | Out-Null
    Copy-Item (Join-Path $work 'tex-ls.exe') $bin -Force
    foreach ($name in @('LICENSE', 'unicode-math.LICENSE', 'unicode-math.NOTICE')) {
        Copy-Item (Join-Path $work $name) $bin -Force
    }
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($env:TEX_LS_NO_MODIFY_PATH -ne '1' -and $bin -notin ($userPath -split ';')) {
        [Environment]::SetEnvironmentVariable('Path', "$bin;$userPath", 'User')
    }
    if ($bin -notin ($env:Path -split ';')) { $env:Path = "$bin;$env:Path" }
    Write-Host "Installed tex-ls in $bin"
    Write-Host 'Run tex-ls --version. Restart your editor to pick up the new PATH.'
} finally {
    Remove-Item $work -Recurse -Force
}
