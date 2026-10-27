'use client';
import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { MEETING_CALENDAR_LINK_EVENT, type CalendarAttendee, type MeetingCalendarLink } from '@/lib/calendar-attendees';
import { onLiveSessionChange, readLiveTitle } from '@/lib/live-session';

/**
 * Attendees of the calendar event a meeting was named from: the saved link
 * for a saved meeting, or the live title's event during a recording.
 */
export function useInviteAttendees(meetingId: string | undefined, open: boolean): CalendarAttendee[] {
  const [attendees, setAttendees] = useState<CalendarAttendee[]>([]);
  useEffect(() => {
    if (!open) return;
    if (!meetingId) {
      const read = () => setAttendees(readLiveTitle()?.calendarEvent?.attendees ?? []);
      read();
      return onLiveSessionChange(read);
    }
    let current = true;
    const load = () => invoke<MeetingCalendarLink | null>('get_meeting_calendar_event', { meetingId })
      .then(link => { if (current) setAttendees(link?.attendees ?? []); })
      .catch(() => { if (current) setAttendees([]); });
    void load();
    const changed = (event: Event) => {
      if ((event as CustomEvent<{ meetingId?: string }>).detail?.meetingId === meetingId) void load();
    };
    window.addEventListener(MEETING_CALENDAR_LINK_EVENT, changed);
    return () => { current = false; window.removeEventListener(MEETING_CALENDAR_LINK_EVENT, changed); };
  }, [meetingId, open]);
  return attendees;
}
