'use client';
import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { CalendarDays, RefreshCw } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';
import { Checkbox } from '@/components/ui/checkbox';
import { useCalendar, type CalendarSettings as Settings } from '@/hooks/useCalendar';

export function CalendarSettings() {
  const { snapshot, error, reload } = useCalendar();
  const [working, setWorking] = useState(false);
  const perform = async (command: string, args?: Record<string, unknown>) => {
    setWorking(true);
    try { await invoke(command, args); }
    catch (error) { toast.error('Could not update calendar access', { description: String(error) }); }
    finally { await reload(); setWorking(false); }
  };
  if (!snapshot) return <p className="text-sm text-af-text-3">{error || 'Loading calendar settings…'}</p>;
  const busy = working || snapshot.busy;
  const connected = snapshot.settings.enabled;
  const update = (patch: Partial<Settings>) => { void perform('calendar_update_settings', { settings: { ...snapshot.settings, ...patch } }); };
  return (
    <div className="space-y-5">
      <section className="rounded-2xl border border-af-border bg-af-panel-2/40 p-5">
        <h3 className="flex items-center gap-2 text-[15px] font-semibold text-af-text"><CalendarDays className="h-5 w-5 text-af-accent" />Your calendar, on this Mac</h3>
        <p className="mt-2 text-[13px] leading-relaxed text-af-text-3">
          Suggest meeting titles and remind you to record from calendars synced to macOS Calendar.
          Meetily reads events locally and never edits your calendar. macOS calls the required permission full calendar access. No Microsoft app registration is needed.
        </p>
        {!snapshot.supported ? <p className="mt-3 text-sm text-af-text-2">Local calendar access is currently available on macOS.</p> : <>
          <p className="mt-3 text-xs leading-relaxed text-af-text-3">For an Outlook work calendar, first add your Exchange account in Calendar → Add Account. Then connect and choose it below.</p>
          <div className="mt-4 flex flex-wrap gap-2">
            <Button aria-label={connected ? 'Refresh calendars' : 'Connect macOS Calendar'} size="sm" variant={connected ? 'secondary' : 'primary'} disabled={busy} loading={busy} onClick={() => { void perform(connected ? 'calendar_refresh' : 'calendar_connect'); }}>
              {connected ? <RefreshCw /> : <CalendarDays />}{connected ? 'Refresh calendars' : 'Connect macOS Calendar'}
            </Button>
            {connected && <Button size="sm" variant="ghost" disabled={busy} onClick={() => { void perform('calendar_disconnect'); }}>Disconnect</Button>}
          </div>
        </>}
        {(snapshot.error || error) && <p role="alert" className="mt-3 text-sm text-af-warning">{snapshot.error || error}</p>}
      </section>
      {connected && snapshot.authorization === 'authorized' && <>
        <section className="rounded-2xl border border-af-border bg-af-panel-2/40 p-5">
          <h3 className="text-[15px] font-semibold text-af-text">Calendars to use</h3>
          <p className="mt-1 text-xs text-af-text-3">Only selected calendars supply suggestions and reminders.</p>
          {!snapshot.calendars.length ? <p className="mt-4 text-sm text-af-text-3">No calendars found. Add your account in macOS Calendar, then refresh.</p> :
            <div className="mt-4 space-y-3">{snapshot.calendars.map(calendar => (
              <label key={calendar.id} className="flex items-center gap-3 text-sm text-af-text-2">
                <Checkbox aria-label={calendar.name} checked={snapshot.settings.selected_calendar_ids.includes(calendar.id)} disabled={busy}
                  onCheckedChange={checked => update({ selected_calendar_ids: checked === true ? [...snapshot.settings.selected_calendar_ids, calendar.id] : snapshot.settings.selected_calendar_ids.filter(id => id !== calendar.id) })} />
                <span>{calendar.name}</span>
              </label>
            ))}</div>}
          {!snapshot.settings.selected_calendar_ids.length && <p className="mt-3 text-xs text-af-warning">Choose a calendar to enable suggestions.</p>}
        </section>
        <section className="space-y-5 rounded-2xl border border-af-border bg-af-panel-2/40 p-5">
          <label className="flex items-center justify-between gap-4"><span><span className="block text-sm font-medium text-af-text">Suggest meeting titles</span><span className="mt-1 block text-xs text-af-text-3">Choose an event on the recording screen to use its title.</span></span><Switch aria-label="Suggest meeting titles" checked={snapshot.settings.suggest_titles} disabled={busy} onCheckedChange={suggest_titles => update({ suggest_titles })} /></label>
          <label className="flex items-center justify-between gap-4"><span><span className="block text-sm font-medium text-af-text">Remind me to record</span><span className="mt-1 block text-xs text-af-text-3">Prompt for events with participants or meeting links while Meetily is open. Recording starts only when you click Record.</span></span><Switch aria-label="Remind me to record" checked={snapshot.settings.prompt_recording} disabled={busy} onCheckedChange={prompt_recording => update({ prompt_recording })} /></label>
          <label className="flex items-center justify-between gap-4 text-sm text-af-text-2">Remind me
            <select aria-label="Calendar reminder timing" className="rounded-lg border border-af-border bg-af-panel px-3 py-2 text-sm" disabled={busy} value={snapshot.settings.reminder_minutes} onChange={event => update({ reminder_minutes: Number(event.target.value) })}>
              {[0, 2, 5, 10, 15].map(minutes => <option key={minutes} value={minutes}>{minutes ? `${minutes} minutes before` : 'When the meeting starts'}</option>)}
            </select>
          </label>
          <p className="text-xs text-af-text-4">All-day, free, cancelled and declined events are excluded. Calendar events never stop an ongoing recording.</p>
        </section>
      </>}
    </div>
  );
}
