#!/bin/bash
# Logs Meetily's CPU every 15 s with a recording flag derived from the app log,
# to compare real meetings before/after CPU work: the native process, its
# ffmpeg children, and the WebKit WebContent process that renders its windows.
#
# WebContent runs as an XPC service parented to launchd, so it is matched by
# launch time: the first WebContent process started within 10 s after the
# native process. `webview` is empty when none matches. `cputime` columns are
# cumulative CPU time (exact deltas); `%cpu` is ps's decaying average.
OUT="${1:?usage: sample-meetily-cpu.sh <out.log>}"; LOG="$HOME/Library/Logs/Meetily/meetily.log"
start_seconds() { date -j -f "%a %b %d %T %Y" "$(ps -o lstart= -p "$1")" +%s 2>/dev/null; }
webcontent_for() {
  local native_start; native_start=$(start_seconds "$1") || return
  for W in $(pgrep -f "com.apple.WebKit.WebContent.xpc"); do
    local s; s=$(start_seconds "$W") || continue
    if (( s >= native_start && s - native_start <= 10 )); then echo "$W"; return; fi
  done
}
P_KNOWN=""; W=""
while true; do
  P=$(pgrep -f "Meetily - Actually Free.app/Contents/MacOS/meetily" | head -1)
  if [ -n "$P" ]; then
    if [ "$P" != "$P_KNOWN" ] || ! ps -p "${W:-0}" >/dev/null 2>&1; then W=$(webcontent_for "$P"); P_KNOWN=$P; fi
    CPU=$(ps -o pcpu= -p "$P" | tr -d ' ')
    TIME=$(ps -o time= -p "$P" | tr -d ' ')
    KIDS=$(pgrep -P "$P" | xargs -r ps -o pcpu= -p 2>/dev/null | awk '{s+=$1} END{print s+0}')
    WEB=""; [ -n "$W" ] && WEB="webview_pid=$W webview_cpu=$(ps -o pcpu= -p "$W" | tr -d ' ') webview_cputime=$(ps -o time= -p "$W" | tr -d ' ')"
    REC=$(grep -E "Recording started|Recording stopped successfully|IS_RECORDING to true" "$LOG" 2>/dev/null | tail -1 | grep -qE "started|IS_RECORDING to true" && echo 1 || echo 0)
    echo "$(date -u +%FT%TZ) pid=$P cpu=$CPU cputime=$TIME children=$KIDS $WEB recording=$REC load=$(sysctl -n vm.loadavg | awk '{print $2}')" >> "$OUT"
  fi
  sleep 15
done
