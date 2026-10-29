#!/usr/bin/env bash
# T15/L0 shell tests for scripts/host-tune.sh and scripts/host-doctor.sh.
#
# No root, no live host mutation: every destination and every /proc read is
# redirected into a temporary directory, sysctl/systemctl/ss are fake scripts.
# Run: bash scripts/tests/test_host_tune.sh
#
# This is a plain-bash test (the repo has no bats dependency); it prints
# `ok - name` / `not ok - name` lines and exits non-zero on any failure.

set -uo pipefail

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
TUNE="$REPO_ROOT/scripts/host-tune.sh"
DOCTOR="$REPO_ROOT/scripts/host-doctor.sh"
SYSCTL_SRC="$REPO_ROOT/deploy/host/sysctl/90-vks-mcu.conf"
USER_DROPIN_SRC="$REPO_ROOT/deploy/host/systemd/user/vks-mcu.service.d/10-l0-resources.conf"
PROD_DROPIN_SRC="$REPO_ROOT/deploy/host/systemd/vks-mcu-prod.service.d/10-l0-resources.conf"
SLICE_SRC="$REPO_ROOT/deploy/host/systemd/vks.slice"

TMP=$(mktemp -d)
trap 'rm -rf -- "$TMP"' EXIT

PASSED=0
FAILED=0

ok() {
    PASSED=$((PASSED + 1))
    printf 'ok - %s\n' "$1"
}

not_ok() {
    FAILED=$((FAILED + 1))
    printf 'not ok - %s\n' "$1"
}

# ---------------------------------------------------------------------------
# fixtures
# ---------------------------------------------------------------------------

make_fake_root() {
    local root=$1 key value path
    rm -rf -- "$root"
    while IFS=$'\t' read -r key value; do
        path="$root/proc/sys/${key//./\/}"
        mkdir -p -- "$(dirname -- "$path")"
        printf '%s\n' "$value" >"$path"
    done < <(awk '
        /^[[:space:]]*#/ { next }
        /^[[:space:]]*$/ { next }
        {
            line = $0; sub(/[[:space:]]*#.*$/, "", line)
            eq = index(line, "="); if (eq == 0) next
            key = substr(line, 1, eq - 1); val = substr(line, eq + 1)
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", key)
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", val)
            print key "\t" val
        }
    ' "$SYSCTL_SRC")
}

make_fake_bins() {
    local dir=$1
    mkdir -p -- "$dir"
    cat >"$dir/sysctl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${FAKE_SYSCTL_LOG:?}"
exit 0
EOF
    cat >"$dir/systemctl" <<'EOF'
#!/usr/bin/env bash
args=("$@")
if [ "${args[0]:-}" = "--user" ]; then
    args=("${args[@]:1}")
fi
command=${args[0]:-}
case "$command" in
    is-active)
        exit "${FAKE_IS_ACTIVE:-0}"
        ;;
    show)
        unit=${args[1]:-}
        prop=${args[3]:-}
        if [ "$unit" = "vks.slice" ]; then
            var="FAKE_SLICE_${prop}"
        else
            var="FAKE_${prop}"
        fi
        value=${!var:-[not set]}
        printf '%s=%s\n' "$prop" "$value"
        ;;
    daemon-reload)
        printf 'daemon-reload\n' >>"${FAKE_SYSTEMCTL_LOG:?}"
        ;;
esac
exit 0
EOF
    cat >"$dir/ss" <<'EOF'
#!/usr/bin/env bash
cat "${FAKE_SS_OUT:?}"
EOF
    chmod +x "$dir/sysctl" "$dir/systemctl" "$dir/ss"
}

