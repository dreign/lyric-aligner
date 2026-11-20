//! 对齐引擎：L1 能量/Onset + 谷值 DP；L2 VAD；L3 ASR 强制对齐。
//! 对应需求 FR-3.3~3.8。

use crate::audio::{resample_16k, AudioData};
use crate::lyrics::{AlignEngine, AlignResult, LyricLine, TimeSource, WordStamp};

/// 音频特征：按 10 ms hop 的能量/频谱包络。
pub struct Features {
    pub hop: f64,        // 每帧秒数
    pub energy: Vec<f32>, // 归一化能量 0..1
    pub vocal: Vec<f32>,  // 人声打分 0..1
    pub duration: f64,
}

/// 提取对齐用特征（L1 使用原始采样率，L2/L3 使用 16k）。
pub fn extract_features(audio: &AudioData) -> Features {
    let sr = audio.sample_rate as usize;
    let sr = sr.max(1);
    let hop_samples = (sr as f64 * 0.010).round() as usize; // 10 ms
    let win = hop_samples * 2;
    let src = audio.samples.as_slice();
    if src.is_empty() || hop_samples == 0 {
        return Features { hop: 0.01, energy: Vec::new(), vocal: Vec::new(), duration: 0.0 };
    }

    let frames = (src.len().saturating_sub(win)) / hop_samples;
    let mut energy = Vec::with_capacity(frames);
    let mut vocal = Vec::with_capacity(frames);
    let mut peak = 1e-6f32;

    for i in 0..frames {
        let start = i * hop_samples;
        let end = (start + win).min(src.len());
        if end <= start { break; }
        let mut sum = 0.0f64;
        let mut zc = 0usize;
        let mut prev = 0.0f32;
        for &v in &src[start..end] {
            sum += v as f64 * v as f64;
            if (v >= 0.0) != (prev >= 0.0) { zc += 1; }
            prev = v;
        }
        let rms = (sum / (end - start) as f64).sqrt() as f32;
        energy.push(rms);
        if rms > peak { peak = rms; }

        // 人声打分：中频能量占比近似 —— 用一阶差分能量（高频/瞬态）与 RMS 比值，
        // 结合过零率。鼓点低频差分低、ZCR 低；人声/齿音 ZCR 高。
        let zcr = zc as f32 / (end - start) as f32;
        vocal.push((rms, zcr.min(0.5) / 0.5));
    }

    // 归一化能量
    let peak = peak.max(1e-6);
    let energy: Vec<f32> = energy.iter().map(|e| (e / peak).min(1.0)).collect();

    // 人声打分：RMS 中等 + ZCR 适中 → 高分；纯低频鼓点 ZCR 低 → 低分
    let max_rms = energy.iter().cloned().fold(0.0f32, f32::max).max(1e-6);
    let vocal: Vec<f32> = vocal.iter().map(|(rms, zcr)| {
        let r = (rms / max_rms).clamp(0.0, 1.0);
        let z = (zcr - 0.08).clamp(0.0, 0.42) / 0.42;
        (r * 0.6 + z * 0.4).clamp(0.0, 1.0)
    }).collect();

    Features { hop: hop_samples as f64 / sr as f64, energy, vocal, duration: audio.duration }
}

