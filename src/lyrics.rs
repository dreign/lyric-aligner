//! 歌词模块：TXT/LRC 解析、编码探测、文本归一化、LRC 导出、工程序列化。
//! 对应需求 FR-1.3~1.6、FR-2.x、FR-5.x。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// 对齐引擎等级（FR-3.1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlignEngine {
    /// L1：能量包络 + Onset 谷值 DP，零外部依赖。
    Fast,
    /// L2：VAD 语音段对齐（whisper.cpp 内置 Silero VAD）。
    Vad,
    /// L3：Whisper ASR 字级时间戳 + NW 文本对齐。
    Asr,
}

impl AlignEngine {
    pub const ALL: [AlignEngine; 3] = [AlignEngine::Fast, AlignEngine::Vad, AlignEngine::Asr];
    pub fn label(self) -> &'static str {
        match self {
            AlignEngine::Fast => "快速（能量）",
            AlignEngine::Vad => "增强（VAD）",
            AlignEngine::Asr => "精确（Whisper）",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            AlignEngine::Fast => "能量包络 + 谷值动态规划，几秒完成，零依赖",
            AlignEngine::Vad => "Silero VAD 提取人声段，抗伴奏干扰",
            AlignEngine::Asr => "Whisper 字级时间戳 + 文本对齐，最精准（需模型）",
        }
    }
}

/// 时间来源（用于 UI 标识与置信度解释）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeSource {
    /// 自动对齐产生。
    Auto,
    /// 人工打点/拖动/输入产生。
    Manual,
    /// 从已有 LRC 导入。
    Imported,
}

/// 逐字时间戳（增强 LRC）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WordStamp {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// 单行歌词及其时间轴。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LyricLine {
    /// 行号（从 1 开始）。
    pub index: usize,
    /// 原文（导出用，保持用户输入的标点/空格）。
    pub text: String,
    /// 归一化文本（对齐计算用）。
    pub norm: String,
    /// 起始时间（秒）。
    pub start: f64,
    /// 结束时间（秒）。
    pub end: f64,
    /// 置信度 0..1。
    pub confidence: f32,
    /// 逐字时间戳（ASR 模式填充）。
    pub words: Vec<WordStamp>,
    /// 时间来源。
    pub source: TimeSource,
}

impl LyricLine {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

/// 对齐结果（含诊断信息）。
#[derive(Clone, Debug)]
pub struct AlignResult {
    pub lines: Vec<LyricLine>,
    pub warnings: Vec<String>,
    pub engine: AlignEngine,
}

/// 工程文件（.lyricproj，JSON）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub audio_path: Option<PathBuf>,
    pub lyrics_path: Option<PathBuf>,
    pub lines: Vec<LyricLine>,
    pub engine: AlignEngine,
    pub duration: f64,
}

impl Project {
    pub const VERSION: u32 = 1;

    pub fn new() -> Self {
        Self {
            version: Self::VERSION,
            audio_path: None,
            lyrics_path: None,
            lines: Vec::new(),
            engine: AlignEngine::Fast,
            duration: 0.0,
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("工程序列化失败: {e}"))?;
        fs::write(path, json).map_err(|e| format!("工程保存失败: {e}"))
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let json = fs::read_to_string(path).map_err(|e| format!("工程读取失败: {e}"))?;
        let p: Project = serde_json::from_str(&json).map_err(|e| format!("工程解析失败: {e}"))?;
        if p.version > Self::VERSION {
            return Err(format!("工程版本 {} 高于当前支持的 {}", p.version, Self::VERSION));
        }
        Ok(p)
    }
}

