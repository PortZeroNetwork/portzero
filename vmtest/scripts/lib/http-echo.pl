#!/usr/bin/env perl
# Minimal HTTP server that THIS process owns the listening socket for (so the
# daemon's port->PID discovery attributes the port to us). Cross-platform via
# core Perl (IO::Socket::INET) — the Linux/macOS VMs have perl but not python.
# The caller sets PZ_TUNNEL in the environment before launching this so the
# daemon discovers this process as the tagged service.
#   http-echo.pl <body> [port]
use strict;
use warnings;
use IO::Socket::INET;

my $body = defined $ARGV[0] ? $ARGV[0] : "ok";
my $port = defined $ARGV[1] ? $ARGV[1] : 18080;

my $srv = IO::Socket::INET->new(
    LocalAddr => '127.0.0.1',
    LocalPort => $port,
    Proto     => 'tcp',
    Listen    => 16,
    ReuseAddr => 1,
) or die "bind 127.0.0.1:$port failed: $!\n";

$| = 1;
print "PORT=$port\n";

my $resp = "HTTP/1.1 200 OK\r\n"
    . "Content-Type: text/plain\r\n"
    . "Content-Length: " . length($body) . "\r\n"
    . "Connection: close\r\n\r\n"
    . $body;

while (my $client = $srv->accept) {
    # Best-effort drain of the request headers, time-bounded so a client that
    # holds the connection open can't wedge the server.
    eval {
        local $SIG{ALRM} = sub { die "timeout\n" };
        alarm 1;
        while (my $line = <$client>) { last if $line =~ /^\r?\n$/; }
        alarm 0;
    };
    print $client $resp;
    close $client;
}
