$ErrorActionPreference = "Stop"

$baseUrl = "https://pcb.api.diode.computer/pcb"
$installDir = $env:PCB_INSTALL_DIR
if (-not $installDir) {
    $installDir = Join-Path $env:USERPROFILE ".pcb\bin"
}

if ($env:PROCESSOR_ARCHITECTURE -ne "AMD64") {
    throw "unsupported platform: Windows $env:PROCESSOR_ARCHITECTURE"
}

function Add-InstallDirToPath($dir) {
    if ($env:GITHUB_PATH) {
        Add-Content -Path $env:GITHUB_PATH -Value $dir
    }

    $registry = "registry::HKEY_CURRENT_USER\Environment"
    $current = (Get-Item -LiteralPath $registry).GetValue("Path", "", "DoNotExpandEnvironmentNames")
    $entries = $current -split ";" | Where-Object { $_ }
    if ($entries -contains $dir) {
        return
    }

    $newPath = (@($dir) + $entries) -join ";"
    Set-ItemProperty -Type ExpandString -LiteralPath $registry Path $newPath

    $name = "pcb-path-update-" + [guid]::NewGuid().ToString()
    [Environment]::SetEnvironmentVariable($name, "1", "User")
    [Environment]::SetEnvironmentVariable($name, $null, "User")

    Write-Host "Added $dir to PATH. Restart your terminal or run: `$env:Path = `"$dir;`$env:Path`""
}

$latest = Invoke-RestMethod "$baseUrl/pcb-latest.json"
$artifact = "pcb-x86_64-pc-windows-msvc.exe"
$tmp = New-Item -ItemType Directory -Path (Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName()))

try {
    $binary = Join-Path $tmp "pcb.exe"
    $sum = Join-Path $tmp "pcb.exe.sha256"
    Invoke-WebRequest "$baseUrl/$($latest.tag)/$artifact.sha256" -OutFile $sum
    $zstd = Get-Command zstd -ErrorAction SilentlyContinue
    $downloadedCompressed = $false
    if ($zstd) {
        $compressedPath = Join-Path $tmp "pcb.exe.zst"
        try {
            Invoke-WebRequest "$baseUrl/$($latest.tag)/$artifact.zst" -OutFile $compressedPath
            $downloadedCompressed = $true
        } catch {
            $downloadedCompressed = $false
        }
    }

    if ($downloadedCompressed) {
        & $zstd.Source -q -d -f $compressedPath -o $binary
    } else {
        Invoke-WebRequest "$baseUrl/$($latest.tag)/$artifact" -OutFile $binary
    }

    $expected = ((Get-Content $sum -Raw) -split "\s+")[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $binary).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "checksum mismatch"
    }

    New-Item -ItemType Directory -Force $installDir | Out-Null
    Move-Item -Force $binary (Join-Path $installDir "pcb.exe")

    $configDir = Join-Path $env:LOCALAPPDATA "pcb"
    New-Item -ItemType Directory -Force $configDir | Out-Null
    $receipt = @{ install_prefix = $installDir } | ConvertTo-Json -Compress
    [IO.File]::WriteAllText((Join-Path $configDir "pcb-receipt.json"), $receipt, (New-Object Text.UTF8Encoding $false))

    Add-InstallDirToPath $installDir

    Write-Host "Installed pcb to $(Join-Path $installDir "pcb.exe")"
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
