#!/usr/bin/env bash
# LyricAligner 验证脚本
# 默认测试素材：《声声慢》戏腔版（F:/《声声慢》戏腔版.mp3 + 同名 txt）
# 用法:
#   bash tools/verify.sh          # 默认 snap：启动加载并截图到 .work/verify.png
#   bash tools/verify.sh snap     # 同上
#   bash tools/verify.sh align    # 快速对齐并导出 .work/verify-result.json
# 可用环境变量覆盖: MP3_TEST_AUDIO / MP3_TEST_LYRICS / MP3_TEST_ENGINE (FAST|VAD|ASR) / MP3_TEST_EXE
set -euo pipefail
cd "$(dirname "$0")/.."

AUDIO="${MP3_TEST_AUDIO:-F:/《声声慢》戏腔版.mp3}"
LYRICS="${MP3_TEST_LYRICS:-${AUDIO%.*}.txt}"
ENGINE="${MP3_TEST_ENGINE:-FAST}"
EXE="${MP3_TEST_EXE:-./target/debug/lyric-aligner.exe}"
MODE="${1:-snap}"

export MP3_AUTO_LOAD="$AUDIO|$LYRICS|$ENGINE"
mkdir -p .work

case "$MODE" in
  snap)
    export MP3_SNAP=1
    "$EXE" &
    PID=$!
    # 轮询等待窗口出现（最多约 10 秒），避免固定 sleep 不足导致截图失败
    OK=0
    for _ in $(seq 1 20); do
      sleep 0.5
      if python tools/snap.py .work/verify.bmp >/dev/null 2>&1; then
        OK=1
        break
      fi
    done
    if [ "$OK" -eq 1 ]; then
      ffmpeg -y -loglevel error -i .work/verify.bmp .work/verify.png
      rm -f .work/verify.bmp
      echo "截图已保存: .work/verify.png"
    else
      echo "错误：等待窗口超时，未找到「歌词自动对齐」窗口" >&2
    fi
    kill "$PID" 2>/dev/null || true
    ;;
  align)
    export MP3_AUTO_DUMP=".work/verify-result.json"
    "$EXE"
    echo "对齐结果: .work/verify-result.json"
    ;;
  *)
    echo "用法: $0 [snap|align]" >&2
    exit 1
    ;;
esac
