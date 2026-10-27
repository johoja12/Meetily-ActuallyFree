/**
 * The invite's attendees as speaker-picker suggestions. Native code already
 * leaves out the user, rooms and people who declined, and cleans names; this
 * matches them to contacts (email first, then name) and orders them.
 */
import { splitSpeakerLabel } from '@/utils/speakerUtils';

export interface CalendarAttendee {
  name: string;
  email: string | null;
  organizer: boolean;
}

/** What `get_meeting_calendar_event` returns. */
export interface MeetingCalendarLink {
  eventId: string;
  title: string;
  startMs: number;
  endMs: number;
  attendees: CalendarAttendee[];
}

export interface InviteSuggestion {
  key: string;
  /** The name to apply: the matched contact's, else the attendee's. */
  name: string;
  email: string | null;
  personId?: string;
  /** Already used for a speaker in this meeting. */
  assigned: boolean;
}

interface ContactLike { id: string; displayName: string; email?: string | null }

const nameKey = (value: string) => value.trim().replace(/\s+/g, ' ').toLowerCase();
const emailKey = (value: string | null | undefined) => value?.trim().toLowerCase() || null;

export function inviteSuggestions(attendees: CalendarAttendee[], people: ContactLike[], speakerLabels: string[]): InviteSuggestion[] {
  const used = new Set(speakerLabels.flatMap(splitSpeakerLabel).map(nameKey));
  const seen = new Set<string>();
  const entries: InviteSuggestion[] = [];
  for (const attendee of attendees) {
    const email = emailKey(attendee.email);
    const attendeeName = attendee.name.trim().replace(/\s+/g, ' ');
    if (!attendeeName) continue;
    const person = (email && people.find(candidate => emailKey(candidate.email) === email))
      || people.find(candidate => nameKey(candidate.displayName) === nameKey(attendeeName));
    const name = person?.displayName ?? attendeeName;
    const key = person ? `person:${person.id}` : email ? `email:${email}` : `name:${nameKey(name)}`;
    // A name-only entry repeating someone already listed is the same person.
    const names = `name:${nameKey(name)}`;
    if (seen.has(key) || (email && seen.has(`email:${email}`)) || (!email && seen.has(names))) continue;
    for (const value of [key, names, email && `email:${email}`]) if (value) seen.add(value);
    entries.push({ key, name, email, personId: person?.id, assigned: used.has(nameKey(name)) });
  }
  return [...entries.filter(entry => !entry.assigned), ...entries.filter(entry => entry.assigned)];
}