# systemd size -> bytes (mirrors host-doctor.sh `to_bytes`, test-side copy).
to_bytes_test() {
    local raw=$1 num suffix
    case "$raw" in
        '' | *[!0-9KkMmGgTt]*) printf '%s' "$raw"; return 0 ;;
    esac
    num=${raw%[KkMmGgTt]}
    suffix=${raw#"$num"}
    case "$suffix" in
        K | k) num=$((num * 1024)) ;;
        M | m) num=$((num * 1024 * 1024)) ;;
        G | g) num=$((num * 1024 * 1024 * 1024)) ;;
        '') : ;;
        *) printf '%s' "$raw"; return 0 ;;
    esac
    printf '%s' "$num"
}

artifact_value() {
    local file=$1 key=$2
    awk -v key="$key" '
        /^\[/ { insec = ($0 == "[Service]" || $0 == "[Slice]") }
        insec && index($0, key "=") == 1 { sub(/^[^=]*=/, ""); print; exit }
    ' "$file"
}

# Export FAKE_* properties so the fake systemctl matches an artifact.
export_fake_props() {
    local artifact=$1
    local memory_high memory_max cpu_weight io_weight nofile nice policy slice
    memory_high=$(artifact_value "$artifact" MemoryHigh)
    memory_max=$(artifact_value "$artifact" MemoryMax)
    cpu_weight=$(artifact_value "$artifact" CPUWeight)
    io_weight=$(artifact_value "$artifact" IOWeight)
    nofile=$(artifact_value "$artifact" LimitNOFILE)
    nice=$(artifact_value "$artifact" Nice)
    policy=$(artifact_value "$artifact" CPUSchedulingPolicy)
    slice=$(artifact_value "$artifact" Slice)
    export FAKE_MemoryHigh="$(to_bytes_test "$memory_high")"
    export FAKE_MemoryMax="$(to_bytes_test "$memory_max")"
    [ -n "$cpu_weight" ] && export FAKE_CPUWeight="$cpu_weight" || export FAKE_CPUWeight='[not set]'
    [ -n "$io_weight" ] && export FAKE_IOWeight="$io_weight" || export FAKE_IOWeight='[not set]'
    export FAKE_LimitNOFILE="$nofile"
    export FAKE_Nice="$nice"
    case "$policy" in
        other) export FAKE_CPUSchedulingPolicy=0 ;;
        fifo) export FAKE_CPUSchedulingPolicy=1 ;;
        rr) export FAKE_CPUSchedulingPolicy=2 ;;
        *) export FAKE_CPUSchedulingPolicy="${policy:-0}" ;;
    esac
    [ -n "$slice" ] && export FAKE_Slice="$slice" || export FAKE_Slice='[not set]'
}

make_fake_proc_pid() {
    local root=$1 pid=$2
    mkdir -p -- "$root/proc/$pid"
    printf 'Max open files            65536                65536                files\n' >"$root/proc/$pid/limits"
    printf 'VKS_UDP_RCVBUF_BYTES=65536\0VKS_GATEWAY_UDP_RCVBUF_BYTES=65536\0' >"$root/proc/$pid/environ"
}

write_ss_fixture() {
    local file=$1 pid=$2 rb_media=$3 rb_gateway=$4
    cat >"$file" <<EOF
UNCONN 0 0 0.0.0.0:40000 0.0.0.0:* users:(("vks-server",pid=$pid,fd=12))
	 skmem:(r0,rb$rb_media,t0,tb212992,f0,w0,o0,bl0,d0)
UNCONN 0 0 0.0.0.0:41000 0.0.0.0:* users:(("vks-server",pid=$pid,fd=14))
	 skmem:(r0,rb$rb_gateway,t0,tb212992,f0,w0,o0,bl0,d0)
EOF
}

# ---------------------------------------------------------------------------
# 0. syntax / static checks
# ---------------------------------------------------------------------------

if bash -n "$TUNE" && bash -n "$DOCTOR"; then
    ok "bash -n host-tune.sh + host-doctor.sh"
else
    not_ok "bash -n"
fi

