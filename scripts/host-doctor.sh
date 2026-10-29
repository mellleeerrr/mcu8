#!/usr/bin/env bash
# host-doctor.sh — T15/L0 verification: effective sysctl + cgroup v2 + socket buffers.
#
# Read-only. Never writes sysctl, never restarts/reloads the service, never
# needs root for the default user-unit scope. Exit codes:
#   0 = all checked values match (skips are printed explicitly)
#   1 = drift: an effective value differs from the versioned artifact
#   2 = usage / missing tool
#
# It checks the *effective* values, not just the installed files:
#   * sysctl: /proc/sys (delegated to scripts/host-tune.sh check)
#   * systemd: `systemctl show` for MemoryHigh/MemoryMax/CPUWeight/IOWeight/
#     LimitNOFILE/Nice/CPUSchedulingPolicy/Slice (user unit by default)
#   * cgroup v2 slice values for the prod unit (--prod)
#   * process NOFILE from /proc/<pid>/limits
#   * UDP socket SO_RCVBUF from `ss -u -m -p`, per role, using the defaults
#     parsed from crates/vks-common/src/net_tuning.rs (single source of truth)
#
# Env overrides (tests/sandboxes):
#   VKS_HOST_ROOT                 prefix for /proc reads (fake root)
#   VKS_HOST_DOCTOR_UNIT          unit name (default vks-mcu.service / prod name)
#   VKS_HOST_DOCTOR_SYSTEMCTL     systemctl binary (fake in tests)
#   VKS_HOST_DOCTOR_SS            ss binary (fake in tests)
#   VKS_HOST_DOCTOR_USER_UNIT_DIR user drop-in dir (must mirror host-tune.sh)
#   VKS_HOST_DOCTOR_SYSTEMD_DIR   system unit dir (prod)

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(dirname -- "$SCRIPT_DIR")

SYSCTL_SRC="$REPO_ROOT/deploy/host/sysctl/90-vks-mcu.conf"
SLICE_SRC="$REPO_ROOT/deploy/host/systemd/vks.slice"
PROD_DROPIN_SRC="$REPO_ROOT/deploy/host/systemd/vks-mcu-prod.service.d/10-l0-resources.conf"
USER_DROPIN_SRC="$REPO_ROOT/deploy/host/systemd/user/vks-mcu.service.d/10-l0-resources.conf"
NET_TUNING_SRC="$REPO_ROOT/crates/vks-common/src/net_tuning.rs"

SYSTEMCTL_BIN=${VKS_HOST_DOCTOR_SYSTEMCTL:-${VKS_HOST_TUNE_SYSTEMCTL_BIN:-systemctl}}
SS_BIN=${VKS_HOST_DOCTOR_SS:-ss}
PROC_ROOT=${VKS_HOST_ROOT:-}
USER_UNIT_DIR=${VKS_HOST_DOCTOR_USER_UNIT_DIR:-${VKS_HOST_TUNE_USER_UNIT_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user}}
SYSTEMD_DIR=${VKS_HOST_DOCTOR_SYSTEMD_DIR:-${VKS_HOST_TUNE_SYSTEMD_DIR:-/etc/systemd/system}}

MODE='user'
SKIP_SOCKET=0
REQUIRE_ACTIVE=0
FAILS=0
SKIPS=0
PASSES=0

usage() {
    cat <<'EOF'
usage: host-doctor.sh [--user|--prod] [--skip-socket] [--require-active] [-h]

  --user            check the dev user unit (default): vks-mcu.service
  --prod            check the production system unit: vks-mcu-prod.service,
                    its vks.slice cgroup values and the system drop-in
  --skip-socket     do not check UDP SO_RCVBUF via `ss -u -m` (useful while the
                    service still runs the pre-T15 binary)
  --require-active  fail when the unit is not active (default: skip its checks)

Exit 0 = pass (possibly with printed skips), 1 = drift, 2 = usage/tool error.
No root required; no state is modified.
EOF
}

ok() { printf '[ ok ] %s\n' "$*"; PASSES=$((PASSES + 1)); }
bad() { printf '[FAIL] %s\n' "$*"; FAILS=$((FAILS + 1)); }
skip() { printf '[skip] %s\n' "$*"; SKIPS=$((SKIPS + 1)); }
note() { printf '[info] %s\n' "$*"; }
die() {
    printf 'host-doctor: error: %s\n' "$*" >&2
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --user) MODE='user' ;;
        --prod) MODE='prod' ;;
        --skip-socket) SKIP_SOCKET=1 ;;
        --require-active) REQUIRE_ACTIVE=1 ;;
        -h | --help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
    shift
done

