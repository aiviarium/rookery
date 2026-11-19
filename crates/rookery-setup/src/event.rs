//! The keys a prompt takes, and what each does to a line being typed. The
//! command line turns the terminal's events into these.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub code: Code,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// What a key does to a line being typed, if anything.
pub fn edit(input: &mut tui_input::Input, key: &Key) {
    use tui_input::InputRequest::*;
    let request = match (key.code, key.ctrl, key.alt) {
        (Code::Backspace, false, false) | (Code::Char('h'), true, false) => DeletePrevChar,
        (Code::Delete, false, false) => DeleteNextChar,
        (Code::Left, false, false) | (Code::Char('b'), true, false) => GoToPrevChar,
        (Code::Left, true, false) | (Code::Char('b'), false, true) => GoToPrevWord,
        (Code::Right, false, false) | (Code::Char('f'), true, false) => GoToNextChar,
        (Code::Right, true, false) | (Code::Char('f'), false, true) => GoToNextWord,
        (Code::Char('u'), true, false) => DeleteLine,
        (Code::Char('w'), true, false)
        | (Code::Char('d'), false, true)
        | (Code::Backspace, false, true) => DeletePrevWord,
        (Code::Delete, true, false) => DeleteNextWord,
        (Code::Char('k'), true, false) => DeleteTillEnd,
        (Code::Char('y'), true, false) => Yank,
        (Code::Char('a'), true, false) | (Code::Home, false, false) => GoToStart,
        (Code::Char('e'), true, false) | (Code::End, false, false) => GoToEnd,
        (Code::Char(ch), false, false) | (Code::Char(ch), true, true) => InsertChar(ch),
        _ => return,
    };
    input.handle(request);
}
