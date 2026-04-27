//! Form-level FSM. Stub for I1; implemented in I2.

use crate::form::{Form, FormEvent};
use hjkl_engine::Input;

impl Form {
    /// Phase I1 stub: the FSM lands in I2.
    pub fn handle_input(&mut self, _input: Input) -> Option<FormEvent> {
        None
    }
}
