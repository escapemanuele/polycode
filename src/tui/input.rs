use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    Quit,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Escape,
    Tab,
    BackTab,
    Backspace,
    Delete,
    NewRun,
    Runs,
    Resume,
    Retry,
    Stop,
    Attention,
    Artifact,
    Logs,
    Diff,
    Apply,
    Publish,
    Fix,
    Continue,
    FollowUps,
    Discard,
    Archive,
    ShowArchived,
    /// Delete the selected archived run for good.
    DeleteForever,
    DismissMessage,
    ToggleRaw,
    ExpandTask,
    TechnicalDetails,
    Help,
    Character(char),
    /// Decline the selected permission request and let the task continue.
    Skip,
    Ignore,
}

pub(crate) fn map_key(event: KeyEvent) -> Intent {
    if event.modifiers.contains(KeyModifiers::CONTROL) && event.code == KeyCode::Char('c') {
        return Intent::Quit;
    }
    match event.code {
        KeyCode::Up | KeyCode::Char('k') => Intent::Up,
        KeyCode::Down | KeyCode::Char('j') => Intent::Down,
        KeyCode::Left => Intent::Left,
        KeyCode::Right => Intent::Right,
        KeyCode::PageUp => Intent::PageUp,
        KeyCode::PageDown => Intent::PageDown,
        KeyCode::Home => Intent::Home,
        KeyCode::End => Intent::End,
        KeyCode::Enter => Intent::Enter,
        KeyCode::Esc => Intent::Escape,
        KeyCode::Tab => Intent::Tab,
        KeyCode::BackTab => Intent::BackTab,
        KeyCode::Backspace => Intent::Backspace,
        KeyCode::Delete => Intent::Delete,
        KeyCode::Char('n') => Intent::NewRun,
        KeyCode::Char('R') => Intent::Runs,
        KeyCode::Char('r') => Intent::Resume,
        KeyCode::Char('t') => Intent::Retry,
        KeyCode::Char('s') => Intent::Stop,
        KeyCode::Char('u') => Intent::Attention,
        KeyCode::Char('o') => Intent::Artifact,
        KeyCode::Char('l') => Intent::Logs,
        KeyCode::Char('d') => Intent::Diff,
        KeyCode::Char('a') => Intent::Apply,
        KeyCode::Char('P') => Intent::Publish,
        KeyCode::Char('f') => Intent::Fix,
        KeyCode::Char('c') => Intent::Continue,
        KeyCode::Char('w') => Intent::FollowUps,
        KeyCode::Char('X') => Intent::Discard,
        KeyCode::Char('h') => Intent::Archive,
        KeyCode::Char('H') => Intent::ShowArchived,
        KeyCode::Char('D') => Intent::DeleteForever,
        KeyCode::Char('x') => Intent::DismissMessage,
        KeyCode::Char('m') => Intent::ToggleRaw,
        KeyCode::Char('e') => Intent::ExpandTask,
        KeyCode::Char('i') => Intent::TechnicalDetails,
        KeyCode::Char('?') => Intent::Help,
        KeyCode::Char('q') => Intent::Quit,
        KeyCode::Char(character)
            if event.modifiers.is_empty() || event.modifiers == KeyModifiers::SHIFT =>
        {
            Intent::Character(character)
        }
        _ => Intent::Ignore,
    }
}

pub(crate) fn map_text_key(event: KeyEvent) -> Intent {
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        match event.code {
            KeyCode::Char('c') => return Intent::Quit,
            KeyCode::Char('s') => return Intent::Skip,
            _ => {}
        }
    }
    match event.code {
        KeyCode::Up => Intent::Up,
        KeyCode::Down => Intent::Down,
        KeyCode::Left => Intent::Left,
        KeyCode::Right => Intent::Right,
        KeyCode::Home => Intent::Home,
        KeyCode::End => Intent::End,
        KeyCode::Enter => Intent::Enter,
        KeyCode::Esc => Intent::Escape,
        KeyCode::Tab => Intent::Tab,
        KeyCode::BackTab => Intent::BackTab,
        KeyCode::Backspace => Intent::Backspace,
        KeyCode::Delete => Intent::Delete,
        KeyCode::Char(character)
            if event.modifiers.is_empty() || event.modifiers == KeyModifiers::SHIFT =>
        {
            Intent::Character(character)
        }
        _ => Intent::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_mode_ctrl_s_skips_and_plain_s_still_types() {
        assert_eq!(
            map_text_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Intent::Skip
        );
        assert_eq!(
            map_text_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)),
            Intent::Character('s')
        );
        assert_eq!(
            map_text_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Intent::Quit
        );
    }

    #[test]
    fn maps_ctrl_c_and_context_keys() {
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Intent::Quit
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT)),
            Intent::Discard
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('é'), KeyModifiers::NONE)),
            Intent::Character('é')
        );
    }

    #[test]
    fn lowercase_x_dismisses_and_stays_distinct_from_uppercase_discard() {
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Intent::DismissMessage
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::SHIFT)),
            Intent::Discard
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE)),
            Intent::ToggleRaw
        );
    }

    #[test]
    fn lowercase_h_archives_and_stays_distinct_from_uppercase_show_archived() {
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE)),
            Intent::Archive
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT)),
            Intent::ShowArchived
        );
    }
}
