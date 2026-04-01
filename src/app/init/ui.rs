use crate::app_state::AppState;
use std::sync::Arc;

pub fn build_initial_ui(
    app: &gtk4::Application,
    state: Arc<AppState>,
) -> (gtk4::ApplicationWindow, crate::ui::transport::TransportBar) {
    crate::ui::app_window::build_window(app, state)
}
