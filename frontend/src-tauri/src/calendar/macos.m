#import <Foundation/Foundation.h>
#import <EventKit/EventKit.h>
#import <dispatch/dispatch.h>
#include <stdlib.h>

// Called only on a blocking worker. All EventKit objects stay within this
// invocation; Rust owns the returned UTF-8 allocation until calendar_free.
static char *result_json(NSDictionary *result) {
    NSData *data = [NSJSONSerialization dataWithJSONObject:result options:0 error:nil];
    if (!data) return strdup("{\"error\":\"Calendar response could not be encoded\"}");
    NSString *text = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
    return strdup(text.UTF8String);
}

char *meetily_calendar_read(bool requestAccess, const char *selectedJSON, double start, double end) {
    @autoreleasepool {
        @try {
            EKEventStore *store = [[EKEventStore alloc] init];
            EKAuthorizationStatus status = [EKEventStore authorizationStatusForEntityType:EKEntityTypeEvent];
            if (requestAccess && status == EKAuthorizationStatusNotDetermined) {
                dispatch_semaphore_t done = dispatch_semaphore_create(0);
                [store requestFullAccessToEventsWithCompletion:^(BOOL granted, NSError *error) {
                    // Retain the store until permission completes, including if
                    // the caller's bounded wait expires before the user replies.
                    (void)[store class];
                    dispatch_semaphore_signal(done);
                }];
                if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 120 * NSEC_PER_SEC))) {
                    return result_json(@{@"error": @"Calendar permission timed out. Try Connect again after answering the macOS prompt."});
                }
                status = [EKEventStore authorizationStatusForEntityType:EKEntityTypeEvent];
            }
            NSString *authorization = @"not_determined";
            if (status == EKAuthorizationStatusFullAccess) authorization = @"authorized";
            else if (status == EKAuthorizationStatusDenied || status == EKAuthorizationStatusWriteOnly) authorization = @"denied";
            else if (status == EKAuthorizationStatusRestricted) authorization = @"restricted";
            if (![authorization isEqualToString:@"authorized"]) {
                return result_json(@{@"authorization": authorization, @"calendars": @[], @"events": @[]});
            }

            NSData *idsData = [[NSString stringWithUTF8String:selectedJSON] dataUsingEncoding:NSUTF8StringEncoding];
            NSArray *ids = [NSJSONSerialization JSONObjectWithData:idsData options:0 error:nil];
            if (![ids isKindOfClass:[NSArray class]]) return result_json(@{@"error": @"Invalid calendar selection"});
            NSMutableArray *sources = [NSMutableArray array];
            NSMutableArray *selected = [NSMutableArray array];
            for (EKCalendar *calendar in [store calendarsForEntityType:EKEntityTypeEvent]) {
                NSString *identifier = calendar.calendarIdentifier;
                if (!identifier) continue;
                NSString *name = [NSString stringWithFormat:@"%@ · %@", calendar.title ?: @"Calendar", calendar.source.title ?: @"On this Mac"];
                [sources addObject:@{@"id": identifier, @"name": name}];
                if ([ids containsObject:identifier]) [selected addObject:calendar];
            }
            NSMutableArray *meetings = [NSMutableArray array];
            if (selected.count) {
                NSPredicate *predicate = [store predicateForEventsWithStartDate:[NSDate dateWithTimeIntervalSince1970:start]
                    endDate:[NSDate dateWithTimeIntervalSince1970:end] calendars:selected];
                NSArray *events = [[store eventsMatchingPredicate:predicate] sortedArrayUsingSelector:@selector(compareStartDateWithEvent:)];
                for (EKEvent *event in events) {
                    if (meetings.count >= 500) break;
                    if (event.allDay || event.status == EKEventStatusCanceled || event.availability == EKEventAvailabilityFree) continue;
                    BOOL declined = NO;
                    for (EKParticipant *person in event.attendees) {
                        if (person.isCurrentUser && person.participantStatus == EKParticipantStatusDeclined) declined = YES;
                    }
                    if (declined || !event.eventIdentifier || !event.startDate || !event.endDate) continue;
                    NSString *title = [event.title stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceAndNewlineCharacterSet]];
                    if (!title.length || title.length > 200) continue;
                    BOOL meeting = event.attendees.count > 0;
                    NSString *links = [[NSString stringWithFormat:@"%@ %@ %@", event.URL.absoluteString ?: @"", event.location ?: @"", [(event.notes ?: @"") substringToIndex:MIN(event.notes.length, 4096)]] lowercaseString];
                    for (NSString *host in @[@"webex.com", @"zoom.us", @"teams.microsoft.com", @"teams.live.com", @"meet.google.com"]) {
                        if ([links containsString:host]) meeting = YES;
                    }
                    // People only: not the user, rooms, resources, groups, or
                    // anyone who declined. Rust cleans names and emails.
                    NSMutableArray *people = [NSMutableArray array];
                    // Read once and compare by value: EventKit may hand out a new
                    // wrapper per access, and the organizer may also be listed.
                    EKParticipant *organizer = event.organizer;
                    NSMutableArray *participants = [NSMutableArray arrayWithArray:event.attendees ?: @[]];
                    if (organizer && ![participants containsObject:organizer]) [participants addObject:organizer];
                    for (EKParticipant *person in participants) {
                        if (people.count >= 100) break;
                        if (person.isCurrentUser || person.participantStatus == EKParticipantStatusDeclined) continue;
                        if (person.participantType == EKParticipantTypeRoom || person.participantType == EKParticipantTypeResource ||
                            person.participantType == EKParticipantTypeGroup) continue;
                        NSString *address = [person.URL.scheme.lowercaseString isEqualToString:@"mailto"] ? person.URL.resourceSpecifier : nil;
                        [people addObject:@{
                            @"name": person.name ?: @"",
                            @"email": address ?: [NSNull null],
                            @"organizer": @([person isEqual:organizer] || person.participantRole == EKParticipantRoleChair)
                        }];
                    }
                    [meetings addObject:@{
                        @"id": event.eventIdentifier, @"title": title,
                        @"calendar_id": event.calendar.calendarIdentifier,
                        @"calendar_name": event.calendar.title ?: @"Calendar",
                        @"start_ms": @((long long)(event.startDate.timeIntervalSince1970 * 1000)),
                        @"end_ms": @((long long)(event.endDate.timeIntervalSince1970 * 1000)),
                        @"is_meeting": @(meeting),
                        @"attendees": people
                    }];
                }
            }
            return result_json(@{@"authorization": authorization, @"calendars": sources, @"events": meetings});
        } @catch (NSException *exception) {
            // Event titles, notes and provider errors can contain private data.
            return result_json(@{@"error": @"macOS could not read the selected calendars. Check Calendar access and try again."});
        }
    }
}

void meetily_calendar_free(char *value) { free(value); }
