use hjkl_engine::{Input, Key, VimMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum MapMode {
    Normal,
    Visual,
    Insert,
    OperatorPending,
    CommandLine,
    Terminal,
}

/// A side-table record of one user-registered runtime map, kept for `:map`
/// listing. The trie owns the actual dispatch; this struct only tracks what
/// was registered so `:map` (list) can enumerate user maps without leaking
/// built-in bindings.
#[derive(Debug, Clone)]
pub(crate) struct UserKeymapRecord {
    pub mode: MapMode,
    pub lhs: Vec<Input>,
    pub rhs: Vec<Input>,
    pub recursive: bool,
}

/// Render a list of user maps filtered to `modes` as a displayable string.
pub(crate) fn format_user_map_list(records: &[UserKeymapRecord], modes: &[MapMode]) -> String {
    let mut lines = Vec::new();
    for &mode in modes {
        let label = match mode {
            MapMode::Normal => "normal",
            MapMode::Visual => "visual",
            MapMode::Insert => "insert",
            MapMode::OperatorPending => "operator-pending",
            MapMode::CommandLine => "command-line",
            MapMode::Terminal => "terminal",
        };
        lines.push(format!("[{label}]"));
        let mut entries: Vec<&UserKeymapRecord> =
            records.iter().filter(|r| r.mode == mode).collect();
        entries.sort_by(|a, b| {
            a.lhs
                .len()
                .cmp(&b.lhs.len())
                .then_with(|| a.rhs.len().cmp(&b.rhs.len()))
        });
        if entries.is_empty() {
            lines.push("  (none)".into());
            continue;
        }
        for r in entries {
            lines.push(format!(
                "  {} {} {}",
                if r.recursive { "map" } else { "noremap" },
                display_keys(&r.lhs),
                display_keys(&r.rhs)
            ));
        }
    }
    lines.join("\n")
}

/// Translate an `hjkl_engine::Input` to an `hjkl_keymap::KeyEvent`.
///
/// `Key::Null` has no clean equivalent; it maps to `KeyCode::Char('\0')` so
/// dispatch is consistent and nothing special happens for it.
pub(crate) fn input_to_km_event(input: Input) -> hjkl_keymap::KeyEvent {
    use hjkl_keymap::{KeyCode as KmCode, KeyEvent as KmEvent, KeyModifiers as KmMods};
    let code = match input.key {
        Key::Char(c) => KmCode::Char(c),
        Key::Backspace => KmCode::Backspace,
        Key::Enter => KmCode::Enter,
        Key::Left => KmCode::Left,
        Key::Right => KmCode::Right,
        Key::Up => KmCode::Up,
        Key::Down => KmCode::Down,
        Key::Tab => KmCode::Tab,
        Key::Delete => KmCode::Delete,
        Key::Home => KmCode::Home,
        Key::End => KmCode::End,
        Key::PageUp => KmCode::PageUp,
        Key::PageDown => KmCode::PageDown,
        Key::Esc => KmCode::Esc,
        Key::Null => KmCode::Char('\0'),
    };
    let mut modifiers = KmMods::NONE;
    if input.ctrl {
        modifiers |= KmMods::CTRL;
    }
    if input.alt {
        modifiers |= KmMods::ALT;
    }
    if input.shift {
        modifiers |= KmMods::SHIFT;
    }
    KmEvent::new(code, modifiers)
}

/// Editor modes used by the hjkl umbrella's keymap dispatch. Defined in
/// `hjkl-vim`; re-exported here under the legacy alias so existing
/// `crate::app::keymap::HjklMode` references continue to resolve unchanged.
/// `hjkl-keymap` is generic over the mode discriminator — any
/// `Copy + Eq + Hash + Debug` type satisfies the blanket `Mode` impl.
pub use hjkl_vim::Mode as HjklMode;

/// Map a [`MapMode`] to an [`HjklMode`].
///
/// Returns `None` for [`MapMode::Terminal`] — there's no Terminal variant.
/// Terminal-mode runtime maps are silently skipped with a status message at
/// the call site.
pub(crate) fn map_mode_to_km_mode(mode: MapMode) -> Option<HjklMode> {
    match mode {
        MapMode::Normal => Some(HjklMode::Normal),
        MapMode::Visual => Some(HjklMode::Visual),
        MapMode::Insert => Some(HjklMode::Insert),
        MapMode::OperatorPending => Some(HjklMode::OpPending),
        MapMode::CommandLine => Some(HjklMode::CommandLine),
        MapMode::Terminal => None,
    }
}

pub(crate) fn map_mode_for_vim(mode: VimMode) -> Option<MapMode> {
    match mode {
        VimMode::Normal => Some(MapMode::Normal),
        VimMode::Insert => Some(MapMode::Insert),
        VimMode::Visual | VimMode::VisualLine | VimMode::VisualBlock => Some(MapMode::Visual),
    }
}

pub(crate) fn parse_key_sequence(text: &str, leader: char) -> Vec<Input> {
    let mut normalized = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '<' {
            normalized.push(ch);
            continue;
        }

        let mut tag = String::new();
        let mut closed = false;
        for next in chars.by_ref() {
            if next == '>' {
                closed = true;
                break;
            }
            tag.push(next);
        }

        if !closed {
            normalized.push('<');
            normalized.push_str(&tag);
            break;
        }

        match tag.to_ascii_lowercase().as_str() {
            "leader" => normalized.push(leader),
            "space" => normalized.push(' '),
            "cr" => normalized.push_str("<CR>"),
            "esc" => normalized.push_str("<Esc>"),
            "tab" => normalized.push_str("<Tab>"),
            "bs" => normalized.push_str("<BS>"),
            "lt" => normalized.push_str("<lt>"),
            "del" => normalized.push_str("<Del>"),
            "home" => normalized.push_str("<Home>"),
            "end" => normalized.push_str("<End>"),
            "pageup" => normalized.push_str("<PageUp>"),
            "pagedown" => normalized.push_str("<PageDown>"),
            "up" => normalized.push_str("<Up>"),
            "down" => normalized.push_str("<Down>"),
            "left" => normalized.push_str("<Left>"),
            "right" => normalized.push_str("<Right>"),
            _ => {
                normalized.push('<');
                normalized.push_str(&tag);
                normalized.push('>');
            }
        }
    }

    hjkl_engine::decode_macro(&normalized)
}

