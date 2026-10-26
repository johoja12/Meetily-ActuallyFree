/**
 * Automatic voice learning (Labs voice profiles). Naming a speaker learns that
 * contact's voice in the native worker, which then names matching unnamed
 * speakers in other meetings. This module carries those results to the UI.
 */
import { invoke } from '@tauri-apps/api/core';
import type { VoiceMatch } from './voice-match-summary';

export type { VoiceMatch } from './voice-match-summary';

/** Native events emitted by `diarization/voice_learning.rs`. */
export const VOICE_LEARNING_STATUS_EVENT = 'voice-learning-status';
export const VOICE_MATCHES_APPLIED_EVENT = 'voice-matches-applied';
export const NATIVE_VOICE_PROFILES_CHANGED_EVENT = 'voice-profiles-changed';

/** Window event: these meetings' speaker labels changed. detail: { meetingIds } */
export const VOICE_MATCHES_CHANGED_EVENT = 'meetily-voice-matches-changed';

export function announceVoiceMatchChanges(meetingIds: string[]) {
  if (typeof window === 'undefined' || meetingIds.length === 0) return;
  window.dispatchEvent(new CustomEvent(VOICE_MATCHES_CHANGED_EVENT, { detail: { meetingIds } }));
}

/** Restores the original labels and keeps those names from coming back. */
export function undoVoiceMatches(matches: VoiceMatch[]): Promise<string[]> {
  return invoke<string[]>('undo_voice_matches', {
    matches: matches.map(({ meeting_id, person_id, original_label }) => ({ meeting_id, person_id, original_label })),
  });
}

/** Starts a background check of every saved meeting against the saved voices. */
export function applyVoicesToPastMeetings(): Promise<void> {
  return invoke<void>('apply_voices_to_past_meetings');
}

export function getVoiceLearningRunning(): Promise<boolean> {
  return invoke<boolean>('get_voice_learning_running');
}
