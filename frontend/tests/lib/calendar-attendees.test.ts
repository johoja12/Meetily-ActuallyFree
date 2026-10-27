import { test } from 'node:test';
import assert from 'node:assert/strict';
import { inviteSuggestions, type CalendarAttendee } from '../../src/lib/calendar-attendees';

const attendee = (name: string, email: string | null = null): CalendarAttendee => ({ name, email, organizer: false });
const people = [
  { id: 'p-jane', displayName: 'Jane D.', email: 'JANE@a.com' },
  { id: 'p-ann', displayName: 'Ann Lee', email: null },
  { id: 'p-other', displayName: 'Other Person', email: 'other@a.com' },
];

test('attendees match contacts by email first, then by name, and keep invite order', () => {
  const result = inviteSuggestions([attendee('Jane Doe', 'jane@a.com'), attendee('ann  lee'), attendee('Bob Roe', 'bob@a.com')], people, []);
  assert.deepEqual(result.map(entry => [entry.name, entry.personId ?? null, entry.email]), [
    ['Jane D.', 'p-jane', 'jane@a.com'],
    ['Ann Lee', 'p-ann', null],
    ['Bob Roe', null, 'bob@a.com'],
  ]);
});

test('attendees already used for a speaker sink to the bottom', () => {
  const result = inviteSuggestions([attendee('Jane Doe', 'jane@a.com'), attendee('Bob Roe'), attendee('Cy Poe')], people, ['Jane D. + Speaker 3', 'bob roe']);
  assert.deepEqual(result.map(entry => [entry.name, entry.assigned]), [['Cy Poe', false], ['Jane D.', true], ['Bob Roe', true]]);
});

test('duplicate and nameless attendees are dropped', () => {
  const result = inviteSuggestions([attendee('Bob Roe', 'bob@a.com'), attendee('Robert', 'BOB@a.com'), attendee('  '), attendee('Bob Roe')], [], []);
  assert.deepEqual(result.map(entry => entry.name), ['Bob Roe']);
});

test('a same-name contact with a different email is not the attendee', () => {
  const result = inviteSuggestions([attendee('Ann Lee', 'ann@other.com'), attendee('Jane D.', 'jane@a.com')],
    [{ id: 'p-ann', displayName: 'Ann Lee', email: 'ann@a.com' }, { id: 'p-jane', displayName: 'Jane D.', email: 'jane@a.com' }, { id: 'p-noemail', displayName: 'Bob', email: null }], []);
  assert.deepEqual(result.map(entry => [entry.name, entry.personId ?? null]), [['Ann Lee', null], ['Jane D.', 'p-jane']]);
  // A contact with no email still matches by name.
  assert.equal(inviteSuggestions([attendee('Bob', 'bob@a.com')], [{ id: 'p-noemail', displayName: 'Bob', email: null }], [])[0].personId, 'p-noemail');
});