/// 归一化文本：去标点、统一繁简（常见字）、全半角、大小写，供对齐计算使用。
/// 注意：导出仍用原文。
pub fn normalize_text(s: &str) -> String {
    s.chars()
        .filter_map(|c| {
            // 过滤空白与常见标点
            if c.is_whitespace() || is_punctuation(c) {
                return None;
            }
            // 全角 ASCII 转半角
            let c = if ('\u{FF01}'..='\u{FF5E}').contains(&c) {
                char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
            } else {
                c
            };
            // 常见繁体字映射（歌词高频字）
            let c = match c {
                '愛' => '爱', '時' => '时', '風' => '风', '花' => '花', '開' => '开',
                '這' => '这', '個' => '个', '來' => '来', '去' => '去', '說' => '说',
                '錯' => '错', '過' => '过', '還' => '还', '現' => '现', '給' => '给',
                '讓' => '让', '從' => '从', '會' => '会', '覺' => '觉', '得' => '得',
                '裡' => '里', '內' => '内', '為' => '为', '與' => '与', '裡' => '里',
                _ => c,
            };
            Some(c.to_lowercase().next().unwrap_or(c))
        })
        .collect()
}

fn is_punctuation(c: char) -> bool {
    matches!(c,
        '，' | '。' | '！' | '？' | '；' | '：' | '、' | '…' | '—' | '－' |
        ',' | '.' | '!' | '?' | ';' | ':' | '-' | '_' | '~' | '"' | '\'' |
        '（' | '）' | '【' | '】' | '「' | '」' | '《' | '》' | '(' | ')' |
        '[' | ']' | '{' | '}' | '〈' | '〉' | '·' | '♪' | '♫' | '*' | '#'
    )
}

/// 判断文本是否像 LRC（含时间标签）。
pub fn looks_like_lrc(text: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with('[') && t.len() > 6
            && t.as_bytes().get(1).map_or(false, |b| b.is_ascii_digit())
    })
}

/// 解析 LRC 文本为带时间的歌词行。
pub fn parse_lrc(text: &str) -> Vec<LyricLine> {
    let mut rows: Vec<(f64, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // 跳过元数据标签 [ar:...] [ti:...] [offset:...] 等
        if line.starts_with('[') && !line[1..].starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        // 提取行首所有 [mm:ss.xx] 标签，取第一个作为时间
        let mut time: Option<f64> = None;
        let mut content = line;
        while content.starts_with('[') {
            if let Some(end) = content.find(']') {
                let tag = &content[1..end];
                if let Some(t) = parse_lrc_time(tag) {
                    if time.is_none() {
                        time = Some(t);
                    }
                    content = &content[end + 1..];
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        if let Some(t) = time {
            let txt = content.trim().to_string();
            if !txt.is_empty() {
                rows.push((t, txt));
            }
        }
    }
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    rows.dedup_by(|a, b| (a.0 - b.0).abs() < 0.01 && a.1 == b.1);
    let n = rows.len();
    rows.into_iter()
        .enumerate()
        .map(|(i, (t, txt))| LyricLine {
            index: i + 1,
            norm: normalize_text(&txt),
            text: txt,
            start: t,
            end: if i + 1 < n { 0.0 } else { t + 3.0 }, // end 由后续 fix_line_ends 统一计算
            confidence: 1.0,
            words: Vec::new(),
            source: TimeSource::Imported,
        })
        .collect()
}

/// 解析 [mm:ss.xx] 或 [mm:ss:xx] 标签内容，返回秒。
fn parse_lrc_time(tag: &str) -> Option<f64> {
    let tag = tag.trim();
    // 支持 mm:ss.xx / mm:ss:xx / ss.xx
    let parts: Vec<&str> = tag.split(|c| c == ':' || c == '.').collect();
    if parts.len() < 2 {
        return None;
    }
    let min: f64 = parts[0].parse().ok()?;
    let sec: f64 = parts[1].parse().ok()?;
    let frac = if parts.len() >= 3 {
        let f: f64 = parts[2].parse().ok()?;
        // 两位小数按百分秒，一位按十分秒
        if parts[2].len() == 2 { f / 100.0 } else { f / 10.0 }
    } else {
        0.0
    };
    Some(min * 60.0 + sec + frac)
}

/// 过滤歌词中的元数据行（作词/作曲/歌词作者等）。
fn is_metadata_line(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() { return true; }
    let lower = t.to_lowercase();
    for kw in ["作词", "作曲", "编曲", "演唱", "歌词", "lyrics:", "lyric:", "作詞", "作曲:", " sung by", "composed by", "written by"] {
        if lower.contains(kw) && t.len() < 40 {
            return true;
        }
    }
    false
}

/// 解析纯文本歌词（一行一句）。
pub fn parse_plain(text: &str, filter_meta: bool) -> Vec<LyricLine> {
    text.lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .filter(|l| !filter_meta || !is_metadata_line(l))
        .enumerate()
        .map(|(i, txt)| LyricLine {
            index: i + 1,
            norm: normalize_text(&txt),
            text: txt,
            start: 0.0,
            end: 0.0,
            confidence: 0.0,
            words: Vec::new(),
            source: TimeSource::Auto,
        })
        .collect()
}

/// 用 BOM / 编码特征探测并解码歌词文件（FR-1.6）。
pub fn decode_lyrics_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("读取歌词文件失败: {e}"))?;
    decode_bytes(&bytes)
}

pub fn decode_bytes(bytes: &[u8]) -> Result<String, String> {
    // BOM 检测
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Ok(String::from_utf8_lossy(&bytes[3..]).into_owned());
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return Ok(String::from_utf16_lossy(&units));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return Ok(String::from_utf16_lossy(&units));
    }
    // 无 BOM：先试 UTF-8，失败则用 GB18030
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Ok(s.to_string());
    }
    let (cow, _, _) = encoding_rs::GB18030.decode(bytes);
    Ok(cow.into_owned())
}

