/// Compare the requested route with the resolved input, not the OS default:
/// reconnecting a preferred mic does not switch an already-open capture stream.
pub fn mismatch_message(requested: Option<&str>, actual: &str) -> Option<String> {
    let requested = requested?.trim();
    let name = requested.strip_suffix("(input)").unwrap_or(requested).trim();
    if name.is_empty() || name == actual {
        return None;
    }
    Some(format!(
        "Requested microphone \"{}\" is unavailable. Meetily is recording with \"{}\" instead. Select the intended microphone in the recording controls, or reconnect it and start a new recording. Reconnecting alone does not switch this recording.",
        name, actual
    ))
}

#[cfg(test)]
mod tests {
    use super::mismatch_message;

    #[test]
    fn warns_with_both_routes_for_missing_preferred_mic() {
        let warning = mismatch_message(Some("Jabra Speak 710 (input)"), "Desk Pro Web Camera").unwrap();
        assert!(warning.contains("Jabra Speak 710"));
        assert!(warning.contains("Desk Pro Web Camera"));
        assert!(warning.contains("Reconnecting alone does not switch"));
    }

    #[test]
    fn selected_mic_and_default_selection_do_not_warn() {
        assert!(mismatch_message(Some("Jabra Speak 710 (input)"), "Jabra Speak 710").is_none());
        assert!(mismatch_message(Some("Jabra Speak 710"), "Jabra Speak 710").is_none());
        assert!(mismatch_message(None, "Desk Pro Web Camera").is_none());
        assert!(mismatch_message(Some("  "), "Desk Pro Web Camera").is_none());
    }
}
