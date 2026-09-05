$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot

Write-Host "Bloom Squash Engine - Windows VST3 build" -ForegroundColor Cyan

# Rust/cargo
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
        throw "Rust is not installed and winget is unavailable. Install Rust from https://rustup.rs/ and rerun this script."
    }
    Write-Host "Installing Rustup..." -ForegroundColor Yellow
    winget install --id Rustlang.Rustup -e --accept-source-agreements --accept-package-agreements
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
}

if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "Cargo is still unavailable. Close/reopen PowerShell and rerun BUILD_WINDOWS.ps1."
}

# MSVC build tools. Rust's windows-msvc target needs Microsoft's linker/toolchain.
$vswhere = "$env:ProgramFiles(x86)\Microsoft Visual Studio\Installer\vswhere.exe"
$hasVC = $false
if (Test-Path $vswhere) {
    $vcPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    $hasVC = -not [string]::IsNullOrWhiteSpace($vcPath)
}

if (-not $hasVC) {
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
        throw "Visual Studio C++ Build Tools are required. Install VS 2022 Build Tools with 'Desktop development with C++'."
    }
    Write-Host "Installing Visual Studio 2022 C++ Build Tools. This may take several minutes..." -ForegroundColor Yellow
    winget install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-source-agreements --accept-package-agreements --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
}

Write-Host "Preparing Rust stable toolchain..." -ForegroundColor Cyan
rustup default stable
rustup target add x86_64-pc-windows-msvc

Write-Host "Building Bloom Squash Engine..." -ForegroundColor Cyan
cargo xtask bundle polarity_sc_dark --release --target=x86_64-pc-windows-msvc

$bundle = Join-Path $PSScriptRoot "target\bundled\Bloom Squash Engine.vst3"
if (-not (Test-Path $bundle)) {
    throw "Build command completed but the expected VST3 bundle was not found: $bundle"
}

Write-Host "" 
Write-Host "SUCCESS" -ForegroundColor Green
Write-Host "VST3: $bundle"
Write-Host "Copy this folder to C:\Program Files\Common Files\VST3\ and rescan Ableton Live." -ForegroundColor Green
Start-Process explorer.exe -ArgumentList "/select,`"$bundle`""
