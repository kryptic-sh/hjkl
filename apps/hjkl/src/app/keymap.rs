use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hjkl_engine::{Input, Key, VimMode};
use std::collections::BTreeMap;

use super::App;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum MapMode {
    Normal,
    Visual,
    Insert,
    OperatorPending,
    CommandLine,
    Terminal,
}

#[derive(Debug, Clone)]
struct Mapping {
    lhs: Vec<Input>,
    rhs: Vec<Input>,
    recursive: bool,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct RuntimeKeymaps {
    maps: BTreeMap<MapMode, Vec<Mapping>>,
    pending: Vec<Input>,
    pending_mode: Option<MapMode>,
}

impl RuntimeKeymaps {
    fn mappings_mut(&mut self, mode: MapMode) -> &mut Vec<Mapping> {
        self.maps.entry(mode).or_default()
    }

    fn mappings(&self, mode: MapMode) -> &[Mapping] {
        self.maps.get(&mode).map(Vec::as_slice).unwrap_or(&[])
    }

    fn clear_pending(&mut self) {
        self.pending.clear();
        self.pending_mode = None;
    }

    pub(crate) fn add(
        &mut self,
        modes: &[MapMode],
        lhs: Vec<Input>,
        rhs: Vec<Input>,
        recursive: bool,
    ) {
        for &mode in modes {
            let list = self.mappings_mut(mode);
            list.retain(|m| m.lhs != lhs);
            list.push(Mapping {
                lhs: lhs.clone(),
                rhs: rhs.clone(),
                recursive,
            });
        }
    }

    pub(crate) fn remove(&mut self, modes: &[MapMode], lhs: &[Input]) -> bool {
        let mut removed = false;
        for &mode in modes {
            if let Some(list) = self.maps.get_mut(&mode) {
                let before = list.len();
                list.retain(|m| m.lhs.as_slice() != lhs);
                removed |= list.len() != before;
            }
        }
        removed
    }

    pub(crate) fn clear(&mut self, modes: &[MapMode]) {
        for &mode in modes {
            self.maps.remove(&mode);
        }
        self.clear_pending();
    }

    pub(crate) fn list(&self, modes: &[MapMode]) -> String {
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
            let mut entries: Vec<&Mapping> = self.mappings(mode).iter().collect();
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
            for mapping in entries {
                lines.push(format!(
                    "  {} {} {}",
                    if mapping.recursive { "map" } else { "noremap" },
                    display_keys(&mapping.lhs),
                    display_keys(&mapping.rhs)
                ));
            }
        }
        lines.join("\n")
    }

    pub(crate) fn translate(&mut self, mode: Option<MapMode>, input: Input) -> Option<Vec<Input>> {
        let Some(mode) = mode else {
            self.clear_pending();
            return Some(vec![input]);
        };

        if self.pending_mode != Some(mode) {
            self.clear_pending();
            self.pending_mode = Some(mode);
        }

        self.pending.push(input);
        let mut out = Vec::new();
        let mut guard = 0usize;

        loop {
            guard += 1;
            if guard > 1024 {
                out.extend(self.pending.drain(..));
                break;
            }

            if self.pending.is_empty() {
                break;
            }

            if let Some(mapping) = self.find_exact(mode) {
                let lhs_len = mapping.lhs.len();
                self.pending.drain(..lhs_len);
                if mapping.recursive {
                    for rhs in mapping.rhs.into_iter().rev() {
                        self.pending.insert(0, rhs);
                    }
                } else {
                    out.extend(mapping.rhs);
                }
                continue;
            }

            if self.has_prefix(mode) {
                return None;
            }

            out.push(self.pending.remove(0));
        }

        Some(out)
    }

    fn find_exact(&self, mode: MapMode) -> Option<Mapping> {
        let mut best: Option<Mapping> = None;
        for mapping in self.mappings(mode) {
            if mapping.lhs == self.pending {
                if best
                    .as_ref()
                    .is_none_or(|cur| mapping.lhs.len() >= cur.lhs.len())
                {
                    best = Some(mapping.clone());
                }
            }
        }
        best
    }

    fn has_prefix(&self, mode: MapMode) -> bool {
        self.mappings(mode)
            .iter()
            .any(|mapping| mapping.lhs.starts_with(&self.pending))
    }
}

pub(crate) fn map_mode_for_vim(mode: VimMode) -> Option<MapMode> {
    match mode {
        VimMode::Normal => Some(MapMode::Normal),
        VimMode::Insert => Some(MapMode::Insert),
        VimMode::Visual | VimMode::VisualLine | VimMode::VisualBlock => Some(MapMode::Visual),
    }
}

pub(crate) fn key_event_to_input(key: KeyEvent) -> Input {
    key.into()
}

pub(crate) fn input_to_key_event(input: Input) -> KeyEvent {
    let code = match input.key {
        Key::Char(c) => KeyCode::Char(c),
        Key::Backspace => KeyCode::Backspace,
        Key::Enter => KeyCode::Enter,
        Key::Left => KeyCode::Left,
        Key::Right => KeyCode::Right,
        Key::Up => KeyCode::Up,
        Key::Down => KeyCode::Down,
        Key::Tab => KeyCode::Tab,
        Key::Delete => KeyCode::Delete,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::PageUp => KeyCode::PageUp,
        Key::PageDown => KeyCode::PageDown,
        Key::Esc => KeyCode::Esc,
        Key::Null => KeyCode::Null,
    };

    let mut modifiers = KeyModifiers::NONE;
    if input.ctrl {
        modifiers |= KeyModifiers::CONTROL;
    }
    if input.alt {
        modifiers |= KeyModifiers::ALT;
    }
    if input.shift {
        modifiers |= KeyModifiers::SHIFT;
    }

    KeyEvent::new(code, modifiers)
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

pub(crate) fn is_runtime_map_command(cmd: &str) -> bool {
    parse_runtime_map_command(cmd, '\\').is_some()
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

impl App {
    pub(crate) fn runtime_map_mode(&self) -> Option<MapMode> {
        map_mode_for_vim(self.active().editor.vim_mode())
    }

    pub(crate) fn apply_runtime_map(&mut self, key: KeyEvent) -> Option<Vec<KeyEvent>> {
        let input = key_event_to_input(key);
        let mode = self.runtime_map_mode();
        let expanded = self.runtime_keymaps.translate(mode, input)?;
        Some(expanded.into_iter().map(input_to_key_event).collect())
    }

    pub(crate) fn clear_runtime_map_pending(&mut self) {
        self.runtime_keymaps.clear_pending();
    }
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
    fn recursive_mapping_expands_rhs_again() {
        let mut km = RuntimeKeymaps::default();
        km.add(&[MapMode::Normal], vec![ch('a')], vec![ch('b')], true);
        km.add(&[MapMode::Normal], vec![ch('b')], vec![ch('c')], true);
        let out = km.translate(Some(MapMode::Normal), ch('a')).unwrap();
        assert_eq!(out, vec![ch('c')]);
    }

    #[test]
    fn noremap_keeps_rhs_literal() {
        let mut km = RuntimeKeymaps::default();
        km.add(&[MapMode::Normal], vec![ch('a')], vec![ch('b')], false);
        km.add(&[MapMode::Normal], vec![ch('b')], vec![ch('c')], true);
        let out = km.translate(Some(MapMode::Normal), ch('a')).unwrap();
        assert_eq!(out, vec![ch('b')]);
    }
}
