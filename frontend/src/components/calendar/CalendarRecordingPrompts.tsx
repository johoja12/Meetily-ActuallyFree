'use client';
import { useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import type { CalendarMeeting } from '@/lib/calendar-meetings';
import { calendarTime, startCalendarRecording } from './CalendarMeetings';

/** Mounted once above navigation. Native code owns scheduling and deduplication. */
export function CalendarRecordingPrompts({ enabled, onStart }: { enabled: boolean; onStart: () => void }) {
  const start = useRef(onStart);
  start.current = onStart;
  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    const drain = async () => {
      try {
        const prompts = await invoke<CalendarMeeting[]>('calendar_take_prompts');
        if (disposed) return;
        for (const event of prompts) {
          toast('Record your scheduled meeting?', {
            id: `calendar:${event.id}:${event.start_ms}`,
            description: `${event.title} · ${calendarTime(event)}`,
            duration: 60_000,
            action: { label: 'Record', onClick: () => { void startCalendarRecording(event, () => start.current()); } },
          });

        }
      } catch { /* Settings shows provider errors; recording remains available. */ }
    };
    void listen('calendar-changed', () => { void drain(); }).then(unlisten => {
      if (disposed) unlisten(); else { stop = unlisten; void drain(); }
    }).catch(() => { void drain(); });
    const timer = window.setInterval(() => { void drain(); }, 30_000);
    return () => { disposed = true; stop?.(); window.clearInterval(timer); };
  }, [enabled]);
  return null;
}
