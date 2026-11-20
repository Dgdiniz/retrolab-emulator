//! The progress bars a command shows on standard error when asked
//! (`--progress`); they draw nothing when standard error is not a terminal.

use indicatif::{ProgressBar, ProgressStyle};

/// A bar of `len` steps counted in `unit`, with the time left.
pub fn bar(show: bool, len: u64, unit: &str) -> Option<ProgressBar> {
    styled(
        show,
        ProgressBar::new(len),
        ProgressStyle::default_bar()
            .template(&format!(
                "{{spinner:.green}} [{{bar:40.cyan/blue}}] {{pos}}/{{len}} {unit} ({{eta}})"
            ))
            .map(|style| style.progress_chars("#>-")),
    )
}

/// A spinner counting instructions: `what: N instructions`.
pub fn spinner(show: bool, what: &str) -> Option<ProgressBar> {
    styled(
        show,
        ProgressBar::new_spinner(),
        ProgressStyle::default_spinner()
            .template(&format!("{{spinner:.green}} {what}: {{pos}} instructions")),
    )
}

/// A bar counting lines, up to `len` when it is known, with the time spent.
pub fn line_bar(show: bool, len: Option<u64>) -> Option<ProgressBar> {
    styled(
        show,
        ProgressBar::new(len.unwrap_or(u64::MAX)),
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos} lines ({elapsed})")
            .map(|style| style.progress_chars("#>-")),
    )
}

/// Take the bar off the terminal.
pub fn finish(bar: Option<ProgressBar>) {
    if let Some(bar) = bar {
        bar.finish_and_clear();
    }
}

fn styled(
    show: bool,
    bar: ProgressBar,
    style: Result<ProgressStyle, indicatif::style::TemplateError>,
) -> Option<ProgressBar> {
    if !show {
        return None;
    }
    if let Ok(style) = style {
        bar.set_style(style);
    }
    Some(bar)
}
