//! 主界面：文件导入、波形时间轴、歌词列表、播放控制、打点/拖动/微调、撤销重做。
//! 对应需求 FR-4.x、FR-5.x、FR-6.x。

use crate::align::{align_asr_tokens, align_fast, align_vad_from_segments};
use crate::asr::{detect_vad_segments, transcribe, AsrConfig};
use crate::audio::{decode_from_bytes, load_audio, waveform_peaks, AudioData, Player};
use crate::lyrics::{
    decode_lyrics_file, find_lyrics_candidates, fix_line_ends, parse_lrc, parse_plain,
    shift_all_lines, to_lrc, AlignEngine, AlignResult, LyricLine, Project, TimeSource,
};
use eframe::egui;
use egui::{Color32, Pos2, Rect, RichText, Stroke, Vec2};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

/// 撤销操作快照。
#[derive(Clone)]
struct Snapshot {
    lines: Vec<LyricLine>,
}

/// 后台对齐任务消息。
enum AlignMsg {
    Progress(String),
    Done(Result<AlignResult, String>),
}

/// 应用状态。
struct App {
    // 数据
    audio: Option<AudioData>,
    audio_path: Option<PathBuf>,
    lyrics_path: Option<PathBuf>,
    lines: Vec<LyricLine>,
    durations: f64,
    engine: AlignEngine,
    asr_cfg: AsrConfig,

    // 播放器
    player: Option<Player>,
    playing: bool,
    volume: f32,
    seek_dragging: bool,

    // UI 状态
    selected: Option<usize>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    status: String,
    status_time: Instant,
    filter_low_conf: bool,
    show_enhanced: bool,
    filter_meta: bool,

    /// 整体平移的精确步长（秒），用于歌词列表的精确输入框。
    shift_amount: f64,

    // 时间轴交互
    timeline_drag: Option<TimelineDrag>,
    timeline_rect: Rect,

    // 异步任务
    align_rx: Option<mpsc::Receiver<AlignMsg>>,
    aligning: bool,
    align_progress: String,

    // 对话框
    pending_audio: Option<PathBuf>,
}

#[derive(Clone)]
enum TimelineDrag {
    /// 拖动整行（起点终点一起平移）。
    Move { row: usize, start_t: f64, anchor_x: f32 },
    /// 调整起点。
    ResizeStart { row: usize, anchor_x: f32 },
    /// 调整终点。
    ResizeEnd { row: usize, anchor_x: f32 },
    /// 拖动进度条。
    Seek,
}

