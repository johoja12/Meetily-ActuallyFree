/**
 * Wording for names that saved voices gave to unnamed speakers. Kept free of
 * Tauri imports so it can be tested under node.
 */
export interface VoiceMatch {
  meeting_id: string;
  meeting_title: string;
  person_id: string;
  name: string;
  /** The meeting-local label the name replaced, e.g. "Speaker 3". */
  original_label: string;
}

function listNames(names: string[]): string {
  if (names.length <= 2) return names.join(' and ');
  const others = names.length - 2;
  return `${names[0]}, ${names[1]} and ${others} other${others === 1 ? '' : 's'}`;
}

/** "Named David Gurley in 2 meetings" */
export function describeVoiceMatches(matches: VoiceMatch[]): string {
  const names = [...new Set(matches.map((match) => match.name))];
  const meetings = new Set(matches.map((match) => match.meeting_id)).size;
  return `Named ${listNames(names)} in ${meetings} meeting${meetings === 1 ? '' : 's'}`;
}

/** Up to three meeting titles, then "and N more". */
export function describeMatchedMeetings(matches: VoiceMatch[]): string {
  const titles: string[] = [];
  for (const match of matches) {
    const title = match.meeting_title.trim() || 'Untitled meeting';
    if (!titles.includes(title)) titles.push(title);
  }
  const shown = titles.slice(0, 3).join(', ');
  return titles.length > 3 ? `${shown} and ${titles.length - 3} more` : shown;
}
