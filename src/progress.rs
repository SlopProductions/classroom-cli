use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::borrow::Cow;
use std::time::Duration;

const TICKS: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ";

pub struct Spinner(ProgressBar);

impl Spinner {
    pub fn new(msg: impl Into<Cow<'static, str>>) -> Self {
        let pb = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .expect("valid spinner template")
                .tick_chars(TICKS),
        );
        pb.set_message(msg);
        pb.enable_steady_tick(Duration::from_millis(80));
        Self(pb)
    }

    pub fn set(&self, msg: impl Into<Cow<'static, str>>) {
        self.0.set_message(msg);
    }

    pub fn println(&self, line: impl AsRef<str>) {
        if self.0.is_hidden() {
            eprintln!("{}", line.as_ref());
        } else {
            self.0.println(line);
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.0.finish_and_clear();
    }
}

pub fn spin<T>(msg: impl Into<Cow<'static, str>>, f: impl FnOnce() -> T) -> T {
    let _spinner = Spinner::new(msg);
    f()
}

pub fn bytes(msg: impl Into<Cow<'static, str>>, total: u64) -> ProgressBar {
    let pb = ProgressBar::with_draw_target(Some(total), ProgressDrawTarget::stderr());
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan} {msg} [{bar:30.cyan/dim}] {bytes}/{total_bytes} {bytes_per_sec}",
        )
        .expect("valid progress template")
        .tick_chars(TICKS)
        .progress_chars("━━─"),
    );
    pb.set_message(msg);
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}
