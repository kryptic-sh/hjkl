use hjkl_splash::start_screen::{StartScreen as SplashStartScreen, StartScreenTheme};
use ratatui::{Frame, layout::Rect, style::Color};

pub use hjkl_splash::start_screen::StartScreen;

/// Build a `StartScreen` wired to the app's live palette.
pub fn new_with_theme(theme: &crate::theme::AppTheme) -> StartScreen {
    let ui = &theme.ui;
    let mut screen = SplashStartScreen::build(env!("CARGO_PKG_VERSION"));
    screen.palette = StartScreenTheme {
        text_dim: color_to_rgb(ui.text_dim),
        text: color_to_rgb(ui.text),
        cursor_line_bg: color_to_rgb(ui.cursor_line_bg),
    };
    screen
}

fn color_to_rgb(c: Color) -> hjkl_splash::Rgb {
    match c {
        Color::Rgb(r, g, b) => hjkl_splash::Rgb(r, g, b),
        // Fallback: mid-grey — named/indexed colours don't map to Rgb.
        _ => hjkl_splash::Rgb(0x8b, 0x95, 0xa7),
    }
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    screen: &StartScreen,
    _theme: &crate::theme::AppTheme,
) {
    hjkl_splash_tui::render(frame, area, screen);
}
