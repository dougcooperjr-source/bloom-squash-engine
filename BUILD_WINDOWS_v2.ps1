$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-Location -LiteralPath $PSScriptRoot

$log = Join-Path $PSScriptRoot 'BUILD_WINDOWS.log'
try {
    Start-Transcript -Path $log -Force | Out-Null
} catch {}

function Wait-At-End {
    Write-Host ''
    Write-Host 'Press ENTER to close this window...' -ForegroundColor DarkGray
    [void](Read-Host)
}

try {
    Write-Host 'Bloom Squash Engine - Windows VST3 build' -ForegroundColor Cyan
    Write-Host "Folder: $PSScriptRoot" -ForegroundColor DarkGray
    Write-Host "Log:    $log" -ForegroundColor DarkGray
    Write-Host ''

    # Make the most common Rust install location available in this process.
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"

    # Rust / Cargo
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
            throw 'Rust/Cargo is not installed and winget is unavailable. Install Rust from https://rustup.rs/ and run this script again.'
        }

        Write-Host 'Rust/Cargo was not found. Installing Rustup with winget...' -ForegroundColor Yellow
        winget install --id Rustlang.Rustup -e --accept-source-agreements --accept-package-agreements
        if ($LASTEXITCODE -ne 0) {
            throw "Rustup installation failed (winget exit code $LASTEXITCODE)."
        }

        $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
    }

    if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
        throw 'rustup is still unavailable after installation. Close this window, reopen it, and run BUILD_WINDOWS_v2.ps1 again.'
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw 'cargo is still unavailable after installation. Close this window, reopen it, and run BUILD_WINDOWS_v2.ps1 again.'
    }

    Write-Host "Cargo:  $((Get-Command cargo).Source)" -ForegroundColor Green
    Write-Host "Rustup: $((Get-Command rustup).Source)" -ForegroundColor Green
    Write-Host ''

    # MSVC build tools. Rust's windows-msvc target needs Microsoft's linker/toolchain.
    $vswhere = "$env:ProgramFiles(x86)\Microsoft Visual Studio\Installer\vswhere.exe"
    $hasVC = $false
    if (Test-Path $vswhere) {
        $vcPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        $hasVC = -not [string]::IsNullOrWhiteSpace($vcPath)
    }

    if (-not $hasVC) {
        if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
            throw "Visual Studio 2022 C++ Build Tools are required. Install 'Desktop development with C++', then run this script again."
        }

        Write-Host 'Visual Studio C++ Build Tools were not detected.' -ForegroundColor Yellow
        Write-Host 'Installing Visual Studio 2022 Build Tools. Windows may show a UAC prompt.' -ForegroundColor Yellow
        Write-Host 'This can take several minutes.' -ForegroundColor Yellow

        winget install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-source-agreements --accept-package-agreements --override '--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
        if ($LASTEXITCODE -ne 0) {
            throw "Visual Studio Build Tools installation failed (winget exit code $LASTEXITCODE)."
        }
    }

    Write-Host ''
    Write-Host 'Preparing Rust stable toolchain...' -ForegroundColor Cyan
    rustup default stable
    if ($LASTEXITCODE -ne 0) { throw "rustup default stable failed (exit code $LASTEXITCODE)." }

    rustup target add x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw "rustup target add failed (exit code $LASTEXITCODE)." }

    Write-Host ''
    Write-Host 'Building Bloom Squash Engine...' -ForegroundColor Cyan
    cargo xtask bundle polarity_sc_dark --release --target=x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit code $LASTEXITCODE). See BUILD_WINDOWS.log for details." }

    $bundle = Join-Path $PSScriptRoot 'target\bundled\Bloom Squash Engine.vst3'
    if (-not (Test-Path $bundle)) {
        throw "Build completed but the expected VST3 bundle was not found: $bundle"
    }

    Write-Host ''
    Write-Host 'SUCCESS' -ForegroundColor Green
    Write-Host "VST3: $bundle" -ForegroundColor Green
    Write-Host ''
    Write-Host 'Next: copy the entire Bloom Squash Engine.vst3 folder to:' -ForegroundColor Cyan
    Write-Host 'C:\Program Files\Common Files\VST3\' -ForegroundColor White
    Write-Host 'Then rescan plug-ins in Ableton Live.' -ForegroundColor Cyan

    Start-Process explorer.exe -ArgumentList "/select,`"$bundle`""
}
catch {
    Write-Host ''
    Write-Host 'BUILD FAILED' -ForegroundColor Red
    Write-Host $_.Exception.Message -ForegroundColor Red
    Write-Host ''
    Write-Host "Full log: $log" -ForegroundColor Yellow
}
finally {
    try { Stop-Transcript | Out-Null } catch {}
    Wait-At-End
}
