//! ASR 模块：调用 whisper.cpp CLI，解析 token 时间戳；VAD 语音段检测。
//! 对应需求 FR-3.4、FR-3.5、FR-6.1。

use crate::align::{pcm16, to_wav_16k};
use crate::audio::AudioData;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// ASR 运行时配置。
#[derive(Clone, Debug)]
pub struct AsrConfig {
    pub cli_path: PathBuf,
    pub model_path: PathBuf,
    pub language: String,
    pub threads: usize,
    pub work_dir: PathBuf,
}

impl AsrConfig {
    /// 默认配置：相对可执行文件目录的 tools/whisper 与 models。
    pub fn detect() -> Self {
        let exe = std::env::current_exe().unwrap_or_default();
        let base = exe.parent().unwrap_or(Path::new("."));
        Self {
            cli_path: base.join("tools/whisper/Release/whisper-cli.exe"),
            model_path: base.join("models/ggml-large-v3-q5_0.bin"),
            language: "zh".to_string(),
            threads: 4,
            work_dir: base.join(".work"),
        }
    }

    /// 开发态：当前工作目录探测（cargo run 时 exe 在 target/debug）。
    pub fn detect_dev() -> Self {
        let cwd = std::env::current_dir().unwrap_or_default();
        let prod = Self::detect();
        if prod.model_path.exists() {
            return prod;
        }
        Self {
            cli_path: cwd.join("tools/whisper/Release/whisper-cli.exe"),
            model_path: cwd.join("models/ggml-large-v3-q5_0.bin"),
            language: "zh".to_string(),
            threads: 4,
            work_dir: cwd.join(".work"),
        }
    }

    pub fn is_ready(&self) -> Result<(), String> {
        if !self.cli_path.exists() {
            return Err(format!("未找到 whisper-cli: {}", self.cli_path.display()));
        }
        if !self.model_path.exists() {
            return Err(format!("未找到 Whisper 模型: {}", self.model_path.display()));
        }
        Ok(())
    }
}

/// whisper.cpp -ojf 输出的 JSON 结构。
#[derive(Debug, Deserialize)]
struct WhisperJson {
    transcription: Vec<WhisperSegment>,
}

#[derive(Debug, Deserialize)]
struct WhisperSegment {
    #[allow(dead_code)]
    text: String,
    tokens: Vec<WhisperToken>,
}

