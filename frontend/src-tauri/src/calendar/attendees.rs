//! Invite attendees as EventKit reports them, cleaned up for display and for
//! matching against contacts. The native bridge already leaves out the user,
//! rooms/resources and people who declined.
use serde::{Deserialize, Serialize};

pub const MAX_ATTENDEES: usize = 100;
const MAX_FIELD: usize = 200;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarAttendee {
    pub name: String,
    pub email: Option<String>,
    pub organizer: bool,
}

fn clean_email(raw: &str) -> Option<String> {
    let value = raw.trim();
    let value = value.strip_prefix("mailto:").or_else(|| value.strip_prefix("MAILTO:")).unwrap_or(value);
    let value = value.trim().to_lowercase();
    let (local, domain) = value.split_once('@')?;
    let plain = !local.is_empty() && !domain.is_empty() && !domain.contains('@') && !value.chars().any(char::is_whitespace);
    (plain && value.chars().count() <= MAX_FIELD).then_some(value)
}

fn collapse(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "jane.doe+work@a.com" → "Jane Doe". None when the local part has no letters.
pub fn name_from_email(email: &str) -> Option<String> {
    let (local, domain) = email.trim().split_once('@')?;
    if domain.is_empty() {
        return None;
    }
    let local = local.split('+').next().unwrap_or(local);
    let words: Vec<String> = local
        .split(['.', '_', '-'])
        .filter(|word| word.chars().any(char::is_alphabetic))
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect())
                .unwrap_or_default()
        })
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// Cleans names and emails, fills a missing or email-shaped name from the
/// email, merges duplicates (by email, else by name) and bounds the list.
pub fn normalize(raw: Vec<CalendarAttendee>) -> Vec<CalendarAttendee> {
    let mut cleaned: Vec<CalendarAttendee> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for attendee in raw {
        let email = attendee.email.as_deref().and_then(clean_email);
        let name = collapse(&attendee.name);
        let name = if name.is_empty() || name.contains('@') {
            email.as_deref().and_then(name_from_email).unwrap_or_default()
        } else {
            name
        };
        let name: String = name.chars().take(MAX_FIELD).collect();
        if name.is_empty() {
            continue;
        }
        let key = email.clone().unwrap_or_else(|| format!("name:{}", name.to_lowercase()));
        if let Some(index) = keys.iter().position(|existing| *existing == key) {
            cleaned[index].organizer |= attendee.organizer;
            continue;
        }
        if cleaned.len() == MAX_ATTENDEES {
            break;
        }
        keys.push(key);
        cleaned.push(CalendarAttendee { name, email, organizer: attendee.organizer });
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(name: &str, email: Option<&str>) -> CalendarAttendee {
        CalendarAttendee { name: name.into(), email: email.map(str::to_string), organizer: false }
    }

    #[test]
    fn names_come_from_email_local_parts() {
        assert_eq!(name_from_email("jane.doe+work@a.com").as_deref(), Some("Jane Doe"));
        assert_eq!(name_from_email("j_smith-2@a.com").as_deref(), Some("J Smith"));
        assert_eq!(name_from_email("1234@a.com"), None);
        assert_eq!(name_from_email("not-an-email"), None);
    }

    #[test]
    fn email_shaped_or_missing_names_are_replaced_and_empty_entries_dropped() {
        let cleaned = normalize(vec![
            person("  Jane.Doe@A.com ", Some("mailto:Jane.Doe@A.com")),
            person("", Some("bob@a.com")),
            person("   ", None),
            person(" Ann  Lee ", None),
        ]);
        assert_eq!(
            cleaned,
            vec![
                person("Jane Doe", Some("jane.doe@a.com")),
                person("Bob", Some("bob@a.com")),
                person("Ann Lee", None),
            ]
        );
    }

    #[test]
    fn duplicates_merge_by_email_then_name_keeping_the_organizer_flag() {
        let mut organizer = person("Jane Doe", Some("JANE@a.com"));
        organizer.organizer = true;
        let cleaned = normalize(vec![
            person("Jane", Some("jane@a.com")),
            organizer,
            person("Ann Lee", None),
            person("ann lee", None),
        ]);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(cleaned[0].name, "Jane");
        assert!(cleaned[0].organizer);
        assert_eq!(cleaned[1].name, "Ann Lee");
    }

    #[test]
    fn lists_and_fields_are_bounded() {
        let many: Vec<_> = (0..150).map(|i| person(&format!("P{i}"), None)).collect();
        assert_eq!(normalize(many).len(), MAX_ATTENDEES);
        let long = normalize(vec![person(&"x".repeat(500), Some(&format!("{}@a.com", "y".repeat(500))))]);
        assert!(long[0].name.chars().count() <= MAX_FIELD);
        assert_eq!(long[0].email, None);
    }
}
