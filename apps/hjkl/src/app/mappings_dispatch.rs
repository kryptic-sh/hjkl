//! Phase 4d1: `:map` family intercept extracted from `ex_dispatch`.
//!
//! Handles the ~24 `:map` / `:noremap` / `:unmap` / `:mapclear` verb forms via
//! the single shared parser `keymap::parse_runtime_map_command`.  Keeping them
//! in a dedicated module avoids forcing 24 individual `HostCmd` impls that would
//! either duplicate the parser call or make registration purely ceremonial.

use hjkl_info_popup::InfoPopup;
use hjkl_which_key::truncate_desc;

use crate::keymap_actions::AppAction;

use super::{App, keymap};

impl App {
    /// Try to handle a `:map`-family ex command.
    ///
    /// Returns `true` if `raw` was a map command and has been applied (the
    /// caller must `return` immediately).  Returns `false` if
    /// `parse_runtime_map_command` returned `None` — i.e. `raw` is not a map
    /// verb — and the caller should continue with normal dispatch.
    pub(crate) fn try_handle_runtime_map(&mut self, raw: &str) -> bool {
        let Some(map_cmd) = keymap::parse_runtime_map_command(raw, self.config.editor.leader)
        else {
            return false;
        };

        match map_cmd {
            keymap::RuntimeMapCommand::Add {
                modes,
                recursive,
                lhs,
                rhs,
            } => {
                let lhs_km: Vec<hjkl_keymap::KeyEvent> =
                    lhs.iter().map(|&i| keymap::input_to_km_event(i)).collect();
                let rhs_km: Vec<hjkl_keymap::KeyEvent> =
                    rhs.iter().map(|&i| keymap::input_to_km_event(i)).collect();
                let action = AppAction::Replay {
                    keys: rhs_km.clone(),
                    recursive,
                };
                let leader = self.config.editor.leader;
                // Build a human-readable "→ <rhs notation>" desc, truncated at 40 chars.
                let rhs_notation = truncate_desc(
                    &format!("→ {}", hjkl_keymap::Chord(rhs_km).to_notation(leader)),
                    40,
                );
                let mut any_skipped = false;
                for &mode in &modes {
                    let Some(km_mode) = keymap::map_mode_to_km_mode(mode) else {
                        // Terminal mode: no keymap equivalent yet — skip silently.
                        any_skipped = true;
                        continue;
                    };
                    // Convert lhs_km to a notation string and use Keymap::add so
                    // the trie round-trips through Chord::parse (avoids submodule edits).
                    let lhs_chord = hjkl_keymap::Chord(lhs_km.clone());
                    let notation = lhs_chord.to_notation(leader);
                    let binding = hjkl_keymap::Binding {
                        action: action.clone(),
                        desc: rhs_notation.clone(),
                        recursive,
                        condition: None,
                    };
                    // Re-parse to get canonical Chord, then add_chord.
                    if let Ok(chord) = hjkl_keymap::Chord::parse(&notation, leader) {
                        self.app_keymap.add_chord(km_mode, chord, binding);
                    }
                    // Record for listing (de-dup by mode+lhs).
                    self.user_keymap_records
                        .retain(|r| !(r.mode == mode && r.lhs == lhs));
                    self.user_keymap_records.push(keymap::UserKeymapRecord {
                        mode,
                        lhs: lhs.clone(),
                        rhs: rhs.clone(),
                        recursive,
                    });
                }
                if any_skipped {
                    self.status_message = Some("mapping added (terminal mode skipped)".into());
                } else {
                    self.status_message = Some("mapping added".into());
                }
            }
            keymap::RuntimeMapCommand::Remove { modes, lhs } => {
                let leader = self.config.editor.leader;
                let lhs_km: Vec<hjkl_keymap::KeyEvent> =
                    lhs.iter().map(|&i| keymap::input_to_km_event(i)).collect();
                let notation = hjkl_keymap::Chord(lhs_km).to_notation(leader);
                let mut removed = false;
                for &mode in &modes {
                    let Some(km_mode) = keymap::map_mode_to_km_mode(mode) else {
                        continue;
                    };
                    if let Ok(true) = self.app_keymap.remove(km_mode, &notation) {
                        removed = true;
                    }
                    self.user_keymap_records
                        .retain(|r| !(r.mode == mode && r.lhs == lhs));
                }
                self.status_message = Some(
                    if removed {
                        "mapping removed"
                    } else {
                        "E31: No such mapping"
                    }
                    .into(),
                );
            }
            keymap::RuntimeMapCommand::Clear { modes } => {
                let leader = self.config.editor.leader;
                let to_remove: Vec<keymap::UserKeymapRecord> = self
                    .user_keymap_records
                    .iter()
                    .filter(|r| modes.contains(&r.mode))
                    .cloned()
                    .collect();
                for r in &to_remove {
                    if let Some(km_mode) = keymap::map_mode_to_km_mode(r.mode) {
                        let lhs_km: Vec<hjkl_keymap::KeyEvent> = r
                            .lhs
                            .iter()
                            .map(|&i| keymap::input_to_km_event(i))
                            .collect();
                        let notation = hjkl_keymap::Chord(lhs_km).to_notation(leader);
                        let _ = self.app_keymap.remove(km_mode, &notation);
                    }
                }
                self.user_keymap_records
                    .retain(|r| !modes.contains(&r.mode));
                self.status_message = Some("mappings cleared".into());
            }
            keymap::RuntimeMapCommand::List { modes } => {
                self.info_popup = Some(InfoPopup::new(
                    "mappings",
                    keymap::format_user_map_list(&self.user_keymap_records, &modes),
                ));
            }
        }

        true
    }
}
