'use client';
import { useEffect, useRef, useState } from 'react';
import Link from 'next/link';
import { invoke } from '@tauri-apps/api/core';
import { CalendarDays } from 'lucide-react';
import { useCalendar } from '@/hooks/useCalendar';
import type { CalendarMeeting } from '@/lib/calendar-meetings';
import { Button } from '@/components/ui/button';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { calendarTime } from './CalendarMeetings';

export interface RecordingCalendarSuggestions {
  start_ms: number;
  end_ms: number | null;
  matches: Array<{ event: CalendarMeeting; overlap_ms: number; gap_ms: number }>;
}

/** Keying the inner view cancels UI ownership when navigation changes the meeting. */
export function SavedCalendarTitleSuggestions(props: { meetingId: string; onRename: (title: string) => Promise<boolean> }) {
  return <SavedSuggestions key={props.meetingId} {...props} />;
}
function SavedSuggestions({ meetingId, onRename }: { meetingId: string; onRename: (title: string) => Promise<boolean> }) {
  const { snapshot } = useCalendar();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [saving, setSaving] = useState(false);
  const [result, setResult] = useState<RecordingCalendarSuggestions | null>(null);
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);
  useEffect(() => () => { generation.current++; }, []);
  const configured = snapshot?.settings.enabled && snapshot.settings.suggest_titles && snapshot.settings.selected_calendar_ids.length > 0;
  // A settings change invalidates private event choices, including a pending read.
  const selection = JSON.stringify(snapshot?.settings);
  useEffect(() => { generation.current++; setResult(null); setBusy(false); setSaving(false); setError(null); }, [selection]);
  async function load() {
    const request = ++generation.current;
    setBusy(true); setError(null); setResult(null);
    try {
      const next = await invoke<RecordingCalendarSuggestions>('calendar_recording_suggestions', { meetingId });
      if (generation.current === request) setResult(next);
    } catch (error) {
      if (generation.current === request) setError(String(error));
    } finally { if (generation.current === request) setBusy(false); }
  }
  return <Popover open={open} onOpenChange={next => {
    if (saving) return;
    setOpen(next);
    if (next && configured) void load();
    if (!next) { generation.current++; setResult(null); setBusy(false); }
  }}>
    <PopoverTrigger asChild><Button variant="ghost" size="sm"><CalendarDays size={14} />Suggest name</Button></PopoverTrigger>
    <PopoverContent align="start" className="w-80 max-w-[calc(100vw-2rem)] p-2">
      <p className="px-2 py-2 text-xs font-medium text-af-text-2">Match this recording to your calendar</p>
      {!configured ? <p className="px-2 pb-2 text-xs text-af-text-3">Connect and choose calendars in <Link className="text-af-accent underline" href="/settings?section=calendar">Settings → Calendar</Link> to suggest a name.</p> : <>
        {busy && <p role="status" className="px-2 py-3 text-xs text-af-text-3">Looking up the recording’s date…</p>}
        {error && <div role="alert" className="px-2 pb-2 text-xs text-af-text-3"><p>{error}</p><Button variant="ghost" size="sm" onClick={() => void load()}>Try again</Button></div>}
        {result && <>
          <p className="px-2 pb-2 text-xs text-af-text-3">{new Date(result.start_ms).toLocaleDateString()} · {result.end_ms ? 'Best time overlaps first. Choose a title to apply it.' : 'Duration unavailable; matching near the start time.'}</p>
          {!result.matches.length && <p className="px-2 py-3 text-xs text-af-text-3">No events overlap or fall within 10 minutes of this recording.</p>}
          <div className="max-h-72 overflow-y-auto">{result.matches.map(({ event, overlap_ms, gap_ms }) => <button type="button" key={`${event.id}:${event.start_ms}`} disabled={saving} className="block w-full rounded-lg px-2 py-2 text-left hover:bg-af-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-af-accent disabled:opacity-50" onClick={async () => {
            const request = generation.current;
            setSaving(true); setError(null);
            try {
              const renamed = await onRename(event.title);
              if (request === generation.current) {
                if (renamed) { setOpen(false); setResult(null); }
                else setError('Could not rename this recording. Try again.');
              }
            } catch { if (request === generation.current) setError('Could not rename this recording. Try again.'); }
            finally { if (request === generation.current) setSaving(false); }
          }}>
            <span className="block text-sm font-medium">{event.title}</span>
            <span className="mt-1 block text-xs text-af-text-3">{calendarTime(event)} · {event.calendar_name}</span>
            <span className="mt-1 block text-[11px] text-af-text-3">{overlap_ms > 0 ? `${Math.max(1, Math.round(overlap_ms / 60_000))} min overlap` : gap_ms > 0 ? `${Math.ceil(gap_ms / 60_000)} min from recording` : 'At recording start'}</span>
          </button>)}</div>
        </>}
      </>}
    </PopoverContent>
  </Popover>;
}