if command -v shellcheck >/dev/null 2>&1; then
    if shellcheck -x "$TUNE" "$DOCTOR" >/dev/null 2>&1; then
        ok "shellcheck host-tune.sh + host-doctor.sh"
    else
        not_ok "shellcheck reported findings"
        shellcheck -x "$TUNE" "$DOCTOR" || true
    fi
else
    printf '# shellcheck not installed: static shell lint skipped (bash -n + behaviour tests still run)\n'
fi

BINS="$TMP/bin"
make_fake_bins "$BINS"

# ---------------------------------------------------------------------------
# 1. host-tune check: pass / drift / missing key (read-only, no root)
# ---------------------------------------------------------------------------

case_dir="$TMP/case1"
FAKE_ROOT="$case_dir/root"
make_fake_root "$FAKE_ROOT"
export VKS_HOST_ROOT="$FAKE_ROOT"
export VKS_HOST_TUNE_SYSCTL_BIN="$BINS/sysctl"
export FAKE_SYSCTL_LOG="$case_dir/sysctl.log"
: >"$FAKE_SYSCTL_LOG"

if "$TUNE" check >"$case_dir/check.out" 2>&1; then
    ok "check passes when all effective sysctl values match the artifact"
else
    not_ok "check should pass on a matching fake root"
    cat "$case_dir/check.out"
fi

printf '2097152\n' >"$FAKE_ROOT/proc/sys/net/core/rmem_max"
if "$TUNE" check >"$case_dir/drift.out" 2>&1; then
    not_ok "check must fail on a drifted rmem_max"
else
    if grep -q 'net.core.rmem_max' "$case_dir/drift.out"; then
        ok "check fails and names the drifted key"
    else
        not_ok "check failure output does not name net.core.rmem_max"
        cat "$case_dir/drift.out"
    fi
fi
printf '16777216\n' >"$FAKE_ROOT/proc/sys/net/core/rmem_max"

rm -f -- "$FAKE_ROOT/proc/sys/net/core/netdev_budget"
if "$TUNE" check >/dev/null 2>&1; then
    not_ok "check must fail when a key is missing/unreadable"
else
    ok "check fails when a key is missing"
fi
make_fake_root "$FAKE_ROOT"

# ---------------------------------------------------------------------------
# 2. apply / rollback: dry-run, idempotency, pre-T15 state restore
# ---------------------------------------------------------------------------

case_dir="$TMP/case2"
mkdir -p -- "$case_dir"
FAKE_ROOT="$case_dir/root"
make_fake_root "$FAKE_ROOT"
DEST="$case_dir/etc/sysctl.d/90-vks-mcu.conf"
STATE="$case_dir/state.previous"
FAKE_LOG="$case_dir/sysctl.log"
: >"$FAKE_LOG"
export VKS_HOST_ROOT="$FAKE_ROOT"
export VKS_HOST_TUNE_SYSCTL_DEST="$DEST"
export VKS_HOST_TUNE_STATE_FILE="$STATE"
export VKS_HOST_TUNE_SYSCTL_BIN="$BINS/sysctl"
export VKS_HOST_TUNE_ALLOW_UNPRIVILEGED=1
export FAKE_SYSCTL_LOG="$FAKE_LOG"

if "$TUNE" apply --dry-run >"$case_dir/dry.out" 2>&1; then
    if [ ! -e "$DEST" ] && [ ! -s "$FAKE_LOG" ]; then
        ok "apply --dry-run writes nothing and does not call sysctl"
    else
        not_ok "apply --dry-run wrote a file or invoked sysctl"
    fi
else
    not_ok "apply --dry-run should exit 0 without root"
    cat "$case_dir/dry.out"
fi

if "$TUNE" apply >"$case_dir/apply1.out" 2>&1; then
    if grep -qF 'managed by scripts/host-tune.sh' "$DEST" && [ -s "$STATE" ]; then
        ok "apply installs the sysctl file and captures pre-T15 values"
    else
        not_ok "apply did not install the artifact or capture state"
        cat "$case_dir/apply1.out"
    fi