impl App {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self {
            audio: None, audio_path: None, lyrics_path: None, lines: Vec::new(),
            durations: 0.0, engine: AlignEngine::Fast, asr_cfg: AsrConfig::detect_dev(),
            player: None, playing: false, volume: 0.8, seek_dragging: false,
            selected: None, undo: Vec::new(), redo: Vec::new(),
            status: "就绪：打开一个 MP3 开始".to_string(), status_time: Instant::now(),
            filter_low_conf: false, show_enhanced: false, filter_meta: true,
            shift_amount: 0.1,
            timeline_drag: None, timeline_rect: Rect::NOTHING,
            align_rx: None, aligning: false, align_progress: String::new(),
            pending_audio: None,
        };
        // 自动化验证开关：启动时加载指定音频与歌词并自动对齐。
        // 格式: MP3_AUTO_LOAD=<音频路径>|<歌词路径>[|FAST|VAD|ASR]
        if let Ok(spec) = std::env::var("MP3_AUTO_LOAD") {
            let parts: Vec<&str> = spec.split('|').collect();
            if parts.len() >= 2 {
                let audio = PathBuf::from(parts[0]);
                let lyrics = PathBuf::from(parts[1]);
                if parts.len() >= 3 {
                    app.engine = match parts[2] {
                        "VAD" => AlignEngine::Vad,
                        "ASR" => AlignEngine::Asr,
                        _ => AlignEngine::Fast,
                    };
                }
                app.load_audio_and_lyrics(audio);
                app.load_lyrics_file(lyrics);
                // 截图验证开关：加载后停留在界面，不触发对齐/退出。
                if std::env::var("MP3_SNAP").is_ok() {
                    return app;
                }
                let all_imported = app.lines.iter().all(|l| l.source == TimeSource::Imported);
                if all_imported {
                    // 自动化验证：可选地整体平移后再写出，验证 FR-4.7 批量平移。
                    if let Ok(shift) = std::env::var("MP3_AUTO_SHIFT") {
                        if let Ok(delta) = shift.parse::<f64>() {
                            app.shift_all(delta);
                        }
                    }
                    // 已有时间戳：直接写出结果，验证 LRC 解析路径
                    let dump = serde_json::json!({
                        "engine": "Imported", "status": app.status, "warnings": [],
                        "lines": app.lines.iter().map(|l| serde_json::json!({
                            "index": l.index, "text": l.text, "start": l.start,
                            "end": l.end, "confidence": l.confidence,
                            "source": format!("{:?}", l.source), "words": 0,
                        })).collect::<Vec<_>>(),
                    });
                    if let Ok(out) = std::env::var("MP3_AUTO_DUMP") {
                        let _ = std::fs::write(&out, serde_json::to_string_pretty(&dump).unwrap_or_default());
                    }
                    std::process::exit(0);
                }
                app.start_align();
            }
        }
        app
    }

    fn set_status(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.status_time = Instant::now();
    }

    // ---------- 文件加载 ----------

    fn open_audio_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("音频", &["mp3", "wav", "flac", "m4a", "ogg"])
            .pick_file()
        {
            self.load_audio_and_lyrics(path);
        }
    }

    fn open_lyrics_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("歌词", &["txt", "lrc"])
            .pick_file()
        {
            self.load_lyrics_file(path);
        }
    }

    fn load_audio_and_lyrics(&mut self, path: PathBuf) {
        match load_audio(&path) {
            Ok(audio) => {
                self.durations = audio.duration;
                self.audio = Some(audio);
                self.audio_path = Some(path.clone());
                self.lines.clear();
                self.selected = None;
                self.undo.clear();
                self.redo.clear();
                // 初始化播放器
                self.player = match Player::new() {
                    Ok(mut p) => {
                        if let Err(e) = p.load(&path, self.durations) {
                            self.set_status(format!("播放器加载失败: {e}"));
                        }
                        p.set_volume(self.volume);
                        Some(p)
                    }
                    Err(e) => {
                        self.set_status(format!("音频设备不可用: {e}"));
                        None
                    }
                };
                self.playing = false;

                // 自动发现同名歌词
                let cands = find_lyrics_candidates(&path);
                if cands.len() == 1 {
                    self.load_lyrics_file(cands[0].clone());
                } else if cands.len() > 1 {
                    self.pending_audio = Some(path);
                    self.set_status(format!("找到 {} 个歌词候选，请在歌词区选择", cands.len()));
                } else {
                    self.set_status("未找到同名 TXT/LRC 歌词，请手动指定");
                }
            }
            Err(e) => self.set_status(format!("打开音频失败: {e}")),
        }
    }

    fn load_lyrics_file(&mut self, path: PathBuf) {
        match decode_lyrics_file(&path) {
            Ok(text) => {
                let lines = if crate::lyrics::looks_like_lrc(&text) {
                    parse_lrc(&text)
                } else {
                    parse_plain(&text, self.filter_meta)
                };
                if lines.is_empty() {
                    self.set_status("歌词文件为空或没有有效行");
                    return;
                }
                self.lines = lines;
                self.lyrics_path = Some(path);
                self.selected = None;
                self.undo.clear();
                self.redo.clear();
                self.set_status(format!("已加载 {} 行歌词", self.lines.len()));
            }
            Err(e) => self.set_status(format!("读取歌词失败: {e}")),
        }
    }

    // ---------- 对齐 ----------

    fn start_align(&mut self) {
        if self.audio.is_none() {
            self.set_status("请先打开 MP3");
            return;
        }
        if self.lines.is_empty() {
            self.set_status("请先加载歌词");
            return;
        }
        // 已带时间的 LRC 无需重新对齐（除非用户明确想重新对齐）
        let all_imported = self.lines.iter().all(|l| l.source == TimeSource::Imported && l.start > 0.0);
        if all_imported {
            self.set_status("歌词已包含时间戳，无需对齐；可直接导出或手动微调");
            return;
        }
        if self.aligning {
            self.set_status("正在对齐中…");
            return;
        }

        let audio = self.audio.clone().unwrap();
        let lines = self.lines.clone();
        let engine = self.engine;
        let cfg = self.asr_cfg.clone();
        let (tx, rx) = mpsc::channel();
        self.align_rx = Some(rx);
        self.aligning = true;
        self.align_progress = "准备中…".to_string();

        std::thread::spawn(move || {
            let result = match engine {
                AlignEngine::Fast => {
                    let _ = tx.send(AlignMsg::Progress("L1 能量特征提取中…".to_string()));
                    Ok(align_fast(&audio, &lines))
                }
                AlignEngine::Vad => {
                    let _ = tx.send(AlignMsg::Progress("L2 VAD 语音段检测中…".to_string()));
                    if let Err(e) = cfg.is_ready() {
                        let _ = tx.send(AlignMsg::Progress("VAD 运行时缺失，降级到 L1".to_string()));
                        Ok(align_fast(&audio, &lines))
                    } else {
                        match detect_vad_segments(&audio, &cfg, None) {
                            Ok(segs) => Ok(align_vad_from_segments(&audio, &lines, &segs)),
                            Err(e) => {
                                let _ = tx.send(AlignMsg::Progress(format!("VAD 失败({e})，降级到 L1")));
                                Ok(align_fast(&audio, &lines))
                            }
                        }
                    }
                }
                AlignEngine::Asr => {
                    let _ = tx.send(AlignMsg::Progress("L3 Whisper 识别中（可能需要 1–3 分钟）…".to_string()));
                    match transcribe(&audio, &cfg) {
                        Ok(tokens) => {
                            let _ = tx.send(AlignMsg::Progress("ASR 完成，正在文本对齐…".to_string()));
                            Ok(align_asr_tokens(&lines, &tokens, audio.duration))
                        }
                        Err(e) => {
                            let _ = tx.send(AlignMsg::Progress(format!("ASR 失败({e})，降级到 L1")));
                            Ok(align_fast(&audio, &lines))
                        }
                    }
                }
            };
            let _ = tx.send(AlignMsg::Done(result));
        });
    }

    fn poll_align(&mut self) {
        let mut done: Option<Result<AlignResult, String>> = None;
        if let Some(rx) = &self.align_rx {
            loop {
                match rx.try_recv() {
                    Ok(AlignMsg::Progress(s)) => self.align_progress = s,
                    Ok(AlignMsg::Done(r)) => { done = Some(r); break; }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => break,
                }
            }
        }
        if let Some(result) = done {
            self.aligning = false;
            self.align_rx = None;
            match result {
                Ok(r) => {
                    self.push_undo();
                    self.lines = r.lines;
                    self.selected = None;
                    let warn = if r.warnings.is_empty() { String::new() } else {
                        format!("｜{}", r.warnings.join("；"))
                    };
                    self.set_status(format!("对齐完成（{}）｜{} 行{warn}", r.engine.label(), self.lines.len()));

                    // 自动化验证：对齐完成后写出结果 JSON 并退出。
                    if let Ok(out) = std::env::var("MP3_AUTO_DUMP") {
                    let dump = serde_json::json!({
                        "engine": format!("{:?}", r.engine), "warnings": r.warnings,
                            "lines": self.lines.iter().map(|l| serde_json::json!({
                                "index": l.index, "text": l.text, "start": l.start,
                                "end": l.end, "confidence": l.confidence,
                                "source": format!("{:?}", l.source),
                                "words": l.words.len(),
                            })).collect::<Vec<_>>(),
                        });
                        let _ = std::fs::write(&out, serde_json::to_string_pretty(&dump).unwrap_or_default());
                        std::process::exit(0);
                    }
                }
                Err(e) => self.set_status(format!("对齐失败: {e}")),
            }
        }
    }

    // ---------- 撤销/重做 ----------

    fn push_undo(&mut self) {
        self.undo.push(Snapshot { lines: self.lines.clone() });
        if self.undo.len() > 50 { self.undo.remove(0); }
        self.redo.clear();
    }

    fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(Snapshot { lines: self.lines.clone() });
            self.lines = s.lines;
            self.set_status("已撤销");
        }
    }

    fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(Snapshot { lines: self.lines.clone() });
            self.lines = s.lines;
            self.set_status("已重做");
        }
    }

    // ---------- 导出 ----------

    fn export_lrc(&mut self, enhanced: bool) {
        if self.lines.is_empty() {
            self.set_status("没有歌词可导出");
            return;
        }
        let default_name = self.audio_path.as_ref()
            .and_then(|p| p.file_stem().and_then(|s| s.to_str()))
            .map(|s| format!("{s}.lrc"))
            .unwrap_or_else(|| "歌词.lrc".to_string());
        let mut dialog = rfd::FileDialog::new()
            .set_file_name(&default_name)
            .add_filter("LRC 歌词", &["lrc"]);
        if let Some(dir) = self.audio_path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.save_file() {
            let meta = self.read_id3_meta();
            let m = meta.as_ref().map(|(a, t, al)| (a.as_str(), t.as_str(), al.as_str()));
            let content = to_lrc(&self.lines, m, enhanced);
            match std::fs::write(&path, content) {
                Ok(()) => self.set_status(format!("已导出: {}", path.display())),
                Err(e) => self.set_status(format!("导出失败: {e}")),
            }
        }
    }

    /// 简单读取 ID3v1 元数据（文件末尾 128 字节）；不依赖额外 crate。
    fn read_id3_meta(&self) -> Option<(String, String, String)> {
        let path = self.audio_path.as_ref()?;
        let data = std::fs::read(path).ok()?;
        if data.len() < 128 { return None; }
        let tail = &data[data.len() - 128..];
        if &tail[..3] != b"TAG" { return None; }
        let trim = |b: &[u8]| -> String {
            String::from_utf8_lossy(b).trim_matches(char::from(0)).trim().to_string()
        };
        Some((trim(&tail[3..33]), trim(&tail[33..63]), trim(&tail[63..93])))
    }

    fn save_project(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name("歌词对齐.lyricproj")
            .add_filter("歌词对齐工程", &["lyricproj"])
            .save_file()
        {
            let mut p = Project::new();
            p.audio_path = self.audio_path.clone();
            p.lyrics_path = self.lyrics_path.clone();
            p.lines = self.lines.clone();
            p.engine = self.engine;
            p.duration = self.durations;
            match p.save(&path) {
                Ok(()) => self.set_status(format!("工程已保存: {}", path.display())),
                Err(e) => self.set_status(format!("工程保存失败: {e}")),
            }
        }
    }

    fn load_project(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("歌词对齐工程", &["lyricproj"])
            .pick_file()
        {
            match Project::load(&path) {
                Ok(p) => {
                    self.engine = p.engine;
                    self.lines = p.lines;
                    self.durations = p.duration;
                    self.audio_path = p.audio_path.clone();
                    self.lyrics_path = p.lyrics_path.clone();
                    self.selected = None;
                    self.undo.clear();
                    self.redo.clear();
                    if let Some(ap) = &p.audio_path {
                        if ap.exists() {
                            if let Ok(audio) = load_audio(ap) {
                                self.durations = audio.duration;
                                self.audio = Some(audio);
                                self.player = Player::new().ok();
                                if let Some(pl) = &mut self.player { let _ = pl.load(ap, self.durations); }
                            }
                        }
                    }
                    self.set_status(format!("工程已加载: {} 行", self.lines.len()));
                }
                Err(e) => self.set_status(format!("工程加载失败: {e}")),
            }
        }
    }

    // ---------- 播放控制 ----------

    fn toggle_play(&mut self) {
        let Some(player) = &mut self.player else {
            self.set_status("请先打开 MP3");
            return;
        };
        if player.is_paused() {
            player.play();
            self.playing = true;
        } else {
            player.pause();
            self.playing = false;
        }
    }

    fn seek_to(&mut self, t: f64) {
        let Some(path) = self.audio_path.clone() else { return; };
        let t = t.clamp(0.0, self.durations);
        if let Some(player) = &mut self.player {
            match player.seek(&path, t) {
                Ok(()) => {
                    player.set_volume(self.volume);
                    if self.playing { player.play(); }
                }
                Err(e) => self.set_status(format!("跳转失败: {e}")),
            }
        }
    }

    fn current_position(&self) -> f64 {
        self.player.as_ref().map(|p| p.position()).unwrap_or(0.0)
    }

    // ---------- 时间编辑 ----------

    fn mark_current_as_start(&mut self) {
        let Some(idx) = self.selected else {
            self.set_status("请先选中一行歌词");
            return;
        };
        self.push_undo();
        let t = self.current_position();
        if idx < self.lines.len() {
            self.lines[idx].start = t;
            self.lines[idx].source = TimeSource::Manual;
            fix_line_ends(&mut self.lines, self.durations);
            self.set_status(format!("第 {} 行起点设为 {t:.2}s", self.lines[idx].index));
        }
    }

    fn nudge_selected(&mut self, delta: f64, all_selected: bool) {
        if self.lines.is_empty() { return; }
        self.push_undo();
        if all_selected {
            // 简化：有 filter 时平移所有低置信行，否则平移所有行
            if self.filter_low_conf {
                for l in &mut self.lines {
                    if l.confidence < 0.45 {
                        l.start = (l.start + delta).max(0.0);
                        l.end = (l.end + delta).max(0.0);
                        l.source = TimeSource::Manual;
                    }
                }
            } else {
                for l in &mut self.lines {
                    l.start = (l.start + delta).max(0.0);
                    l.end = (l.end + delta).max(0.0);
                    l.source = TimeSource::Manual;
                }
            }
        } else if let Some(idx) = self.selected {
            if idx < self.lines.len() {
                let l = &mut self.lines[idx];
                l.start = (l.start + delta).max(0.0);
                l.end = (l.end + delta).max(0.0);
                l.source = TimeSource::Manual;
            }
        }
        fix_line_ends(&mut self.lines, self.durations);
    }

    /// 全部歌词行整体前移／后移。
    /// delta > 0 后移、delta < 0 前移；整批使用同一偏移量以保持行间相对间隔。
    /// 平移量会被钳制在「首行不早于 0、末行不晚于音频时长」范围内；
    /// 逐字时间戳（增强 LRC）同步平移。返回实际应用的偏移量（0 表示未变化）。
    fn shift_all(&mut self, delta: f64) -> f64 {
        if self.lines.is_empty() || delta.abs() < 1e-9 {
            return 0.0;
        }
        // 先判断实际可应用量（钳制到音频边界），无变化则不记历史。
        let probe = shift_all_lines(&mut self.lines.clone(), delta, self.durations);
        if probe.abs() < 1e-9 {
            self.set_status(if delta < 0.0 { "已到开头，无法继续前移" } else { "已到结尾，无法继续后移" });
            return 0.0;
        }
        let clamped = (probe - delta).abs() > 1e-9;
        self.push_undo(); // 快照必须在修改之前
        let applied = shift_all_lines(&mut self.lines, delta, self.durations);
        if clamped {
            self.set_status("已钳制到音频边界");
        }
        self.set_status(format!("全部 {} 行整体{}{:.2}s",
            self.lines.len(),
            if applied > 0.0 { "后移 " } else { "前移 " },
            applied.abs()));
        applied
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_align();
        ctx.request_repaint(); // 播放光标需要持续刷新

        // 顶栏
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("歌词自动对齐");
                ui.separator();
                if ui.button("打开 MP3").clicked() { self.open_audio_dialog(); }
                if ui.button("打开歌词").clicked() { self.open_lyrics_dialog(); }
                ui.separator();
                ui.label("引擎:");
                for e in AlignEngine::ALL {
                    let selected = self.engine == e;
                    if ui.selectable_label(selected, e.label()).clicked() {
                        self.engine = e;
                    }
                }
                ui.separator();
                let align_text = if self.aligning { "对齐中…".to_string() } else { "开始对齐".to_string() };
                if ui.button(RichText::new(align_text).strong()).clicked() && !self.aligning {
                    self.start_align();
                }
                ui.separator();
                if ui.button("导出 LRC").clicked() { self.export_lrc(false); }
                if ui.button("导出逐字 LRC").clicked() { self.export_lrc(true); }
            });
        });

        // 底栏：播放控制 + 状态
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let play_text = if self.playing { "⏸ 暂停" } else { "▶ 播放" };
                if ui.button(play_text).clicked() { self.toggle_play(); }
                let pos = self.current_position();
                ui.label(format!("{:02}:{:05.2} / {:02}:{:05.2}",
                    (pos / 60.0) as u64, pos % 60.0,
                    (self.durations / 60.0) as u64, self.durations % 60.0));

                // 进度条（自绘，支持拖拽）
                let (rect, resp) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width() - 140.0, 22.0), egui::Sense::click_and_drag());
                self.draw_progress(ui, rect, &resp, pos);
                if resp.clicked() || resp.drag_started() { self.seek_dragging = true; }
                if resp.dragged() {
                    let x = resp.interact_pointer_pos().map(|p| p.x).unwrap_or(rect.left());
                    let t = ((x - rect.left()) / rect.width().max(1.0)) as f64 * self.durations;
                    self.seek_to(t);
                }
                if resp.drag_released() { self.seek_dragging = false; }

                ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).text("音量").show_value(false));
                if let Some(p) = &mut self.player { p.set_volume(self.volume); }
            });
            ui.horizontal(|ui| {
                if self.aligning {
                    ui.spinner();
                    ui.label(&self.align_progress);
                } else {
                    ui.label(&self.status);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("工程").clicked() {
                        // 简易菜单：保存/加载
                    }
                    if ui.small_button("保存工程").clicked() { self.save_project(); }
                    if ui.small_button("加载工程").clicked() { self.load_project(); }
                });
            });
        });

        // 主体：左侧歌词列表 + 右侧时间轴
        egui::SidePanel::left("lyrics_list").default_width(280.0).show(ctx, |ui| {
            self.draw_lyrics_list(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.draw_timeline(ui, ctx);
        });
    }
}