if [ "$MODE" = 'prod' ]; then
    UNIT=${VKS_HOST_DOCTOR_UNIT:-vks-mcu-prod.service}
    ARTIFACT=$PROD_DROPIN_SRC
    SCOPE=()
    UNIT_DROPIN="$SYSTEMD_DIR/vks-mcu-prod.service.d/10-l0-resources.conf"
else
    UNIT=${VKS_HOST_DOCTOR_UNIT:-vks-mcu.service}
    ARTIFACT=$USER_DROPIN_SRC
    SCOPE=(--user)
    UNIT_DROPIN="$USER_UNIT_DIR/vks-mcu.service.d/10-l0-resources.conf"
fi

# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------

cmd_systemctl() { "$SYSTEMCTL_BIN" "${SCOPE[@]}" "$@"; }

systemd_prop() {
    local unit=$1 prop=$2 out
    out=$(cmd_systemctl show "$unit" -p "$prop" 2>/dev/null || true)
    printf '%s' "$out" | sed -n "s/^${prop}=//p" | head -n1
}

# Expected value of KEY from the [Service] section of an artifact drop-in.
unit_expected() {
    local file=$1 key=$2
    [ -r "$file" ] || return 1
    awk -v key="$key" '
        /^\[/ { in_service = ($0 == "[Service]" || $0 == "[Slice]") }
        in_service && index($0, key "=") == 1 { sub(/^[^=]*=/, ""); print; exit }
    ' "$file"
}

# systemd size ("2G", "1536M", "1024K", raw bytes) -> bytes.
to_bytes() {
    local raw=$1
    case "$raw" in
        '' | *[!0-9KkMmGgTt%]*) printf '%s' "$raw"; return 0 ;;
    esac
    local num suffix
    num=${raw%[KkMmGgTt]}
    suffix=${raw#"$num"}
    case "$suffix" in
        K | k) num=$((num * 1024)) ;;
        M | m) num=$((num * 1024 * 1024)) ;;
        G | g) num=$((num * 1024 * 1024 * 1024)) ;;
        T | t) num=$((num * 1024 * 1024 * 1024 * 1024)) ;;
        '') : ;;
        *) printf '%s' "$raw"; return 0 ;;
    esac
    printf '%s' "$num"
}

sched_policy_number() {
    case "$1" in
        other) printf '0' ;;
        fifo) printf '1' ;;
        rr) printf '2' ;;
        batch) printf '3' ;;
        idle) printf '4' ;;
        deadline) printf '5' ;;
        *) printf '%s' "$1" ;;
    esac
}

# Defaults from the Rust module (single source of truth); fallback constants
# keep the doctor usable if the source tree is not readable.
rust_default_bytes() {
    local name=$1 fallback=$2 expr value
    expr=$(sed -n "s/^pub const ${name}: usize = \(.*\);/\1/p" "$NET_TUNING_SRC" 2>/dev/null | head -n1)
    if [ -n "$expr" ] && [[ "$expr" =~ ^[0-9+\ */_]+$ ]]; then
        value=$((expr))
        printf '%s' "$value"
        return 0
    fi
    printf '%s' "$fallback"
}

check_property() {
    local unit=$1 prop=$2 expected=$3 actual kind=${4:-plain}
    [ -n "$expected" ] || return 0
    actual=$(systemd_prop "$unit" "$prop")
    if [ -z "$actual" ]; then
        bad "$unit: $prop has no effective value (unit missing or property unsupported)"
        return 0
    fi
    case "$kind" in
        bytes)
            local want got
            want=$(to_bytes "$expected")
            got=$(to_bytes "$actual")
            if [ "$want" = "$got" ]; then
                ok "$unit: $prop=$actual (artifact $expected)"
            else
                bad "$unit: $prop expected $expected ($want B), effective $actual ($got B)"
            fi
            ;;
        policy)
            local want got
            want=$(sched_policy_number "$expected")
            got=$actual
            if [ "$want" = "$got" ]; then
                ok "$unit: CPUSchedulingPolicy=$expected"
            else
                bad "$unit: CPUSchedulingPolicy expected $expected ($want), effective $actual"
            fi
            ;;
        nofile)
            local got=${actual%%:*}
            if [ "$got" = "$expected" ]; then
                ok "$unit: $prop=$got"
            else
                bad "$unit: $prop expected $expected, effective $got"
            fi
            ;;
        *)
            if [ "$actual" = "$expected" ]; then
                ok "$unit: $prop=$actual"
            else
                bad "$unit: $prop expected '$expected', effective '$actual'"
            fi
            ;;
    esac
}

check_artifact_installed() {
    local src=$1 dest=$2 label=$3
    if [ ! -f "$dest" ]; then
        bad "$label: artifact not installed at $dest"
    elif cmp -s -- "$src" "$dest"; then
        ok "$label: installed artifact matches repository"
    else
        bad "$label: $dest differs from $src (host-tune.sh apply --force)"
    fi
}

