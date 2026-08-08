#!/usr/bin/env perl
# Minimal HTTP server that THIS process owns the listening socket for (so the
# daemon's port->PID discovery attributes the port to us). Cross-platform via
# core Perl — the Linux/macOS VMs have perl but not python. The caller sets
# PZ_TUNNEL in the environment before launching this so the daemon discovers
# this process as the tagged service.
#   http-echo.pl <body> [port] [bind-address]
#
# The bind address defaults to 127.0.0.1 and exists so a test can stand up a
# service on `::1` ALONE — the shape that used to be discovered correctly and
# then proxied to an empty 127.0.0.1 (see docs/developers/backend-address-
# selection.md). IO::Socket::IP (core since perl 5.20) is what makes an IPv6
# bind possible; IO::Socket::INET is the IPv4-only fallback for older perls.
use strict;
use warnings;

my $body = defined $ARGV[0] ? $ARGV[0] : "ok";
my $port = defined $ARGV[1] ? $ARGV[1] : 18080;
my $bind = defined $ARGV[2] ? $ARGV[2] : '127.0.0.1';

my %args = (
    LocalAddr => $bind,
    LocalPort => $port,
    Proto     => 'tcp',
    Listen    => 16,
    ReuseAddr => 1,
);

my $srv;
if (eval { require IO::Socket::IP; 1 }) {
    # V6Only (IPv6 binds only): a bind to :: must NOT also accept IPv4, so a
    # test asking for an IPv6-only listener gets exactly that.
    $args{V6Only} = 1 if $bind =~ /:/;
    $srv = IO::Socket::IP->new(%args);
} else {
    die "bind $bind:$port failed: IO::Socket::IP is required for a non-IPv4 bind\n"
        if $bind =~ /:/;
    require IO::Socket::INET;
    $srv = IO::Socket::INET->new(%args);
}
$srv or die "bind $bind:$port failed: $!\n";

$| = 1;
print "PORT=$port BIND=$bind\n";

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