else
    not_ok "apply failed with the unprivileged test override"
    cat "$case_dir/apply1.out"
fi

cp -- "$DEST" "$case_dir/installed.copy"
if "$TUNE" apply >"$case_dir/apply2.out" 2>&1; then
    if grep -q 'unchanged' "$case_dir/apply2.out" && cmp -s "$case_dir/installed.copy" "$DEST"; then
        ok "second apply is idempotent (unchanged, same content)"
    else
        not_ok "second apply changed the artifact or did not report unchanged"
        cat "$case_dir/apply2.out"
    fi
else
    not_ok "second apply failed"
fi

if "$TUNE" rollback >"$case_dir/rollback1.out" 2>&1; then
    if [ ! -e "$DEST" ] && grep -q -- '-w net.core.rmem_max=' "$FAKE_LOG"; then
        ok "rollback removes the file and restores captured values via sysctl -w"
    else
        not_ok "rollback did not remove the file or restore captured values"
        cat "$case_dir/rollback1.out"
    fi
else
    not_ok "rollback failed"
fi

if "$TUNE" rollback >"$case_dir/rollback2.out" 2>&1 && grep -q 'already absent' "$case_dir/rollback2.out"; then
    ok "second rollback is idempotent (already absent)"
else
    not_ok "second rollback is not idempotent"
fi

# Foreign file must not be removed without --force.
mkdir -p -- "$(dirname -- "$DEST")"
printf '# operator file, no host-tune marker\nnet.core.rmem_max = 1\n' >"$DEST"
if "$TUNE" rollback >/dev/null 2>&1; then
    not_ok "rollback must refuse a destination without the host-tune marker"
elif [ -f "$DEST" ]; then
    ok "rollback refuses to delete a foreign sysctl file"
else
    not_ok "rollback deleted a foreign sysctl file"
fi
if "$TUNE" rollback --force >/dev/null 2>&1 && [ ! -e "$DEST" ]; then
    ok "rollback --force removes the foreign file only when explicit"
else
    not_ok "rollback --force did not remove the foreign file"
fi

# ---------------------------------------------------------------------------
# 3. systemd + user artifact install/check/rollback (no live systemd)
# ---------------------------------------------------------------------------

case_dir="$TMP/case3"
mkdir -p -- "$case_dir/user"
export VKS_HOST_TUNE_SYSTEMD_DIR="$case_dir/systemd"
export VKS_HOST_TUNE_USER_UNIT_DIR="$case_dir/user"
export VKS_HOST_TUNE_SYSTEMCTL_BIN="$BINS/systemctl"
export FAKE_SYSTEMCTL_LOG="$case_dir/systemctl.log"
: >"$FAKE_SYSTEMCTL_LOG"

if "$TUNE" apply --systemd >/dev/null 2>&1 \
    && [ -f "$case_dir/systemd/vks.slice" ] \
    && [ -f "$case_dir/systemd/vks-mcu-prod.service.d/10-l0-resources.conf" ]; then
    ok "apply --systemd installs the slice and prod drop-in"
else
    not_ok "apply --systemd did not install both artifacts"
fi

if "$TUNE" check --systemd >/dev/null 2>&1; then
    ok "check --systemd passes for installed, up-to-date artifacts"
else
    not_ok "check --systemd failed for freshly installed artifacts"
fi

if "$TUNE" apply --user >/dev/null 2>&1 \
    && [ -f "$case_dir/user/vks-mcu.service.d/10-l0-resources.conf" ]; then
    ok "apply --user installs the user drop-in without root"
else
    not_ok "apply --user did not install the drop-in"
fi

if "$TUNE" check --user >/dev/null 2>&1; then
    ok "check --user passes for the installed drop-in"
else
    not_ok "check --user failed for a freshly installed drop-in"
fi

printf '# local edit\n' >>"$case_dir/user/vks-mcu.service.d/10-l0-resources.conf"
if "$TUNE" check --user >/dev/null 2>&1; then
    not_ok "check --user must fail when the drop-in drifts from the artifact"