check_socket_buffers() {
    local pid=$1
    if ! command -v "$SS_BIN" >/dev/null 2>&1; then
        skip "ss not available ($SS_BIN); UDP SO_RCVBUF check skipped"
        return 0
    fi
    # Defaults: vks_common::net_tuning (bytes). Env overrides come from the
    # process environment itself (what the running binary actually saw).
    local media_default gateway_default rcv_media rcv_gateway
    media_default=$(rust_default_bytes DEFAULT_MEDIA_RCVBUF_BYTES 4194304)
    gateway_default=$(rust_default_bytes DEFAULT_GATEWAY_RCVBUF_BYTES 524288)
    rcv_media=$media_default
    rcv_gateway=$gateway_default
    local env_file="$PROC_ROOT/proc/$pid/environ"
    if [ -r "$env_file" ]; then
        local value
        value=$(tr '\0' '\n' <"$env_file" | sed -n 's/^VKS_UDP_RCVBUF_BYTES=//p' | head -n1)
        case "$value" in '' | 0 | *[!0-9]*) : ;; *) rcv_media=$value ;; esac
        value=$(tr '\0' '\n' <"$env_file" | sed -n 's/^VKS_GATEWAY_UDP_RCVBUF_BYTES=//p' | head -n1)
        case "$value" in '' | 0 | *[!0-9]*) : ;; *) rcv_gateway=$value ;; esac
    else
        note "cannot read $env_file; using net_tuning.rs defaults for socket checks"
    fi

    local ss_out checked=0 line local_addr port rb expected role
    local pending_port='' pending_role='' pending_expected=''
    # -a: media sockets are unconnected/listening (plain `ss -u` hides them).
    ss_out=$("$SS_BIN" -u -m -p -a 2>/dev/null || true)
    if [ -z "$ss_out" ]; then
        skip "ss produced no UDP sockets; SO_RCVBUF check skipped"
        return 0
    fi

    socket_role_and_expected() {
        local p=$1
        role=''
        expected=''
        if [ "$p" = '40000' ]; then
            role='media_loop'
            expected=$rcv_media
        elif [ "$p" -ge 43000 ] && [ "$p" -le 43100 ]; then
            role='media_loop'
            expected=$rcv_media
        elif [ "$p" -ge 41000 ] && [ "$p" -le 41199 ]; then
            role='gateway_rtp'
            expected=$rcv_gateway
        fi
    }

    judge_socket() {
        local p=$1 r=$2 e=$3 b=$4
        [ -n "$r" ] || return 0
        checked=$((checked + 1))
        if [ "$b" -ge "$e" ]; then
            ok "socket :$p role=$r SO_RCVBUF=$b (>= $e)"
        else
            bad "socket :$p role=$r SO_RCVBUF=$b < $e (net_tuning not active in this binary / sysctl rmem_max too low)"
        fi
    }

    # `ss -m` prints skmem on a continuation line; remember the socket whose
    # users:(...pid=$pid...) matched and evaluate it on the next skmem line.
    while IFS= read -r line; do
        case "$line" in
            *"pid=$pid,"*)
                local_addr=$(printf '%s\n' "$line" | awk '{for (i = 1; i <= NF; i++) if ($i ~ /:[0-9]+$/ && $i !~ /^\*:/) { print $i; exit }}')
                port=${local_addr##*:}
                case "$port" in
                    '' | *[!0-9]*) pending_port=''; continue ;;
                esac
                socket_role_and_expected "$port"
                if [ -z "$role" ]; then
                    pending_port=''
                    continue
                fi
                rb=$(printf '%s\n' "$line" | sed -n 's/.*skmem:(r[0-9]*,rb\([0-9]*\).*/\1/p')
                if [ -n "$rb" ]; then
                    judge_socket "$port" "$role" "$expected" "$rb"
                    pending_port=''
                else
                    pending_port=$port
                    pending_role=$role
                    pending_expected=$expected
                fi
                ;;
            *skmem:*)
                if [ -n "$pending_port" ]; then
                    rb=$(printf '%s\n' "$line" | sed -n 's/.*skmem:(r[0-9]*,rb\([0-9]*\).*/\1/p')
                    if [ -n "$rb" ]; then
                        judge_socket "$pending_port" "$pending_role" "$pending_expected" "$rb"
                    fi
                    pending_port=''
                fi
                ;;
            *)
                # A new socket line without our pid ends any pending match.
                [ -n "$pending_port" ] && pending_port=''
                ;;
        esac
    done <<<"$ss_out"
    if [ "$checked" -eq 0 ]; then
        skip "no active media UDP sockets for pid $pid (media_loop/gateway_rtp ranges)"
    fi
}

