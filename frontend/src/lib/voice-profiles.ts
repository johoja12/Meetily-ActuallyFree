/**
 * Labs voice profiles: a contact's voice, learned from the call audio of a
 * meeting they spoke in. Later meetings name a matching speaker after them
 * when speakers are identified. Rust keeps each voice under its contact's
 * current name and forgets it when the contact is deleted.
 */
import { invoke } from '@tauri-apps/api/core';
import { setLabsFeature } from '@/lib/labs-features';

export interface VoiceProfile {
  person_id: string;
  name: string;
  /** Clear turns the voice was learned from. */
  samples: number;
  /** Meetings those turns came from. */
  meetings: number;
}

/** "14 clear turns in 3 meetings" */
export function describeVoiceSource(profile: Pick<VoiceProfile, 'samples' | 'meetings'>): string {
  const meetings = Math.max(1, profile.meetings);
  return `${profile.samples} clear turn${profile.samples === 1 ? '' : 's'} in ${meetings} meeting${meetings === 1 ? '' : 's'}`;
}

export const VOICE_PROFILES_CHANGED_EVENT = 'meetily-voice-profiles-changed';

function announce() {
  window.dispatchEvent(new Event(VOICE_PROFILES_CHANGED_EVENT));
}

export function listVoiceProfiles(): Promise<VoiceProfile[]> {
  return invoke<VoiceProfile[]>('list_voice_profiles');
}

/**
 * Without a meeting, (re)learns the voice from all the contact's recent
 * meetings, including ones recorded since it was first learned. With one,
 * adds that meeting's audio to the voice.
 */
export async function learnContactVoice(personId: string, meetingId?: string): Promise<VoiceProfile> {
  const profile = await invoke<VoiceProfile>('enroll_person_voice', { personId, meetingId: meetingId ?? null });
  announce();
  return profile;
}

/** Adds one named speaker's lines in a saved meeting to their voice. */
export async function learnSpeakerVoice(meetingId: string, speaker: string): Promise<VoiceProfile> {
  const profile = await invoke<VoiceProfile>('enroll_voice_profile', { meetingId, speaker });
  announce();
  return profile;
}

export async function forgetVoice(personId: string): Promise<void> {
  await invoke('delete_voice_profile', { personId });
  announce();
}

export function describeVoiceError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** What `voice_learning_candidates` reports for a contact. */
export interface VoiceCandidates {
  /** Meetings where you named them (not a voice match). */
  named: number;
  /** Of those, meetings whose call audio is still saved. */
  with_audio: number;
  /** Meetings where a voice match named them. These never teach a voice. */
  automatic: number;
  /** The speaker models are downloaded. */
  models_ready: boolean;
}

export function getVoiceCandidates(personId: string): Promise<VoiceCandidates> {
  return invoke<VoiceCandidates>('voice_learning_candidates', { personId });
}

/** Why a voice cannot be learned yet, or null when it can. */
export function voiceLearnBlocker(candidates: VoiceCandidates, who: string): string | null {
  if (!candidates.models_ready) return 'Learning a voice needs the speaker models. Download them in Settings > Transcription.';
  if (candidates.named === 0) {
    return candidates.automatic > 0
      ? `${who} was only named automatically from their voice. Confirm or name them yourself in a meeting to learn from it.`
      : `Name ${who} as a speaker in a recorded meeting first.`;
  }
  if (candidates.with_audio === 0) {
    return `None of the meetings where you named ${who} still has its call audio. Learning needs a meeting recorded with Save audio on.`;
  }
  return null;
}

export type VoiceFilter = 'all' | 'voice' | 'none';

/** Contacts with a saved voice ('voice'), without one ('none'), or all. */
export function filterByVoice<T extends { id: string }>(people: T[], voices: VoiceProfile[] | null, filter: VoiceFilter): T[] {
  if (filter === 'all' || !voices) return people;
  const known = new Set(voices.map((voice) => voice.person_id));
  return people.filter((person) => known.has(person.id) === (filter === 'voice'));
}

/**
 * Learns a contact's voice from their meetings. With Labs voice profiles off,
 * turns them on first (learning needs them, and a learned voice is only used
 * while they are on), and back off if learning fails.
 */
export async function learnVoiceForContact(personId: string, labsOn: boolean): Promise<{ profile: VoiceProfile; turnedOn: boolean }> {
  if (!labsOn) await setLabsFeature('voiceProfiles', true);
  try {
    return { profile: await learnContactVoice(personId), turnedOn: !labsOn };
  } catch (error) {
    if (!labsOn) await setLabsFeature('voiceProfiles', false).catch(() => undefined);
    throw error;
  }
}
