//! Windows: calendars from classic Outlook through its COM object model (issue
//! #13). Runs on the calendar worker like EventKit and returns the same JSON
//! shape as `macos.m`; Rust cleans attendees afterwards. Nothing is written.
//!
//! - Outlook is only attached to while it is running. A refresh runs every
//!   minute, and launching Outlook that often would start and stop it in the
//!   background; Connect (`request_access`) may launch it once.
//! - Only the default calendar of each store (mailbox, PST) and its direct
//!   subfolders are listed; shared calendars opened from someone else's
//!   mailbox are not.
//! - "New Outlook" has no COM object model and is not read.
//! - Recipient addresses are guarded properties. With antivirus inactive,
//!   Outlook may ask the user before allowing them; a denied read only drops
//!   attendees. Notes (Body) are not read, so links are matched in Location.
//!
//! The pure helpers below are platform-neutral and unit tested everywhere;
//! the COM calls compile only on Windows.
#![cfg_attr(not(windows), allow(dead_code))]

/// Outlook stores times as OLE Automation dates in local time: days since
/// 1899-12-30 with the time of day as the fraction.
pub(crate) fn ole_date_to_ms(days: f64) -> Option<i64> {
    use chrono::TimeZone;
    if !days.is_finite() || !(0.0..=2_958_465.0).contains(&days) {
        return None;
    }
    let epoch = chrono::NaiveDate::from_ymd_opt(1899, 12, 30)?.and_hms_opt(0, 0, 0)?;
    let local = epoch + chrono::Duration::milliseconds((days * 86_400_000.0).round() as i64);
    // A wall time skipped by a DST change has no instant; one repeated by it
    // takes the earlier.
    Some(chrono::Local.from_local_datetime(&local).earliest()?.timestamp_millis())
}

const MEETING_HOSTS: [&str; 5] = ["webex.com", "zoom.us", "teams.microsoft.com", "teams.live.com", "meet.google.com"];
/// OlMeetingStatus: 0 is a plain appointment, 5 and 7 are cancelled meetings.
const OL_NON_MEETING: i32 = 0;
const OL_MEETING_CANCELED: [i32; 2] = [5, 7];
/// OlResponseStatus / OlBusyStatus values used below.
const OL_RESPONSE_DECLINED: i32 = 4;
const OL_BUSY_FREE: i32 = 0;
/// OlMeetingRecipientType for appointment recipients.
const OL_ORGANIZER: i32 = 0;
const OL_RESOURCE: i32 = 3;

/// The appointment fields that decide whether it is offered at all.
#[derive(Default)]
pub(crate) struct Appointment {
    pub all_day: bool,
    pub busy_status: i32,
    pub meeting_status: i32,
    pub response_status: i32,
    pub location: String,
}
impl Appointment {
    /// Mirrors the EventKit filter: no all-day, free, cancelled or declined items.
    pub(crate) fn offered(&self) -> bool {
        !self.all_day
            && self.busy_status != OL_BUSY_FREE
            && !OL_MEETING_CANCELED.contains(&self.meeting_status)
            && self.response_status != OL_RESPONSE_DECLINED
    }
    /// An invitation, or an appointment whose location is a meeting link.
    pub(crate) fn is_meeting(&self) -> bool {
        let location = self.location.to_lowercase();
        self.meeting_status != OL_NON_MEETING || MEETING_HOSTS.iter().any(|host| location.contains(host))
    }
}

/// Whether a recipient belongs in the attendee list: people only, not rooms,
/// resources, decliners or the user.
pub(crate) fn attendee_listed(kind: i32, response: i32, name: &str, email: Option<&str>, me: &[String]) -> bool {
    let is_me = |value: &str| me.iter().any(|mine| !mine.is_empty() && mine.eq_ignore_ascii_case(value.trim()));
    kind != OL_RESOURCE && response != OL_RESPONSE_DECLINED && !is_me(name) && !email.is_some_and(is_me)
}

pub(crate) fn is_organizer(kind: i32, name: &str, organizer: &str) -> bool {
    kind == OL_ORGANIZER || (!organizer.trim().is_empty() && name.trim().eq_ignore_ascii_case(organizer.trim()))
}

/// Outlook's Restrict parses dates in the user's locale, so the bounds are
/// formatted with that locale. Overlap: starts before the window ends and
/// ends after it starts.
pub(crate) fn overlap_filter(window_start: &str, window_end: &str) -> String {
    format!("[Start] <= '{window_end}' AND [End] >= '{window_start}'")
}

