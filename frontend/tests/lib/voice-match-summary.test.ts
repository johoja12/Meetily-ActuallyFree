// Run with: node --test tests/lib/voice-match-summary.test.ts
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { describeMatchedMeetings, describeVoiceMatches, type VoiceMatch } from '../../src/lib/voice-match-summary.ts';

const match = (meeting: string, name: string, title = `Meeting ${meeting}`): VoiceMatch => ({
  meeting_id: meeting,
  meeting_title: title,
  person_id: `p-${name}`,
  name,
  original_label: 'Speaker 2',
});

test('one person across meetings', () => {
  assert.equal(describeVoiceMatches([match('m1', 'David Gurley')]), 'Named David Gurley in 1 meeting');
  assert.equal(describeVoiceMatches([match('m1', 'David Gurley'), match('m2', 'David Gurley')]), 'Named David Gurley in 2 meetings');
});

test('several people are listed briefly and meetings are counted once', () => {
  assert.equal(describeVoiceMatches([match('m1', 'Ana'), match('m1', 'Ben')]), 'Named Ana and Ben in 1 meeting');
  assert.equal(
    describeVoiceMatches([match('m1', 'Ana'), match('m2', 'Ben'), match('m3', 'Cy'), match('m3', 'Di')]),
    'Named Ana, Ben and 2 others in 3 meetings',
  );
});

test('meeting titles are deduplicated and truncated', () => {
  assert.equal(describeMatchedMeetings([match('m1', 'Ana', 'Standup'), match('m1', 'Ben', 'Standup')]), 'Standup');
  assert.equal(describeMatchedMeetings([match('m1', 'Ana', '  ')]), 'Untitled meeting');
  const many = ['A', 'B', 'C', 'D', 'E'].map((title, index) => match(`m${index}`, 'Ana', title));
  assert.equal(describeMatchedMeetings(many), 'A, B, C and 2 more');
});
