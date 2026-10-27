/**
 * How a meeting's saved transcript was produced, as `api_get_transcript_provenance`
 * reports it. The native step that produced the rows records this in the same
 * commit (see migration 20261028000000_add_transcript_provenance). Every field is
 * null for meetings saved before that, and the header then shows nothing rather
 * than guessing.
 */
export interface TranscriptProvenance {
  transcript_pass: 'live' | 'enhanced' | string | null;
  transcript_provider: string | null;
  transcript_model: string | null;
  speakers_identified_by: string | null;
}

const PROVIDER_NAMES: Record<string, string> = {
  parakeet: 'Parakeet',
  whisper: 'Whisper',
  localWhisper: 'Whisper',
  deepgram: 'Deepgram',
  groq: 'Groq',
  openai: 'OpenAI',
};

const ENGINE_NAMES: Record<string, string> = {
  nemotron: 'Nemotron',
  pyannote: 'Pyannote',
};

/** "Parakeet tdt-0.6b-v3-int8": the provider once, then the model id without it. */
export function transcriptModelLabel(provenance: TranscriptProvenance): string | null {
  const { transcript_provider: provider, transcript_model: model } = provenance;
  if (!provider && !model) return null;
  const name = provider ? PROVIDER_NAMES[provider] ?? provider : null;
  const bareModel = model?.replace(new RegExp(`^${name ?? ''}[-_ ]`, 'i'), '').trim() || null;
  return [name, bareModel].filter(Boolean).join(' ');
}

export function diarizationEngineLabel(engine: string): string {
  return ENGINE_NAMES[engine] ?? engine;
}

/**
 * The live pass transcribes short streamed chunks while recording; enhancing
 * retranscribes the saved recording as a whole. Recommend it only when the
 * rows are known to be live and the recording is there to retranscribe.
 */
export function isEnhancementRecommended(
  provenance: TranscriptProvenance | null,
  hasRecording: boolean,
): boolean {
  return hasRecording && provenance?.transcript_pass === 'live';
}

/**
 * Whether to say speakers were not identified. Only for meetings whose pass is
 * recorded: an older meeting's null engine means unknown, not "not done".
 */
export function speakersNotIdentified(provenance: TranscriptProvenance | null): boolean {
  return !!provenance?.transcript_pass && !provenance.speakers_identified_by;
}
