//! Colours and the logo.
//!
//! The mark is a rook's head in profile, in the tones of Rookery's icon
//! (`docs/public/brand/rookery-icon.svg`): amber shade for the bird, the highlight for its bill.
//! The greys are Aiviarium's neutrals and the accent is Rookery's amber.
//! rookery paints its own background, so the colours below are chosen against
//! `BASE` and not against whatever the terminal happens to use.

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Padding},
};

/// What a colour is for. The screens, the command line's prompts, its
/// progress, its tables and its help all take their colours from here, so
/// that the two look like one program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Body text.
    Text,
    /// Secondary text: hints, help, what was already said.
    Muted,
    /// Values, keys, commands and what is selected.
    Accent,
    Good,
    Warn,
    Bad,
    /// The background of the selected row.
    Selection,
}

impl Role {
    /// The colour in a terminal that shows any colour.
    pub const fn rgb(self) -> (u8, u8, u8) {
        match self {
            Role::Text => (237, 237, 239),
            Role::Muted => (161, 161, 170),
            // Amber, the base tone of Rookery's icon on a dark ground.
            Role::Accent => (242, 179, 61),
            Role::Good => (158, 216, 140),
            // A lemon yellow, so that a warning doesn't read as the amber accent.
            Role::Warn => (232, 224, 106),
            Role::Bad => (242, 128, 150),
            Role::Selection => (24, 24, 27),
        }
    }

    const fn colour(self) -> Color {
        let (r, g, b) = self.rgb();
        Color::Rgb(r, g, b)
    }

    /// The nearest of the sixteen colours every terminal has; `None` where
    /// the terminal's own text colour or dim text serves better.
    const fn basic(self) -> Option<Color> {
        match self {
            Role::Text | Role::Muted | Role::Selection => None,
            Role::Accent => Some(Color::Yellow),
            Role::Good => Some(Color::Green),
            Role::Warn => Some(Color::LightYellow),
            Role::Bad => Some(Color::LightRed),
        }
    }
}

/// How many colours the terminal shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Depth {
    /// None: `--color never`, `NO_COLOR` or a dumb terminal.
    None,
    /// The sixteen colours of any terminal.
    #[default]
    Basic,
    /// The 256 colours of xterm.
    Indexed,
    /// Any colour.
    True,
}

impl Depth {
    /// The depth the terminal says it has, from `TERM` and `COLORTERM`
    /// (and Windows Terminal's `WT_SESSION`), given that colours are on.
    pub fn of_terminal(var: impl Fn(&str) -> Option<String>) -> Depth {
        let colorterm = var("COLORTERM").unwrap_or_default().to_lowercase();
        let term = var("TERM").unwrap_or_default().to_lowercase();
        if colorterm == "truecolor" || colorterm == "24bit" || var("WT_SESSION").is_some() {
            Depth::True
        } else if term == "dumb" {
            Depth::None
        } else if term.contains("256color") || term.contains("direct") {
            Depth::Indexed
        } else {
            Depth::Basic
        }
    }
}

/// The colour of `role` at `depth`, if it has one there.
pub fn colour(role: Role, depth: Depth) -> Option<Color> {
    match depth {
        Depth::None => None,
        Depth::Basic => role.basic(),
        Depth::Indexed => {
            let (r, g, b) = role.rgb();
            Some(Color::Indexed(indexed(r, g, b)))
        }
        Depth::True => Some(role.colour()),
    }
}

/// Text in the colour of `role` at `depth`. Secondary text is dim where
/// there are only a few colours, as a grey of sixteen is often unreadable.
pub fn style(role: Role, depth: Depth) -> Style {
    let style = match colour(role, depth) {
        Some(colour) if role == Role::Selection => Style::new().bg(colour),
        Some(colour) => Style::new().fg(colour),
        None => Style::new(),
    };
    match (role, depth) {
        (Role::Muted, Depth::Basic) => style.dim(),
        _ => style,
    }
}

