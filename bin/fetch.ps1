# Build step (Windows): puts bin\herdr-respawn.exe in place. Prefers a local build, then the release binary, then cargo.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
$out = 'bin\herdr-respawn.exe'

if (Test-Path 'target\release\herdr-respawn.exe') {
    Copy-Item 'target\release\herdr-respawn.exe' $out -Force
    exit 0
}

$ver = ((Select-String -Path 'herdr-plugin.toml' -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value)
$name = 'herdr-respawn-windows-x86_64.exe'
$url = "https://github.com/joao-carmassi/herdr-respawn/releases/download/v$ver/$name"
$tmp = "$out.download"
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -UseBasicParsing $url -OutFile $tmp
    $want = ((Invoke-WebRequest -UseBasicParsing "$url.sha256").Content.ToString().Trim() -split '\s+')[0]
    if ((Get-FileHash $tmp -Algorithm SHA256).Hash -ieq $want) {
        Move-Item $tmp $out -Force
        exit 0
    }
    Write-Error "checksum mismatch for $name" -ErrorAction Continue
} catch {
    Write-Host "download failed: $_"
}
Remove-Item $tmp -Force -ErrorAction SilentlyContinue

if (Get-Command cargo -ErrorAction SilentlyContinue) {
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { exit 1 }
    Copy-Item 'target\release\herdr-respawn.exe' $out -Force
    exit 0
}
Write-Host "could not download $url and cargo is missing; install Rust from https://rustup.rs"
exit 1
