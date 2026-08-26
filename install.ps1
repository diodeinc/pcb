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
$zstd = Get-Command zstd -ErrorAction SilentlyContinue
$tmp = New-Item -ItemType Directory -Path (Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName()))

# Download and checksum-verify one release binary into $tmp, preferring the
# zstd-compressed artifact when zstd is available. Returns the local path.
function Get-ReleaseBinary($name) {
    $artifact = "$name-x86_64-pc-windows-msvc.exe"
    $binary = Join-Path $tmp "$name.exe"
    $sum = Join-Path $tmp "$name.exe.sha256"
    Invoke-WebRequest "$baseUrl/$($latest.tag)/$artifact.sha256" -OutFile $sum

    $downloadedCompressed = $false
    if ($zstd) {
        $compressedPath = Join-Path $tmp "$name.exe.zst"
        try {
            Invoke-WebRequest "$baseUrl/$($latest.tag)/$artifact.zst" -OutFile $compressedPath
            $downloadedCompressed = $true
        } catch {
            $downloadedCompressed = $false
        }
    }
    if ($downloadedCompressed) {
        & $zstd.Source -q -d -f $compressedPath -o $binary
        if ($LASTEXITCODE -ne 0) {
            Remove-Item -Force -ErrorAction SilentlyContinue $binary
            Invoke-WebRequest "$baseUrl/$($latest.tag)/$artifact" -OutFile $binary
        }
    } else {
        Invoke-WebRequest "$baseUrl/$($latest.tag)/$artifact" -OutFile $binary
    }

    $expected = ((Get-Content $sum -Raw) -split "\s+")[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $binary).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "$name checksum mismatch"
    }
    return $binary
}

try {
    $binary = Get-ReleaseBinary "pcb"

    $launcherBinary = $null
    try {
        $launcherBinary = Get-ReleaseBinary "pcb-launcher"
    } catch {
        Write-Warning "Skipping optional pcb-launcher for $($latest.tag): $_"
    }

    New-Item -ItemType Directory -Force $installDir | Out-Null
    $installedPcb = Join-Path $installDir "pcb.exe"
    $installedLauncher = Join-Path $installDir "pcb-launcher.exe"
    Move-Item -Force $binary $installedPcb

    if ($launcherBinary) {
        try {
            Move-Item -Force $launcherBinary $installedLauncher
        } catch {
            $launcherBinary = $null
            Write-Warning "Installed pcb, but could not install the Diode URL launcher: $_"
        }
    }

    if ($launcherBinary) {
        # pcb-launcher uses the Windows GUI subsystem, so invoke it through a
        # process handle to wait for registration and read the correct exit code.
        try {
            $launcherInstall = Start-Process -FilePath $installedLauncher -ArgumentList "--install --toolchain latest" -Wait -PassThru
            if ($launcherInstall.ExitCode -ne 0) {
                Write-Warning "Installed pcb, but could not register the Diode URL launcher. See $HOME\.pcb\pcb-launcher.log for details"
            }
        } catch {
            Write-Warning "Installed pcb, but could not register the Diode URL launcher: $_"
        }
    }

    Add-InstallDirToPath $installDir

    Write-Host "Installed pcb to $installedPcb"
    if ($launcherBinary) {
        Write-Host "Installed Diode URL launcher to $installedLauncher"
    }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
