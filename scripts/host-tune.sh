#!/usr/bin/env bash
# host-tune.sh — T15/L0 host tuning: sysctl profile + systemd cgroup v2 artifacts.
#
# Idempotent apply/check/rollback for:
#   * deploy/host/sysctl/90-vks-mcu.conf              -> /etc/sysctl.d/
#   * deploy/host/systemd/vks.slice                    -> /etc/systemd/system/
#   * deploy/host/systemd/vks-mcu-prod.service.d/*.conf -> /etc/systemd/system/
#   * deploy/host/systemd/user/vks-mcu.service.d/*.conf -> ~/.config/systemd/user/
#
# Root: sysctl and system units require root (or sudo). `--dry-run` never needs
# root and never writes/reloads. The user drop-in alone is unprivileged.
#
# This script never restarts the live server; it only writes artifacts and runs
# `sysctl --system` / `systemctl daemon-reload`. A unit restart is a deliberate
# operator/T17 action (it drops live calls).
#
# Exit codes: 0 = ok/pass, 1 = drift/apply/rollback failure, 2 = usage or
# missing privilege/tool.
#
# Env overrides (tests/sandboxes only):
#   VKS_HOST_ROOT                    prefix for /proc/sys reads (fake root)
#   VKS_HOST_TUNE_SYSCTL_DEST        destination sysctl file
#   VKS_HOST_TUNE_SYSTEMD_DIR        system unit directory
#   VKS_HOST_TUNE_USER_UNIT_DIR      user unit directory
#   VKS_HOST_TUNE_SYSCTL_BIN         sysctl binary (fake in tests)
#   VKS_HOST_TUNE_SYSTEMCTL_BIN      systemctl binary (fake in tests)
#   VKS_HOST_TUNE_STATE_FILE         where pre-apply sysctl values are saved
#   VKS_HOST_TUNE_ALLOW_UNPRIVILEGED 1 = allow apply/rollback as non-root
#                                     (ONLY valid with overridden destinations)

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(dirname -- "$SCRIPT_DIR")

SYSCTL_SRC="$REPO_ROOT/deploy/host/sysctl/90-vks-mcu.conf"
SLICE_SRC="$REPO_ROOT/deploy/host/systemd/vks.slice"
PROD_DROPIN_SRC="$REPO_ROOT/deploy/host/systemd/vks-mcu-prod.service.d/10-l0-resources.conf"
USER_DROPIN_SRC="$REPO_ROOT/deploy/host/systemd/user/vks-mcu.service.d/10-l0-resources.conf"

SYSCTL_DEST=${VKS_HOST_TUNE_SYSCTL_DEST:-/etc/sysctl.d/90-vks-mcu.conf}
SYSTEMD_DIR=${VKS_HOST_TUNE_SYSTEMD_DIR:-/etc/systemd/system}
SLICE_DEST="$SYSTEMD_DIR/vks.slice"
PROD_DROPIN_DEST="$SYSTEMD_DIR/vks-mcu-prod.service.d/10-l0-resources.conf"
USER_UNIT_DIR=${VKS_HOST_TUNE_USER_UNIT_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user}
USER_DROPIN_DEST="$USER_UNIT_DIR/vks-mcu.service.d/10-l0-resources.conf"
SYSCTL_BIN=${VKS_HOST_TUNE_SYSCTL_BIN:-sysctl}
SYSTEMCTL_BIN=${VKS_HOST_TUNE_SYSTEMCTL_BIN:-systemctl}
STATE_FILE=${VKS_HOST_TUNE_STATE_FILE:-/var/lib/vks/host-tune.previous}
PROC_SYS_ROOT=${VKS_HOST_ROOT:-}
ALLOW_UNPRIVILEGED=${VKS_HOST_TUNE_ALLOW_UNPRIVILEGED:-0}
MARKER='managed by scripts/host-tune.sh'

MODE=''
DRY_RUN=0
FORCE=0
TARGETS=()
CHECK_FAILS=0

usage() {
    cat <<'EOF'
usage: host-tune.sh <apply|check|rollback|status> [options]

Targets (default: --sysctl):
  --sysctl    /etc/sysctl.d/90-vks-mcu.conf (root required to apply/rollback)
  --systemd   /etc/systemd/system/vks.slice + vks-mcu-prod.service.d drop-in
  --user      ~/.config/systemd/user/vks-mcu.service.d drop-in (no root)

Options:
  --dry-run   show what would change; never write/reload (root not required)
  --force     overwrite a destination that differs from the repo artifact
  -h, --help  this text

check is read-only and never needs root; it exits 1 when effective sysctl
values differ from the artifact or when --systemd/--user files are not
installed/up to date.

Examples:
  scripts/host-tune.sh check                 # sysctl effective values
  sudo scripts/host-tune.sh apply            # sysctl only
  sudo scripts/host-tune.sh apply --systemd  # slice + prod drop-in + reload
  scripts/host-tune.sh apply --dry-run --sysctl --systemd --user
  sudo scripts/host-tune.sh rollback --sysctl --systemd
EOF
}