else
    ok "check --user detects a drifted drop-in"
fi

if "$TUNE" rollback --systemd --user >/dev/null 2>&1 \
    && [ ! -e "$case_dir/systemd/vks.slice" ] \
    && [ ! -e "$case_dir/user/vks-mcu.service.d/10-l0-resources.conf" ]; then
    ok "rollback removes system and user artifacts and reloads systemd"
else
    not_ok "rollback left installed artifacts behind"
fi
if grep -q 'daemon-reload' "$FAKE_SYSTEMCTL_LOG"; then
    ok "rollback/apply invoked systemctl daemon-reload (fake)"
else
    not_ok "systemctl daemon-reload was not invoked"
fi

# ---------------------------------------------------------------------------
# 4. host-doctor: pass, sysctl drift, cgroup drift, socket drift, inactive unit
# ---------------------------------------------------------------------------

case_dir="$TMP/case4"
mkdir -p -- "$case_dir"
FAKE_ROOT="$case_dir/root"
make_fake_root "$FAKE_ROOT"
PID=424242
make_fake_proc_pid "$FAKE_ROOT" "$PID"
write_ss_fixture "$case_dir/ss.out" "$PID" 131072 131072

export VKS_HOST_ROOT="$FAKE_ROOT"
export VKS_HOST_TUNE_SYSCTL_BIN="$BINS/sysctl"
export VKS_HOST_DOCTOR_SYSTEMCTL="$BINS/systemctl"
export VKS_HOST_DOCTOR_SS="$BINS/ss"
export VKS_HOST_DOCTOR_USER_UNIT_DIR="$case_dir/user"
export VKS_HOST_TUNE_USER_UNIT_DIR="$case_dir/user"
export VKS_HOST_TUNE_SYSTEMD_DIR="$case_dir/systemd"
export FAKE_SYSTEMCTL_LOG="$case_dir/systemctl.log"
export FAKE_SS_OUT="$case_dir/ss.out"
export FAKE_MainPID="$PID"
export FAKE_IS_ACTIVE=0
export_fake_props "$USER_DROPIN_SRC"
"$TUNE" apply --user >/dev/null 2>&1 || true

if "$DOCTOR" >"$case_dir/doctor-pass.out" 2>&1; then
    ok "host-doctor passes for matching sysctl/cgroup/socket fixtures"
else
    not_ok "host-doctor unexpectedly failed for matching fixtures"
    cat "$case_dir/doctor-pass.out"
fi

# sysctl drift must fail the doctor even when the unit is healthy.
printf '1\n' >"$FAKE_ROOT/proc/sys/net/core/rmem_max"
if "$DOCTOR" >"$case_dir/doctor-sysctl.out" 2>&1; then
    not_ok "host-doctor must fail on sysctl drift"
else
    if grep -q 'sysctl: effective values differ' "$case_dir/doctor-sysctl.out"; then
        ok "host-doctor fails on sysctl drift"
    else
        not_ok "sysctl drift failure is not reported by host-doctor"
    fi
fi
make_fake_root "$FAKE_ROOT"
make_fake_proc_pid "$FAKE_ROOT" "$PID"

# cgroup drift: MemoryMax not set / infinity.
export FAKE_MemoryMax='infinity'
if "$DOCTOR" >"$case_dir/doctor-mem.out" 2>&1; then
    not_ok "host-doctor must fail when MemoryMax differs from the artifact"
else
    if grep -q 'MemoryMax' "$case_dir/doctor-mem.out"; then
        ok "host-doctor fails on MemoryMax drift"
    else
        not_ok "MemoryMax drift is not named by host-doctor"
    fi
fi
export_fake_props "$USER_DROPIN_SRC"