/// L1：能量 Onset + 谷值 DP 对齐。
pub fn align_fast(audio: &AudioData, lines: &[LyricLine]) -> AlignResult {
    let feat = extract_features(audio);
    let n = lines.len();
    if n == 0 {
        return AlignResult { lines: Vec::new(), warnings: Vec::new(), engine: AlignEngine::Fast };
    }
    if feat.energy.len() < 16 {
        return AlignResult {
            lines: uniform_lines(lines, audio.duration),
            warnings: vec!["音频过短，使用均匀时间分配".to_string()],
            engine: AlignEngine::Fast,
        };
    }

    // 平滑能量包络（3 点均值）
    let smoothed = smooth(&feat.energy, 3);
    // 谷值打分：局部能量低点 → 适合作行起点
    let valley = valley_scores(&smoothed, 5);
    // 人声活动打分
    let speech = smooth(&feat.vocal, 5);

    // DP：把 n 行的时间点单调地放在 audio_duration 上，最小化代价。
    // dp[row][t] = 第 row 行起点在 t 帧时的最小累计代价；
    // prev[row][t] = 该状态下第 row-1 行的起点帧（用于完整回溯）。
    let total_frames = smoothed.len();
    let uniform_gap = total_frames as f64 / (n as f64 + 1.0);
    let mut dp = vec![vec![f64::INFINITY; total_frames]; n];
    let mut prev: Vec<Vec<usize>> = vec![vec![0usize; total_frames]; n];

    // 每行的候选窗口：以均匀位置为中心 ±55% gap；首行允许从 0 开始（前奏极短的情况）。
    let windows: Vec<(usize, usize)> = (0..n).map(|row| {
        let center = (row as f64) * uniform_gap;
        let lo = ((center - uniform_gap * 0.55).max(0.0)) as usize;
        let hi = ((center + uniform_gap * 0.55).min(total_frames as f64 - 1.0)) as usize;
        (lo.min(hi), hi)
    }).collect();

    // 第 0 行：无前驱，代价为自身位置代价。
    for t in windows[0].0..=windows[0].1 {
        if t >= total_frames { break; }
        dp[0][t] = position_cost(t, &smoothed, &valley, &speech, uniform_gap, 0, total_frames);
    }

    // 后续行：从上一行窗口内、且严格早于当前 t 的状态转移。
    for row in 1..n {
        let (plo, phi) = windows[row - 1];
        let (lo, hi) = windows[row];
        for t in lo..=hi {
            if t >= total_frames { break; }
            let mut best = f64::INFINITY;
            let mut best_prev = 0usize;
            for pt in plo..=phi.min(t.saturating_sub(1)) {
                let c = dp[row - 1][pt];
                if !c.is_finite() { continue; }
                // 行间隔过近/过远都惩罚，鼓励均匀节奏
                let gap_ratio = (t - pt) as f64 / uniform_gap.max(1.0);
                let spacing = (gap_ratio - 1.0).abs() * 0.9;
                let total = c + spacing
                    + position_cost(t, &smoothed, &valley, &speech, uniform_gap, row, total_frames);
                if total < best { best = total; best_prev = pt; }
            }
            if best.is_finite() {
                dp[row][t] = best;
                prev[row][t] = best_prev;
            }
        }
    }

    // 回溯：从最后一行窗口内代价最小的状态往回走。
    let (last_lo, last_hi) = windows[n - 1];
    let mut cur = (last_lo..=last_hi.min(total_frames - 1))
        .filter(|&t| dp[n - 1][t].is_finite())
        .min_by(|&a, &b| dp[n - 1][a].partial_cmp(&dp[n - 1][b]).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(0);
    if !dp[n - 1][cur].is_finite() {
        return AlignResult {
            lines: uniform_lines(lines, audio.duration),
            warnings: vec!["对齐求解失败，使用均匀时间分配".to_string()],
            engine: AlignEngine::Fast,
        };
    }
    let mut times = vec![0usize; n];
    for row in (0..n).rev() {
        times[row] = cur;
        if row > 0 { cur = prev[row][cur]; }
    }

    // 生成行时间：start = DP 时间，end = 下一行 start
    let mut out = Vec::with_capacity(n);
    for (i, line) in lines.iter().enumerate() {
        let t = times[i] as f64 * feat.hop;
        let conf = confidence_at(&valley, &speech, times[i], feat.hop, t, audio.duration);
        out.push(LyricLine {
            start: t,
            end: 0.0,
            confidence: conf,
            source: TimeSource::Auto,
            ..line.clone()
        });
    }
    crate::lyrics::fix_line_ends(&mut out, audio.duration);
    let warnings = low_confidence_warnings(&out);
    AlignResult { lines: out, warnings, engine: AlignEngine::Fast }
}

/// 单行起点在某帧的局部代价：能量峰惩罚 + 谷值/人声收益 + 偏离均匀位置惩罚。
fn position_cost(
    t: usize, smoothed: &[f32], valley: &[f32], speech: &[f32],
    uniform_gap: f64, row: usize, total_frames: usize,
) -> f64 {
    let center = (row as f64) * uniform_gap;
    let dev = ((t as f64 - center) / uniform_gap.max(1.0)).abs();
    let energy = smoothed.get(t).copied().unwrap_or(0.0) as f64;
    let v = valley.get(t).copied().unwrap_or(0.0) as f64;
    let s = speech.get(t).copied().unwrap_or(0.0) as f64;
    let edge = if t < total_frames / 20 || t > total_frames * 19 / 20 { 0.15 } else { 0.0 };
    dev * 1.2 + energy * 0.7 - v * 1.5 - s * 0.6 + edge
}

/// L2：VAD 语音段对齐。复用 whisper.cpp 的 Silero VAD 输出段，
/// 把语音段边界按 DP 映射到歌词行；模型缺失时由上层降级到 L1。
pub fn align_vad_from_segments(audio: &AudioData, lines: &[LyricLine], segments: &[(f64, f64)]) -> AlignResult {
    let n = lines.len();
    if n == 0 || segments.is_empty() {
        return AlignResult {
            lines: uniform_lines(lines, audio.duration),
            warnings: vec!["未检测到语音段，使用均匀时间".to_string()],
            engine: AlignEngine::Vad,
        };
    }
    // 每个语音段的起点作为一个候选行起点；段数多于行数时合并，少于行数时在长段内切分。
    let mut starts: Vec<f64> = segments.iter().map(|s| s.0).collect();
    starts.retain(|t| *t < audio.duration);
    if starts.len() > n {
        // 合并相邻过近的段，保留能量最强的 n 个（这里简化：等间隔取样）
        let step = starts.len() as f64 / n as f64;
        starts = (0..n).map(|i| starts[(i as f64 * step) as usize]).collect();
    } else if starts.len() < n {
        // 段数不足：在已有段之间插值补齐
        let mut extra = Vec::new();
        let total = audio.duration.max(1.0);
        let gap = total / (n as f64 + 1.0);
        for i in 0..n {
            let t = i as f64 * gap;
            if !starts.iter().any(|s| (*s - t).abs() < gap * 0.3) {
                extra.push(t);
            }
        }
        starts.extend(extra);
        starts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        starts.truncate(n);
    }
    // 单调化
    for i in 1..starts.len() {
        if starts[i] <= starts[i - 1] { starts[i] = starts[i - 1] + 0.15; }
    }
    let mut out = Vec::with_capacity(n);
    for (i, line) in lines.iter().enumerate() {
        out.push(LyricLine {
            start: starts.get(i).copied().unwrap_or(i as f64 * 2.0),
            end: 0.0,
            confidence: 0.85,
            source: TimeSource::Auto,
            ..line.clone()
        });
    }
    crate::lyrics::fix_line_ends(&mut out, audio.duration);
    AlignResult { lines: out, warnings: Vec::new(), engine: AlignEngine::Vad }
}

/// 均匀分配（降级用）。
pub fn uniform_lines(lines: &[LyricLine], duration: f64) -> Vec<LyricLine> {
    let n = lines.len();
    if n == 0 { return Vec::new(); }
    let gap = duration / (n as f64 + 1.0);
    let mut out: Vec<LyricLine> = lines.iter().enumerate().map(|(i, line)| LyricLine {
        start: (i as f64 + 1.0) * gap,
        end: 0.0,
        confidence: 0.3,
        source: TimeSource::Auto,
        ..line.clone()
    }).collect();
    crate::lyrics::fix_line_ends(&mut out, duration);
    out
}

/// L3：用 ASR token 时间戳对齐歌词文本（NW 全局对齐）。
/// tokens: (文本, 起始秒, 结束秒, 概率)
pub fn align_asr_tokens(
    lines: &[LyricLine],
    tokens: &[(String, f64, f64, f32)],
    audio_duration: f64,
) -> AlignResult {
    if lines.is_empty() || tokens.is_empty() {
        return AlignResult {
            lines: uniform_lines(lines, audio_duration),
            warnings: vec!["ASR 未返回可用 token，使用均匀时间".to_string()],
            engine: AlignEngine::Asr,
        };
    }

    // 把 token 文本拼成序列，歌词按归一化字符展开。
    let token_chars: Vec<(char, usize)> = {
        let mut v = Vec::new();
        for (ti, (text, _, _, _)) in tokens.iter().enumerate() {
            for c in normalize_asr(text).chars() {
                v.push((c, ti));
            }
        }
        v
    };

    // 为每行歌词生成归一化字符序列
    let line_chars: Vec<Vec<char>> = lines.iter()
        .map(|l| crate::lyrics::normalize_text(&l.text).chars().collect())
        .collect();

    // 把所有歌词字符串成一条序列，记录每个字符属于哪一行
    let mut lyric_seq: Vec<char> = Vec::new();
    let mut lyric_line: Vec<usize> = Vec::new();
    for (li, chars) in line_chars.iter().enumerate() {
        for &c in chars {
            lyric_seq.push(c);
            lyric_line.push(li);
        }
        // 行间插入分隔哨兵（不参与匹配，仅帮助 NW 定位边界）
        lyric_seq.push('\u{0}');
        lyric_line.push(usize::MAX);
    }

    // Needleman–Wunsch 全局对齐（带仿射简化：线性 gap）
    let m = lyric_seq.len();
    let k = token_chars.len();
    const MATCH: i32 = 2;
    const MISMATCH: i32 = -1;
    const GAP: i32 = -2;
    let mut dp = vec![vec![0i32; k + 1]; m + 1];
    for i in 1..=m { dp[i][0] = dp[i - 1][0] + GAP; }
    for j in 1..=k { dp[0][j] = dp[0][j - 1] + GAP; }
    for i in 1..=m {
        for j in 1..=k {
            let a = lyric_seq[i - 1];
            let b = token_chars[j - 1].0;
            let s = if a == '\u{0}' { GAP } else if a == b { MATCH } else { MISMATCH };
            dp[i][j] = (dp[i - 1][j - 1] + s)
                .max(dp[i - 1][j] + GAP)
                .max(dp[i][j - 1] + GAP);
        }
    }

    // 回溯：收集每个歌词行匹配到的 token 索引
    let mut line_tokens: Vec<Vec<usize>> = vec![Vec::new(); lines.len()];
    let mut matched = 0usize;
    let mut i = m;
    let mut j = k;
    while i > 0 && j > 0 {
        let a = lyric_seq[i - 1];
        let b = token_chars[j - 1].0;
        let s = if a == '\u{0}' { GAP } else if a == b { MATCH } else { MISMATCH };
        if dp[i][j] == dp[i - 1][j - 1] + s {
            if a != '\u{0}' {
                let li = lyric_line[i - 1];
                let ti = token_chars[j - 1].1;
                if li < lines.len() { line_tokens[li].push(ti); }
                if a == b { matched += 1; }
            }
            i -= 1; j -= 1;
        } else if dp[i][j] == dp[i - 1][j] + GAP {
            i -= 1;
        } else {
            j -= 1;
        }
    }

    let total_chars: usize = line_chars.iter().map(|c| c.len()).sum();
    let match_rate = if total_chars > 0 { matched as f32 / total_chars as f32 } else { 0.0 };

    let mut out = Vec::with_capacity(lines.len());
    let mut warnings = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        let mut tis = line_tokens[li].clone();
        tis.sort_unstable();
        tis.dedup();
        let start = tis.first().and_then(|&t| tokens.get(t)).map(|t| t.1)
            .unwrap_or(li as f64 * 2.0);
        let end = tis.last().and_then(|&t| tokens.get(t)).map(|t| t.2)
            .unwrap_or(start + 2.0);
        let prob = if tis.is_empty() { 0.0 } else {
            tis.iter().filter_map(|&t| tokens.get(t)).map(|t| t.3).sum::<f32>() / tis.len() as f32
        };
        let conf = (match_rate * 0.6 + prob * 0.4).clamp(0.0, 1.0);
        if conf < 0.5 {
            warnings.push(format!("第 {} 行匹配度低（{:.0}%），请人工核对", line.index, conf * 100.0));
        }

        // 逐字时间戳：把该行匹配到的 token 按原文字符顺序填入
        let words: Vec<WordStamp> = tis.iter().filter_map(|&t| tokens.get(t)).map(|(text, s, e, _)| {
            WordStamp { text: text.trim().to_string(), start: *s, end: *e }
        }).collect();

        out.push(LyricLine {
            start,
            end,
            confidence: conf,
            words,
            source: TimeSource::Auto,
            ..line.clone()
        });
    }
    // 单调化 + 修正结束时间
    for i in 1..out.len() {
        if out[i].start <= out[i - 1].start {
            out[i].start = out[i - 1].start + 0.12;
        }
    }
    crate::lyrics::fix_line_ends(&mut out, audio_duration);
    AlignResult { lines: out, warnings, engine: AlignEngine::Asr }
}

