// Run with: node --test tests/lib/transcript-provenance.test.ts
import { describe, test } from 'node:test';
import assert from 'node:assert/strict';
import {
  diarizationEngineLabel,
  isEnhancementRecommended,
  speakersNotIdentified,
  transcriptModelLabel,
  type TranscriptProvenance,
} from '../../src/lib/transcript-provenance.ts';

const unknown: TranscriptProvenance = {
  transcript_pass: null,
  transcript_provider: null,
  transcript_model: null,
  speakers_identified_by: null,
};
const live: TranscriptProvenance = {
  ...unknown,
  transcript_pass: 'live',
  transcript_provider: 'parakeet',
  transcript_model: 'parakeet-tdt-0.6b-v3-int8',
};

describe('transcriptModelLabel', () => {
  test('names the provider once', () => {
    assert.equal(transcriptModelLabel(live), 'Parakeet tdt-0.6b-v3-int8');
    assert.equal(
      transcriptModelLabel({ ...live, transcript_provider: 'localWhisper', transcript_model: 'large-v3-turbo' }),
      'Whisper large-v3-turbo',
    );
  });

  test('keeps unfamiliar providers and is empty when unknown', () => {
    assert.equal(transcriptModelLabel({ ...live, transcript_provider: 'acme', transcript_model: 'v2' }), 'acme v2');
    assert.equal(transcriptModelLabel(unknown), null);
  });
});

describe('isEnhancementRecommended', () => {
  test('only for a live transcript with its recording', () => {
    assert.equal(isEnhancementRecommended(live, true), true);
    assert.equal(isEnhancementRecommended(live, false), false);
    assert.equal(isEnhancementRecommended({ ...live, transcript_pass: 'enhanced' }, true), false);
    assert.equal(isEnhancementRecommended(unknown, true), false);
    assert.equal(isEnhancementRecommended(null, true), false);
  });
});

describe('speakersNotIdentified', () => {
  test('never claims "not identified" for meetings saved before tracking', () => {
    assert.equal(speakersNotIdentified(unknown), false);
    assert.equal(speakersNotIdentified(live), true);
    assert.equal(speakersNotIdentified({ ...live, speakers_identified_by: 'nemotron' }), false);
    assert.equal(diarizationEngineLabel('nemotron'), 'Nemotron');
  });
});