/// 在同一目录查找与音频同名的歌词文件（FR-1.3）。
/// 候选策略：完全同名 > 去编号后缀 > 受限模糊匹配。
pub fn find_lyrics_candidates(audio: &Path) -> Vec<PathBuf> {
    let Some(dir) = audio.parent() else { return Vec::new(); };
    let Some(stem) = audio.file_stem().and_then(|s| s.to_str()) else { return Vec::new(); };
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new(); };

    let mut exact = Vec::new();
    let mut numbered = Vec::new();
    let mut fuzzy = Vec::new();
    let stem_lower = stem.to_lowercase();

    for e in entries.flatten() {
        let p = e.path();
        if !p.is_file() { continue; }
        let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
        if ext != "txt" && ext != "lrc" { continue; }
        let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
        if name == stem_lower {
            exact.push(p);
        } else {
            // 去编号后缀：(1) / [1] / -1 / _1
            let stripped = strip_trailing_number(&name);
            if stripped == stem_lower {
                numbered.push(p);
            } else if fuzzy_match(&stem_lower, &stripped) {
                fuzzy.push(p);
            }
        }
    }
    exact.extend(numbered);
    exact.extend(fuzzy);
    exact
}

fn strip_trailing_number(s: &str) -> String {
    let mut out = s.trim().to_string();
    for suffix in [" (1)", "[1]", "(1)", "-1", "_1", "（1）"] {
        if out.ends_with(suffix) {
            out.truncate(out.len() - suffix.len());
            break;
        }
    }
    out.trim().to_string()
}

