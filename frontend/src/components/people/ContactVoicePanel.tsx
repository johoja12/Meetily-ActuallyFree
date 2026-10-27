'use client';

/**
 * A contact's voice, or yours with `SELF_VOICE_ID`: whether Meetily knows it,
 * and learning it from their meetings. Shown whether or not Labs voice
 * profiles are on; learning turns them on, since a learned voice is only used
 * while they are.
 */
import { useEffect, useState } from 'react';
import { toast } from 'sonner';
import { Fingerprint } from 'lucide-react';
import { useLabs } from '@/hooks/useLabs';
import { useVoiceProfiles } from '@/hooks/useVoiceProfiles';
import { setLabsFeature } from '@/lib/labs-features';
import {
  describeVoiceError,
  describeVoiceSource,
  forgetVoice,
  getVoiceCandidates,
  learnVoiceForContact,
  SELF_VOICE_ID,
  voiceLearnBlocker,
  type VoiceCandidates,
} from '@/lib/voice-profiles';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Panel, Skeleton } from '@/components/ui/surface';

export function ContactVoicePanel({ personId, first }: { personId: string; first: string }) {
  const { labs } = useLabs();
  const voices = useVoiceProfiles();
  const voice = voices?.find((profile) => profile.person_id === personId);
  const [candidates, setCandidates] = useState<VoiceCandidates | null>(null);
  const [busy, setBusy] = useState<'learn' | 'forget' | 'enable' | null>(null);
  const own = personId === SELF_VOICE_ID;
  const who = own ? 'you' : first || 'them';
  const whose = own ? 'your' : who === 'them' ? 'their' : `${who}'s`;

  // What a voice could be learned from; reread when voices or contacts change.
  useEffect(() => {
    let current = true;
    getVoiceCandidates(personId)
      .then((next) => current && setCandidates(next))
      .catch(() => current && setCandidates(null));
    return () => { current = false; };
  }, [personId, voices]);

  const blocker = candidates ? voiceLearnBlocker(candidates, who, own) : null;

  // Reads all their recent meetings, so an update picks up new ones.
  const learn = async () => {
    const updating = !!voice;
    setBusy('learn');
    try {
      const { profile, turnedOn } = await learnVoiceForContact(personId, labs.voiceProfiles);
      toast.success(`${updating ? 'Updated' : 'Learned'} ${own ? 'your' : `${first || profile.name}'s`} voice`, {
        description: `From ${describeVoiceSource(profile)}.${turnedOn ? ' Voice profiles are now on in Settings > Labs.' : ''}`,
      });
    } catch (error) {
      toast.error(`Could not ${updating ? 'update' : 'learn'} ${whose} voice`, { description: describeVoiceError(error) });
    } finally {
      setBusy(null);
    }
  };

  const forget = async () => {
    setBusy('forget');
    try {
      await forgetVoice(personId);
      toast.success(`Forgot ${whose} voice`);
    } catch (error) {
      toast.error('Could not forget the voice', { description: describeVoiceError(error) });
    } finally {
      setBusy(null);
    }
  };

  const enable = async () => {
    setBusy('enable');
    try {
      await setLabsFeature('voiceProfiles', true);
    } catch (error) {
      toast.error('Could not turn on voice profiles', { description: describeVoiceError(error) });
    } finally {
      setBusy(null);
    }
  };

  const usable = candidates && !blocker
    ? `Can learn from ${candidates.with_audio} meeting${candidates.with_audio === 1 ? '' : 's'} with call audio.`
    : null;

  return (
    <Panel title={own ? 'Your voice' : 'Voice'} action={<Badge variant="accent" size="xs">Labs</Badge>}>
      {voices === null ? (
        <Skeleton className="h-16" />
      ) : voice ? (
        <>
          <p className="flex items-center gap-2 text-[13px] font-medium text-af-text">
            <Fingerprint className="h-4 w-4 shrink-0 text-af-accent" />
            Voice saved
          </p>
          <p className="mt-1 text-xs leading-relaxed text-af-text-3">
            {labs.voiceProfiles
              ? own
                ? `Learned from ${describeVoiceSource(voice)}. In room-microphone meetings, a speaker with your voice is named You. Update relearns it from your recent meetings.`
                : `Learned from ${describeVoiceSource(voice)}. When speakers are identified in a new meeting, a matching voice is named ${first || voice.name}. Update relearns it from all their recent meetings.`
              : 'Voice profiles are off in Settings > Labs, so this voice is not used right now.'}
          </p>
          <div className="mt-3 flex flex-wrap gap-2">
            {!labs.voiceProfiles && (
              <Button size="sm" onClick={() => void enable()} loading={busy === 'enable'} disabled={busy !== null}>
                Turn on voice profiles
              </Button>
            )}
            <Button size="sm" variant="secondary" onClick={() => void learn()} loading={busy === 'learn'} disabled={busy !== null || !!blocker}>
              <Fingerprint />
              Update voice
            </Button>
            <Button size="sm" variant="danger-ghost" onClick={() => void forget()} loading={busy === 'forget'} disabled={busy !== null}>
              Forget voice
            </Button>
          </div>
        </>
      ) : (
        <>
          <p className="text-[13px] font-medium text-af-text-2">No voice saved</p>
          <p className="mt-1 text-xs leading-relaxed text-af-text-3">
            {blocker ??
              (own
                ? `Learn your voice from your microphone lines, so room-microphone meetings name you automatically. ${usable ?? ''}`
                : `Learn ${whose} voice from meetings where you named them, so later meetings name a matching speaker for you. ${usable ?? ''}`)}
          </p>
          {!labs.voiceProfiles && !blocker && (
            <p className="mt-1 text-[11px] text-af-text-4">This turns on Voice profiles in Settings &gt; Labs.</p>
          )}
          <Button className="mt-3" size="sm" onClick={() => void learn()} loading={busy === 'learn'} disabled={busy !== null || !candidates || !!blocker}>
            <Fingerprint />
            Learn voice
          </Button>
        </>
      )}
    </Panel>
  );
}