info() { printf 'host-tune: %s\n' "$*"; }
warn() { printf 'host-tune: WARN: %s\n' "$*" >&2; }
fail() { printf 'host-tune: FAIL: %s\n' "$*"; CHECK_FAILS=$((CHECK_FAILS + 1)); }
die() {
    printf 'host-tune: error: %s\n' "$*" >&2
    exit 2
}

is_root() { [ "$(id -u)" -eq 0 ]; }

require_root_for() {
    local target=$1
    [ "$DRY_RUN" -eq 1 ] && return 0
    case "$target" in
        sysctl|systemd)
            if ! is_root && [ "$ALLOW_UNPRIVILEGED" != "1" ]; then
                die "root is required to $MODE $target. Re-run with sudo, or use --dry-run to preview. \
(The sysctl/unit files are root-owned by design; applying without root would silently do nothing.)"
            fi
            ;;
        user) : ;;
        *) die "unknown target $target" ;;
    esac
}

run_or_echo() {
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '[dry-run] %s\n' "$*"
        return 0
    fi
    "$@"
}

# ---------------------------------------------------------------------------
# sysctl parsing / reading / normalization
# ---------------------------------------------------------------------------

# Prints "key<TAB>value" for every key = value line (comments/blank ignored).
parse_sysctl_file() {
    awk '
        /^[[:space:]]*#/ { next }
        /^[[:space:]]*$/ { next }
        {
            line = $0
            sub(/[[:space:]]*#.*$/, "", line)
            eq = index(line, "=")
            if (eq == 0) { next }
            key = substr(line, 1, eq - 1)
            val = substr(line, eq + 1)
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", key)
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", val)
            print key "\t" val
        }
    ' "$1"
}

normalize_value() {
    # Collapse tabs/duplicated spaces; strip leading/trailing spaces.
    tr '\t' ' ' | tr -s ' ' | sed -e 's/^ //' -e 's/ $//'
}

# read_sysctl KEY -> prints effective value, or fails if unreadable.
read_sysctl() {
    local key=$1 path
    path="/proc/sys/${key//./\/}"
    if [ -n "$PROC_SYS_ROOT" ]; then
        path="$PROC_SYS_ROOT$path"
    fi
    if [ -r "$path" ]; then
        cat -- "$path"
        return 0
    fi
    command -v "$SYSCTL_BIN" >/dev/null 2>&1 || return 1
    "$SYSCTL_BIN" -n "$key" 2>/dev/null
}

capture_previous_values() {
    # Only once, only on the first install, best effort (rollback restores this).
    [ -e "$STATE_FILE" ] && return 0
    [ "$DRY_RUN" -eq 1 ] && return 0
    local tmp state_dir
    state_dir=$(dirname -- "$STATE_FILE")
    if ! mkdir -p -- "$state_dir" 2>/dev/null; then
        warn "cannot create $state_dir; rollback will not restore pre-T15 values"
        return 0
    fi
    if ! tmp=$(mktemp "$STATE_FILE.XXXXXX" 2>/dev/null); then
        warn "cannot write $STATE_FILE; rollback will not restore pre-T15 values"
        return 0
    fi
    local key value
    while IFS=$'\t' read -r key value; do
        [ -n "$key" ] || continue
        if value=$(read_sysctl "$key" 2>/dev/null); then
            printf '%s\t%s\n' "$key" "$(printf '%s' "$value" | normalize_value)" >>"$tmp"
        fi
    done < <(parse_sysctl_file "$SYSCTL_SRC")
    if [ -s "$tmp" ]; then
        mv -- "$tmp" "$STATE_FILE"
        info "saved pre-T15 sysctl values to $STATE_FILE (rollback restores them)"
    else
        rm -f -- "$tmp"
    fi
}

# ---------------------------------------------------------------------------
# artifact install
# ---------------------------------------------------------------------------

artifact_changed() {
    local src=$1 dest=$2
    [ -f "$dest" ] || return 0
    ! cmp -s -- "$src" "$dest"
}

install_artifact() {
    local src=$1 dest=$2 label=$3
    [ -r "$src" ] || die "missing repository artifact: $src"
    if [ -f "$dest" ] && ! artifact_changed "$src" "$dest"; then
        info "$label: unchanged ($dest)"
        return 0
    fi
    if [ -e "$dest" ] && [ "$FORCE" -ne 1 ]; then
        die "$dest exists and differs from $src; re-run with --force to overwrite it"
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
        if [ -e "$dest" ]; then
            printf '[dry-run] overwrite %s with %s\n' "$dest" "$src"
        else
            printf '[dry-run] install %s -> %s\n' "$src" "$dest"
        fi
        return 0
    fi
    mkdir -p -- "$(dirname -- "$dest")"
    install -m 0644 -- "$src" "$dest"
    info "$label: installed $dest"
}

is_our_file() {
    local path=$1
    [ -f "$path" ] && grep -qiF "$MARKER" "$path"
}

remove_our_file() {
    local path=$1 label=$2
    if [ ! -e "$path" ]; then
        info "$label: already absent ($path)"
        return 0
    fi
    if ! is_our_file "$path" && [ "$FORCE" -ne 1 ]; then
        die "$path does not contain the host-tune marker; refusing to remove it (use --force)"
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '[dry-run] rm %s\n' "$path"
        return 0
    fi
    rm -f -- "$path"
    # Remove now-empty drop-in directory so `systemd-analyze verify` does not
    # warn about an empty drop-in dir.
    local dir
    dir=$(dirname -- "$path")
    if [ -d "$dir" ] && [ -z "$(ls -A -- "$dir")" ]; then
        rmdir -- "$dir" 2>/dev/null || true
    fi
    info "$label: removed $path"
}

reload_sysctl() {
    command -v "$SYSCTL_BIN" >/dev/null 2>&1 || die "sysctl binary not found ($SYSCTL_BIN); cannot reload kernel values"
    run_or_echo "$SYSCTL_BIN" --system
}

reload_systemd() {
    command -v "$SYSTEMCTL_BIN" >/dev/null 2>&1 || die "systemctl binary not found ($SYSTEMCTL_BIN)"
    run_or_echo "$SYSTEMCTL_BIN" daemon-reload
}

reload_systemd_user() {
    command -v "$SYSTEMCTL_BIN" >/dev/null 2>&1 || die "systemctl binary not found ($SYSTEMCTL_BIN)"
    run_or_echo "$SYSTEMCTL_BIN" --user daemon-reload
}

# ---------------------------------------------------------------------------
# apply / check / rollback per target
# ---------------------------------------------------------------------------

apply_target() {
    case "$1" in
        sysctl)
            capture_previous_values
            install_artifact "$SYSCTL_SRC" "$SYSCTL_DEST" "sysctl"
            reload_sysctl
            ;;
        systemd)
            install_artifact "$SLICE_SRC" "$SLICE_DEST" "slice"
            install_artifact "$PROD_DROPIN_SRC" "$PROD_DROPIN_DEST" "prod drop-in"
            reload_systemd
            ;;
        user)
            install_artifact "$USER_DROPIN_SRC" "$USER_DROPIN_DEST" "user drop-in"
            reload_systemd_user
            ;;
        *) die "unknown target $1" ;;
    esac
}