/// 归一化 ASR 文本：去标点、小写、空白。
fn normalize_asr(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && !is_punct(*c))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn is_punct(c: char) -> bool {
    matches!(c, '，'|'。'|'！'|'？'|'；'|'：'|'、'|'…'|'—'|','|'.'|'!'|'?'|';'|':'|'-'|'_'|'"'|'\''|'('|')'|'['|']'|'。'|'♪'|'♫')
}

fn smooth(values: &[f32], radius: usize) -> Vec<f32> {
    if values.len() < 2 || radius == 0 { return values.to_vec(); }
    let mut out = Vec::with_capacity(values.len());
    for i in 0..values.len() {
        let lo = i.saturating_sub(radius);
        let hi = (i + radius + 1).min(values.len());
        let sum: f32 = values[lo..hi].iter().sum();
        out.push(sum / (hi - lo) as f32);
    }
    out
}

/// 谷值打分：值越小越像谷，返回 0..1（1 = 强谷）。
fn valley_scores(smoothed: &[f32], radius: usize) -> Vec<f32> {
    let n = smoothed.len();
    let mut out = vec![0.0f32; n];
    if n < 3 { return out; }
    let max = smoothed.iter().cloned().fold(0.0f32, f32::max).max(1e-6);
    for i in radius..n.saturating_sub(radius) {
        let mut local_max = 0.0f32;
        let mut local_min = f32::MAX;
        for j in i.saturating_sub(radius)..=(i + radius).min(n - 1) {
            local_max = local_max.max(smoothed[j]);
            local_min = local_min.min(smoothed[j]);
        }
        let spread = (local_max - local_min) / max;
        // 当前点接近局部最小 + 邻域动态范围大 → 强谷
        let near_min = 1.0 - (smoothed[i] - local_min) / (spread * max).max(1e-6);
        out[i] = (near_min.clamp(0.0, 1.0) * spread.min(1.0) * 4.0).min(1.0)
            * (1.0 - smoothed[i] / max);
    }
    out
}

