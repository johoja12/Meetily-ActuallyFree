'use client';
import { useEffect, useState } from 'react';
import { useRouter } from 'next/navigation';
import { invoke } from '@tauri-apps/api/core';
import { CalendarDays } from 'lucide-react';
import { toast } from 'sonner';
import { useCalendar } from '@/hooks/useCalendar';
import { calendarTitleSuggestions, OPEN_CALENDAR_TITLE_EVENT, selectCalendarRecording, type CalendarMeeting } from '@/lib/calendar-meetings';
import { launchRecording } from '@/lib/recording-launch';
import { Button } from '@/components/ui/button';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';

export const calendarTime = (event: CalendarMeeting) => {
  const time = (value: number) => new Date(value).toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' }).replace(/\u202f/g, ' ');
  return `${time(event.start_ms)}–${time(event.end_ms)}`;
};

let startPending = false;
/** Recheck native state: a reminder toast may outlive the recording it offered. */
export async function startCalendarRecording(event: CalendarMeeting, start: () => void) {
  if (startPending) return;
  startPending = true;
  try {
    const state = await invoke<{ is_recording: boolean }>('get_recording_state');
    if (state.is_recording) { toast.info('A recording is already running'); return; }
    if (!calendarTitleSuggestions([event], Date.now()).length) { toast.info('This calendar event is no longer starting'); return; }
    selectCalendarRecording(event);
    start();
  } catch {
    toast.error('Could not start the calendar recording', { description: 'Open the recording screen and try again.' });
  } finally { startPending = false; }
}

export function CalendarAgenda() {
  const { snapshot } = useCalendar();
  const router = useRouter();
  const [working, setWorking] = useState(false);
  if (!snapshot?.settings.enabled || !snapshot.settings.suggest_titles) return null;
  const events = calendarTitleSuggestions(snapshot.events, Date.now()).slice(0, 3);
  if (!events.length) return null;
  return (
    <section aria-label="Calendar meetings" className="mb-8 rounded-2xl border border-af-border bg-af-panel-2/40 p-4">
      <h2 className="mb-3 flex items-center gap-2 text-xs font-medium text-af-text-3"><CalendarDays size={14} />From your calendar</h2>
      <div className="space-y-3">{events.map(event => <div key={`${event.id}:${event.start_ms}`} className="flex items-center justify-between gap-4">
        <div className="min-w-0"><p className="truncate text-sm font-medium text-af-text">{event.title}</p><p className="mt-0.5 text-xs text-af-text-3">{calendarTime(event)} · {event.calendar_name}</p></div>
        <Button variant="secondary" size="sm" disabled={working} onClick={() => {
          setWorking(true);
          void startCalendarRecording(event, () => launchRecording(href => router.push(href))).finally(() => setWorking(false));
        }}>Record</Button>
      </div>)}</div>
    </section>
  );
}

export function CalendarTitleSuggestions({ startedAt, onSelect, disabled = false }: { startedAt: number; onSelect: (event: CalendarMeeting) => void; disabled?: boolean }) {
  const { snapshot } = useCalendar();
  const [open, setOpen] = useState(false);
  // "Choose" on the record-start toast opens this menu.
  useEffect(() => {
    const show = () => setOpen(true);
    window.addEventListener(OPEN_CALENDAR_TITLE_EVENT, show);
    return () => window.removeEventListener(OPEN_CALENDAR_TITLE_EVENT, show);
  }, []);
  if (!snapshot?.settings.enabled || !snapshot.settings.suggest_titles) return null;
  const events = calendarTitleSuggestions(snapshot.events, startedAt).slice(0, 8);
  if (!events.length) return null;
  return <Popover open={open} onOpenChange={setOpen}>
    <PopoverTrigger asChild><Button variant="ghost" size="sm" disabled={disabled}><CalendarDays />Calendar title</Button></PopoverTrigger>
    <PopoverContent align="start" className="w-80 rounded-xl border border-af-border bg-af-elevated p-2 text-af-text">
      <p className="px-2 py-2 text-xs text-af-text-3">Which event are you recording?</p>
      {events.map(event => <button type="button" key={`${event.id}:${event.start_ms}`} className="block w-full rounded-lg px-2 py-2 text-left hover:bg-af-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-af-accent" onClick={() => { onSelect(event); setOpen(false); }}>
        <span className="block text-sm font-medium">{event.title}</span><span className="mt-1 block text-xs text-af-text-3">{calendarTime(event)} · {event.calendar_name}</span>
      </button>)}
    </PopoverContent>
  </Popover>;
}
