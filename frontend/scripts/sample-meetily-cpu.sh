#!/bin/bash
# Logs Meetily (native) and its ffmpeg children's CPU every 15 s with a recording flag
# derived from the app log, to compare real meetings before/after the CPU work.
OUT="${1:?usage: sample-meetily-cpu.sh <out.log>}"; LOG="$HOME/Library/Logs/Meetily/meetily.log"
while true; do
  P=$(pgrep -f "Meetily - Actually Free.app/Contents/MacOS/meetily" | head -1)
  if [ -n "$P" ]; then
    CPU=$(ps -o pcpu= -p "$P" | tr -d ' ')
    TIME=$(ps -o time= -p "$P" | tr -d ' ')
    KIDS=$(pgrep -P "$P" | xargs -r ps -o pcpu= -p 2>/dev/null | awk '{s+=$1} END{print s+0}')
    REC=$(grep -E "Recording started|Recording stopped successfully|IS_RECORDING to true" "$LOG" 2>/dev/null | tail -1 | grep -qE "started|IS_RECORDING to true" && echo 1 || echo 0)
    echo "$(date -u +%FT%TZ) pid=$P cpu=$CPU cputime=$TIME children=$KIDS recording=$REC load=$(sysctl -n vm.loadavg | awk '{print $2}')" >> "$OUT"
  fi
  sleep 15
done
