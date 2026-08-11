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
    $launcherArtifact = "pcb-launcher-x86_64-pc-windows-msvc.exe"
    $launcherBinary = Join-Path $tmp "pcb-launcher.exe"
    $launcherSum = Join-Path $tmp "pcb-launcher.exe.sha256"
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
        if ($LASTEXITCODE -ne 0) {
            Remove-Item -Force -ErrorAction SilentlyContinue $binary
            Invoke-WebRequest "$baseUrl/$($latest.tag)/$artifact" -OutFile $binary
        }
    } else {
        Invoke-WebRequest "$baseUrl/$($latest.tag)/$artifact" -OutFile $binary
    }

    $launcherDownloaded = $false
    try {
        Invoke-WebRequest "$baseUrl/$($latest.tag)/$launcherArtifact.sha256" -OutFile $launcherSum
        $launcherDownloadedCompressed = $false
        if ($zstd) {
            $launcherCompressedPath = Join-Path $tmp "pcb-launcher.exe.zst"
            try {
                Invoke-WebRequest "$baseUrl/$($latest.tag)/$launcherArtifact.zst" -OutFile $launcherCompressedPath
                $launcherDownloadedCompressed = $true
            } catch {
                $launcherDownloadedCompressed = $false
            }
        }
        if ($launcherDownloadedCompressed) {
            & $zstd.Source -q -d -f $launcherCompressedPath -o $launcherBinary
            if ($LASTEXITCODE -ne 0) {
                Remove-Item -Force -ErrorAction SilentlyContinue $launcherBinary
                Invoke-WebRequest "$baseUrl/$($latest.tag)/$launcherArtifact" -OutFile $launcherBinary
            }
        } else {
            Invoke-WebRequest "$baseUrl/$($latest.tag)/$launcherArtifact" -OutFile $launcherBinary
        }

        $launcherExpected = ((Get-Content $launcherSum -Raw) -split "\s+")[0].ToLowerInvariant()
        $launcherActual = (Get-FileHash -Algorithm SHA256 $launcherBinary).Hash.ToLowerInvariant()
        if ($launcherActual -ne $launcherExpected) {
            throw "pcb-launcher checksum mismatch"
        }

        $launcherDownloaded = $true
    } catch {
        Write-Warning "Skipping optional pcb-launcher for $($latest.tag): $_"
    }

    $expected = ((Get-Content $sum -Raw) -split "\s+")[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $binary).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "checksum mismatch"
    }
    New-Item -ItemType Directory -Force $installDir | Out-Null
    $installedPcb = Join-Path $installDir "pcb.exe"
    $installedLauncher = Join-Path $installDir "pcb-launcher.exe"
    Move-Item -Force $binary $installedPcb

    if ($launcherDownloaded) {
        try {
            Move-Item -Force $launcherBinary $installedLauncher
        } catch {
            $launcherDownloaded = $false
            Write-Warning "Installed pcb, but could not install the Diode URL launcher: $_"
        }
    }

    if ($launcherDownloaded) {
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
    if ($launcherDownloaded) {
        Write-Host "Installed Diode URL launcher to $installedLauncher"
    }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
