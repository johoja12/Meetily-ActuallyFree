import React from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, expect, mock, test } from 'bun:test';
const calls: Array<[string, any]> = [];
mock.module('@tauri-apps/api/core', () => ({invoke: async (command: string, args: any) => {
  calls.push([command, args]); return {speaker: args.to, count: 1, removedName: false};
}}));
mock.module('@/contexts/WorkspaceContext', () => ({useWorkspace: () => ({people: [
  {id:'p-jane', displayName:'Jane D.', email:'jane@a.com', meetingCount:3},
  {id:'p-zed', displayName:'Zed', meetingCount:1},
]})}));
mock.module('@/hooks/useUserName', () => ({useUserName: () => 'Andrew'}));
mock.module('@/hooks/useInviteAttendees', () => ({useInviteAttendees: () => [
  {name:'Jane Doe', email:'jane@a.com', organizer:true},
  {name:'Bob Roe', email:'bob@a.com', organizer:false},
]}));
mock.module('@/lib/workspace-api', () => ({announceChange: () => {}}));
mock.module('sonner', () => ({toast: {success: () => {}, error: () => {}}}));
const wrap = ({children}: any) => <div>{children}</div>;
mock.module('@/components/ui/dialog', () => ({Dialog:wrap, DialogContent:wrap, DialogDescription:wrap, DialogTitle:wrap}));
mock.module('@/components/ui/command', () => ({Command:wrap, CommandEmpty:wrap, CommandGroup: ({heading, children}: any) => <section data-heading={heading}>{children}</section>, CommandInput:wrap, CommandList:wrap, CommandItem: (props:any) => <button data-choice={props.value} onClick={props.onSelect}>{props.children}</button>}));
mock.module('@/components/ui/avatar', () => ({Avatar:wrap}));
mock.module('@/components/ui/button', () => ({Button: (props:any) => <button {...props}/> }));
const { SpeakerIdentityDialog } = await import('../../src/components/people/SpeakerIdentityDialog');
let root: ReactTestRenderer;
afterEach(async () => {await act(async () => root?.unmount()); calls.length=0;});

const section = (heading: string) => root.root.findAll(node => node.props['data-heading'] === heading)[0];
const choices = (heading: string) => section(heading)?.findAllByType('button').map(button => button.props['data-choice'] as string) ?? [];

test('invitees come first, matched contacts leave the contact list, and picking one renames to that name', async () => {
  await act(async () => {root=create(<SpeakerIdentityDialog open onOpenChange={() => {}} speaker="Speaker 2" meetingId="m1" speakers={['Speaker 1', 'Speaker 2']}/>);});
  expect(choices("In this meeting's invite")).toEqual(['invite Jane D. person:p-jane', 'invite Bob Roe email:bob@a.com', 'invite __not-this-invite__']);
  expect(choices('Contacts')).toEqual(['Zed p-zed']);
  await act(async () => section("In this meeting's invite").findAllByType('button')[1].props.onClick());
  expect(calls).toEqual([['rename_meeting_speaker', {meetingId:'m1', from:'Speaker 2', to:'Bob Roe'}]]);
});

test('an invitee already used for another speaker sinks to the bottom', async () => {
  await act(async () => {root=create(<SpeakerIdentityDialog open onOpenChange={() => {}} speaker="Speaker 2" meetingId="m1" speakers={['Jane D.', 'Speaker 2']}/>);});
  expect(choices("In this meeting's invite")).toEqual(['invite Bob Roe email:bob@a.com', 'invite Jane D. person:p-jane', 'invite __not-this-invite__']);
});

test('the wrong invite can be removed from a saved meeting', async () => {
  await act(async () => {root=create(<SpeakerIdentityDialog open onOpenChange={() => {}} speaker="Speaker 2" meetingId="m1" speakers={['Speaker 2']}/>);});
  const remove = root.root.findAllByType('button').find(button => button.props['data-choice'] === 'invite __not-this-invite__')!;
  await act(async () => remove.props.onClick());
  expect(calls).toEqual([['unlink_meeting_calendar_event', {meetingId:'m1'}]]);
});
