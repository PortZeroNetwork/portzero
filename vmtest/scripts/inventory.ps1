#Requires -Version 5
# Report what dev tooling is already present in the guest, so provisioning only
# downloads what is genuinely missing. Prints one KEY=VALUE line per tool.
$ErrorActionPreference = 'SilentlyContinue'

function Report($name, $cmd) {
    $c = Get-Command $cmd -ErrorAction SilentlyContinue
    if ($c) { "{0}={1}" -f $name, $c.Source } else { "{0}=MISSING" -f $name }
}

Report 'cargo'    'cargo'
Report 'rustc'    'rustc'
Report 'rustup'   'rustup'
Report 'git'      'git'
Report 'python'   'python'
Report 'uv'       'uv'
Report 'cl'       'cl'
Report 'link'     'link'

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (Test-Path $vswhere) {
    $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($vs) { "msvc_vctools=$vs" } else { "msvc_vctools=MISSING" }
} else {
    "msvc_vctools=MISSING"
}

# All certutil.exe on PATH: Windows ships one in System32; Mozilla NSS ships its
# own. trust.rs's NSS path needs the Mozilla one to appear first. Order matters.
$cu = (Get-Command certutil.exe -All -ErrorAction SilentlyContinue).Source
"certutil_count=" + @($cu).Count
foreach ($p in $cu) { "certutil_path=$p" }

# NSS databases the trust installer would scan (Firefox et al.).
$roots = @(
    (Join-Path $env:APPDATA 'Mozilla\Firefox\Profiles'),
    (Join-Path $env:USERPROFILE '.pki\nssdb')
)
$nss = 0
foreach ($r in $roots) {
    if (Test-Path $r) {
        Get-ChildItem $r -Recurse -Filter 'cert9.db' -ErrorAction SilentlyContinue | ForEach-Object { $nss++; "nss_db=$($_.DirectoryName)" }
    }
}
"nss_db_count=$nss"
