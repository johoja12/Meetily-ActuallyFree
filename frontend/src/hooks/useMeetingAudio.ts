'use client';

/**
 * Plays a meeting's recording. Rust finds the file and allows it through the
 * asset protocol; the page gets a streaming URL plus transport controls. The
 * clock ticks on animation frames while playing so the transcript highlight
 * and scrubber move smoothly.
 *
 * Room-microphone recordings gain a dereverberated copy some minutes after the
 * call (`room-playback-cleaned`). It plays by default once it exists; the
 * choice of copy is remembered per computer, and switching keeps the position,
 * speed and play state. `path` stays the original recording (the waveform and
 * every analysis use it).
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getMeetingAudio } from '@/lib/workspace-api';
import { isSameFolder, playbackFile, readPlaybackSource, savePlaybackSource, type PlaybackSource } from '@/lib/playback-source';

export type AudioStatus = 'loading' | 'ready' | 'unavailable' | 'error';

export interface MeetingAudioControls {
  status: AudioStatus;
  /** The recording's file, once found. */
  path: string | null;
  /** A cleaned copy of this room recording exists. */
  cleanedAvailable: boolean;
  /** The copy being played ('cleaned' only when one exists). */
  source: PlaybackSource;
  setSource: (source: PlaybackSource) => void;
  playing: boolean;
  currentTime: number;
  duration: number;
  rate: number;
  play: () => void;
  pause: () => void;
  toggle: () => void;
  seek: (seconds: number, autoplay?: boolean) => void;
  skip: (delta: number) => void;
  setRate: (rate: number) => void;
}

export const PLAYBACK_RATES = [1, 1.25, 1.5, 1.75, 2];
/** Labs waveform scrubbing adds slower speeds for close listening. */
export const SLOW_PLAYBACK_RATES = [0.5, 0.75, ...PLAYBACK_RATES];

