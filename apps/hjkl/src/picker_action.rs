//! App-specific picker actions. Boxed into `PickerAction::Custom` and
//! downcast in the dispatcher.

use std::path::PathBuf;

pub enum AppAction {
    OpenPath(PathBuf),
    OpenPathAtLine(PathBuf, u32),
    ShowCommit(String),
    CheckoutBranch(String),
    CheckoutTag(String),
    FetchRemote(String),
    SwitchSlot(usize),
    StashApply(usize),
    StashPop(usize),
    StashDrop(usize),
}