fn fuzzy_match(a: &str, b: &str) -> bool {
    // 受限模糊：一个包含另一个，或编辑距离 <= 2
    if a.contains(b) || b.contains(a) { return true; }
    if a.len() > 20 || b.len() > 20 { return false; }
    levenshtein(a, b) <= 2
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (cur[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// 修正行结束时间：每行 end = 下一行 start，最后一行按平均时长或默认 3 秒。
pub fn fix_line_ends(lines: &mut [LyricLine], audio_duration: f64) {
    if lines.is_empty() { return; }
    for i in 0..lines.len().saturating_sub(1) {
        let next = lines[i + 1].start;
        lines[i].end = if next > lines[i].start { next } else { lines[i].start + 0.4 };
    }
    let last = lines.len() - 1;
    let avg = if lines.len() > 1 {
        lines.iter().take(lines.len() - 1).map(|l| l.duration()).sum::<f64>() / (lines.len() - 1) as f64
    } else {
        3.0
    };
    let avg = if avg.is_finite() && avg > 0.4 { avg.min(8.0) } else { 3.0 };
    lines[last].end = (lines[last].start + avg).min(audio_duration.max(lines[last].start + 0.4));
}

/// 全部歌词行整体前移／后移（FR-4.7）。
///
/// - `delta > 0` 后移、`delta < 0` 前移；整批使用同一偏移量以保持行间相对间隔。
/// - 前移被钳制到「首行起点不早于 0」，后移被钳制到「末行起点不晚于 `audio_duration`」
///   （`audio_duration <= 0` 表示未知长度，不限制后移）。
/// - 逐字时间戳（`WordStamp`）同步平移，`source` 标记为 `Manual`。
/// - 返回实际应用的偏移量；`0.0` 表示无变化（未修改任何行）。
pub fn shift_all_lines(lines: &mut [LyricLine], delta: f64, audio_duration: f64) -> f64 {
    if lines.is_empty() || delta.abs() < 1e-9 {
        return 0.0;
    }
    let min_start = lines.iter().map(|l| l.start).fold(f64::INFINITY, f64::min);
    let max_start = lines.iter().map(|l| l.start).fold(f64::NEG_INFINITY, f64::max);
    let lower = -min_start;
    let upper = if audio_duration > 0.0 {
        (audio_duration - max_start).max(0.0)
    } else {
        f64::INFINITY
    };
    let applied = delta.clamp(lower.min(0.0), upper.max(0.0));
    if applied.abs() < 1e-9 {
        return 0.0;
    }
    for l in lines.iter_mut() {
        l.start = (l.start + applied).max(0.0);
        l.end = (l.end + applied).max(0.0);
        for w in l.words.iter_mut() {
            w.start = (w.start + applied).max(0.0);
            w.end = (w.end + applied).max(0.0);
        }
        l.source = TimeSource::Manual;
    }
    applied
}

/// 导出标准 LRC（FR-5.1）。
pub fn to_lrc(lines: &[LyricLine], meta: Option<(&str, &str, &str)>, enhanced: bool) -> String {
    let mut out = String::new();
    if let Some((artist, title, album)) = meta {
        if !artist.is_empty() { out.push_str(&format!("[ar:{artist}]\n")); }
        if !title.is_empty() { out.push_str(&format!("[ti:{title}]\n")); }
        if !album.is_empty() { out.push_str(&format!("[al:{album}]\n")); }
    }
    out.push_str("[offset:0]\n");
    for line in lines {
        let tag = format_lrc_time(line.start);
        if enhanced && !line.words.is_empty() {
            out.push_str(&tag);
            for w in &line.words {
                out.push_str(&format!("<{}>{}", format_lrc_time(w.start), w.text));
            }
            out.push('\n');
        } else {
            out.push_str(&format!("{}{}\n", tag, line.text));
        }
    }
    out
}

/// 格式化为 [mm:ss.xx]。
pub fn format_lrc_time(sec: f64) -> String {
    let sec = sec.max(0.0);
    let m = (sec / 60.0) as u64;
    let s = sec - m as f64 * 60.0;
    format!("[{m:02}:{s:05.2}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_lrc_content() {
        let lrc = "[ti:鼓点演示]\r\n[ar:测试]\r\n[00:00.00]前奏\r\n[00:02.00]第一行\r\n";
        assert!(looks_like_lrc(lrc), "应识别为 LRC");
        let plain = "前奏\n第一行\n第二行\n";
        assert!(!looks_like_lrc(plain), "纯文本不应识别为 LRC");
        let lines = parse_lrc(lrc);
        assert_eq!(lines.len(), 2);
        assert!((lines[0].start - 0.0).abs() < 0.01);
        assert_eq!(lines[0].source, TimeSource::Imported);
    }

    #[test]
    fn parse_lrc_basic() {
        let text = "[ar:测试]\n[ti:歌曲]\n[00:12.34]第一行\n[00:16.78]第二行\n";
        let lines = parse_lrc(text);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "第一行");
        assert!((lines[0].start - 12.34).abs() < 0.01);
        assert_eq!(lines[0].source, TimeSource::Imported);
    }

    #[test]
    fn decode_gb18030() {
        // "歌词" 的 GB18030 编码
        let bytes = [0xB8, 0xE8, 0xB4, 0xCA];
        assert_eq!(decode_bytes(&bytes).unwrap(), "歌词");
    }

    #[test]
    fn normalize_removes_punctuation() {
        assert_eq!(normalize_text("你好，世界！"), "你好世界");
    }

    #[test]
    fn find_lyrics_prefers_exact() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("lyric_aligner_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let audio = dir.join("测试.mp3");
        fs::write(dir.join("测试.txt"), "第一行\n第二行").unwrap();
        fs::write(dir.join("测试 (1).txt"), "其他").unwrap();
        let cands = find_lyrics_candidates(&audio);
        assert!(!cands.is_empty());
        assert!(cands[0].file_stem().unwrap() == "测试");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn to_lrc_format() {
        let lines = vec![LyricLine {
            index: 1, text: "你好".into(), norm: "你好".into(),
            start: 65.5, end: 68.0, confidence: 1.0,
            words: vec![], source: TimeSource::Auto,
        }];
        let lrc = to_lrc(&lines, None, false);
        assert!(lrc.contains("[01:05.50]你好"));
    }

    fn sample_lines() -> Vec<LyricLine> {
        vec![
            LyricLine {
                index: 1, text: "一".into(), norm: "一".into(),
                start: 1.0, end: 3.0, confidence: 1.0,
                words: vec![WordStamp { text: "一".into(), start: 1.2, end: 1.6 }],
                source: TimeSource::Auto,
            },
            LyricLine {
                index: 2, text: "二".into(), norm: "二".into(),
                start: 3.0, end: 5.0, confidence: 1.0,
                words: vec![WordStamp { text: "二".into(), start: 3.2, end: 3.7 }],
                source: TimeSource::Auto,
            },
        ]
    }

    #[test]
    fn shift_all_moves_every_line_and_words() {
        let mut lines = sample_lines();
        let applied = shift_all_lines(&mut lines, 0.5, 100.0);
        assert!((applied - 0.5).abs() < 1e-6);
        // 每行起止都后移 0.5s，相对间隔保持不变
        assert!((lines[0].start - 1.5).abs() < 1e-6);
        assert!((lines[0].end - 3.5).abs() < 1e-6);
        assert!((lines[1].start - 3.5).abs() < 1e-6);
        assert!((lines[1].end - 5.5).abs() < 1e-6);
        assert!((lines[1].start - lines[0].start - 2.0).abs() < 1e-6, "行间相对间隔应不变");
        // 逐字时间戳同步平移
        assert!((lines[0].words[0].start - 1.7).abs() < 1e-6);
        assert!((lines[1].words[0].end - 4.2).abs() < 1e-6);
        assert!(lines.iter().all(|l| l.source == TimeSource::Manual));
    }

    #[test]
    fn shift_all_clamps_at_start() {
        let mut lines = sample_lines(); // 首行 start=1.0
        let applied = shift_all_lines(&mut lines, -10.0, 100.0);
        assert!((applied - (-1.0)).abs() < 1e-6, "前移应钳制到 -首行起点");
        assert!(lines[0].start >= 0.0);
        assert!((lines[0].start - 0.0).abs() < 1e-6);
        // 相对间隔仍保持
        assert!((lines[1].start - 2.0).abs() < 1e-6);
    }

    #[test]
    fn shift_all_clamps_at_end() {
        let mut lines = sample_lines(); // 末行 start=3.0
        let applied = shift_all_lines(&mut lines, 100.0, 10.0);
        assert!((applied - 7.0).abs() < 1e-6, "后移应钳制到 时长-末行起点");
        assert!(lines[1].start <= 10.0);
        assert!((lines[1].start - 10.0).abs() < 1e-6);
    }

    #[test]
    fn shift_all_no_change_returns_zero() {
        let mut lines = sample_lines();
        // 已在开头（首行 0s），再前移应无变化
        lines[0].start = 0.0;
        let before: Vec<(f64, f64)> = lines.iter().map(|l| (l.start, l.end)).collect();
        let applied = shift_all_lines(&mut lines, -1.0, 100.0);
        assert_eq!(applied, 0.0);
        let after: Vec<(f64, f64)> = lines.iter().map(|l| (l.start, l.end)).collect();
        assert_eq!(before, after, "无变化不应修改任何行");
    }
}
