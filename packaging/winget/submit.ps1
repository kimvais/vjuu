# Submits vjuu <version> to microsoft/winget-pkgs from the manifests next to this script, without komac's prompts.
# Run after the GitHub release exists: ./packaging/winget/submit.ps1 0.3.3 [-DryRun]
param(
    [Parameter(Mandatory)][string]$Version,
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
$base = "https://github.com/kimvais/vjuu/releases/download/v$Version"
$tmp = New-Item -ItemType Directory -Path (Join-Path ([IO.Path]::GetTempPath()) "vjuu-winget-$Version-$PID")
$sha = @{}
foreach ($arch in 'x86_64', 'aarch64') {
    $zip = Join-Path $tmp "$arch.zip"
    Invoke-WebRequest "$base/vjuu-v$Version-$arch-pc-windows-msvc.zip" -OutFile $zip
    $sha[$arch] = (Get-FileHash $zip -Algorithm SHA256).Hash
    Remove-Item $zip
}
$out = New-Item -ItemType Directory -Path (Join-Path $tmp 'manifests')
foreach ($file in Get-ChildItem (Join-Path $PSScriptRoot 'manifest') -Filter *.yaml) {
    $text = (Get-Content $file -Raw).
        Replace('@VERSION@', $Version).
        Replace('@DATE@', (Get-Date -Format 'yyyy-MM-dd')).
        Replace('@SHA_X64@', $sha['x86_64']).
        Replace('@SHA_ARM64@', $sha['aarch64'])
    if ($text -match '@\w+@') { throw "Unfilled placeholder $($Matches[0]) in $($file.Name)" }
    [IO.File]::WriteAllText((Join-Path $out $file.Name), $text)
}
if ($DryRun) { komac submit $out --dry-run } else { komac submit $out --yes }
"Manifests: $out"
