'use client';

import { useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { PROBE_INTERVAL_MS, reportableLateness } from '@/lib/timer-lateness';

/**
 * Meeting automation and the end-of-meeting checks run on WebView timers while
 * the window is hidden in the tray or menu bar. Windows has no setting to stop
 * WebView2 throttling them (macOS sets backgroundThrottling), so log late
 * ticks and visibility changes to meetily.log to measure it (issue #12).
 */
export function HiddenTimerProbe() {
  useEffect(() => {
    let previous = Date.now();
    const report = (event: string, lateMs?: number) =>
      invoke('log_webview_timing', { event, visibility: document.visibilityState, lateMs: lateMs ?? null }).catch(() => {});
    const tick = window.setInterval(() => {
      const now = Date.now();
      const late = reportableLateness(previous, now);
      if (late != null) report('timer-late', late);
      previous = now;
    }, PROBE_INTERVAL_MS);
    const visibility = () => report('visibility');
    document.addEventListener('visibilitychange', visibility);
    return () => {
      window.clearInterval(tick);
      document.removeEventListener('visibilitychange', visibility);
    };
  }, []);
  return null;
}
