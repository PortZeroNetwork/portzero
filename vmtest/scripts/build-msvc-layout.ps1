#Requires -Version 5
# Build the MSVC Build Tools offline layout into the Mac-side cache over the
# \\Mac\Home share, so it survives VM snapshot reverts and can install offline.
# Only downloads; installs nothing. Idempotent: re-running resumes/repairs.
$ErrorActionPreference = 'Stop'

$layout = '\\Mac\Home\Documents\src\PortZeroNetwork\portzero-local\vmtest\cache\vs_layout'
$boot   = 'C:\Windows\Temp\vs_BuildTools.exe'

$args = @(
    '--layout', $layout,
    '--add', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
    '--add', 'Microsoft.VisualStudio.Component.Windows11SDK.22621',
    '--add', 'Microsoft.VisualStudio.Component.VC.CoreBuildTools',
    '--includeRecommended',
    '--lang', 'en-US',
    '--quiet', '--wait'
)
"Building MSVC layout -> $layout"
$p = Start-Process -FilePath $boot -ArgumentList $args -Wait -PassThru -NoNewWindow
"layout_exit=$($p.ExitCode)"
if (Test-Path $layout) {
    $sz = (Get-ChildItem $layout -Recurse -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum
    "layout_size_gb={0:N2}" -f ($sz / 1GB)
}
