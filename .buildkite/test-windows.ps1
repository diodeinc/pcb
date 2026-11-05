#Requires -Version 7.4
$ErrorActionPreference = 'Stop'
# A failed native command fails the job.
$PSNativeCommandUseErrorActionPreference = $true
# The worker keeps Cargo, rustup and the checkout on its cache disk between jobs.

Write-Output '--- Install test tools'
# Cargo's Git dependency checkouts include paths longer than MAX_PATH.
git config --global core.longpaths true
rustup set default-host x86_64-pc-windows-msvc
rustup set profile minimal
# rust-toolchain.toml selects and installs the repository's pinned toolchain.
rustc --version --verbose

$cargoBin = Join-Path $HOME '.cargo/bin'
$archive = Join-Path $env:TEMP 'cargo-nextest.zip'
Invoke-WebRequest https://get.nexte.st/0.9.151/windows -OutFile $archive
if ((Get-FileHash $archive -Algorithm SHA256).Hash -ne 'ba35ee0ce14ace65cd8c7aaa9613adb7d2644b0fe3866e85131eb4c269786efb') {
    throw 'cargo-nextest archive SHA256 mismatch'
}
Expand-Archive $archive -DestinationPath $cargoBin -Force
Remove-Item $archive
cargo nextest --version

Write-Output '--- Windows workspace tests'
cargo nextest run --workspace --profile ci --locked