check_sysctl() {
    local key expected effective total=0
    [ -r "$SYSCTL_SRC" ] || die "missing repository artifact: $SYSCTL_SRC"
    while IFS=$'\t' read -r key expected; do
        [ -n "$key" ] || continue
        total=$((total + 1))
        if ! effective=$(read_sysctl "$key" 2>/dev/null); then
            fail "sysctl $key: not readable (is the key valid on this kernel?)"
            continue
        fi
        if [ "$(printf '%s' "$effective" | normalize_value)" = "$(printf '%s' "$expected" | normalize_value)" ]; then
            info "PASS sysctl $key = $expected"
        else
            fail "sysctl $key: expected '$expected', effective '$(printf '%s' "$effective" | normalize_value)'"
        fi
    done < <(parse_sysctl_file "$SYSCTL_SRC")
    [ "$total" -gt 0 ] || die "no sysctl keys parsed from $SYSCTL_SRC"
}

check_artifacts() {
    local src dest label
    case "$1" in
        systemd)
            for pair in "$SLICE_SRC:$SLICE_DEST:slice" "$PROD_DROPIN_SRC:$PROD_DROPIN_DEST:prod-drop-in"; do
                IFS=: read -r src dest label <<<"$pair"
                check_one_artifact "$src" "$dest" "$label" "apply --systemd"
            done
            ;;
        user)
            check_one_artifact "$USER_DROPIN_SRC" "$USER_DROPIN_DEST" "user-drop-in" "apply --user"
            ;;
        *) die "unknown target $1" ;;
    esac
}

check_one_artifact() {
    local src=$1 dest=$2 label=$3 hint=$4
    if [ ! -f "$dest" ]; then
        fail "$label: not installed at $dest (run: host-tune.sh $hint; root needed for --systemd)"
    elif cmp -s -- "$src" "$dest"; then
        info "PASS $label: $dest matches repository artifact"
    else
        fail "$label: $dest differs from $src (run: host-tune.sh $hint --force)"
    fi
}

