import { beforeEach, expect, mock, test } from 'bun:test';

const calls: Array<[string, any]> = [];
let failLearn = false;
mock.module('@tauri-apps/api/core', () => ({ invoke: async (command: string, args: any) => {
  calls.push([command, args]);
  if (command === 'enroll_person_voice' && failLearn) throw new Error('No clear audio');
  if (command === 'enroll_person_voice') return { person_id: args.personId, name: 'Jane', samples: 4, meetings: 1 };
  return null;
} }));
const values = new Map<string, string>();
const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => values.set(key, value), removeItem: (key: string) => values.delete(key) };
Object.assign(globalThis, { localStorage: storage, window: { localStorage: storage, dispatchEvent: () => true } });

const { voiceLearnBlocker, filterByVoice, learnVoiceForContact } = await import('../../src/lib/voice-profiles');
beforeEach(() => { calls.length = 0; failLearn = false; values.clear(); });

const ready = { named: 2, with_audio: 1, automatic: 0, models_ready: true };

test('the reason a voice cannot be learned yet, most actionable first', () => {
  expect(voiceLearnBlocker(ready, 'Jane')).toBeNull();
  expect(voiceLearnBlocker({ ...ready, models_ready: false }, 'Jane')).toContain('Settings > Transcription');
  expect(voiceLearnBlocker({ ...ready, named: 0, with_audio: 0 }, 'Jane')).toContain('Name Jane as a speaker');
  expect(voiceLearnBlocker({ ...ready, named: 0, with_audio: 0, automatic: 2 }, 'Jane')).toContain('named automatically');
  expect(voiceLearnBlocker({ ...ready, with_audio: 0 }, 'Jane')).toContain('call audio');
});

test('contacts filter by whether a voice is saved', () => {
  const people = [{ id: 'a' }, { id: 'b' }, { id: 'c' }];
  const voices = [{ person_id: 'b', name: 'B', samples: 3, meetings: 1 }];
  expect(filterByVoice(people, voices, 'all').map(p => p.id)).toEqual(['a', 'b', 'c']);
  expect(filterByVoice(people, voices, 'voice').map(p => p.id)).toEqual(['b']);
  expect(filterByVoice(people, voices, 'none').map(p => p.id)).toEqual(['a', 'c']);
  expect(filterByVoice(people, null, 'voice').map(p => p.id)).toEqual(['a', 'b', 'c']);
});

test('learning with Labs off turns voice profiles on first, and back off if learning fails', async () => {
  const learned = await learnVoiceForContact('p1', false);
  expect(learned.turnedOn).toBe(true);
  expect(calls.map(([command, args]) => `${command}:${JSON.stringify(args)}`)).toEqual([
    'set_voice_profiles_enabled:{"value":true}',
    'enroll_person_voice:{"personId":"p1","meetingId":null}',
  ]);
  calls.length = 0;
  failLearn = true;
  await expect(learnVoiceForContact('p1', false)).rejects.toThrow('No clear audio');
  expect(calls.map(([command]) => command)).toEqual(['set_voice_profiles_enabled', 'enroll_person_voice', 'set_voice_profiles_enabled']);
  expect(calls[2][1]).toEqual({ value: false });
  calls.length = 0;
  failLearn = false;
  expect((await learnVoiceForContact('p1', true)).turnedOn).toBe(false);
  expect(calls.map(([command]) => command)).toEqual(['enroll_person_voice']);
});