/// The parameters of an SGR escape that writes text in `role`'s colour,
/// such as `38;2;116;178;255`; empty without a colour.
pub fn sgr(role: Role, depth: Depth) -> String {
    let ground = if role == Role::Selection { 48 } else { 38 };
    match (colour(role, depth), role, depth) {
        (_, Role::Muted, Depth::Basic) => "2".into(),
        (Some(Color::Rgb(r, g, b)), ..) => format!("{ground};2;{r};{g};{b}"),
        (Some(Color::Indexed(index)), ..) => format!("{ground};5;{index}"),
        (Some(Color::LightYellow), ..) => format!("{}", ground + 55),
        (Some(Color::Green), ..) => format!("{}", ground - 6),
        (Some(Color::Yellow), ..) => format!("{}", ground - 5),
        (Some(Color::LightRed), ..) => format!("{}", ground + 53),
        _ => String::new(),
    }
}

/// The nearest colour of xterm's 6×6×6 cube or its grey ramp.
fn indexed(r: u8, g: u8, b: u8) -> u8 {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let near = |value: u8| {
        (0..6)
            .min_by_key(|&step| (i32::from(STEPS[step]) - i32::from(value)).abs())
            .unwrap_or(0)
    };
    let (cr, cg, cb) = (near(r), near(g), near(b));
    let cube = 16 + 36 * cr + 6 * cg + cb;
    let distance = |x: (u8, u8, u8)| {
        let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
        d(x.0, r) + d(x.1, g) + d(x.2, b)
    };
    let average = ((u32::from(r) + u32::from(g) + u32::from(b)) / 3) as u8;
    let grey_step = (u32::from(average.saturating_sub(8)) / 10).min(23) as u8;
    let grey = 8 + 10 * grey_step;
    if distance((grey, grey, grey)) < distance((STEPS[cr], STEPS[cg], STEPS[cb])) {
        232 + grey_step
    } else {
        cube as u8
    }
}

/// The screen.
pub const BASE: Color = Color::Rgb(10, 10, 11);
/// Pop-ups, the selected row and the current tab.
pub const SURFACE: Color = Role::Selection.colour();
/// The selected row in a pop-up, and inline code.
pub const RAISED: Color = Color::Rgb(42, 42, 48);
/// Panel borders.
pub const LINE: Color = Color::Rgb(42, 42, 48);
/// Body text.
pub const TEXT: Color = Role::Text.colour();
/// Secondary text.
pub const MUTED: Color = Role::Muted.colour();
/// Selection, values and keys.
pub const ACCENT: Color = Role::Accent.colour();
/// The plumage of the mark, and quiet marks beside it.
pub const PLUMAGE: Color = Color::Rgb(173, 123, 40);
/// The bill of the mark.
pub const BILL: Color = Color::Rgb(249, 218, 151);
pub const GOOD: Color = Role::Good.colour();
pub const WARN: Color = Role::Warn.colour();
pub const BAD: Color = Role::Bad.colour();

/// The mark in front of the selected row, in the screens and in a prompt.
pub const CURSOR: &str = "▌ ";

/// The current tab: the accent on a neutral plate, as the accent is never a fill.
pub fn plate() -> Style {
    Style::new().fg(ACCENT).bg(SURFACE).bold()
}

/// What everything else is drawn on.
pub fn backdrop() -> Block<'static> {
    Block::new().style(Style::new().bg(BASE).fg(TEXT))
}

/// A titled panel; `focused` when it takes the keys.
pub fn panel(title: &str, focused: bool) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { ACCENT } else { LINE }))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(if focused { ACCENT } else { MUTED }).bold(),
        ))
        .padding(Padding::horizontal(1))
}

/// A panel floating over the screen.
pub fn popup(title: &str, colour: Color) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(colour))
        .style(Style::new().bg(SURFACE).fg(TEXT))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(colour).bold(),
        ))
        .padding(Padding::horizontal(1))
}