rollback_sysctl() {
    local had_file=0
    if [ -e "$SYSCTL_DEST" ]; then
        had_file=1
        remove_our_file "$SYSCTL_DEST" "sysctl"
    else
        info "sysctl: already absent ($SYSCTL_DEST)"
    fi
    # Reload remaining /etc/sysctl.d files first, then restore the captured
    # pre-T15 values (a bare file removal leaves the tuned values in the kernel
    # until reboot, because the builtin defaults are not a sysctl.d file).
    reload_sysctl
    if [ -f "$STATE_FILE" ]; then
        local key value restored=0
        while IFS=$'\t' read -r key value; do
            [ -n "$key" ] || continue
            if [ -z "$value" ]; then
                warn "cannot restore empty value for $key; restore manually after reboot"
                continue
            fi
            run_or_echo "$SYSCTL_BIN" -w "$key=$value"
            restored=$((restored + 1))
        done < <(parse_state_file)
        if [ "$DRY_RUN" -eq 0 ]; then
            rm -f -- "$STATE_FILE"
        fi
        info "sysctl: restored $restored pre-T15 value(s) from $STATE_FILE"
    elif [ "$had_file" -eq 1 ]; then
        warn "no $STATE_FILE: tuned values stay in the kernel until reboot (or restore them manually)"
    fi
}

parse_state_file() {
    awk -F '\t' 'NF >= 1 && $1 != "" { print $1 "\t" $2 }' "$STATE_FILE"
}

rollback_target() {
    case "$1" in
        sysctl) rollback_sysctl ;;
        systemd)
            remove_our_file "$PROD_DROPIN_DEST" "prod drop-in"
            remove_our_file "$SLICE_DEST" "slice"
            reload_systemd
            ;;
        user)
            remove_our_file "$USER_DROPIN_DEST" "user drop-in"
            reload_systemd_user
            ;;
        *) die "unknown target $1" ;;
    esac
}

status_target() {
    local src dest label state
    case "$1" in
        sysctl)
            state='missing'
            [ -f "$SYSCTL_DEST" ] && { cmp -s -- "$SYSCTL_SRC" "$SYSCTL_DEST" && state='up-to-date' || state='differs'; }
            printf 'sysctl   %-10s %s\n' "$state" "$SYSCTL_DEST"
            ;;
        systemd)
            for pair in "$SLICE_SRC:$SLICE_DEST:slice" "$PROD_DROPIN_SRC:$PROD_DROPIN_DEST:prod-drop-in"; do
                IFS=: read -r src dest label <<<"$pair"
                state='missing'
                [ -f "$dest" ] && { cmp -s -- "$src" "$dest" && state='up-to-date' || state='differs'; }
                printf '%-8s %-10s %s\n' "$label" "$state" "$dest"
            done
            ;;
        user)
            state='missing'
            [ -f "$USER_DROPIN_DEST" ] && {
                cmp -s -- "$USER_DROPIN_SRC" "$USER_DROPIN_DEST" && state='up-to-date' || state='differs'
            }
            printf '%-8s %-10s %s\n' 'user' "$state" "$USER_DROPIN_DEST"
            ;;
        *) die "unknown target $1" ;;
    esac
}

# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

[ $# -gt 0 ] || { usage >&2; exit 2; }
while [ $# -gt 0 ]; do
    case "$1" in
        apply | check | rollback | status)
            [ -z "$MODE" ] || die "exactly one command is required"
            MODE=$1
            ;;
        --dry-run) DRY_RUN=1 ;;
        --force) FORCE=1 ;;
        --sysctl | --systemd | --user) TARGETS+=("${1#--}") ;;
        -h | --help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
    shift
done
[ -n "$MODE" ] || { usage >&2; exit 2; }
[ "${#TARGETS[@]}" -gt 0 ] || TARGETS=(sysctl)

for target in "${TARGETS[@]}"; do
    case "$MODE" in
        apply) require_root_for "$target" ;;
        rollback) require_root_for "$target" ;;
        check | status) : ;;
    esac
done

for target in "${TARGETS[@]}"; do
    case "$MODE" in
        apply) apply_target "$target" ;;
        check)
            case "$target" in
                sysctl) check_sysctl ;;
                systemd | user) check_artifacts "$target" ;;
            esac
            ;;
        rollback) rollback_target "$target" ;;
        status) status_target "$target" ;;
    esac
done

if [ "$MODE" = 'check' ]; then
    if [ "$CHECK_FAILS" -ne 0 ]; then
        printf 'host-tune: %d drift(s) detected\n' "$CHECK_FAILS" >&2
        exit 1
    fi
    info "all checks passed"
fi
