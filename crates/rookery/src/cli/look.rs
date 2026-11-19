//! What the command line paints with: the colours of its results, hints and
//! prompts, in the tones of Rookery's icon. The greys are Aiviarium's
//! neutrals and the accent is Rookery's amber, the base tone of the bird on a
//! dark ground.

use ratatui::style::{Color, Style};

/// What a colour is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Secondary text: hints, help, what was already said.
    Muted,
    /// Values, keys, commands and what is selected.
    Accent,
    Good,
    Warn,
    Bad,
}

impl Role {
    /// The colour in a terminal that shows any colour.
    pub const fn rgb(self) -> (u8, u8, u8) {
        match self {
            Role::Muted => (161, 161, 170),
            // Amber, the base tone of Rookery's icon on a dark ground.
            Role::Accent => (242, 179, 61),
            Role::Good => (158, 216, 140),
            // A lemon yellow, so that a warning doesn't read as the amber accent.
            Role::Warn => (232, 224, 106),
            Role::Bad => (242, 128, 150),
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
            Role::Muted => None,
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
        Some(colour) => Style::new().fg(colour),
        None => Style::new(),
    };
    match (role, depth) {
        (Role::Muted, Depth::Basic) => style.dim(),
        _ => style,
    }
}

/// The parameters of an SGR escape that writes text in `role`'s colour,
/// such as `38;2;242;179;61`; empty without a colour.
pub fn sgr(role: Role, depth: Depth) -> String {
    match (colour(role, depth), role, depth) {
        (_, Role::Muted, Depth::Basic) => "2".into(),
        (Some(Color::Rgb(r, g, b)), ..) => format!("38;2;{r};{g};{b}"),
        (Some(Color::Indexed(index)), ..) => format!("38;5;{index}"),
        (Some(Color::LightYellow), ..) => "93".into(),
        (Some(Color::Green), ..) => "32".into(),
        (Some(Color::Yellow), ..) => "33".into(),
        (Some(Color::LightRed), ..) => "91".into(),
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

/// The mark in front of the choice a question is on.
pub const CURSOR: &str = "▌ ";

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
    fn the_accent_is_the_ambers_of_rookerys_icon() {
        assert_eq!(sgr(Role::Accent, Depth::True), "38;2;242;179;61");
        assert_eq!(sgr(Role::Accent, Depth::Basic), "33");
        assert_eq!(sgr(Role::Bad, Depth::Basic), "91");
        assert_eq!(sgr(Role::Good, Depth::Basic), "32");
        assert_eq!(sgr(Role::Warn, Depth::Basic), "93");
        assert_eq!(sgr(Role::Muted, Depth::Basic), "2");
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