/// The mark in front of the selected row.
pub fn cursor() -> Line<'static> {
    Line::styled(CURSOR, Style::new().fg(ACCENT))
}

/// Key hints such as `↑↓ select · Enter open`: the key stands out, the rest recedes.
pub fn hints(text: &str) -> Line<'static> {
    let mut spans = vec![];
    for (index, hint) in text.split(" · ").enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", Style::new()));
        }
        let (key, action) = hint.split_once(' ').unwrap_or((hint, ""));
        spans.push(Span::styled(key.to_owned(), Style::new().fg(ACCENT).bold()));
        spans.push(Span::styled(format!(" {action}"), Style::new().fg(MUTED)));
    }
    Line::from(spans)
}

/// The mark, three rows high, with the name beside it.
pub fn logo() -> Vec<Line<'static>> {
    let plumage = Style::new().fg(PLUMAGE);
    let bill = Style::new().fg(BILL);
    let muted = Style::new().fg(MUTED);
    let name = vec![
        Span::styled(" ▄██▄  ", plumage),
        Span::raw("   "),
        Span::styled("rookery", Style::new().fg(ACCENT).bold()),
    ];
    let about = vec![
        Span::styled("████", plumage),
        Span::styled("▀▀ ", bill),
        Span::raw("   "),
        Span::styled(
            "provisioning and management for development machines",
            muted,
        ),
    ];
    let version = vec![
        Span::styled(" ▀██▀  ", plumage),
        Span::raw("   "),
        Span::styled(format!("v{}", rookery_api::VERSION), muted),
    ];
    vec![Line::from(name), Line::from(about), Line::from(version)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_depth_has_a_colour_for_the_accent_and_the_states() {
        for role in [Role::Accent, Role::Good, Role::Warn, Role::Bad] {
            for depth in [Depth::Basic, Depth::Indexed, Depth::True] {
                assert!(colour(role, depth).is_some(), "{role:?} at {depth:?}");
                assert!(!sgr(role, depth).is_empty(), "{role:?} at {depth:?}");
            }
            assert_eq!(colour(role, Depth::None), None);
            assert_eq!(sgr(role, Depth::None), "");
        }
    }

    #[test]
    fn the_screens_and_the_command_line_share_the_accent() {
        assert_eq!(colour(Role::Accent, Depth::True), Some(ACCENT));
        assert_eq!(sgr(Role::Accent, Depth::True), "38;2;242;179;61");
        assert_eq!(sgr(Role::Accent, Depth::Basic), "33");
        assert_eq!(sgr(Role::Bad, Depth::Basic), "91");
        assert_eq!(sgr(Role::Good, Depth::Basic), "32");
        assert_eq!(sgr(Role::Warn, Depth::Basic), "93");
        assert_eq!(sgr(Role::Muted, Depth::Basic), "2");
        assert_eq!(sgr(Role::Selection, Depth::True), "48;2;24;24;27");
    }

    #[test]
    fn colours_come_near_in_256() {
        assert_eq!(indexed(0, 0, 0), 16);
        assert_eq!(indexed(255, 255, 255), 231);
        // The accent is an amber of the cube, and a warning a paler yellow.
        assert_eq!(indexed(242, 179, 61), 215);
        assert_eq!(indexed(232, 224, 106), 185);
        assert_eq!(indexed(128, 128, 128), 244);
    }

    #[test]
    fn the_depth_follows_the_terminal() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            }
        };
        assert_eq!(
            Depth::of_terminal(env(&[("COLORTERM", "truecolor")])),
            Depth::True
        );
        assert_eq!(
            Depth::of_terminal(env(&[("TERM", "xterm-256color")])),
            Depth::Indexed
        );
        assert_eq!(Depth::of_terminal(env(&[("TERM", "linux")])), Depth::Basic);
        assert_eq!(Depth::of_terminal(env(&[("TERM", "dumb")])), Depth::None);
    }
}