impl App {
    fn draw_progress(&self, ui: &mut egui::Ui, rect: Rect, resp: &egui::Response, pos: f64) {
        let p = ui.painter();
        let h = rect.height().max(14.0);
        let half = h * 0.5;
        // 未播放轨道：深凹槽 + 清楚描边
        p.rect_filled(rect, half, Color32::from_gray(38));
        p.rect_stroke(rect, half, Stroke::new(1.0, Color32::from_gray(90)));
        if self.durations > 0.0 {
            let filled = rect.width() * (pos / self.durations).clamp(0.0, 1.0) as f32;
            let fill_rect = Rect::from_min_size(rect.min, Vec2::new(filled, rect.height()));
            // 已播放：明亮蓝（不压暗）+ 顶部高光
            p.rect_filled(fill_rect, half, Color32::from_rgb(96, 178, 255));
            let gloss = Rect::from_min_max(
                Pos2::new(rect.left(), rect.top() + 1.0),
                Pos2::new(rect.left() + filled, rect.top() + h * 0.38),
            );
            p.rect_filled(gloss, half * 0.5, Color32::from_white_alpha(60));
            let knob = Pos2::new(rect.left() + filled, rect.center().y);
            p.circle_filled(knob, 8.0, Color32::from_rgb(96, 178, 255));
            p.circle_stroke(knob, 8.0, Stroke::new(2.0, Color32::WHITE));
            p.circle_filled(knob, 3.0, Color32::WHITE);
        }
        if resp.hovered() || resp.dragged() {
            p.rect_stroke(rect.expand(2.0), half + 2.0,
                Stroke::new(1.5, Color32::from_rgb(120, 200, 255)));
        }
    }

