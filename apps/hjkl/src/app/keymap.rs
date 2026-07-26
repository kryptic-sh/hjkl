use hjkl_engine::input::encode_macro;
use hjkl_engine::{Input, Key, VimMode};
use hjkl_keymap::{Chord, KeyCode as KmKeyCode, KeyModifiers, key::KeyEvent};

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
                encode_macro(&r.lhs),
                encode_macro(&r.rhs)
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

/// Map a [`MapMode`] to a [`hjkl_vim::Mode`].
///
/// Returns `None` for [`MapMode::Terminal`] — there's no Terminal variant.
/// Terminal-mode runtime maps are silently skipped with a status message at
/// the call site.
pub(crate) fn map_mode_to_km_mode(mode: MapMode) -> Option<hjkl_vim::Mode> {
    match mode {
        MapMode::Normal => Some(hjkl_vim::Mode::Normal),
        MapMode::Visual => Some(hjkl_vim::Mode::Visual),
        MapMode::Insert => Some(hjkl_vim::Mode::Insert),
        MapMode::OperatorPending => Some(hjkl_vim::Mode::OpPending),
        MapMode::CommandLine => Some(hjkl_vim::Mode::CommandLine),
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

pub(crate) fn parse_key_sequence(text: &str, leader: char) -> Result<Vec<Input>, String> {
    let mut result = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '<' {
            result.push(Input {
                key: Key::Char(ch),
                ..Input::default()
            });
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
            result.push(Input {
                key: Key::Char('<'),
                ..Input::default()
            });
            for c in tag.chars() {
                result.push(Input {
                    key: Key::Char(c),
                    ..Input::default()
                });
            }
            break;
        }

        let chord = Chord::parse(&format!("<{tag}>"), leader).map_err(|e| e.to_string())?;
        for ev in &chord.0 {
            match chord_event_to_input(ev) {
                Some(input) => result.push(input),
                None => return Err(format!("cannot map <{tag}> to an engine input")),
            }
        }
    }
    Ok(result)
}

/// Map an [`hjkl_keymap::KeyEvent`] to an [`hjkl_engine::Input`].
///
/// Returns `None` for key codes the engine cannot represent (`Insert`, `F(n)`).
///
/// SHIFT handling mirrors `hjkl_keymap_tui::from_crossterm`: on ASCII letters it
/// folds into the uppercase char and the flag is cleared (vim convention, and
/// what the runtime delivers); on every other key code it is preserved, so
/// `<S-Tab>` stays distinct from `<Tab>` and matches the Tab+SHIFT event that
/// crossterm's `BackTab` becomes.
fn chord_event_to_input(ev: &KeyEvent) -> Option<Input> {
    let ctrl = ev.modifiers.contains(KeyModifiers::CTRL);
    let alt = ev.modifiers.contains(KeyModifiers::ALT);
    let mut shift = ev.modifiers.contains(KeyModifiers::SHIFT);

    let key = match ev.code {
        KmKeyCode::Char(c) => {
            // SHIFT on letters folds into uppercase and clears the flag.
            if c.is_ascii_alphabetic() && shift {
                shift = false;
                Key::Char(c.to_ascii_uppercase())
            } else {
                Key::Char(c)
            }
        }
        KmKeyCode::Enter => Key::Enter,
        KmKeyCode::Esc => Key::Esc,
        KmKeyCode::Tab => Key::Tab,
        KmKeyCode::Backspace => Key::Backspace,
        KmKeyCode::Delete => Key::Delete,
        KmKeyCode::Insert => return None,
        KmKeyCode::Up => Key::Up,
        KmKeyCode::Down => Key::Down,
        KmKeyCode::Left => Key::Left,
        KmKeyCode::Right => Key::Right,
        KmKeyCode::Home => Key::Home,
        KmKeyCode::End => Key::End,
        KmKeyCode::PageUp => Key::PageUp,
        KmKeyCode::PageDown => Key::PageDown,
        KmKeyCode::F(_) => return None,
    };
    Some(Input {
        key,
        ctrl,
        alt,
        shift,
    })
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

/// Parse a `:map`-family ex command.
///
/// Three outcomes, kept distinct so the caller can tell "not mine" from
/// "mine but broken":
/// * `None` — `cmd` is not a map verb at all; the caller continues with
///   normal ex dispatch.
/// * `Some(Err(msg))` — `cmd` *is* a map verb but the lhs/rhs key notation
///   is invalid; the caller reports `msg` and stops.
/// * `Some(Ok(cmd))` — parsed successfully.
pub(crate) fn parse_runtime_map_command(
    cmd: &str,
    leader: char,
) -> Option<Result<RuntimeMapCommand, String>> {
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
        return Some(Ok(RuntimeMapCommand::Clear { modes }));
    }

    let is_remove = name.ends_with("unmap");
    if rest.is_empty() {
        return Some(Ok(RuntimeMapCommand::List { modes }));
    }

    let split = rest
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    let (lhs_text, rhs_text) = rest.split_at(split);
    let lhs_text = lhs_text.trim();
    let rhs_text = rhs_text.trim();
    let lhs = match parse_key_sequence(lhs_text, leader) {
        Ok(keys) => keys,
        Err(reason) => return Some(Err(format!("{name}: invalid lhs `{lhs_text}`: {reason}"))),
    };
    if is_remove {
        return Some(Ok(RuntimeMapCommand::Remove { modes, lhs }));
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
    let rhs = match parse_key_sequence(rhs_text, leader) {
        Ok(keys) => keys,
        Err(reason) => return Some(Err(format!("{name}: invalid rhs `{rhs_text}`: {reason}"))),
    };
    Some(Ok(RuntimeMapCommand::Add {
        modes,
        recursive,
        lhs,
        rhs,
    }))
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

    /// Pins the `:map` listing rendering, including the two notation cases
    /// most likely to drift if the key-spelling table is ever duplicated
    /// again: `<lt>` for a literal `<` and `<CR>` for Enter.
    #[test]
    fn user_map_list_renders_lt_and_cr_notation() {
        let records = vec![
            UserKeymapRecord {
                mode: MapMode::Normal,
                lhs: vec![ch('<'), ch('x')],
                rhs: vec![
                    ch('a'),
                    Input {
                        key: Key::Enter,
                        ..Input::default()
                    },
                ],
                recursive: true,
            },
            UserKeymapRecord {
                mode: MapMode::Insert,
                lhs: vec![Input {
                    key: Key::Char('d'),
                    ctrl: true,
                    ..Input::default()
                }],
                rhs: vec![Input {
                    key: Key::Esc,
                    ..Input::default()
                }],
                recursive: false,
            },
        ];
        let out = format_user_map_list(&records, &[MapMode::Normal, MapMode::Insert]);
        assert_eq!(
            out,
            "[normal]\n  map <lt>x a<CR>\n[insert]\n  noremap <C-d> <Esc>"
        );
    }

    #[test]
    fn parse_leader_and_cr_tags() {
        let keys = parse_key_sequence("<leader>w<CR>", '\\').unwrap();
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

    #[test]
    fn runtime_map_bad_lhs_notation_is_some_err() {
        let out = parse_runtime_map_command("nmap <NoSuchKey> x", '\\');
        let Some(Err(msg)) = out else {
            panic!("bad lhs notation must be Some(Err(_))");
        };
        assert!(
            msg.contains("NoSuchKey"),
            "message must name the offending tag, got {msg:?}"
        );
        assert!(
            msg.contains("nmap"),
            "message must name the map verb, got {msg:?}"
        );
    }

    #[test]
    fn runtime_map_unrepresentable_lhs_is_some_err() {
        // <F5> parses as a chord but has no engine Input equivalent.
        let out = parse_runtime_map_command("nmap <F5> x", '\\');
        let Some(Err(msg)) = out else {
            panic!("unrepresentable lhs must be Some(Err(_))");
        };
        assert!(
            msg.contains("F5"),
            "message must name the offending tag, got {msg:?}"
        );
    }

    #[test]
    fn runtime_map_bad_rhs_notation_is_some_err() {
        let out = parse_runtime_map_command("nmap x <F5>", '\\');
        let Some(Err(msg)) = out else {
            panic!("bad rhs notation must be Some(Err(_))");
        };
        assert!(
            msg.contains("rhs") && msg.contains("F5"),
            "message must flag the rhs and the tag, got {msg:?}"
        );
    }

    #[test]
    fn non_map_command_is_none() {
        assert!(parse_runtime_map_command("nonsense stuff", '\\').is_none());
    }

    #[test]
    fn valid_runtime_map_is_some_ok() {
        let out = parse_runtime_map_command("nmap <CR> x", '\\');
        let Some(Ok(RuntimeMapCommand::Add { lhs, rhs, .. })) = out else {
            panic!("valid nmap must be Some(Ok(Add {{ .. }}))");
        };
        assert_eq!(
            lhs,
            vec![Input {
                key: Key::Enter,
                ..Input::default()
            }]
        );
        assert_eq!(rhs, vec![ch('x')]);
    }
}
