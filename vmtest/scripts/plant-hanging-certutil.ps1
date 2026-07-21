#Requires -Version 5
# Reproduce the FAILURE MODE of the v0.1.0 Windows trust-install hang without
# needing GitHub's exact windows-latest image. trust.rs's
# is_mozilla_certutil_windows() runs `<candidate> -H` on every non-System32
# certutil.exe on PATH and waits on .output() forever if one blocks. We plant a
# certutil.exe that blocks, which is precisely that condition.
#
# Args: -Mode plant | probe | clean
#   plant  : create C:\vmtest-bin\certutil.exe (a stub that hangs) and prepend
#            it to the machine PATH.
#   probe  : mimic the daemon's probe — run the first non-System32 certutil.exe
#            on PATH with `-H`, time-boxed, and report whether it wedges.
#   clean  : remove the stub and PATH entry.
param([ValidateSet('plant','probe','clean')] [string]$Mode = 'probe')

$dir = 'C:\vmtest-bin'
$stub = Join-Path $dir 'certutil.exe'

function Add-PathEntry($d) {
    $p = [Environment]::GetEnvironmentVariable('Path','Machine')
    if ($p -notlike "*$d*") { [Environment]::SetEnvironmentVariable('Path', "$d;$p", 'Machine') }
}
function Remove-PathEntry($d) {
    $p = [Environment]::GetEnvironmentVariable('Path','Machine')
    $new = ($p -split ';' | Where-Object { $_ -and $_ -ne $d }) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $new, 'Machine')
}

switch ($Mode) {
    'plant' {
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        # A tiny C# exe that blocks forever on any invocation — the essence of
        # the hang (a certutil that never returns / never closes its pipes).
        $src = Join-Path $dir 'hang.cs'
        @'
class P { static void Main() { System.Threading.Thread.Sleep(System.Threading.Timeout.Infinite); } }
'@ | Set-Content -Path $src -Encoding ASCII
        $csc = Join-Path ([Runtime.InteropServices.RuntimeEnvironment]::GetRuntimeDirectory()) 'csc.exe'
        & $csc /nologo /out:$stub $src | Out-Null
        Add-PathEntry $dir
        "planted=$stub exists=$(Test-Path $stub)"
    }
    'probe' {
        $cands = @()
        foreach ($d in ([Environment]::GetEnvironmentVariable('Path','Machine') -split ';')) {
            if (-not $d) { continue }
            $p = Join-Path $d 'certutil.exe'
            if ((Test-Path $p) -and ($p -notmatch '(?i)\\(system32|syswow64)\\')) { $cands += $p }
        }
        "nonsystem_certutil=$($cands.Count)"
        foreach ($p in $cands) {
            $job = Start-Job { & $using:p -H 2>&1 | Out-Null }
            if (Wait-Job $job -Timeout 12) { "probe ok (returned): $p" }
            else { Stop-Job $job; "probe WEDGED (>=12s): $p  <-- this is the CI hang" }
            Remove-Job $job -Force
        }
    }
    'clean' {
        Remove-PathEntry $dir
        Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
        "cleaned"
    }
}