    fn draw_lyrics_list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("歌词");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("↑").clicked() {
                    if let Some(i) = self.selected { if i > 0 {
                        self.push_undo();
                        self.lines.swap(i - 1, i);
                        self.selected = Some(i - 1);
                        renumber(&mut self.lines);
                    }}
                }
                if ui.small_button("↓").clicked() {
                    if let Some(i) = self.selected { if i + 1 < self.lines.len() {
                        self.push_undo();
                        self.lines.swap(i, i + 1);
                        self.selected = Some(i + 1);
                        renumber(&mut self.lines);
                    }}
                }
            });
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.filter_low_conf, "只看低置信");
            ui.checkbox(&mut self.filter_meta, "过滤元数据");
        });
        ui.horizontal(|ui| {
            if ui.small_button("撤销").clicked() { self.undo(); }
            if ui.small_button("重做").clicked() { self.redo(); }
            if ui.small_button("打点 T").clicked() { self.mark_current_as_start(); }
        });
        // 整体平移（所有行一起前移/后移，保持相对间隔）
        ui.horizontal(|ui| {
            ui.label("整体:");
            if ui.small_button("◀◀ 0.5").clicked() { self.shift_all(-0.5); }
            if ui.small_button("◀ 0.1").clicked() { self.shift_all(-0.1); }
            ui.add(egui::DragValue::new(&mut self.shift_amount)
                .speed(0.01).range(0.01..=60.0).suffix(" s"));
            if ui.small_button("0.1 ▶").clicked() { self.shift_all(0.1); }
            if ui.small_button("0.5 ▶▶").clicked() { self.shift_all(0.5); }
        });
        ui.horizontal(|ui| {
            if ui.small_button(format!("⇤ 整体前移 {:.2}s", self.shift_amount)).clicked() {
                self.shift_all(-self.shift_amount);
            }
            if ui.small_button(format!("整体后移 {:.2}s ⇥", self.shift_amount)).clicked() {
                self.shift_all(self.shift_amount);
            }
        });
        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            let pos = self.current_position();
            for i in 0..self.lines.len() {
                // 先复制需要的字段，避免后续 self.seek_to 的可变借用冲突
                let (conf, start, end, index, text) = {
                    let line = &self.lines[i];
                    (line.confidence, line.start, line.end, line.index, line.text.clone())
                };
                if self.filter_low_conf && conf >= 0.45 { continue; }
                let is_current = pos >= start && pos < end;
                let is_selected = self.selected == Some(i);

                let label = format!("{:02} [{:05.2}] {}", index, start, text);
                let mut text_style = RichText::new(label).monospace();
                if is_selected { text_style = text_style.color(Color32::from_rgb(120, 200, 255)).strong(); }
                else if is_current { text_style = text_style.color(Color32::YELLOW).strong(); }
                else if conf < 0.45 { text_style = text_style.color(Color32::from_rgb(255, 180, 80)); }

                let resp = ui.selectable_label(is_selected, text_style);
                if resp.clicked() { self.selected = Some(i); }
                if resp.double_clicked() {
                    self.selected = Some(i);
                    self.seek_to(start);
                }
                // 悬停提示
                resp.on_hover_text(format!("起点 {:.2}s / 终点 {:.2}s / 置信 {:.0}%",
                    start, end, conf * 100.0));
            }
        });
    }

    fn draw_timeline(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.heading("时间轴");
            ui.label(format!("引擎 {}｜{} 行｜低置信 {} 行",
                self.engine.label(), self.lines.len(),
                self.lines.iter().filter(|l| l.confidence < 0.45).count()));
        });
        ui.label(concat!(
            "提示：拖动色块移动歌词行，拖左右边缘调整起止；选中后按 T 打点，方向键微调；",
            "Ctrl+方向键整体前移/后移；左侧「整体」按钮可批量平移全部歌词"));
        ui.separator();

        // 快捷键（需在 timeline 区域捕获焦点）
        ui.input(|i| {
            if i.key_pressed(egui::Key::T) { self.mark_current_as_start(); }
            if i.key_pressed(egui::Key::ArrowLeft) {
                let d = if i.modifiers.shift { 0.5 } else { 0.05 };
                if i.modifiers.ctrl {
                    // Ctrl+← ：全部歌词整体前移（保持相对间隔）
                    self.shift_all(-d);
                } else {
                    self.nudge_selected(-d, false);
                }
            }
            if i.key_pressed(egui::Key::ArrowRight) {
                let d = if i.modifiers.shift { 0.5 } else { 0.05 };
                if i.modifiers.ctrl {
                    // Ctrl+→ ：全部歌词整体后移（保持相对间隔）
                    self.shift_all(d);
                } else {
                    self.nudge_selected(d, false);
                }
            }
            if i.modifiers.ctrl && i.key_pressed(egui::Key::Z) { self.undo(); }
            if i.modifiers.ctrl && i.key_pressed(egui::Key::Y) { self.redo(); }
            if i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace) {
                if let Some(idx) = self.selected {
                    if idx < self.lines.len() {
                        self.push_undo();
                        self.lines.remove(idx);
                        renumber(&mut self.lines);
                        self.selected = None;
                    }
                }
            }
            if i.key_pressed(egui::Key::Space) { self.toggle_play(); }
        });

        let available = ui.available_size();
        let timeline_h = ((available.y - 120.0).max(120.0)).min(520.0);
        let (rect, resp) = ui.allocate_exact_size(
            Vec2::new(available.x - 8.0, timeline_h), egui::Sense::click_and_drag());
        self.timeline_rect = rect;
        let p = ui.painter();

        // 背景
        p.rect_filled(rect, 4.0, Color32::from_gray(22));
        p.rect_stroke(rect, 4.0, Stroke::new(1.0, Color32::from_gray(50)));

        // 时间刻度
        let duration = self.durations.max(1.0);
        let tick_step = if duration > 240.0 { 30.0 } else if duration > 90.0 { 15.0 } else { 5.0 };
        let mut t = 0.0;
        while t <= duration {
            let x = rect.left() + (t / duration) as f32 * rect.width();
            p.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(1.0, Color32::from_gray(40)));
            p.text(Pos2::new(x + 2.0, rect.top() + 2.0), egui::Align2::LEFT_TOP,
                &format!("{t:.0}s"), egui::FontId::proportional(10.0), Color32::from_gray(120));
            t += tick_step;
        }

        // 波形（背景层）：以面板垂直中心为中线，半高限制在面板内并上下留边距，
        // 避免时间轴较高时波峰溢出面板顶部（此前中线固定 top+44 会随高度上挤）。
        if let Some(audio) = &self.audio {
            let peaks = waveform_peaks(audio, rect.width().max(1.0) as usize);
            let mid = rect.center().y;
            let half = (rect.height() * 0.5 - 24.0).max(10.0);
            for (i, (min, max)) in peaks.iter().enumerate() {
                let x = rect.left() + i as f32;
                if x > rect.right() { break; }
                let y1 = mid - max * half;
                let y2 = mid - min * half;
                p.line_segment([Pos2::new(x, y1), Pos2::new(x, y2)],
                    Stroke::new(1.0, Color32::from_rgba_premultiplied(90, 130, 180, 180)));
            }
        }

        // 歌词行块
        let row_h = 20.0;
        let top = rect.top() + 14.0;
        for (i, line) in self.lines.iter().enumerate() {
            if self.filter_low_conf && line.confidence >= 0.45 { continue; }
            let x1 = rect.left() + (line.start / duration) as f32 * rect.width();
            let x2 = rect.left() + (line.end / duration) as f32 * rect.width();
            let y = top + i as f32 * row_h;
            if y > rect.bottom() - 4.0 { break; }
            let row_rect = Rect::from_min_size(Pos2::new(x1, y), Vec2::new((x2 - x1).max(4.0), row_h - 3.0));
            let is_sel = self.selected == Some(i);
            let base = if line.confidence < 0.45 {
                Color32::from_rgb(180, 120, 40)
            } else {
                Color32::from_rgb(50, 110, 170)
            };
            let col = if is_sel { Color32::from_rgb(90, 180, 240) } else { base };
            p.rect_filled(row_rect, 2.0, col);
            if is_sel {
                p.rect_stroke(row_rect, 2.0, Stroke::new(2.0, Color32::WHITE));
            }
            // 文本标签（截取前几个字）
            if row_rect.width() > 20.0 {
                p.text(Pos2::new(row_rect.left() + 3.0, row_rect.center().y),
                    egui::Align2::LEFT_CENTER, &line.text,
                    egui::FontId::proportional(11.0), Color32::WHITE);
            }
            // 左右边缘手柄提示
            if is_sel {
                p.rect_filled(Rect::from_min_size(row_rect.min, Vec2::new(6.0, row_rect.height())),
                    2.0, Color32::from_rgb(220, 230, 240));
                p.rect_filled(Rect::from_min_size(Pos2::new(row_rect.right() - 6.0, row_rect.top()),
                    Vec2::new(6.0, row_rect.height())), 2.0, Color32::from_rgb(220, 230, 240));
            }
        }

        // 播放光标
        let pos = self.current_position();
        let cx = rect.left() + (pos / duration) as f32 * rect.width();
        p.line_segment([Pos2::new(cx, rect.top()), Pos2::new(cx, rect.bottom())],
            Stroke::new(2.0, Color32::from_rgb(255, 80, 80)));

        // 时间轴交互
        if resp.drag_started() || resp.clicked() {
            let pointer = resp.interact_pointer_pos().unwrap_or(Pos2::ZERO);
            if rect.contains(pointer) {
                // 判断是否点中某行块
                let mut hit: Option<(usize, TimelineDrag)> = None;
                for (i, line) in self.lines.iter().enumerate() {
                    if self.filter_low_conf && line.confidence >= 0.45 { continue; }
                    let x1 = rect.left() + (line.start / duration) as f32 * rect.width();
                    let x2 = rect.left() + (line.end / duration) as f32 * rect.width();
                    let y = top + i as f32 * row_h;
                    if y > rect.bottom() - 4.0 { break; }
                    let row_rect = Rect::from_min_size(Pos2::new(x1, y),
                        Vec2::new((x2 - x1).max(4.0), row_h - 3.0));
                    if row_rect.contains(pointer) {
                        let kind = if (pointer.x - x1).abs() < 7.0 {
                            TimelineDrag::ResizeStart { row: i, anchor_x: pointer.x }
                        } else if (x2 - pointer.x).abs() < 7.0 {
                            TimelineDrag::ResizeEnd { row: i, anchor_x: pointer.x }
                        } else {
                            TimelineDrag::Move { row: i, start_t: line.start, anchor_x: pointer.x }
                        };
                        hit = Some((i, kind));
                        break;
                    }
                }
                if let Some((i, kind)) = hit {
                    if self.selected != Some(i) { self.push_undo(); }
                    self.selected = Some(i);
                    self.timeline_drag = Some(kind);
                } else {
                    // 空白处点击：移动播放光标
                    let t = ((pointer.x - rect.left()) / rect.width().max(1.0)) as f64 * duration;
                    self.seek_to(t);
                    self.timeline_drag = Some(TimelineDrag::Seek);
                }
            }
        }
        if resp.dragged() {
            let pointer = resp.interact_pointer_pos().unwrap_or(Pos2::ZERO);
            if let Some(drag) = self.timeline_drag.clone() {
                match drag {
                    TimelineDrag::Move { row, start_t, anchor_x } => {
                        let dt = ((pointer.x - anchor_x) / rect.width().max(1.0)) as f64 * duration;
                        if row < self.lines.len() {
                            let new_start = (start_t + dt).max(0.0);
                            let old_start = self.lines[row].start;
                            let dur = self.lines[row].duration();
                            self.lines[row].start = new_start;
                            self.lines[row].end = new_start + dur;
                            self.lines[row].source = TimeSource::Manual;
                            if (new_start - old_start).abs() > 0.001 {
                                fix_line_ends(&mut self.lines, self.durations);
                            }
                        }
                    }
                    TimelineDrag::ResizeStart { row, .. } => {
                        if row < self.lines.len() {
                            let t = ((pointer.x - rect.left()) / rect.width().max(1.0)) as f64 * duration;
                            self.lines[row].start = t.clamp(0.0, self.lines[row].end - 0.2);
                            self.lines[row].source = TimeSource::Manual;
                        }
                    }
                    TimelineDrag::ResizeEnd { row, .. } => {
                        if row < self.lines.len() {
                            let t = ((pointer.x - rect.left()) / rect.width().max(1.0)) as f64 * duration;
                            self.lines[row].end = t.max(self.lines[row].start + 0.2);
                            self.lines[row].source = TimeSource::Manual;
                        }
                    }
                    TimelineDrag::Seek => {
                        let t = ((pointer.x - rect.left()) / rect.width().max(1.0)) as f64 * duration;
                        self.seek_to(t);
                    }
                }
            }
        }
        if resp.drag_released() {
            self.timeline_drag = None;
        }

        // 选中的行详情 + 精确输入
        ui.separator();
        if let Some(idx) = self.selected {
            if idx < self.lines.len() {
                let index = self.lines[idx].index;
                let confidence = self.lines[idx].confidence;
                {
                    let line = &mut self.lines[idx];
                    ui.horizontal(|ui| {
                        ui.label(format!("第 {index} 行"));
                        ui.label("起点:");
                        let dur = self.durations.max(1.0);
                        ui.add(egui::DragValue::new(&mut line.start)
                            .speed(0.01).range(0.0..=dur).suffix(" s"));
                        ui.label("终点:");
                        ui.add(egui::DragValue::new(&mut line.end)
                            .speed(0.01).range(0.0..=dur).suffix(" s"));
                        ui.label(format!("置信 {:.0}%", confidence * 100.0));
                    });
                }
                // 按钮操作收集后统一执行，避免与 line 借用冲突
                let mut nudge: Option<f64> = None;
                let mut mark = false;
                ui.horizontal(|ui| {
                    if ui.small_button("◀ -0.5").clicked() { nudge = Some(-0.5); }
                    if ui.small_button("◀ -0.05").clicked() { nudge = Some(-0.05); }
                    if ui.small_button("T 打点").clicked() { mark = true; }
                    if ui.small_button("+0.05 ▶").clicked() { nudge = Some(0.05); }
                    if ui.small_button("+0.5 ▶").clicked() { nudge = Some(0.5); }
                });
                if let Some(d) = nudge { self.nudge_selected(d, false); }
                if mark { self.mark_current_as_start(); }
                ui.label(&self.lines[idx].text);
            }
        } else {
            ui.label("选中一行歌词后可精确调整时间");
        }

        // 强制重绘（光标移动）
        if self.playing {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}

fn renumber(lines: &mut [LyricLine]) {
    for (i, l) in lines.iter_mut().enumerate() {
        l.index = i + 1;
    }
}

pub fn run() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("歌词自动对齐工具"),
        ..Default::default()
    };
    eframe::run_native(
        "歌词自动对齐工具",
        options,
        Box::new(|cc| {
            // 注入中文字体，避免方块字
            if let Some(font_data) = std::fs::read("C:/Windows/Fonts/simhei.ttf").ok() {
                let mut fonts = egui::FontDefinitions::default();
                fonts.font_data.insert("simhei".to_string(),
                    egui::FontData::from_owned(font_data));
                fonts.families.entry(egui::FontFamily::Proportional)
                    .or_default().insert(0, "simhei".to_string());
                fonts.families.entry(egui::FontFamily::Monospace)
                    .or_default().insert(0, "simhei".to_string());
                cc.egui_ctx.set_fonts(fonts);
            }
            Ok(Box::new(App::new(cc)))
        }),
    ).expect("eframe 运行失败");
}