pub(crate) fn parse_mode_groups(cmd: &str) -> Option<Vec<MapMode>> {
    match cmd {
        "map" | "noremap" | "nm" => Some(vec![
            MapMode::Normal,
            MapMode::Visual,
            MapMode::OperatorPending,
        ]),
        "nmap" | "nnoremap" | "nunmap" | "nmapclear" => Some(vec![MapMode::Normal]),
        "vmap" | "vnoremap" | "vunmap" | "vmapclear" => Some(vec![MapMode::Visual]),
        "xmap" | "xnoremap" | "xunmap" | "xmapclear" => Some(vec![MapMode::Visual]),
        "imap" | "inoremap" | "iunmap" | "imapclear" => Some(vec![MapMode::Insert]),
        "omap" | "onoremap" | "ounmap" | "omapclear" => Some(vec![MapMode::OperatorPending]),
        "cmap" | "cnoremap" | "cunmap" | "cmapclear" => Some(vec![MapMode::CommandLine]),
        "tmap" | "tnoremap" | "tunmap" | "tmapclear" => Some(vec![MapMode::Terminal]),
        "unmap" | "mapclear" => Some(vec![
            MapMode::Normal,
            MapMode::Visual,
            MapMode::OperatorPending,
            MapMode::Insert,
            MapMode::CommandLine,
            MapMode::Terminal,
        ]),
        _ => None,
    }
}

pub(crate) enum RuntimeMapCommand {
    Add {
        modes: Vec<MapMode>,
        recursive: bool,
        lhs: Vec<Input>,
        rhs: Vec<Input>,
    },
    Remove {
        modes: Vec<MapMode>,
        lhs: Vec<Input>,
    },
    Clear {
        modes: Vec<MapMode>,
    },
    List {
        modes: Vec<MapMode>,
    },
}