#[derive(Debug, Deserialize)]
struct WhisperToken {
    text: String,
    offsets: TokenOffsets,
    p: Option<f32>,
    id: Option<i64>,
    #[allow(dead_code)]
    t_dtw: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct TokenOffsets {
    from: i64, // 毫秒
    to: i64,
}

/// 从音频提取 ASR token（文本、起点秒、终点秒、概率）。
/// 使用 16k WAV 临时文件，避免 whisper 解码 MP3 的不确定性。
pub fn transcribe(audio: &AudioData, cfg: &AsrConfig) -> Result<Vec<(String, f64, f64, f32)>, String> {
    cfg.is_ready()?;
    std::fs::create_dir_all(&cfg.work_dir)
        .map_err(|e| format!("创建工作目录失败: {e}"))?;

    let wav_path = cfg.work_dir.join("asr_input.wav");
    let pcm = pcm16(audio);
    std::fs::write(&wav_path, to_wav_16k(&pcm))
        .map_err(|e| format!("写入临时 WAV 失败: {e}"))?;

    let out_prefix = cfg.work_dir.join("asr_out");
    let _ = std::fs::remove_file(cfg.work_dir.join("asr_out.json"));

    let lang = if cfg.language.is_empty() { "auto".to_string() } else { cfg.language.clone() };
    let output = Command::new(&cfg.cli_path)
        .args([
            "-m", cfg.model_path.to_str().unwrap_or(""),
            "-f", wav_path.to_str().unwrap_or(""),
            "-l", &lang,
            "-ojf",
            "-of", out_prefix.to_str().unwrap_or(""),
            "-t", &cfg.threads.to_string(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("启动 whisper-cli 失败: {e}"))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Whisper 识别失败: {}", err.lines().rev().take(3).collect::<Vec<_>>().join(" ")));
    }

    let json_path = cfg.work_dir.join("asr_out.json");
    let json = std::fs::read_to_string(&json_path)
        .map_err(|e| format!("读取 ASR 结果失败: {e}"))?;
    let parsed: WhisperJson = serde_json::from_str(&json)
        .map_err(|e| format!("解析 ASR 结果失败: {e}"))?;

    let mut tokens = Vec::new();
    for seg in &parsed.transcription {
        for t in &seg.tokens {
            // 过滤特殊 token（[_BEG_] 等）：id 通常很大，text 以 [ 开头
            let txt = t.text.trim();
            if txt.is_empty() || txt.starts_with('[') || txt.starts_with('<') {
                continue;
            }
            if t.offsets.to <= t.offsets.from {
                continue;
            }
            tokens.push((
                txt.to_string(),
                t.offsets.from as f64 / 1000.0,
                t.offsets.to as f64 / 1000.0,
                t.p.unwrap_or(0.5),
            ));
        }
    }

    // 清理临时文件
    let _ = std::fs::remove_file(&wav_path);
    let _ = std::fs::remove_file(&json_path);

    if tokens.is_empty() {
        return Err("Whisper 未识别出任何有效 token（可能是纯音乐或语言不符）".to_string());
    }
    Ok(tokens)
}

/// whisper.cpp 内置 VAD：用 -vad 输出语音段。
/// 注意：whisper-cli 的 VAD 需要额外模型（-vm），若缺失则降级。
pub fn detect_vad_segments(audio: &AudioData, cfg: &AsrConfig, vad_model: Option<&Path>) -> Result<Vec<(f64, f64)>, String> {
    let vad_model = vad_model.map(PathBuf::from)
        .unwrap_or_else(|| cfg.model_path.parent().unwrap_or(Path::new(".")).join("ggml-silero-v5.1.2.bin"));
    if !vad_model.exists() {
        return Err(format!("未找到 VAD 模型: {}", vad_model.display()));
    }
    std::fs::create_dir_all(&cfg.work_dir).map_err(|e| e.to_string())?;
    let wav_path = cfg.work_dir.join("vad_input.wav");
    let pcm = pcm16(audio);
    std::fs::write(&wav_path, to_wav_16k(&pcm)).map_err(|e| e.to_string())?;

    let out_prefix = cfg.work_dir.join("vad_out");
    let output = Command::new(&cfg.cli_path)
        .args([
            "-m", cfg.model_path.to_str().unwrap_or(""),
            "-f", wav_path.to_str().unwrap_or(""),
            "--vad",
            "-vm", vad_model.to_str().unwrap_or(""),
            "-t", &cfg.threads.to_string(),
            "-otxt",
            "-of", out_prefix.to_str().unwrap_or(""),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("启动 VAD 失败: {e}"))?;
    let _ = std::fs::remove_file(&wav_path);

    // 解析 stdout 中的 [start --> end] 片段（whisper-cli 标准输出）
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut segments = Vec::new();
    for line in stdout.lines() {
        if let Some(a) = line.find('[') {
            if let Some(b) = line.find(" --> ") {
                if let Some(c) = line.find(']') {
                    if b > a && c > b {
                        let s = &line[a + 1..b];
                        let e = &line[b + 5..c];
                        if let (Some(st), Some(en)) = (parse_ts(s), parse_ts(e)) {
                            segments.push((st, en));
                        }
                    }
                }
            }
        }
    }
    // 补充：解析 .txt 输出（每行时间戳）
    if segments.is_empty() {
        let txt_path = cfg.work_dir.join("vad_out.txt");
        if let Ok(txt) = std::fs::read_to_string(&txt_path) {
            for line in txt.lines() {
                if let Some(a) = line.find('[') {
                    if let Some(b) = line.find(" --> ") {
                        if let Some(c) = line.find(']') {
                            if b > a && c > b {
                                if let (Some(st), Some(en)) = (parse_ts(&line[a + 1..b]), parse_ts(&line[b + 5..c])) {
                                    segments.push((st, en));
                                }
                            }
                        }
                    }
                }
            }
            let _ = std::fs::remove_file(&txt_path);
        }
    }
    if segments.is_empty() {
        return Err("VAD 未检测到语音段".to_string());
    }
    Ok(segments)
}

/// 解析 "00:00:01.234" 或 "00:00:01,234" 为秒。
fn parse_ts(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 2 { return None; }
    let mut sec = 0.0f64;
    for p in parts {
        sec = sec * 60.0 + p.parse::<f64>().ok()?;
    }
    Some(sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_timestamp() {
        assert!((parse_ts("00:01:02.500").unwrap() - 62.5).abs() < 1e-6);
        assert!((parse_ts("01:23,456").unwrap() - 83.456).abs() < 1e-6);
    }

    #[test]
    fn asr_config_detect() {
        let cfg = AsrConfig::detect_dev();
        assert!(cfg.cli_path.to_string_lossy().contains("whisper"));
        assert_eq!(cfg.language, "zh");
    }
}
