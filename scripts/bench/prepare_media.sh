#!/usr/bin/env bash
# prepare_media.sh — эталонные медиапотоки для ботов (ffmpeg 8.0.1).
#
# Генерируется ОДИН раз, кешируется в /tmp/vks-bench/media. Боты читают файлы
# в память и пакетизируют сами (RTP-пакетизация — в боте, не в ffmpeg).
#
# Профили выбраны под реальные ограничения нашего SIP UAS (см.
# crates/vks-gateway-sip/src/uas.rs):
#   * H.264: только Baseline (profile_idc=66) и level >= 3.1 (uas.rs:659-680)
#     packetization-mode=1 (FU-A) — поэтому -profile:v baseline -level 3.1/4.0;
#   * H.265: Main (profile-id=1), без sprop-max-don-diff (uas.rs:694-709);
#   * H.264 High (6400xx) НЕ генерируется для M4: UAS отклонит видео (488) —
#     это осознанное решение, а не забывчивость, см. 06_perf_lab.md §1.7.
#
# Использование: scripts/bench/prepare_media.sh [--force] [--seconds 120]
set -euo pipefail
export LC_ALL=C

OUT="/tmp/vks-bench/media"
FORCE=0
SECONDS_GEN=120

while [[ $# -gt 0 ]]; do
  case "$1" in
    --force) FORCE=1; shift ;;
    --seconds) SECONDS_GEN="$2"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) printf '[media] unknown arg: %s\n' "$1" >&2; exit 2 ;;
  esac
done

log() { printf '[media] %s\n' "$*"; }
command -v ffmpeg >/dev/null || { printf '[media] FATAL: нет ffmpeg\n' >&2; exit 1; }
ffmpeg -hide_banner -encoders 2>/dev/null | grep -q libx264 || { printf '[media] FATAL: ffmpeg без libx264\n' >&2; exit 1; }

mkdir -p "$OUT"

gen_video() { # name width height fps profile level bitrate_kbps codec params_flag
  local name="$1" w="$2" h="$3" fps="$4" prof="$5" level="$6" br="$7" codec="$8" flag="$9"
  local out="$OUT/$name"
  if [[ -f "$out" && "$FORCE" -eq 0 ]]; then
    log "skip $name (есть)"
    return 0
  fi
  log "генерю $name (${w}x${h}@${fps}, ${codec}, ${prof} ${level}, ${br}k)"
  local extra=()
  if [[ "$codec" == "libx264" ]]; then
    extra=(-profile:v "$prof" -level:v "$level" -bsf:v h264_mp4toannexb)
  else
    extra=(-x265-params "log-level=error:bframes=0:rc-lookahead=0:scenecut=0:frame-threads=1:pools=1:repeat-headers=1")
  fi
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=size=${w}x${h}:rate=${fps}" -t "$SECONDS_GEN" \
    -c:v "$codec" -preset veryfast -tune zerolatency -pix_fmt yuv420p \
    -b:v "${br}k" -maxrate "${br}k" -bufsize "$((br * 2))k" \
    -g "$fps" -keyint_min "$fps" "${extra[@]}" -f "$flag" "$out"
}

# --- видео: ровно те профили, что принимает наш UAS -------------------------
gen_video 720p30_baseline.h264   1280 720  30 baseline 3.1 1500 libx264 h264
gen_video 1080p30_baseline.h264  1920 1080 30 baseline 4.0 2500 libx264 h264
gen_video 360p15_baseline.h264   640  360  15 baseline 3.0 384  libx264 h264
gen_video 1080p60_main.h265      1920 1080 60 main     -   4000 libx265 hevc

# --- аудио: сырые PCM 20 мс кадры, бот кодирует сам ------------------------
if [[ ! -f "$OUT/speech_8k.s16" || "$FORCE" -eq 1 ]]; then
  log "генерю speech_8k.s16 (1 кГц тон + AM) — 120 с, 8000 Гц, s16le"
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "sine=frequency=1000:sample_rate=8000:duration=${SECONDS_GEN}" \
    -af "volume='0.5+0.4*sin(2*PI*t*2)':eval=frame" \
    -f s16le -acodec pcm_s16le -ac 1 "$OUT/speech_8k.s16"
fi
if [[ ! -f "$OUT/speech_48k.s16" || "$FORCE" -eq 1 ]]; then
  log "генерю speech_48k.s16 — 120 с, 48000 Гц, s16le"
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "sine=frequency=1000:sample_rate=48000:duration=${SECONDS_GEN}" \
    -af "volume='0.5+0.4*sin(2*PI*t*2)':eval=frame" \
    -f s16le -acodec pcm_s16le -ac 1 "$OUT/speech_48k.s16"
fi

# --- индекс с провенансом ---------------------------------------------------
{
  printf '# media manifest\n'
  printf '# generated: %s\n' "$(date -Iseconds)"
  printf '# ffmpeg: %s\n' "$(ffmpeg -hide_banner -version | head -1)"
  for f in "$OUT"/*; do
    [[ -f "$f" ]] || continue
    printf '%s  %s  %s\n' "$(sha256sum "$f" | awk '{print $1}')" "$(stat -c %s "$f")" "$(basename "$f")"
  done
} >"$OUT/SHA256SUM"
log "манифест: $OUT/SHA256SUM"
ls -l "$OUT"

# КРИТЕРИЙ ПРИЁМКИ (проверяется глазами/скриптом):
#   ffprobe -hide_banner -select_streams v:0 -show_entries stream=codec_name,width,height,r_frame_rate,profile,level -of default=nw=1 /tmp/vks-bench/media/720p30_baseline.h264
#   ожидаем: codec_name=h264 width=1280 height=720 r_frame_rate=30/1 profile=Constrained Baseline level=31
