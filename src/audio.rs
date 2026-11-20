//! 音频模块：解码 MP3 到统一 PCM、播放控制、波形数据、16 kHz 重采样。
//! 对应需求 FR-3.2。

use rodio::{Decoder, OutputStream, OutputStreamHandle, Sample, Sink, Source};use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// 对齐用的统一声道音频数据（单声道 f32，原始采样率）。
#[derive(Clone)]
pub struct AudioData {
    pub path: PathBuf,
    pub samples: Arc<Vec<f32>>,
    pub sample_rate: u32,
    pub duration: f64,
}

impl AudioData {
    pub fn duration_f64(&self) -> f64 {
        self.duration
    }
}

/// 解码文件为单声道 PCM（对齐引擎的输入）。
/// 立体声取左右平均，避免相位抵消使用绝对平均不可取，普通平均即可。
pub fn load_audio(path: &Path) -> Result<AudioData, String> {
    let file = File::open(path).map_err(|e| format!("无法打开文件: {e}"))?;
    let reader = BufReader::new(file);
    let decoder = Decoder::new(reader).map_err(|e| format!("不支持的音频格式: {e}"))?;
    decode_from_source(decoder, path)
}

/// 从内存字节解码（供 VAD 临时 WAV 使用）。
pub fn decode_from_bytes(bytes: Vec<u8>, label: &str) -> Result<AudioData, String> {
    let reader = Cursor::new(bytes);
    let decoder = Decoder::new(reader).map_err(|e| format!("音频解码失败({label}): {e}"))?;
    decode_from_source(decoder, Path::new(label))
}

fn decode_from_source<S>(decoder: S, path: &Path) -> Result<AudioData, String>
where
    S: Source,
    S::Item: Sample,
{
    let sample_rate = decoder.sample_rate();
    let channels = decoder.channels() as usize;
    if sample_rate == 0 || channels == 0 {
        return Err("音频参数无效".to_string());
    }

    let mut mono = Vec::new();
    let mut buf = vec![0.0f32; channels];
    let mut iter = decoder;
    loop {
        let mut count = 0;
        for slot in buf.iter_mut() {
            match iter.next() {
                Some(v) => {
                    *slot = v.to_f32();
                    count += 1;
                }
                None => break,
            }
        }
        if count < channels { break; }
        let avg: f32 = buf.iter().sum::<f32>() / channels as f32;
        mono.push(avg);
    }
    if mono.is_empty() {
        return Err("文件中没有可解码的音频采样".to_string());
    }
    let duration = mono.len() as f64 / sample_rate as f64;
    Ok(AudioData {
        path: path.to_path_buf(),
        samples: Arc::new(mono),
        sample_rate,
        duration,
    })
}

/// 线性插值重采样到 16 kHz（Whisper/VAD 标准采样率）。
/// 对齐场景精度足够；如需专业质量可换 sinc 插值。
pub fn resample_16k(audio: &AudioData) -> Vec<f32> {
    const TARGET: f64 = 16000.0;
    let src = audio.samples.as_slice();
    if audio.sample_rate as f64 == TARGET {
        return src.to_vec();
    }
    let ratio = audio.sample_rate as f64 / TARGET;
    let out_len = (src.len() as f64 / ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let base = pos as usize;
        let frac = (pos - base as f64) as f32;
        let a = src.get(base).copied().unwrap_or(0.0);
        let b = src.get(base + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

/// 按固定 hop 抽取波形峰值，供时间轴绘制。
/// 返回每个像素列的 (min, max) 振幅。
pub fn waveform_peaks(audio: &AudioData, columns: usize) -> Vec<(f32, f32)> {
    let src = audio.samples.as_slice();
    if src.is_empty() || columns == 0 {
        return Vec::new();
    }
    let per = src.len() / columns;
    let mut peaks = Vec::with_capacity(columns);
    for c in 0..columns {
        let start = c * per;
        let end = if c + 1 == columns { src.len() } else { (c + 1) * per };
        let mut min = 1.0f32;
        let mut max = -1.0f32;
        for &v in &src[start..end.min(src.len())] {
            if v < min { min = v; }
            if v > max { max = v; }
        }
        if min > max { min = 0.0; max = 0.0; }
        peaks.push((min, max));
    }
    peaks
}

/// 音频播放引擎（rodio Sink）。
pub struct Player {
    _stream: OutputStream,
    _handle: OutputStreamHandle,
    sink: Sink,
    path: Option<PathBuf>,
    duration: f64,
    start_offset: f64,
    paused_at: f64,
}

impl Player {
    pub fn new() -> Result<Self, String> {
        let (stream, handle) = OutputStream::try_default()
            .map_err(|e| format!("无法打开音频输出设备: {e}"))?;
        let sink = Sink::try_new(&handle).map_err(|e| format!("无法创建播放器: {e}"))?;
        Ok(Self {
            _stream: stream, _handle: handle, sink,
            path: None, duration: 0.0, start_offset: 0.0, paused_at: 0.0,
        })
    }

    pub fn load(&mut self, path: &Path, duration: f64) -> Result<(), String> {
        self.stop();
        let file = File::open(path).map_err(|e| format!("无法打开音频: {e}"))?;
        let reader = BufReader::new(file);
        let decoder = Decoder::new(reader).map_err(|e| format!("音频解码失败: {e}"))?;
        self.sink.append(decoder);
        self.path = Some(path.to_path_buf());
        self.duration = duration;
        self.start_offset = 0.0;
        self.paused_at = 0.0;
        Ok(())
    }

    pub fn play(&mut self) {
        self.sink.play();
    }

    pub fn pause(&mut self) {
        self.sink.pause();
        self.paused_at = self.position();
    }

    pub fn is_paused(&self) -> bool {
        self.sink.is_paused()
    }

    pub fn is_empty(&self) -> bool {
        self.sink.empty()
    }

    pub fn stop(&mut self) {
        self.sink.stop();
        self.path = None;
        self.duration = 0.0;
        self.start_offset = 0.0;
        self.paused_at = 0.0;
    }

    pub fn position(&self) -> f64 {
        if self.path.is_none() { return 0.0; }
        if self.sink.is_paused() {
            return self.paused_at;
        }
        let pos = self.sink.get_pos().as_secs_f64();
        (self.start_offset + pos).min(self.duration)
    }

    /// 跳转到指定时间（重建 Sink）。
    pub fn seek(&mut self, path: &Path, t: f64) -> Result<(), String> {
        self.stop();
        let file = File::open(path).map_err(|e| format!("无法打开音频: {e}"))?;
        let reader = BufReader::new(file);
        let mut decoder = Decoder::new(reader).map_err(|e| format!("音频解码失败: {e}"))?;
        // rodio Decoder 的 seek 需要 Seek + Source，BufReader<File> 支持
        let _ = decoder.try_seek(Duration::from_secs_f64(t.max(0.0)));
        self.sink.append(decoder);
        self.path = Some(path.to_path_buf());
        self.start_offset = t.max(0.0);
        self.paused_at = t.max(0.0);
        Ok(())
    }

    pub fn set_volume(&mut self, v: f32) {
        self.sink.set_volume(v.clamp(0.0, 1.0));
    }

    pub fn duration(&self) -> f64 { self.duration }
}
