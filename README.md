# Lyric Aligner

离线歌词自动对齐工具：打开 MP3/WAV，自动找到同名 TXT 歌词，通过**音频能量 / VAD 语音检测 / Whisper ASR** 生成带时间轴的 LRC，并提供可视化时间轴手动微调。完全独立运行，无需联网。

## 功能特性

- **三种对齐引擎**（自动按可用条件选择）：
  1. **快速模式（Fast）**：人声能量谷 + 动态规划，无需 ASR，秒级出结果。
  2. **VAD 模式**：Silero VAD 检测语音段，映射到歌词行。
  3. **ASR 模式**：whisper.cpp 逐 token 时间戳 + Needleman–Wunsch 文本对齐，精度最高。
- **歌词输入**：`.txt`（纯文本，自动探测 GBK/UTF-16/UTF-8 编码）与 `.lrc`（带时间轴，直接导入）。
- **手动校对**：波形时间轴上拖动色块移动整行、拖左右边缘调整起止；`T` 打点、方向键微调。
- **整体平移**：一键将全部歌词前移/后移，同步逐字时间戳，保持行间相对间隔。
- **撤销/重做**：所有手动调整均可撤销。
- **导出**：标准 LRC、增强 LRC（逐字时间戳）、`.lyricproj` 工程文件。
- **纯本地**：音频解码、对齐、ASR 全部离线完成。

## 技术栈

| 用途 | 依赖 |
| --- | --- |
| GUI | `eframe` / `egui` 0.29 |
| 音频解码 / 播放 | `symphonia` / `rodio` |
| 频谱特征 | `rustfft` 6 |
| 文件对话框 | `rfd` 0.15 |
| 中文编码探测 | `encoding_rs` 0.8 |
| 工程序列化 | `serde` / `serde_json` |
| ASR（外部） | whisper.cpp CLI |

## 环境要求

- Rust 1.70+（edition 2021）
- Windows（GUI 与 whisper 工具链为 Windows 版本）
- **ASR 模式**需额外准备（见下节）；Fast / VAD 模式无需 ASR 也能工作。

## 外部模型 / 工具准备（可选，仅 ASR / VAD 模式需要）

本仓库**不含**大体积的 whisper 工具链与模型文件，请自行下载放置到如下目录（与可执行文件同目录）：

```
lyric-aligner/
  tools/whisper/Release/whisper-cli.exe   # whisper.cpp 命令行程序
  models/ggml-large-v3-q5_0.bin           # Whisper large-v3 Q5 模型（约 1.1 GB）
  models/ggml-silero-v5.1.2.bin           # Silero VAD 模型
```

获取方式：

- **whisper.cpp**：从 [ggml-org/whisper.cpp](https://github.com/ggml-org/whisper.cpp) 克隆并按其文档编译 Windows Release 版本，将 `whisper-cli.exe` 及其依赖 DLL 一并放入 `tools/whisper/Release/`。
- **Whisper 模型**：从 [ggml-org/whisper.cpp/models](https://github.com/ggml-org/whisper.cpp/tree/master/models) 下载 `ggml-large-v3-q5_0.bin`；若想用更小的模型，可在界面/配置中调整模型路径（小模型精度稍低但速度更快）。
- **VAD 模型**：`ggml-silero-v5.1.2.bin`，同样来自 whisper.cpp 的模型发布页。

> 若不放置这些文件，程序仍可正常启动并使用 Fast 模式完成对齐。

## 构建与运行

```bash
# 调试运行
cargo run

# 发布构建（LTO 优化）
cargo build --release
./target/release/lyric-aligner.exe
```

## 使用流程

1. 打开程序，点击「打开音频」选择 MP3/WAV。
2. 程序自动查找同目录**同名**的 `.txt` / `.lrc` 歌词；也可手动选择。
3. 选择对齐引擎（Fast / VAD / ASR）并开始对齐。
4. 在波形时间轴上核对：拖动色块调整每行时间，`T` 在当前播放位置打点，方向键微调。
5. 用「整体前移/后移」修正整体偏移。
6. 导出标准/增强 LRC 或保存工程。

## 快捷键

| 按键 | 作用 |
| --- | --- |
| `T` | 将当前播放位置设为选中行的起点 |
| `←` / `→` | 选中行微调 ±0.05 s（Shift 加倍到 ±0.5 s） |
| `Ctrl+←` / `Ctrl+→` | 全部歌词整体平移 ±0.05 s |

## 项目结构

```
src/
  main.rs     程序入口
  app.rs      主界面、时间轴、手动编辑、撤销重做、导出
  align.rs    三级对齐引擎（能量谷 DP / VAD / ASR 映射）与单调修复
  asr.rs      whisper.cpp CLI 调用、token 时间戳解析、VAD 段检测
  audio.rs    MP3/WAV 解码为 16 kHz 单声道 PCM、播放、波形
  lyrics.rs   歌词模型、编码探测、TXT/LRC 解析、LRC 导出、工程序列化
test_assets/  演示用歌词与音频（生成脚本见 tools/snap.py 所在目录）
tools/        辅助脚本与 whisper.cpp 工具链（工具链需自行下载）
```

## 许可证

原型项目，暂未指定开源许可证。
