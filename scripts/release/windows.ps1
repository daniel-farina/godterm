# Windows release: GodTerm for x86_64 (and aarch64 with -Arch arm64), as a
# portable zip and a per user Inno Setup installer, into dist\. Runs ON a
# Windows machine with Rust (MSVC) and Inno Setup 6; scripts/release/
# windows-remote.sh drives it from a Mac over ssh.
#
#   .\scripts\release\windows.ps1                 x64 zip + installer
#   .\scripts\release\windows.ps1 -Arch arm64     zip only (no arm64 installer yet)
#   .\scripts\release\windows.ps1 -NoInstaller
#
# Signing: if $env:WINDOWS_PFX (path) and $env:WINDOWS_PFX_PASSWORD are set,
# godterm.exe and the installer are Authenticode signed with signtool
# (SHA-256, RFC 3161 timestamp). There is no certificate today, so builds
# ship unsigned (SmartScreen warns on first run). See docs/RELEASING.md.
param(
    [ValidateSet("x64", "arm64")][string]$Arch = "x64",
    [switch]$NoInstaller,
    [string]$Profile = "dist"
)
# "Continue": Windows PowerShell 5.1 turns a native tool's stderr (cargo's
# progress lines) into a terminating error under "Stop" when redirected.
# Every native call checks $LASTEXITCODE instead.
$ErrorActionPreference = "Continue"
$ProgressPreference = "SilentlyContinue"
$Root = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
Set-Location $Root
$env:PATH = "$env:USERPROFILE\.cargo\bin;" + $env:PATH

$Target = if ($Arch -eq "arm64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
$Version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches.Groups[1].Value
$Dist = if ($env:DIST) { $env:DIST } else { Join-Path $Root "dist" }
New-Item -ItemType Directory -Force -Path $Dist | Out-Null
Write-Host "==> GodTerm $Version for $Target ($Profile)"

& rustup target add $Target 2>&1 | Out-Null

$jobs = [Math]::Max(2, [Environment]::ProcessorCount - 2)
& cargo build --locked --profile $Profile --target $Target -j $jobs
if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root "target" }
$Exe = Join-Path $TargetDir "$Target\$Profile\godterm.exe"
if (-not (Test-Path $Exe)) { throw "missing $Exe" }

function Sign-File($Path) {
    if (-not $env:WINDOWS_PFX) { return }
    $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" | Sort-Object FullName | Select-Object -Last 1
    & $signtool.FullName sign /fd SHA256 /td SHA256 /tr http://timestamp.digicert.com /f $env:WINDOWS_PFX /p $env:WINDOWS_PFX_PASSWORD $Path
    if ($LASTEXITCODE -ne 0) { throw "signtool failed for $Path" }
}

# Stage: the exe plus docs (LICENSE.txt so Notepad opens it).
$Name = "godterm-$Version-$Target"
$Stage = Join-Path ([IO.Path]::GetTempPath()) "godterm-stage-$Arch"
Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
$Dir = Join-Path $Stage $Name
New-Item -ItemType Directory -Force -Path $Dir | Out-Null
Copy-Item $Exe (Join-Path $Dir "godterm.exe")
Sign-File (Join-Path $Dir "godterm.exe")
Copy-Item README.md, CHANGELOG.md $Dir
Copy-Item LICENSE (Join-Path $Dir "LICENSE.txt")
& (Join-Path $Dir "godterm.exe") --version
if ($LASTEXITCODE -ne 0) { throw "godterm --version failed" }

$Zip = Join-Path $Dist "$Name.zip"
Remove-Item -Force $Zip -ErrorAction SilentlyContinue
Compress-Archive -Path $Dir -DestinationPath $Zip
Write-Host "==> $Zip"

if (-not $NoInstaller -and $Arch -eq "x64") {
    $iscc = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $iscc) { throw "Inno Setup 6 not found (winget install JRSoftware.InnoSetup)" }
    & $iscc /Q "/DMyAppVersion=$Version" "/DPayload=$Dir" "/DOutDir=$Dist" (Join-Path $Root "packaging\windows\GodTerm.iss")
    if ($LASTEXITCODE -ne 0) { throw "iscc failed ($LASTEXITCODE)" }
    $Setup = Join-Path $Dist "GodTerm-$Version-windows-x64-setup.exe"
    Sign-File $Setup
    Write-Host "==> $Setup"
}
Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
Get-ChildItem $Dist | Where-Object { $_.Name -like "*windows*" } | ForEach-Object { "{0}  {1:N1} MB" -f $_.Name, ($_.Length / 1MB) }
Write-Host "BUILD_RESULT:0"