# socket drift: SO_RCVBUF below the requested media buffer. Remove the fake
# process environ so host-doctor falls back to the net_tuning.rs media default
# (4 MiB) instead of the 64 KiB test override.
rm -f -- "$FAKE_ROOT/proc/$PID/environ"
write_ss_fixture "$case_dir/ss.out" "$PID" 212992 131072
if "$DOCTOR" >"$case_dir/doctor-sock.out" 2>&1; then
    not_ok "host-doctor must fail when SO_RCVBUF is too small"
else
    if grep -q 'SO_RCVBUF=212992' "$case_dir/doctor-sock.out"; then
        ok "host-doctor fails on a too-small media SO_RCVBUF"
    else
        not_ok "socket buffer drift is not named by host-doctor"
    fi
fi
make_fake_proc_pid "$FAKE_ROOT" "$PID"
write_ss_fixture "$case_dir/ss.out" "$PID" 131072 131072

# --skip-socket must ignore the socket drift and pass on matching cgroup data.
write_ss_fixture "$case_dir/ss.out" "$PID" 212992 212992
if "$DOCTOR" --skip-socket >/dev/null 2>&1; then
    ok "--skip-socket skips the SO_RCVBUF check"
else
    not_ok "--skip-socket still failed on socket buffers"
fi
write_ss_fixture "$case_dir/ss.out" "$PID" 131072 131072

# Inactive unit: default is a skip (exit 0), --require-active fails.
export FAKE_IS_ACTIVE=3
if "$DOCTOR" >"$case_dir/doctor-inactive.out" 2>&1; then
    ok "inactive unit is skipped by default (exit 0)"
else
    not_ok "inactive unit should be skipped by default"
fi
if "$DOCTOR" --require-active >/dev/null 2>&1; then
    not_ok "--require-active must fail for an inactive unit"
else
    ok "--require-active fails for an inactive unit"
fi
export FAKE_IS_ACTIVE=0

# prod mode: slice + prod drop-in installed, fake systemctl reports slice values.
case_dir="$TMP/case5"
mkdir -p -- "$case_dir"
FAKE_ROOT="$case_dir/root"
make_fake_root "$FAKE_ROOT"
make_fake_proc_pid "$FAKE_ROOT" "$PID"
write_ss_fixture "$case_dir/ss.out" "$PID" 131072 131072
export VKS_HOST_ROOT="$FAKE_ROOT"
export VKS_HOST_DOCTOR_USER_UNIT_DIR="$case_dir/user"
export VKS_HOST_DOCTOR_SYSTEMD_DIR="$case_dir/systemd"
export VKS_HOST_TUNE_SYSTEMD_DIR="$case_dir/systemd"
export VKS_HOST_TUNE_USER_UNIT_DIR="$case_dir/user"
export FAKE_SS_OUT="$case_dir/ss.out"
export FAKE_MainPID="$PID"
"$TUNE" apply --systemd --user >/dev/null 2>&1 || true
export_fake_props "$PROD_DROPIN_SRC"
# Slice properties are queried on vks.slice; prod drop-in has no io weight for
# the slice check, so mirror the slice artifact explicitly.
export FAKE_SLICE_MemoryHigh="$(to_bytes_test "$(artifact_value "$SLICE_SRC" MemoryHigh)")"
export FAKE_SLICE_MemoryMax="$(to_bytes_test "$(artifact_value "$SLICE_SRC" MemoryMax)")"
export FAKE_SLICE_CPUWeight="$(artifact_value "$SLICE_SRC" CPUWeight)"
export FAKE_SLICE_IOWeight="$(artifact_value "$SLICE_SRC" IOWeight)"
if "$DOCTOR" --prod >"$case_dir/prod.out" 2>&1; then
    ok "host-doctor --prod passes for slice + prod drop-in fixtures"
else
    not_ok "host-doctor --prod failed for matching fixtures"
    cat "$case_dir/prod.out"
fi

# ---------------------------------------------------------------------------
# summary
# ---------------------------------------------------------------------------

printf '\n%d passed, %d failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