#[cfg(windows)]
pub(crate) fn read(request_access: bool, selected: &[String], start_ms: i64, end_ms: i64) -> serde_json::Value {
    let selected = selected.to_vec();
    // A thread of our own owns its COM apartment; pool threads may already be
    // initialized differently by the audio stack.
    match std::thread::Builder::new()
        .name("outlook-calendar".into())
        .spawn(move || com::read(request_access, &selected, start_ms, end_ms))
        .map(|thread| thread.join())
    {
        Ok(Ok(value)) => value,
        _ => serde_json::json!({ "error": "Outlook could not be read. Try again." }),
    }
}

#[cfg(windows)]
mod com {
    use super::*;
    use serde_json::{json, Value};
    use windows::core::{Interface, BSTR, GUID, HSTRING, PCWSTR, VARIANT};
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, DATE_SHORTDATE, TIME_NOSECONDS};
    use windows::Win32::System::Com::{
        CLSIDFromProgID, CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch, CLSCTX_LOCAL_SERVER,
        COINIT_APARTMENTTHREADED, DISPATCH_FLAGS, DISPATCH_METHOD, DISPATCH_PROPERTYGET, DISPATCH_PROPERTYPUT, DISPPARAMS,
    };
    use windows::Win32::System::Ole::GetActiveObject;

    const LOCALE_USER_DEFAULT: u32 = 0x0400;
    const DISPID_PROPERTYPUT: i32 = -3;
    const VT_DATE: u16 = 7;
    const VT_DISPATCH: u16 = 9;
    const VT_UNKNOWN: u16 = 13;
    const OL_FOLDER_CALENDAR: i32 = 9;
    const OL_APPOINTMENT_ITEM: i32 = 1;
    const MAX_EVENTS: usize = 500;
    const MAX_CALENDARS: usize = 50;

    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    /// Late-bound calls on an Outlook object.
    struct Dispatch(IDispatch);
    impl Dispatch {
        fn invoke(&self, name: &str, flags: DISPATCH_FLAGS, args: &[VARIANT]) -> windows::core::Result<VARIANT> {
            let wide = HSTRING::from(name);
            let names = [PCWSTR(wide.as_ptr())];
            let mut id = 0;
            unsafe { self.0.GetIDsOfNames(&GUID::zeroed(), names.as_ptr(), 1, LOCALE_USER_DEFAULT, &mut id)? };
            // DISPPARAMS lists arguments last to first.
            let mut args: Vec<VARIANT> = args.iter().rev().cloned().collect();
            let mut named = DISPID_PROPERTYPUT;
            let put = flags == DISPATCH_PROPERTYPUT;
            let params = DISPPARAMS {
                rgvarg: args.as_mut_ptr(),
                rgdispidNamedArgs: if put { &mut named } else { std::ptr::null_mut() },
                cArgs: args.len() as u32,
                cNamedArgs: u32::from(put),
            };
            let mut result = VARIANT::default();
            unsafe {
                self.0.Invoke(id, &GUID::zeroed(), LOCALE_USER_DEFAULT, flags, &params, Some(&mut result), None, None)?
            };
            Ok(result)
        }
        fn get(&self, name: &str) -> windows::core::Result<VARIANT> {
            self.invoke(name, DISPATCH_PROPERTYGET, &[])
        }
        /// Methods such as Item(i) and GetDefaultFolder(n) are also property gets.
        fn call(&self, name: &str, args: &[VARIANT]) -> windows::core::Result<VARIANT> {
            self.invoke(name, DISPATCH_METHOD | DISPATCH_PROPERTYGET, args)
        }
        fn put(&self, name: &str, value: VARIANT) -> windows::core::Result<()> {
            self.invoke(name, DISPATCH_PROPERTYPUT, &[value]).map(drop)
        }
        fn object(&self, name: &str) -> Option<Dispatch> {
            object(&self.get(name).ok()?)
        }
        fn call_object(&self, name: &str, args: &[VARIANT]) -> Option<Dispatch> {
            object(&self.call(name, args).ok()?)
        }
        fn text(&self, name: &str) -> String {
            self.get(name).ok().and_then(|value| BSTR::try_from(&value).ok()).map(|text| text.to_string()).unwrap_or_default()
        }
        fn int(&self, name: &str) -> i32 {
            self.get(name).ok().and_then(|value| i32::try_from(&value).ok()).unwrap_or_default()
        }
        fn flag(&self, name: &str) -> bool {
            self.get(name).ok().and_then(|value| bool::try_from(&value).ok()).unwrap_or_default()
        }
        fn date_ms(&self, name: &str) -> Option<i64> {
            let value = self.get(name).ok()?;
            let raw = value.as_raw();
            unsafe {
                if raw.Anonymous.Anonymous.vt != VT_DATE {
                    return None;
                }
                ole_date_to_ms(raw.Anonymous.Anonymous.Anonymous.date)
            }
        }
        fn count(&self) -> i32 {
            self.int("Count").max(0)
        }
        fn item(&self, index: i32) -> Option<Dispatch> {
            self.call_object("Item", &[VARIANT::from(index)])
        }
    }

    /// An object result. Outlook returns VT_DISPATCH, which windows-core's
    /// IUnknown conversion does not accept; an empty result is None.
    fn object(value: &VARIANT) -> Option<Dispatch> {
        let raw = value.as_raw();
        unsafe {
            let vt = raw.Anonymous.Anonymous.vt;
            if vt != VT_DISPATCH && vt != VT_UNKNOWN {
                return None;
            }
            let pointer = raw.Anonymous.Anonymous.Anonymous.pdispVal;
            let unknown = windows::core::IUnknown::from_raw_borrowed(&pointer)?;
            unknown.cast::<IDispatch>().ok().map(Dispatch)
        }
    }

    fn outlook(launch: bool) -> Result<Dispatch, &'static str> {
        let clsid = unsafe { CLSIDFromProgID(windows::core::w!("Outlook.Application")) }
            .map_err(|_| "Classic Outlook is not installed. On Windows, Meetily reads calendars from classic Outlook; the new Outlook is not supported yet.")?;
        let mut running = None;
        if unsafe { GetActiveObject(&clsid, None, &mut running) }.is_ok() {
            if let Some(dispatch) = running.and_then(|unknown| unknown.cast::<IDispatch>().ok()) {
                return Ok(Dispatch(dispatch));
            }
        }
        if !launch {
            return Err("Outlook is not running. Open classic Outlook so Meetily can read your calendar.");
        }
        unsafe { CoCreateInstance::<_, IDispatch>(&clsid, None, CLSCTX_LOCAL_SERVER) }
            .map(Dispatch)
            .map_err(|_| "Classic Outlook could not be started. Open it and try Connect again.")
    }

    /// The user's locale short date and time, which Outlook's Restrict parses.
    fn outlook_time(ms: i64) -> Option<String> {
        use chrono::{Datelike, TimeZone, Timelike};
        let local = chrono::Local.timestamp_millis_opt(ms).single()?;
        let time = SYSTEMTIME {
            wYear: local.year() as u16,
            wMonth: local.month() as u16,
            wDayOfWeek: local.weekday().num_days_from_sunday() as u16,
            wDay: local.day() as u16,
            wHour: local.hour() as u16,
            wMinute: local.minute() as u16,
            wSecond: 0,
            wMilliseconds: 0,
        };
        let mut date = [0u16; 128];
        let mut clock = [0u16; 64];
        unsafe {
            let date_len = GetDateFormatEx(PCWSTR::null(), DATE_SHORTDATE, Some(&time), PCWSTR::null(), Some(&mut date), PCWSTR::null());
            let clock_len = GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, Some(&time), PCWSTR::null(), Some(&mut clock));
            if date_len <= 1 || clock_len <= 1 {
                return None;
            }
            Some(format!(
                "{} {}",
                String::from_utf16_lossy(&date[..date_len as usize - 1]),
                String::from_utf16_lossy(&clock[..clock_len as usize - 1])
            ))
        }
    }

    struct Calendar {
        id: String,
        name: String,
        folder: Dispatch,
    }

    fn calendars(session: &Dispatch) -> Vec<Calendar> {
        let mut found = Vec::new();
        let Some(stores) = session.object("Stores") else { return found };
        for index in 1..=stores.count() {
            let Some(store) = stores.item(index) else { continue };
            // Public folders and some archives have no calendar.
            let Some(default) = store.call_object("GetDefaultFolder", &[VARIANT::from(OL_FOLDER_CALENDAR)]) else {
                continue;
            };
            let store_name = store.text("DisplayName");
            let mut folders = vec![default];
            if let Some(children) = folders[0].object("Folders") {
                for child in 1..=children.count() {
                    if let Some(folder) = children.item(child).filter(|folder| folder.int("DefaultItemType") == OL_APPOINTMENT_ITEM) {
                        folders.push(folder);
                    }
                }
            }
            for folder in folders {
                let id = folder.text("EntryID");
                if id.is_empty() || id.len() > 512 || found.len() >= MAX_CALENDARS {
                    continue;
                }
                let title = folder.text("Name");
                let name = format!("{} · {}", if title.is_empty() { "Calendar" } else { &title }, if store_name.is_empty() { "Outlook" } else { &store_name });
                found.push(Calendar { id, name, folder });
            }
        }
        found
    }

    fn smtp_address(recipient: &Dispatch) -> Option<String> {
        let address = recipient.text("Address");
        if address.contains('@') {
            return Some(address);
        }
        // Exchange recipients carry an X.500 address; the SMTP one is on the user.
        let user = recipient.object("AddressEntry")?.call_object("GetExchangeUser", &[])?;
        Some(user.text("PrimarySmtpAddress")).filter(|address| address.contains('@'))
    }

    fn attendees(appointment: &Dispatch, me: &[String]) -> Vec<Value> {
        let mut people = Vec::new();
        let Some(recipients) = appointment.object("Recipients") else { return people };
        let organizer = appointment.text("Organizer");
        for index in 1..=recipients.count() {
            if people.len() >= super::super::attendees::MAX_ATTENDEES {
                break;
            }
            let Some(recipient) = recipients.item(index) else { continue };
            let name = recipient.text("Name");
            let kind = recipient.int("Type");
            let response = recipient.int("MeetingResponseStatus");
            if kind == OL_RESOURCE || response == OL_RESPONSE_DECLINED {
                continue;
            }
            let email = smtp_address(&recipient);
            if !attendee_listed(kind, response, &name, email.as_deref(), me) {
                continue;
            }
            people.push(json!({ "name": name, "email": email, "organizer": is_organizer(kind, &name, &organizer) }));
        }
        people
    }

    fn events(calendar: &Calendar, filter: &str, me: &[String], out: &mut Vec<Value>) {
        let Some(items) = calendar.folder.object("Items") else { return };
        // Recurrences are expanded only on a collection sorted by Start; its
        // Count is then meaningless, so it is walked with GetFirst/GetNext.
        if items.call("Sort", &[VARIANT::from("[Start]")]).is_err() || items.put("IncludeRecurrences", VARIANT::from(true)).is_err() {
            return;
        }
        let Some(range) = items.call_object("Restrict", &[VARIANT::from(filter)]) else { return };
        let mut next = range.call_object("GetFirst", &[]);
        while let Some(item) = next {
            if out.len() >= MAX_EVENTS {
                break;
            }
            let appointment = Appointment {
                all_day: item.flag("AllDayEvent"),
                busy_status: item.int("BusyStatus"),
                meeting_status: item.int("MeetingStatus"),
                response_status: item.int("ResponseStatus"),
                location: item.text("Location"),
            };
            let id = item.text("EntryID");
            let title = item.text("Subject").trim().to_string();
            if let (true, Some(start_ms), Some(end_ms)) = (appointment.offered(), item.date_ms("Start"), item.date_ms("End")) {
                if !id.is_empty() && !title.is_empty() && title.chars().count() <= 200 {
                    let people = if appointment.meeting_status != OL_NON_MEETING { attendees(&item, me) } else { Vec::new() };
                    out.push(json!({
                        "id": id, "title": title,
                        "calendar_id": calendar.id, "calendar_name": calendar.name,
                        "start_ms": start_ms, "end_ms": end_ms,
                        "is_meeting": appointment.is_meeting(),
                        "attendees": people,
                    }));
                }
            }
            next = range.call_object("GetNext", &[]);
        }
    }

    pub(super) fn read(request_access: bool, selected: &[String], start_ms: i64, end_ms: i64) -> Value {
        if unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_err() {
            return json!({ "error": "Outlook could not be read. Try again." });
        }
        let _apartment = Apartment;
        let app = match outlook(request_access) {
            Ok(app) => app,
            Err(error) => return json!({ "error": error }),
        };
        let Some(session) = app.call_object("GetNamespace", &[VARIANT::from("MAPI")]) else {
            return json!({ "error": "Outlook did not open its mailbox. Check that Outlook has finished starting." });
        };
        let calendars = calendars(&session);
        let sources: Vec<Value> = calendars.iter().map(|calendar| json!({ "id": calendar.id, "name": calendar.name })).collect();
        let me: Vec<String> = session
            .object("CurrentUser")
            .map(|user| vec![user.text("Name"), smtp_address(&user).unwrap_or_default()])
            .unwrap_or_default();
        let mut meetings = Vec::new();
        if let (Some(from), Some(to)) = (outlook_time(start_ms), outlook_time(end_ms)) {
            let filter = overlap_filter(&from, &to);
            for calendar in calendars.iter().filter(|calendar| selected.contains(&calendar.id)) {
                events(calendar, &filter, &me, &mut meetings);
            }
        }
        meetings.sort_by_key(|meeting| meeting["start_ms"].as_i64().unwrap_or_default());
        // Event titles and attendee names never go into error text or logs.
        json!({ "authorization": "authorized", "calendars": sources, "events": meetings })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn ole_dates_are_local_wall_times() {
        // 45658.5 is 2025-01-01 12:00 local.
        let noon = chrono::Local.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).earliest().unwrap();
        assert_eq!(ole_date_to_ms(45658.5), Some(noon.timestamp_millis()));
        let quarter_past = chrono::Local.with_ymd_and_hms(2025, 1, 1, 0, 15, 0).earliest().unwrap();
        assert_eq!(ole_date_to_ms(45658.0 + 15.0 / 1440.0), Some(quarter_past.timestamp_millis()));
        assert_eq!(ole_date_to_ms(f64::NAN), None);
        assert_eq!(ole_date_to_ms(-1.0), None);
    }

    #[test]
    fn appointments_are_offered_like_eventkit_events() {
        let busy = Appointment { busy_status: 2, meeting_status: 3, ..Appointment::default() };
        assert!(busy.offered());
        assert!(!Appointment { all_day: true, ..busy_like() }.offered());
        assert!(!Appointment { busy_status: OL_BUSY_FREE, ..busy_like() }.offered());
        assert!(!Appointment { meeting_status: 5, ..busy_like() }.offered());
        assert!(!Appointment { response_status: OL_RESPONSE_DECLINED, ..busy_like() }.offered());
    }

    fn busy_like() -> Appointment {
        Appointment { busy_status: 2, meeting_status: 1, ..Appointment::default() }
    }

    #[test]
    fn invitations_and_meeting_links_are_meetings() {
        assert!(busy_like().is_meeting());
        let solo = Appointment { busy_status: 2, ..Appointment::default() };
        assert!(!solo.is_meeting());
        assert!(Appointment { location: "https://cisco.webex.com/meet/x".into(), ..solo }.is_meeting());
    }

    #[test]
    fn attendees_leave_out_rooms_decliners_and_the_user() {
        let me = vec!["Pat Lee".to_string(), "me@example.com".to_string()];
        assert!(attendee_listed(1, 3, "Jane Doe", Some("jane@example.com"), &me));
        assert!(!attendee_listed(OL_RESOURCE, 0, "Room 4", None, &me));
        assert!(!attendee_listed(2, OL_RESPONSE_DECLINED, "Jane Doe", None, &me));
        assert!(!attendee_listed(1, 0, "pat lee", None, &me));
        assert!(!attendee_listed(1, 0, "Someone", Some("ME@example.com"), &me));
        assert!(attendee_listed(1, 0, "Jane", None, &[String::new()]));
    }

    #[test]
    fn organizer_is_by_type_or_name() {
        assert!(is_organizer(OL_ORGANIZER, "Jane", ""));
        assert!(is_organizer(1, "Jane Doe ", "jane doe"));
        assert!(!is_organizer(1, "Jane", ""));
    }

    #[test]
    fn restrict_filter_selects_overlapping_items() {
        assert_eq!(
            overlap_filter("11/2/2026 9:00 AM", "11/3/2026 9:00 AM"),
            "[Start] <= '11/3/2026 9:00 AM' AND [End] >= '11/2/2026 9:00 AM'"
        );
    }
}