export function useMeetingAudio(meetingId: string | null | undefined): MeetingAudioControls {
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const frame = useRef<number | null>(null);
  const [status, setStatus] = useState<AudioStatus>('loading');
  const [path, setPath] = useState<string | null>(null);
  const [playing, setPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [rate, setRateState] = useState(1);
  const [cleanedPath, setCleanedPath] = useState<string | null>(null);
  const [preferred, setPreferred] = useState<PlaybackSource>(readPlaybackSource);
  const preferredRef = useRef(preferred);
  const files = useRef<{ path: string | null; cleanedPath: string | null }>({ path: null, cleanedPath: null });

  /** Point the element at another copy without losing the listener's place. */
  const swapTo = useCallback((file: string) => {
    const element = audioRef.current;
    if (!element) return;
    const time = element.currentTime;
    const resume = !element.paused;
    const speed = element.playbackRate;
    element.pause();
    element.src = convertFileSrc(file);
    element.addEventListener(
      'loadedmetadata',
      () => {
        element.currentTime = time;
        element.playbackRate = speed;
        if (resume) void element.play().catch(() => setStatus('error'));
      },
      { once: true },
    );
  }, []);

  useEffect(() => {
    let cancelled = false;
    setStatus('loading');
    setPath(null);
    setCleanedPath(null);
    files.current = { path: null, cleanedPath: null };
    setPlaying(false);
    setCurrentTime(0);
    setDuration(0);
    if (!meetingId) {
      setStatus('unavailable');
      return;
    }
    void getMeetingAudio(meetingId)
      .then((audio) => {
        if (cancelled) return;
        if (!audio.path) {
          setStatus('unavailable');
          return;
        }
        setPath(audio.path);
        setCleanedPath(audio.cleanedPath ?? null);
        files.current = { path: audio.path, cleanedPath: audio.cleanedPath ?? null };
        const element = new Audio();
        element.preload = 'metadata';
        element.src = convertFileSrc(playbackFile(audio, preferredRef.current) ?? audio.path);
        element.onloadedmetadata = () => {
          if (cancelled) return;
          setDuration(Number.isFinite(element.duration) ? element.duration : 0);
          setStatus('ready');
        };
        element.onerror = () => !cancelled && setStatus('error');
        element.onplay = () => setPlaying(true);
        element.onpause = () => setPlaying(false);
        element.onended = () => setPlaying(false);
        element.ontimeupdate = () => setCurrentTime(element.currentTime);
        audioRef.current = element;
      })
      .catch(() => !cancelled && setStatus('unavailable'));
    return () => {
      cancelled = true;
      const element = audioRef.current;
      if (element) {
        element.pause();
        element.removeAttribute('src');
        element.load();
      }
      audioRef.current = null;
    };
  }, [meetingId]);

  // A cleaned copy written while this meeting is open: offer it, and switch to
  // it right away unless the listener is mid-playback or chose the original.
  useEffect(() => {
    if (!meetingId) return;
    let cancelled = false;
    const unlisten = listen<{ folder: string }>('room-playback-cleaned', (event) => {
      if (files.current.cleanedPath || !isSameFolder(event.payload.folder, files.current.path)) return;
      void getMeetingAudio(meetingId).then((audio) => {
        if (cancelled || !audio.cleanedPath || audio.path !== files.current.path) return;
        files.current = { path: audio.path, cleanedPath: audio.cleanedPath };
        setCleanedPath(audio.cleanedPath);
        const element = audioRef.current;
        if (preferredRef.current === 'cleaned' && element?.paused) swapTo(audio.cleanedPath);
      });
    });
    return () => {
      cancelled = true;
      void unlisten.then((stop) => stop());
    };
  }, [meetingId, swapTo]);

  const setSource = useCallback(
    (next: PlaybackSource) => {
      preferredRef.current = next;
      setPreferred(next);
      savePlaybackSource(next);
      const { path: original, cleanedPath: cleaned } = files.current;
      const file = playbackFile({ path: original, cleanedPath: cleaned }, next);
      const element = audioRef.current;
      if (file && element && cleaned && element.src !== convertFileSrc(file)) swapTo(file);
    },
    [swapTo],
  );

  // Smooth clock while playing.
  useEffect(() => {
    if (!playing) return;
    const tick = () => {
      const element = audioRef.current;
      if (element) setCurrentTime(element.currentTime);
      frame.current = requestAnimationFrame(tick);
    };
    frame.current = requestAnimationFrame(tick);
    return () => {
      if (frame.current !== null) cancelAnimationFrame(frame.current);
    };
  }, [playing]);

  const play = useCallback(() => {
    void audioRef.current?.play().catch(() => setStatus('error'));
  }, []);
  const pause = useCallback(() => audioRef.current?.pause(), []);
  const toggle = useCallback(() => {
    const element = audioRef.current;
    if (!element) return;
    if (element.paused) void element.play().catch(() => setStatus('error'));
    else element.pause();
  }, []);
  const seek = useCallback((seconds: number, autoplay = false) => {
    const element = audioRef.current;
    if (!element) return;
    const limit = Number.isFinite(element.duration) ? element.duration : seconds;
    element.currentTime = Math.max(0, Math.min(limit, seconds));
    setCurrentTime(element.currentTime);
    if (autoplay && element.paused) void element.play().catch(() => setStatus('error'));
  }, []);
  const skip = useCallback((delta: number) => {
    const element = audioRef.current;
    if (!element) return;
    element.currentTime = Math.max(0, Math.min(element.duration || Infinity, element.currentTime + delta));
    setCurrentTime(element.currentTime);
  }, []);
  const setRate = useCallback((next: number) => {
    if (audioRef.current) audioRef.current.playbackRate = next;
    setRateState(next);
  }, []);

  const cleanedAvailable = cleanedPath !== null;
  const source: PlaybackSource = cleanedAvailable && preferred === 'cleaned' ? 'cleaned' : 'original';
  return {
    status,
    path,
    cleanedAvailable,
    source,
    setSource,
    playing,
    currentTime,
    duration,
    rate,
    play,
    pause,
    toggle,
    seek,
    skip,
    setRate,
  };
}
