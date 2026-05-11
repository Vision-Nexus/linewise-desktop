# Bundle FFmpeg DLLs and CLI binary alongside the Windows .exe
# Expects FFMPEG_DIR environment variable pointing to extracted FFmpeg shared build

$ErrorActionPreference = "Stop"

# $PSScriptRoot is <repo>/scripts, so one Split-Path -Parent gets us to
# <repo>. The previous double-parent popped one level too many on GitHub
# Actions runners, where the repo lives at D:\a\<repo>\<repo>.
$Root = Split-Path -Parent $PSScriptRoot
$Root = Resolve-Path $Root

$ReleaseDir = Join-Path $Root "target\x86_64-pc-windows-msvc\release"
$BundleDir = Join-Path $Root "target\x86_64-pc-windows-msvc\release\bundle\msi"

# Find the binary location (either in release dir or msi staging)
$ExeDir = $ReleaseDir
if (-not (Test-Path (Join-Path $ExeDir "linewise-desktop.exe"))) {
    Write-Error "linewise-desktop.exe not found in $ExeDir"
    exit 1
}

$FfmpegDir = $env:FFMPEG_DIR
if (-not $FfmpegDir) {
    Write-Error "FFMPEG_DIR environment variable not set"
    exit 1
}

Write-Host "Bundling FFmpeg from: $FfmpegDir"

# Copy ffmpeg.exe
$FfmpegBin = Join-Path $FfmpegDir "bin\ffmpeg.exe"
if (Test-Path $FfmpegBin) {
    Copy-Item $FfmpegBin -Destination $ExeDir
    Write-Host "  Copied ffmpeg.exe"
} else {
    Write-Warning "ffmpeg.exe not found at $FfmpegBin"
}

# Copy DLLs
$RequiredDlls = @(
    "avcodec-*.dll",
    "avformat-*.dll",
    "avutil-*.dll",
    "swscale-*.dll",
    "swresample-*.dll",
    "avfilter-*.dll",
    "avdevice-*.dll"
)
# postproc is GPL-only; not every FFmpeg build ships it.
$OptionalDlls = @(
    "postproc-*.dll"
)

$BinDir = Join-Path $FfmpegDir "bin"
function Copy-Dlls ($patterns, $required) {
    foreach ($pattern in $patterns) {
        $files = Get-ChildItem -Path $BinDir -Filter $pattern -ErrorAction SilentlyContinue
        if (-not $files) {
            if ($required) {
                Write-Error "No DLL matched $pattern under $BinDir"
                Get-ChildItem -Path $BinDir -Filter "*.dll" | ForEach-Object { Write-Host "  present: $($_.Name)" }
                exit 1
            }
            Write-Host "  Skipped $pattern (not present in this FFmpeg build)"
            continue
        }
        foreach ($file in $files) {
            Copy-Item $file.FullName -Destination $ExeDir
            Write-Host "  Copied $($file.Name)"
        }
    }
}
Copy-Dlls $RequiredDlls $true
Copy-Dlls $OptionalDlls $false

Write-Host "Done bundling FFmpeg into $ExeDir"
