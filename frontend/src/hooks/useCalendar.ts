'use client';
import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { CalendarMeeting } from '@/lib/calendar-meetings';

export interface CalendarSettings {
  enabled: boolean;
  suggest_titles: boolean;
  prompt_recording: boolean;
  reminder_minutes: number;
  selected_calendar_ids: string[];
  /** Prompt when the microphone hears the meeting start, not at a set time. */
  listen_for_start?: boolean;
}
export interface CalendarSnapshot {
  supported: boolean;
  settings: CalendarSettings;
  authorization: string;
  calendars: Array<{ id: string; name: string }>;
  events: CalendarMeeting[];
  busy: boolean;
  error: string | null;
  last_updated_ms: number | null;
}

/** Reads the native cache; subscribing or navigating never starts another calendar query. */
export function useCalendar() {
  const [snapshot, setSnapshot] = useState<CalendarSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(false);
  const sequence = useRef(0);
  const reload = useCallback(async () => {
    const request = ++sequence.current;
    try {
      const value = await invoke<CalendarSnapshot>('calendar_snapshot');
      if (mounted.current && request === sequence.current) { setSnapshot(value); setError(null); }
    } catch {
      if (mounted.current && request === sequence.current) setError('Calendar settings could not be loaded.');
    }
  }, []);
  useEffect(() => {
    mounted.current = true;
    let disposed = false;
    void reload();
    let stop: (() => void) | undefined;
    void listen('calendar-changed', () => { void reload(); }).then(unlisten => {
      if (!disposed) stop = unlisten; else unlisten();
    }).catch(() => undefined);
    const timer = window.setInterval(() => { void reload(); }, 30_000);
    return () => { disposed = true; mounted.current = false; sequence.current++; stop?.(); window.clearInterval(timer); };
  }, [reload]);
  return { snapshot, error, reload };
}
