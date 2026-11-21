//! 歌词自动对齐工具 LyricAligner 入口。

mod align;
mod app;
mod asr;
mod audio;
mod lyrics;
mod ui_style;

fn main() {
    app::run();
}
