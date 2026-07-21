#Requires -Version 5
# Reproduce the two candidate hang sites of the daemon's Windows trust install,
# each independently time-boxed so a wedge is proven (not just suspected) and
# named. Runs in whatever account prlctl exec uses (SYSTEM in CI parity).
$ErrorActionPreference = 'Continue'

"account=$([System.Security.Principal.WindowsIdentity]::GetCurrent().Name)"

# Run a scriptblock with a hard timeout, in a child job so a true hang cannot
# wedge this script. Prints PHASE=<name> ok=<bool> secs=<n>.
function Test-Phase($name, [scriptblock]$body, $timeoutSec = 25) {
    $job = Start-Job -ScriptBlock $body
    $done = Wait-Job $job -Timeout $timeoutSec
    $sw = $null
    if ($done) {
        $out = Receive-Job $job 2>&1
        "PHASE=$name ok=true secs=<$timeoutSec detail=$out"
    } else {
        Stop-Job $job -ErrorAction SilentlyContinue
        "PHASE=$name ok=false secs>=$timeoutSec WEDGED"
    }
    Remove-Job $job -Force -ErrorAction SilentlyContinue
}

# --- Phase 1: add a cert to LocalMachine\Root (what install_windows_root_store
# does via CertAddEncodedCertificateToStore). .NET X509Store is the same
# CryptoAPI underneath.
Test-Phase 'root_store_add' {
    $cert = New-SelfSignedCertificate -Type Custom -Subject 'CN=PortZero VMTest CA' `
        -KeyUsage CertSign,CrlSign,DigitalSignature -KeyExportPolicy Exportable `
        -CertStoreLocation 'Cert:\LocalMachine\My' -NotAfter (Get-Date).AddDays(1)
    $store = New-Object System.Security.Cryptography.X509Certificates.X509Store('Root','LocalMachine')
    $store.Open('ReadWrite')
    $store.Add($cert)
    $store.Close()
    # cleanup
    $store.Open('ReadWrite'); $store.Remove($cert); $store.Close()
    Remove-Item "Cert:\LocalMachine\My\$($cert.Thumbprint)" -Force -ErrorAction SilentlyContinue
    'added+removed'
}

# --- Phase 2: the certutil -H probe. find_mozilla_certutil_windows runs
# `<candidate> -H` on every non-System32 certutil.exe on PATH. Enumerate them
# and probe each with a timeout to see if any hangs.
$cands = @()
foreach ($dir in ($env:PATH -split ';')) {
    if ([string]::IsNullOrWhiteSpace($dir)) { continue }
    $p = Join-Path $dir 'certutil.exe'
    if (Test-Path $p) { $cands += $p }
}
"certutil_candidates=$($cands.Count)"
foreach ($p in $cands) {
    $isSys = $p -match '(?i)\\(system32|syswow64)\\certutil\.exe$'
    if ($isSys) { "certutil_skip_system=$p"; continue }
    Test-Phase "certutil_H_probe" { & $using:p -H 2>&1 | Out-Null; 'returned' } 15
}
