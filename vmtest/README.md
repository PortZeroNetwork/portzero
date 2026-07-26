# Parallels VM system-test harness

> The VM control plane now lives in [vmkit](https://github.com/PortZeroNetwork/vmkit)
> (`brew install portzeronetwork/portzero/vmkit`): snapshot ladder, boot policy,
> guarded guest exec, doctor, and the human-setup contract. This repo keeps only
> what is portzero-specific: flavor scripts, provisioning, the installer cache,
> and `./vmkit.conf`. Machine inventory: `~/.config/vmkit/host.conf`.

Host-driven system tests for portzero against real Windows / Linux / macOS
guests, run from the macOS host that owns the Parallels VMs. The point is
coverage the unit/E2E suites can't give: real trust stores, real TUN/wintun
adapters, real DNS, and the real installer (install → use → **uninstall** →
assert nothing is left behind), driven end to end on a genuine OS.

`just vm-test [platform]` is the one recipe you need: one VM reset per
platform runs `install -> local-tunnel test -> upgrade -> cloud-tunnel test ->
uninstall` in a single pass (`vmkit.conf`'s `combined` flavor, backed by
`vmtest/scripts/combined-{linux,macos,windows}.{sh,ps1}`). It installs the
**real package** (Linux `.deb` via `dpkg`, Windows signed MSI via `msiexec`,
macOS an offline `brew install` of a locally-generated formula **plus** the
privileged `setup` flow the Homebrew formula's `post_install` + caveats run)
— the prior released version if one is available, else the new one directly —
proves the real local overlay works, upgrades to the new version and asserts
nothing got duplicated, proves the real cloud tunnel works against staging,
then runs the real uninstaller and asserts every CA / DNS / autostart / TUN
artifact is GONE. See "Running the combined e2e test" below.

For iterating on one phase in isolation without paying for the whole
sequence, `just vm-test-local` / `vm-test-staging` / `vm-test-lifecycle` /
`vm-test-upgrade` run just that slice — see "Ad-hoc single-flavor debugging"
below.

## Why this exists (and its one hard limit)

CI already builds every platform and runs client-interop E2E against staging.
What it *can't* cheaply do is exercise privileged, OS-specific install/runtime
paths interactively and iterate on them in seconds. A local VM can.

**The limit, learned the hard way:** a VM only reproduces a bug to the extent
its image matches the target. The v0.1.0 Windows CI hang (daemon wedged in the
trust-store install) does **not** reproduce on a clean Windows 11 snapshot — it
depends on what GitHub's `windows-latest` image has on `PATH`. So use the VM for
*behaviour you can make faithful* (install the real MSI, create a real wintun
adapter, register a real tunnel), and treat "repro an env-specific CI hang" as a
separate exercise that needs the CI image replicated, or a defensive fix instead.

## Division of responsibility (where the human hands off to the repo)

There is one clean boundary, and it is the **golden powered-off snapshot**:

- **The human owns everything up to and including golden.** Installing the guest
  OS, activating it, and setting up **Parallels Guest Tools** (so `prlctl exec`
  works at all) is manual, one-time, per-VM work. The result is captured as the
  hand-made `golden` snapshot. This is deliberately *not* automated — it needs a
  human at the Parallels GUI, and it changes rarely.
- **Everything after golden is the repo's job.** All provisioning (toolchains,
  the offline caches, Homebrew), every running snapshot (`ready`/`built`/…), and
  every test is scripted under `vmtest/` and checked into `portzero`. Given
  a golden VM on the host, `just vm-…` recipes reproduce the entire rest of the
  state — nothing downstream of golden should require manual GUI steps.

Rule of thumb: if a step needs the Parallels GUI or an OS installer, it belongs
to the human and stops at golden; if it can run over `prlctl exec` / `prlctl`
from the host, it belongs in a script here.

## The reset model (fast, pristine, no boot wait)

Each VM has a hand-made **golden** powered-off snapshot: a clean, configured
baseline (for Windows: the fresh Win11 Pro install). We never test on it.

`just vm-up` reverts to golden, boots once, waits for Parallels Tools, then takes
a **running** snapshot `"<vm>-ready"`. Reverting to a *running* snapshot resumes
from RAM — a fully booted, logged-in OS in ~25s with no boot sequence. That is
the per-test reset point:

```
just vm-up "Windows Pro"      # one-time per session: golden -> boot -> snapshot running state
just vm-reset "Windows Pro"   # between tests: back to clean booted state (~25s)
```

## Running things in the guest

`prlctl exec` runs as `NT AUTHORITY\SYSTEM` on Windows — the same account the
daemon runs as under CI, which is exactly the privilege context most install
bugs live in. Scripts live in `vmtest/scripts/` and run **by UNC path** over the
automatic `\\Mac\Home` share, so nothing is copied into the guest and edits on
the Mac are picked up immediately:

```
just vm-run "Windows Pro" vmtest/scripts/inventory.ps1
just vm-inventory "Windows Pro"      # what dev tooling the guest already has
just vm-repro-trust "Windows Pro"    # the trust-install hang probe
```

## Running the combined e2e test

`just vm-test <platform>` resets that VM to its `built` checkpoint and runs
the combined E2E (`vmkit.conf`'s `combined` flavor:
`vmtest/scripts/combined-{linux,macos,windows}.{sh,ps1}`): install →
local-tunnel test → upgrade → cloud-tunnel test → uninstall, all in one pass.
`just vm-test` with no argument runs windows, then linux, then macos in
series — `vmkit` stops any other running VM before each reset, so only one
guest is ever up at a time.

```
just vm-test            # all three, in series
just vm-test windows
just vm-test linux
just vm-test macos
```

Needs the real artifact staged where the combined scripts look (Linux
`vmtest/.downloaded-artifacts/linux/*.deb`, Windows
`vmtest/.downloaded-artifacts/windows/*.msi`, macOS the host-built binary,
pushed in automatically via `vmkit.conf`'s `VMKIT_ARTIFACT_*`) —
`vm-e2e.yml` stages these; locally, drop the artifact in or set
`PORTZERO_DEB`/`PORTZERO_MSI`/`PORTZERO_EXE` in the guest. The prior-version
artifact (`PORTZERO_DEB_OLD`/`PORTZERO_MSI_OLD`/`PORTZERO_EXE_OLD`) is
fetched from the latest published release via `gh` if unset; the upgrade
step SKIPs cleanly (never a false fail) when none is available. The
cloud-tunnel step needs the rotated seed token (see `STAGING_SECRETS`/
`VMKIT_SECRETS_HOST_DEFAULT` below) and SKIPs cleanly if none is found; set
`COMBINED_SKIP_CLOUD=1` to skip that step outright. Once a seed token *is*
present, an unreachable staging is a **failure** (`PHASE=cloud-reachable
ok=false`), not a SKIP — a dead network or a down staging must never pass as
green with the cloud leg silently unrun. The probe retries three times before
declaring it down.

Each script prints greppable `PHASE=<name> ok=<true|false|SKIP>` lines and a
final `RESULT=PASS|FAIL`; a leftover artifact or a broken tunnel yields
`ok=false` and fails the run.

For anything other than the default script/checkpoint, use
`just vm-test-script "<vm>" vmtest/scripts/<name>.ps1 [checkpoint] [args...]`.

## Ad-hoc single-flavor debugging

`just vm-test-local` / `vm-test-staging` / `vm-test-lifecycle` /
`vm-test-upgrade` / `vm-test-autoupdate [platform]` run one slice in isolation,
useful when iterating on a single phase without paying for the whole
sequence:

| Recipe               | What it runs                                                            |
|-----------------------|--------------------------------------------------------------------------|
| `vm-test-local`       | overlay smoke (TUN + DNS + proxy) against the raw pushed binary — no install, no cloud |
| `vm-test-staging`     | cloud-tunnel smoke against the raw pushed binary — no install |
| `vm-test-lifecycle`   | real install → verify → uninstall → assert-clean (no upgrade, no tunnel tests) |
| `vm-test-upgrade`     | prior-version → new-version install, assert nothing duplicated (no tunnel tests) |
| `vm-test-autoupdate`  | real `portzero update` self-updater: detect → download → unpack → in-place swap against a throwaway local release mirror |

```
just vm-test-local linux
just vm-test-upgrade windows
just vm-test-autoupdate linux
```

The `local`/`staging`/`lifecycle`/`upgrade` flavors are the building blocks
`combined` fuses together; `autoupdate` is a standalone hermetic flavor (its own
reset per platform, run by `vm-e2e.yml` after the combined pass). All map
straight to `vmkit series <flavor> <platform>` for the flavors declared in
`vmkit.conf`.

### Self-update coverage (`autoupdate`)

`autoupdate-{unix.sh,windows.ps1}` exercise the actual `portzero update` command
— the applier the update notice points to — end to end, without a second build
or any network. Each builds a throwaway local release mirror (a `version.json`
in the exact `{"version","commit"}` shape `release.yml` publishes, plus a
`portzero-<os>-<arch>` archive) in a temp dir and points the updater at it via
`PZ_TUNNEL_UPDATE_BASE_URL`, then asserts:

- **detect** — `update --check` against a bumped manifest reports an update is available;
- **apply** — `update --force` downloads, unpacks (system `tar` / `Expand-Archive`),
  and swaps the binary in place; the result runs and its bytes match the archive
  (proof of a real swap: a new inode on Unix, a `portzero.old` rename-aside on Windows);
- **noop** — `update` against a same-version manifest reports up-to-date and swaps nothing;
- **crossver** *(gated)* — a genuinely older binary passed via `PORTZERO_EXE_OLD`
  self-updates and its `--version` advances. This SKIPs until a released build that
  already understands `update` exists — which is exactly the forward-compatibility
  contract the test protects: an installed binary must keep being able to update
  itself against whatever we publish next.

Runs are hermetic — isolated temp dirs and `PZ_TUNNEL_UPDATE_SKIP_SETUP=1`, so no
system integration is touched (the combined flavor already covers the privileged
install/setup path).

## Lifecycle coverage: install → use → UNINSTALL → assert-clean

The install→uninstall→assert-clean half of `vm-test`'s combined flow is the
coverage a launch audit flagged as missing: the whole
download→install→use→**uninstall** path against the **real installer**, with
every uninstall step *asserted*. Leftover trusted-CA or DNS-resolver residue
after uninstall is the loud-complaint bug class, so it is proven gone, not
assumed. To exercise just this slice in isolation (no upgrade, no tunnel
tests), use `just vm-test-lifecycle [platform]`, which runs
`lifecycle-<os>.{sh,ps1}` — the same logic `combined-<os>` folds in.

Per platform, `lifecycle-<os>.{sh,ps1}` install the real artifact, verify it
landed, run the real uninstaller, then assert the artifacts are removed:

| Platform | Install (real)                | Asserts landed → **then GONE**                                              |
|----------|-------------------------------|-----------------------------------------------------------------------------|
| Linux    | `dpkg -i` the built `.deb`    | binary, `setcap` caps, `/etc/hosts` pin, systemd unit; CA in system trust store + `/etc/ssl` + `ca-certificates.conf`; `~/.config` autostart unit; tray binary, app-launcher entry (`/usr/share/applications/portzero.desktop`), tray XDG autostart entry (`/etc/xdg/autostart/portzero-tray.desktop`) |
| Windows  | `msiexec /i` the signed MSI   | binary, scheduled task, CA in `LocalMachine\Root`, `.portzero.local` NRPT rule, Wintun adapter; tray binary, tray autostart (`HKLM…\Run\PortZeroTray`), Start Menu shortcut |
| macOS    | **offline `brew install`** of a generated local formula, **then** privileged `portzero setup` | brew phase: binary linked onto PATH under the brew prefix + `post_install` CA generated + caveats surface `sudo portzero setup`, then `brew uninstall` removes it; tray binary + per-user LaunchAgent plist when a tray build is available; setup phase: CA in System keychain, root LaunchDaemon, `/etc/resolver/portzero.local`, `/etc/hosts` pin — **then GONE** |

**Tray icon + app-launcher coverage.** All three platforms now assert the
packaging that makes "you get a tray icon without further intervention" true:
the tray binary lands, and the OS-native autostart registration for it exists
(systemd XDG autostart entry on Linux, the `HKLM…\Run` key on Windows, a
per-user LaunchAgent plist on macOS) — plus, on Linux and Windows, an
app-launcher entry (`.desktop` file / Start Menu shortcut) for the desktop app.
Whether the tray process is *actually running* right after install can't be a
hard assert on any platform — it depends on a live logged-in desktop session
existing at install time, which a headless `prlctl exec` install can't
guarantee — so that one check reports `ok=SKIP` rather than failing the run
when no such session is present (same idiom as `install-launchdaemon-loaded`
below). macOS ships no `.app`/Applications entry at all (Homebrew CLI-only
delivery — see the caveats below), so there's no "in Launchpad" equivalent to
test there; the tray's LaunchAgent is the closest analogue and needs a
`portzero-tray` build staged into the generated formula (`PORTZERO_TRAY_EXE`,
defaulted from `vm-e2e.yml`'s macOS build step) or its checks SKIP cleanly.

Each prints greppable `PHASE=<name> ok=<true|false>` lines and a final
`RESULT=PASS|FAIL`; a leftover artifact yields `ok=false` and fails the run. The
scripts read the artifact from where `vm-e2e.yml` stages it
(`vmtest/.downloaded-artifacts/<os>/`), or from `PORTZERO_DEB` / `PORTZERO_MSI`.

```
just vm-test-lifecycle            # all three, in series
just vm-test-lifecycle linux
```

(This is the `lifecycle` slice in isolation — `vm-test` runs the same logic as
part of the combined flow.)

**macOS delivery — what's real, what's not (honest scope).** macOS ships via
Homebrew (a release `.tar.gz` + tap formula), *not* a `.pkg`. `lifecycle-macos.sh`
now exercises the real install **mechanism** fully offline: it tars the host-built
binary into a `portzero-darwin-<arch>.tar.gz`, generates a formula whose `url` is
a `file://` path to it and whose `sha256` is computed from it — mirroring the
shipped formula's `install` / `post_install` / `caveats` blocks — then runs
`brew install --formula <generated.rb>` with all Homebrew network access disabled,
asserting the binary lands on PATH, `post_install` generated the CA, and the
caveats surface `sudo portzero setup`, then `brew uninstall` removes it. After
that it runs the privileged `sudo portzero setup` residue lifecycle (CA-trust /
LaunchDaemon / resolver / hosts) and asserts every artifact is removed.

Two honest caveats remain:
- **Homebrew-in-snapshot prerequisite.** The brew phase needs `brew` present in
  the golden macOS snapshot. If it is absent the phase SKIPs cleanly
  (`PHASE=brew-install ok=SKIP reason="Homebrew not installed in golden
  snapshot"`) and the privileged residue lifecycle still runs — the run never
  falsely fails.
- **Published tarball, not exercised here.** We install from a *local* file, so
  the *real published* GitHub URL + its formula `sha256` are **not** verified in
  the VM. That check lives in `release.yml` as a post-publish smoke (see below) —
  the one thing a network-pristine guest genuinely can't cover. There is no macOS
  `.pkg` build to test because none is shipped.

**Where each piece actually runs.** The Linux `.deb` package lifecycle
(postinst/prerm, hosts pin, `setcap`, unit) and the Linux upgrade dup-checks are
validated on hosted CI too (`ci.yml`: `lifecycle-upgrade-linux`, and the
`real_tun_overlay_trust_lifecycle` Rust e2e that install/verify/uninstalls the CA
on Linux). Windows MSI + macOS `brew`/setup lifecycles and the Windows/macOS
**upgrade** dup-checks run only on this Parallels runner (`vm-e2e.yml`), the only
place with a real MSI-capable Windows guest and a real macOS keychain/LaunchDaemon.
The **published** Homebrew formula's URL + sha256 are verified post-publish in
`release.yml` (`verify-homebrew-artifacts`) — see the upgrade/release notes below.

## Upgrade coverage: prior version → new version (all three platforms)

The upgrade step of `vm-test`'s combined flow installs a **prior** released
version, then the new one over the top, and asserts the in-place upgrade
DUPLICATED nothing. To exercise just this slice in isolation (no tunnel
tests), use `just vm-test-upgrade [platform]`, which runs
`upgrade-{linux,windows,macos}.{sh,ps1}` — the same logic `combined-<os>`
folds in:

| Platform | Prior install → new install | Asserts (exactly one of each; no side-by-side) |
|----------|-----------------------------|------------------------------------------------|
| Linux    | prior `.deb` → new `.deb` via `dpkg -i` | one `/etc/hosts` pin, one systemd unit (a postinst that appends instead of replacing is how you get two) |
| Windows  | prior MSI → new MSI via `msiexec /i`    | WiX `<MajorUpgrade>` replaced in place: one installed product with the `UpgradeCode`, one scheduled task, one `.portzero.local` NRPT rule, one Wintun adapter; installed binary is the new `ProductVersion` |
| macOS    | prior `setup` → new `setup`             | one `cloud.portzero.*` LaunchDaemon, one `/etc/resolver/portzero.local`, one System-keychain CA, one `/etc/hosts` pin; LaunchDaemon references the new binary |

The prior artifact is fetched from the **latest published release** via `gh`, or
pinned with `PORTZERO_DEB_OLD` / `PORTZERO_MSI_OLD` / `PORTZERO_EXE_OLD`. Each
script `SKIP`s cleanly (never a false pass) when no prior artifact is available —
before the first release, or a network-pristine guest without `gh`. The Windows
upgrade additionally SKIPs when the prior and new MSI carry the **same**
`ProductVersion` (`<MajorUpgrade>` needs a version bump). This runs as part of
`just vm-test` on all three platforms (wired into `vm-e2e.yml`), or in
isolation with `just vm-test-upgrade`; Linux also runs on hosted CI
(`ci.yml`: `lifecycle-upgrade-linux`).

## Release-time smoke: published Homebrew formula (url + sha256)

The one check a VM can't do offline lives in `release.yml`
(`verify-homebrew-artifacts`, post-publish): it checks out the pushed tap formula
(`PortZeroNetwork/homebrew-portzero`), and for each arch confirms the formula's
**real published** GitHub `url` actually resolves *and* its declared `sha256`
matches the downloaded tarball — catching the `0000…` placeholder `sha256` never
getting filled in, a formula whose version didn't get bumped, or a broken release
asset URL.

## Offline provisioning (metered-connection friendly)

**Cache-first — the rule for every download.** This host is often on a *metered*
connection, so **nothing may be downloaded twice.** Before adding or running any
step that fetches from the network, check whether it is already cached and skip
the download if so. There are two cache forms, by what the vendor allows:

- **File-cacheable** (rustup, Rust toolchains, git, VS Build Tools, apt debs):
  staged as files in `vm-toolchain-cache/` and installed from there offline. Every
  fetch goes through `fetch-installers.sh`, which **skips anything already present**
  — add new downloads there, never inline in a provisioning script.
- **Not file-cacheable** (macOS **Xcode CLT** and **Homebrew** — Apple/Homebrew
  only vend them as live installs, and the macOS guest can't read the shared cache
  anyway; see the macOS notes): the cache form is the **VM snapshot** with them
  already installed, plus its 4 TB `.pvm` mirror. They are installed **once** by
  `just vm-macos-add-brew` and baked into `portzero-built`; the test path only
  reverts to that snapshot and downloads nothing. The install scripts
  (`macos-install-clt.sh`, `macos-install-homebrew.sh`) are idempotent — they
  detect an existing install and skip — so that snapshot IS the cache check.

If you find yourself about to `curl`/`softwareupdate`/`brew` from a script, first
ask: is this cached (as a file, or in the snapshot)? If yes, restore/skip; if it's
a genuinely new one-time download, route it through the cache so the next run is free.

Installers are downloaded **once** to a cross-repo cache on the 4 TB drive,
`/Volumes/MBP-Sidecar/loumtech/vm-toolchain-cache/` (shared into every guest; see
that dir's `README.md` for the full layout and guest mount paths). The cache
survives snapshot reverts, so guests install from it offline and metered sessions
never re-download. macOS's Rust toolchain lives on the **internal** disk
(`~/Parallels/vm-toolchain-cache/macos/`, macOS wants fast storage) and is mirrored
to the 4 TB by `just vm-sync-macos`.

```
TIER=toolchain just vm-fetch    # rustup + git + VS bootstrapper + Rust toolchains
# then the multi-GB parts that need a guest, once:
just vm-run "Windows Pro" vmtest/scripts/build-msvc-layout.ps1   # MSVC offline layout
```

Tiers:
- `artifact` — nothing pre-downloaded; test the CI-built `portzero.exe`/MSI,
  pulled per-run with `gh run download`. Minimal data.
- `toolchain` — Rust + git + MSVC layout / Ubuntu debs, so the daemon builds
  **inside the VM** fully offline (no CI round-trip for platform-only iteration).
  macOS is intentionally artifact-only — the host builds macOS natively and the
  macOS VM stays close to a real end-user machine for install/uninstall testing.

### Homebrew on the macOS guest (one-time, baked into the snapshot)

The macOS guest ships with **Homebrew** (and its Command Line Tools dependency)
baked into its `portzero-built` reset point, so `just vm-test macos` / the CI
VM-E2E job can exercise the real `brew install portzero` path. It is installed
**once** by `just vm-macos-add-brew` — a metered-connection concern, so the test
path never touches it: `vm-test` only *reverts* to the snapshot, downloading
nothing.

Three snapshots make the one-time download permanent (nothing pristine is lost):

| Snapshot             | State              | Role                                        |
|----------------------|--------------------|---------------------------------------------|
| `portzero-built`     | CLT + Homebrew     | reset point `vm-test`/CI revert to (churns) |
| `portzero-toolchain` | CLT + Homebrew     | **permanent anchor** — the durable cache of the download; never auto-overwritten |
| `portzero-pre-brew`  | pristine, no CLT   | clean pre-Homebrew baseline, preserved      |

`portzero-built` is the working reset point, and a future re-provision
(`vm-checkpoint built`) replaces it — so `portzero-toolchain` exists as a
never-touched anchor holding the identical CLT+Homebrew state. **If `built` is
ever clobbered, revert to `portzero-toolchain` and re-checkpoint `built` — no
re-download, ever.** The golden powered-off `MacOS 15.7.7` snapshot is also
untouched. After `vm-macos-add-brew`, power the VM off and `just vm-sync-macos`
to mirror the image (all three snapshots) to the 4 TB drive as the off-machine
backup of the cache. To rebuild Homebrew from scratch (rare), revert to
`portzero-pre-brew` and re-run `just vm-macos-add-brew`.

Under the hood `vm-macos-add-brew` is now just a `vmkit provision` call (the
reusable "reset → run a guest script → re-checkpoint" primitive; see
`$(brew --prefix)/opt/vmkit/libexec/docs/PROVISIONING.md`) — the same primitive
the Windows Defender step below uses.

### Windows Defender exclusions (one-time, baked into the snapshot)

The Windows VM E2E path installs an **intentionally unsigned** MSI (signing is
release-only) straight off the `\\Mac\Home` share, so the installed binary
inherits Mark-of-the-Web and Windows Defender's heuristic engine blocks it from
launching:

```
Start-Process : ... the file contains a virus or potentially unwanted software.
```

The fix is a one-time Defender **exclusion** for the install dir
(`%ProgramFiles%\Port Zero`), the artifact drop dir, and `portzero.exe`. Like
Homebrew on macOS, it must survive the per-test reset, so it's baked into
`portzero-built` **once** and every `just vm-test windows` reset inherits it:

```
just vm-windows-add-defender-exclusions     # bake exclusions into portzero-built
```

That recipe is a `vmkit provision` call
(`vmtest/scripts/windows-add-defender-exclusions.ps1`, `--checkpoint built
--label defender`): it resets to `built`, preserves the pristine pre-exclusion
state once as `portzero-built-pre-defender`, applies the exclusions in the
guest (idempotently, via `Add-MpPreference`), then re-captures `built`. The
guest script SKIPs cleanly if the Defender module is absent, and names Tamper
Protection / policy as the likely cause if an exclusion won't stick. This is a
**test-VM convenience for unsigned pre-release artifacts** — real users install
a signed MSI Defender already trusts, so nothing here ships to users.

> **Note.** This is a test-only convenience, not a product safety toggle. It is
> deliberately a *manual, one-time* provisioning step (it mutates VM/snapshot
> state), never a CI step on the self-hosted runner — same as `vm-macos-add-brew`.

## Adding a system test

1. Write `vmtest/scripts/<name>.ps1` (or `.sh` for Linux/macOS guests). Make it
   print `KEY=value` / `PHASE=… ok=…` lines — greppable assertions, not prose.
2. Time-box anything that can hang (see `repro-trust-install.ps1`: each phase
   runs in a child job with `Wait-Job -Timeout`, so a real wedge is *proven and
   named*, never an indefinite stall).
3. Reset first, then run: `just vm-reset "<vm>" && just vm-run "<vm>" vmtest/scripts/<name>.ps1`.
4. Keep guest state out of the repo; keep the script and its expected output in.

## VMs

| VM            | Guest        | Use                                        |
|---------------|--------------|--------------------------------------------|
| `Windows Pro` | Windows 11   | real MSI install/uninstall, trust store, wintun, tunnels |
| `Ubuntu Linux`| Linux        | real `.deb` install/uninstall, CAP_NET_ADMIN, /dev/net/tun |
| `macOS 15.7.7`| macOS        | offline `brew install` of a generated local formula + privileged `setup`/teardown (CA keychain, LaunchDaemon, resolver), utun, upgrade dup-checks — no `.pkg` is shipped, so none is tested; see the macOS delivery scope above |

Snapshot convention is the same for all three: a golden powered-off baseline
plus a `"<vm>-ready"` running snapshot as the reset point.