pub(crate) fn parse_runtime_map_command(cmd: &str, leader: char) -> Option<RuntimeMapCommand> {
    let cmd = cmd.trim();
    let split = cmd
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(cmd.len());
    let (name, rest) = cmd.split_at(split);
    let rest = rest.trim();
    let modes = parse_mode_groups(name)?;

    if name.ends_with("clear") {
        return Some(RuntimeMapCommand::Clear { modes });
    }

    let is_remove = name.ends_with("unmap");
    if rest.is_empty() {
        return Some(RuntimeMapCommand::List { modes });
    }

    let split = rest
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    let (lhs, rhs) = rest.split_at(split);
    let lhs = lhs.trim();
    let rhs = rhs.trim();
    let lhs = parse_key_sequence(lhs, leader);
    if is_remove {
        return Some(RuntimeMapCommand::Remove { modes, lhs });
    }
    let recursive = !matches!(
        name,
        "noremap"
            | "nnoremap"
            | "vnoremap"
            | "xnoremap"
            | "inoremap"
            | "onoremap"
            | "cnoremap"
            | "tnoremap"
            | "nm"
    );
    let rhs = parse_key_sequence(rhs, leader);
    Some(RuntimeMapCommand::Add {
        modes,
        recursive,
        lhs,
        rhs,
    })
}

fn display_keys(keys: &[Input]) -> String {
    let mut out = String::new();
    for input in keys {
        match input.key {
            Key::Char(c) if input.ctrl => {
                out.push_str("<C-");
                out.push(c);
                out.push('>');
            }
            Key::Char(c) if input.alt => {
                out.push_str("<M-");
                out.push(c);
                out.push('>');
            }
            Key::Char('<') => out.push_str("<lt>"),
            Key::Char(c) => out.push(c),
            Key::Esc => out.push_str("<Esc>"),
            Key::Enter => out.push_str("<CR>"),
            Key::Backspace => out.push_str("<BS>"),
            Key::Tab => out.push_str("<Tab>"),
            Key::Up => out.push_str("<Up>"),
            Key::Down => out.push_str("<Down>"),
            Key::Left => out.push_str("<Left>"),
            Key::Right => out.push_str("<Right>"),
            Key::Delete => out.push_str("<Del>"),
            Key::Home => out.push_str("<Home>"),
            Key::End => out.push_str("<End>"),
            Key::PageUp => out.push_str("<PageUp>"),
            Key::PageDown => out.push_str("<PageDown>"),
            Key::Null => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(c: char) -> Input {
        Input {
            key: Key::Char(c),
            ..Input::default()
        }
    }

    #[test]
    fn parse_leader_and_cr_tags() {
        let keys = parse_key_sequence("<leader>w<CR>", '\\');
        assert_eq!(
            keys,
            vec![
                ch('\\'),
                ch('w'),
                Input {
                    key: Key::Enter,
                    ..Input::default()
                }
            ]
        );
    }

    #[test]
    fn input_to_km_event_char() {
        use hjkl_keymap::{KeyCode, KeyModifiers};
        let ev = input_to_km_event(ch('x'));
        assert_eq!(ev.code, KeyCode::Char('x'));
        assert_eq!(ev.modifiers, KeyModifiers::NONE);
    }

    #[test]
    fn input_to_km_event_ctrl() {
        use hjkl_keymap::{KeyCode, KeyModifiers};
        let input = Input {
            key: Key::Char('w'),
            ctrl: true,
            ..Input::default()
        };
        let ev = input_to_km_event(input);
        assert_eq!(ev.code, KeyCode::Char('w'));
        assert!(ev.modifiers.contains(KeyModifiers::CTRL));
    }

    #[test]
    fn map_mode_to_km_mode_terminal_is_none() {
        assert!(map_mode_to_km_mode(MapMode::Terminal).is_none());
    }
}