# ---------------------------------------------------------------------------
# 1. sysctl
# ---------------------------------------------------------------------------

note "T15/L0 host doctor: mode=$MODE unit=$UNIT"
note "sysctl profile: $SYSCTL_SRC"
if sysctl_out=$("$SCRIPT_DIR/host-tune.sh" check 2>&1); then
    ok "sysctl: effective values match the artifact"
else
    bad "sysctl: effective values differ (details below; apply with: sudo host-tune.sh apply)"
    printf '%s\n' "$sysctl_out" | sed 's/^/        /'
fi

# ---------------------------------------------------------------------------
# 2. unit / cgroup v2 effective values
# ---------------------------------------------------------------------------

if [ ! -r "$ARTIFACT" ]; then
    die "missing drop-in artifact: $ARTIFACT"
fi

if ! command -v "$SYSTEMCTL_BIN" >/dev/null 2>&1; then
    skip "systemctl not available ($SYSTEMCTL_BIN); cgroup checks skipped"
    UNIT_ACTIVE=0
elif ! cmd_systemctl is-active --quiet "$UNIT" 2>/dev/null; then
    UNIT_ACTIVE=0
    if [ "$REQUIRE_ACTIVE" -eq 1 ]; then
        bad "$UNIT is not active (--require-active)"
    else
        skip "$UNIT is not active; cgroup/socket checks skipped (use --require-active to fail)"
    fi
else
    UNIT_ACTIVE=1
    ok "$UNIT is active"
fi

if [ "$UNIT_ACTIVE" -eq 1 ]; then
    check_artifact_installed "$ARTIFACT" "$UNIT_DROPIN" "drop-in"
    for prop in MemoryHigh MemoryMax CPUWeight IOWeight Nice; do
        kind=plain
        case "$prop" in MemoryHigh | MemoryMax) kind=bytes ;; esac
        check_property "$UNIT" "$prop" "$(unit_expected "$ARTIFACT" "$prop" || true)" "$kind"
    done
    check_property "$UNIT" LimitNOFILE "$(unit_expected "$ARTIFACT" LimitNOFILE || true)" nofile
    check_property "$UNIT" CPUSchedulingPolicy "$(unit_expected "$ARTIFACT" CPUSchedulingPolicy || true)" policy
    check_property "$UNIT" Slice "$(unit_expected "$ARTIFACT" Slice || true)" plain

    if [ "$MODE" = 'prod' ]; then
        if [ ! -r "$SLICE_SRC" ]; then
            bad "missing slice artifact: $SLICE_SRC"
        else
            for prop in MemoryHigh MemoryMax CPUWeight IOWeight; do
                kind=plain
                case "$prop" in MemoryHigh | MemoryMax) kind=bytes ;; esac
                check_property 'vks.slice' "$prop" "$(unit_expected "$SLICE_SRC" "$prop" || true)" "$kind"
            done
        fi
    fi

    # 3. process-level NOFILE + socket buffers
    main_pid=$(systemd_prop "$UNIT" MainPID)
    if [ -z "$main_pid" ] || [ "$main_pid" = '0' ]; then
        skip "MainPID is 0; process checks skipped"
    else
        note "main pid: $main_pid"
        limits_file="$PROC_ROOT/proc/$main_pid/limits"
        expected_nofile=$(unit_expected "$ARTIFACT" LimitNOFILE || true)
        if [ -r "$limits_file" ] && [ -n "$expected_nofile" ]; then
            soft=$(awk '$1 == "Max" && $2 == "open" && $3 == "files" { print $4; exit }' "$limits_file")
            if [ -z "$soft" ]; then
                bad "cannot parse Max open files from $limits_file"
            elif [ "$soft" -ge "$expected_nofile" ]; then
                ok "process NOFILE soft=$soft (>= $expected_nofile)"
            else
                bad "process NOFILE soft=$soft < expected $expected_nofile (unit not reloaded/restarted?)"
            fi
        else
            skip "cannot read $limits_file; process NOFILE check skipped"
        fi

        if [ "$SKIP_SOCKET" -eq 1 ]; then
            skip "UDP SO_RCVBUF check disabled by --skip-socket"
        else
            check_socket_buffers "$main_pid"
        fi
    fi
fi

if [ "$FAILS" -ne 0 ]; then
    printf 'host-doctor: %d failure(s), %d ok, %d skipped\n' "$FAILS" "$PASSES" "$SKIPS" >&2
    exit 1
fi
printf 'host-doctor: pass (%d ok, %d skipped)\n' "$PASSES" "$SKIPS"
