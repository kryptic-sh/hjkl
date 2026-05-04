//! App-specific picker actions. Boxed into `PickerAction::Custom` and
//! downcast in the dispatcher.

use std::path::PathBuf;

pub enum AppAction {
    OpenPath(PathBuf),
    OpenPathAtLine(PathBuf, u32),
    ShowCommit(String),
    CheckoutBranch(String),
    SwitchSlot(usize),
    StashApply(usize),
    StashPop(usize),
    StashDrop(usize),
}