fn confidence_at(valley: &[f32], speech: &[f32], frame: usize, hop: f64, t: f64, duration: f64) -> f32 {
    if valley.is_empty() || frame >= valley.len() {
        return 0.3;
    }
    let v = valley[frame];
    let s = speech.get(frame).copied().unwrap_or(0.0);
    let pos_ok = if t < 2.0 || t > duration - 2.0 { 0.7 } else { 1.0 };
    ((v * 0.6 + s * 0.4) * pos_ok).clamp(0.15, 0.98)
}

fn low_confidence_warnings(lines: &[LyricLine]) -> Vec<String> {
    let low: Vec<usize> = lines.iter().filter(|l| l.confidence < 0.45).map(|l| l.index).collect();
    if low.is_empty() {
        Vec::new()
    } else {
        vec![format!("{} 行置信度较低，建议试听校对", low.len())]
    }
}

/// 供 VAD 使用：16k 单声道 PCM（导出为 WAV 字节供 whisper-cli 处理）。
pub fn to_wav_16k(samples: &[f32]) -> Vec<u8> {
    let sr = 16000u32;
    let bits = 16u16;
    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());   // fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes());    // PCM
    wav.extend_from_slice(&1u16.to_le_bytes());    // mono
    wav.extend_from_slice(&sr.to_le_bytes());
    wav.extend_from_slice(&(sr * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes());    // block align
    wav.extend_from_slice(&bits.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        wav.extend_from_slice(&v.to_le_bytes());
    }
    wav
}

