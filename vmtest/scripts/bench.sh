#!/usr/bin/env bash
# VM benchmark/telemetry. Wraps an operation and records HARD numbers to a
# persistent JSONL log: how long it took, how many bytes it wrote to the
# internal SSD (real wear, from NVMe "Data Units Written"), the SSD's running
# wear %, and the change in free space. Also a `report` that summarizes history
# and projects SSD wear-out for the copy-to-internal-per-verify workflow.
#
#   bench.sh record <label> [vm] -- <command...>   time+measure a command
#   bench.sh snapshot <label> [vm]                 record a point-in-time SSD/space sample
#   bench.sh report                                summarize history + wear projection
#   bench.sh ssd                                   print current SSD wear state
#
# NVMe wear metric: "Data Units Written" (1 unit = 512,000 bytes). Reading it
# needs smartctl (brew install smartmontools); no sudo required on this Mac.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
BENCH_DIR="$HERE/../bench"; mkdir -p "$BENCH_DIR"
LOG="$BENCH_DIR/history.jsonl"
DISK="${BENCH_DISK:-/dev/disk0}"

have_smart() { command -v smartctl >/dev/null 2>&1; }
# awk-only parsing: take the first pure-integer field on the line, after stripping
# commas — so "Data Units Written: 337,179,950 [172 TB]" yields 337179950, never
# the "172" from the bracket. (No head/pipe, to avoid pipefail+SIGPIPE aborts.)
# NB: smartctl exits non-zero even on success (its status is a bitmask), so each
# reader ends with `|| true` — otherwise set -e/pipefail abort at `u=$(ssd_units)`.
ssd_units() { have_smart || { echo 0; return 0; }; { smartctl -A "$DISK" 2>/dev/null | awk '/Data Units Written/{for(i=1;i<=NF;i++){gsub(/,/,"",$i); if($i ~ /^[0-9]+$/){print $i; exit}}}'; } || true; }
ssd_pct()   { have_smart || { echo -1; return 0; }; { smartctl -A "$DISK" 2>/dev/null | awk '/Percentage Used/{for(i=1;i<=NF;i++){gsub(/[^0-9]/,"",$i); if($i!=""){print $i; exit}}}'; } || true; }
free_kb()   { df -k / | awk 'NR==2{print $4}'; }
now()       { date +%s; }
iso()       { date -u +%Y-%m-%dT%H:%M:%SZ; }
units_gb()  { awk -v u="${1:-0}" 'BEGIN{printf "%.3f", u*512000/1e9}'; }   # data units -> GB written

append() { printf '%s\n' "$1" >> "$LOG"; }

