#Requires -Version 5
# Minimal HTTP server that a real in-process TCP socket owns (System.Net.Sockets
# .TcpListener binds in THIS process, so the daemon's port->PID discovery
# attributes the listening port to us — unlike HttpListener/http.sys, which
# would show pid 4). Prints "PORT=<n>" once bound, then serves $Body to every
# request until killed. No external deps (the golden VM has no python).
#
# The caller sets $env:PZ_TUNNEL before launching this so the daemon discovers
# this process as the tagged service.
param(
    [string]$Body = "ok",
    [int]$Port = 0            # 0 = ephemeral
)
$ErrorActionPreference = 'Stop'

$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
$listener.Start()
$bound = $listener.LocalEndpoint.Port
Write-Output "PORT=$bound"
[Console]::Out.Flush()

$payload = [System.Text.Encoding]::ASCII.GetBytes($Body)
$header = "HTTP/1.1 200 OK`r`nContent-Type: text/plain`r`nContent-Length: $($payload.Length)`r`nConnection: close`r`n`r`n"
$headerBytes = [System.Text.Encoding]::ASCII.GetBytes($header)

while ($true) {
    $client = $listener.AcceptTcpClient()
    try {
        $stream = $client.GetStream()
        # Drain the request line/headers (best-effort; we don't parse).
        Start-Sleep -Milliseconds 20
        while ($stream.DataAvailable) { $null = $stream.ReadByte() }
        $stream.Write($headerBytes, 0, $headerBytes.Length)
        $stream.Write($payload, 0, $payload.Length)
        $stream.Flush()
    } catch { } finally { $client.Close() }
}