/// 提取 16k PCM（供 ASR/VAD 导出 WAV）。
pub fn pcm16(audio: &AudioData) -> Vec<f32> {
    resample_16k(audio)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::normalize_text;

    #[test]
    fn valley_scores_finds_dip() {
        let s = vec![0.9, 0.9, 0.1, 0.9, 0.9];
        let v = valley_scores(&s, 1);
        assert!(v[2] > v[0], "谷值点得分应高于峰值点: {:?}", v);
    }

    #[test]
    fn uniform_lines_monotonic() {
        let lines: Vec<LyricLine> = (0..4).map(|i| LyricLine {
            index: i + 1, text: format!("行{i}"), norm: normalize_text(&format!("行{i}")),
            start: 0.0, end: 0.0, confidence: 0.0, words: vec![], source: TimeSource::Auto,
        }).collect();
        let out = uniform_lines(&lines, 10.0);
        for i in 1..out.len() {
            assert!(out[i].start > out[i - 1].start);
        }
    }

    #[test]
    fn asr_alignment_maps_lines() {
        let lines: Vec<LyricLine> = (0..2).map(|i| LyricLine {
            index: i + 1, text: if i == 0 { "你好".into() } else { "世界".into() },
            norm: if i == 0 { "你好".into() } else { "世界".into() },
            start: 0.0, end: 0.0, confidence: 0.0, words: vec![], source: TimeSource::Auto,
        }).collect();
        let tokens = vec![
            ("你".to_string(), 1.0, 1.3, 0.9),
            ("好".to_string(), 1.3, 1.6, 0.9),
            ("世".to_string(), 2.0, 2.3, 0.8),
            ("界".to_string(), 2.3, 2.6, 0.8),
        ];
        let r = align_asr_tokens(&lines, &tokens, 5.0);
        assert!((r.lines[0].start - 1.0).abs() < 0.1, "第一行起点: {}", r.lines[0].start);
        assert!((r.lines[1].start - 2.0).abs() < 0.1, "第二行起点: {}", r.lines[1].start);
        assert_eq!(r.lines[0].words.len(), 2);
    }
}