cmd_record() {
    local label="$1"; shift
    local vm="-"; if [ "${1:-}" != "--" ] && [ $# -gt 0 ]; then vm="$1"; shift; fi
    [ "${1:-}" = "--" ] && shift
    local bu bf t0; bu="$(ssd_units || echo 0)"; bf="$(free_kb)"; t0="$(now)"
    "$@"; local rc=$?
    local t1 au af pct; t1="$(now)"; au="$(ssd_units || echo 0)"; af="$(free_kb)"; pct="$(ssd_pct || echo -1)"
    local wrote_gb dur free_gb
    wrote_gb="$(units_gb $(( au>bu ? au-bu : 0 )))"
    dur=$(( t1 - t0 ))
    free_gb="$(awk -v b="$bf" -v a="$af" 'BEGIN{printf "%.2f",(b-a)/1048576}')"   # +consumed / -freed
    append "$(printf '{"ts":"%s","op":"%s","vm":"%s","duration_s":%d,"ssd_written_gb":%s,"ssd_pct_used":%s,"internal_consumed_gb":%s,"rc":%d}' \
        "$(iso)" "$label" "$vm" "$dur" "$wrote_gb" "$pct" "$free_gb" "$rc")"
    echo "bench: $label — ${dur}s, wrote ${wrote_gb} GB to SSD, internal Δ ${free_gb} GB, wear ${pct}%"
    return $rc
}

cmd_snapshot() {
    local label="${1:-sample}" vm="${2:--}"
    append "$(printf '{"ts":"%s","op":"%s","vm":"%s","ssd_units_written":%s,"ssd_written_tb":%.2f,"ssd_pct_used":%s,"internal_free_gb":%.1f}' \
        "$(iso)" "$label" "$vm" "$(ssd_units || echo 0)" "$(awk -v u="$(ssd_units || echo 0)" 'BEGIN{print u*512000/1e12}')" "$(ssd_pct || echo -1)" "$(awk -v k="$(free_kb)" 'BEGIN{print k/1048576}')")"
    echo "bench snapshot '$label' recorded"
}

cmd_ssd() {
    have_smart || { echo "smartctl not installed"; return 1; }
    local u pct tb; u="$(ssd_units)"; pct="$(ssd_pct)"; tb="$(awk -v u="$u" 'BEGIN{printf "%.1f",u*512000/1e12}')"
    local tbw rem; tbw="$(awk -v tb="$tb" -v p="$pct" 'BEGIN{printf "%.0f", (p>0)?tb/(p/100):0}')"
    rem="$(awk -v tbw="$tbw" -v tb="$tb" 'BEGIN{printf "%.0f", tbw-tb}')"
    printf 'SSD %s: %s%% used, %s TB written lifetime; est rated ~%s TBW, ~%s TB remaining\n' \
        "$DISK" "$pct" "$tb" "$tbw" "$rem"
}

cmd_report() {
    [ -s "$LOG" ] || { echo "no history yet ($LOG)"; return 0; }
    cmd_ssd 2>/dev/null || true
    echo "--- recorded operations (most recent 20) ---"
    awk 'BEGIN{FS="\""} /"op":/{ }' "$LOG" >/dev/null 2>&1
    # Pretty table via python for reliable JSON parsing + wear projection.
    python3 - "$LOG" <<'PY'
import json, sys
rows=[json.loads(l) for l in open(sys.argv[1]) if l.strip()]
ops=[r for r in rows if 'ssd_written_gb' in r]
print(f"{'op':28} {'vm':16} {'dur_s':>6} {'SSD_GB':>7} {'intΔ_GB':>8} {'wear%':>5}")
for r in ops[-20:]:
    print(f"{r['op'][:28]:28} {str(r.get('vm','-'))[:16]:16} {r['duration_s']:>6} {r['ssd_written_gb']:>7} {r['internal_consumed_gb']:>8} {r['ssd_pct_used']:>5}")
# Aggregate per-op averages for the copy/delete cycle.
def avg(name, key):
    xs=[r[key] for r in ops if r['op']==name]
    return sum(xs)/len(xs) if xs else None
copy_gb=avg('copy-win11-to-internal','ssd_written_gb'); copy_s=avg('copy-win11-to-internal','duration_s')
del_s=avg('delete-win11-internal','duration_s')
total_ssd=sum(r['ssd_written_gb'] for r in ops)
print(f"\nTotal SSD written by measured VM ops so far: {total_ssd:.1f} GB")
if copy_gb:
    print(f"Per copy-to-internal: {copy_gb:.1f} GB written, {copy_s:.0f}s"
          + (f"; delete: {del_s:.0f}s" if del_s else ""))
    print(f"\nWear per copy-per-verify cycle (one ~{copy_gb:.0f}GB copy each verify):")
    tbw=820  # est from 172TB=21%
    for per_day in (1,3,10):
        tb_yr=copy_gb*per_day*365/1000
        pct_yr=tb_yr/tbw*100
        print(f"  {per_day:>2}/day -> {tb_yr:5.1f} TB/yr = {pct_yr:4.1f}% of rated endurance/yr "
              f"(~{(79/pct_yr):.0f} yr to exhaust the remaining 79%)")
    print("  vs keep-on-internal permanently: ONE 85GB write, then only per-test deltas (near-zero).")
PY
}

case "${1:-}" in
    record)   shift; cmd_record "$@" ;;
    snapshot) shift; cmd_snapshot "$@" ;;
    report)   cmd_report ;;
    ssd)      cmd_ssd ;;
    *) echo "usage: bench.sh {record <label> [vm] -- <cmd>|snapshot <label> [vm]|report|ssd}" >&2; exit 2 ;;
esac
