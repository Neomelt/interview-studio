#!/usr/bin/env bash
# 提交/发布前的隐私闸门。CI 里也跑这一份，本地和远端判定一致。
#
# 这是个录音工具，误提交的代价不是「不好看」，是泄露真实对话或个人信息。
# 所以宁可误报，不可漏报 —— 命中就退出非零，人工确认后再放行。
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

fail=0
hit() { printf '\033[31m✗ %s\033[0m\n' "$1"; shift; printf '    %s\n' "$@"; fail=1; }

# 只扫纳入版本控制的文件，避免把 target/ 里的构建产物也算进来
files=$(git ls-files 2>/dev/null) || { echo "不是 git 仓库，跳过"; exit 0; }
[ -n "$files" ] || { echo "还没有纳入版本控制的文件"; exit 0; }

scan() { # $1=正则 $2=说明
    local m
    m=$(printf '%s\n' "$files" | xargs -r grep -nIE "$1" 2>/dev/null | grep -v '^scripts/privacy-scan.sh:' | head -5)
    [ -n "$m" ] && hit "$2" "$m"
}

scan '/home/[a-z0-9_-]+/'                      "出现了绝对家目录路径（会暴露用户名）"
scan '[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-z]{2,}' "出现了邮箱地址"
scan '\b(10\.[0-9]+|192\.168|100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7]))\.[0-9]+\.[0-9]+' "出现了内网/Tailscale IP"
scan 'alsa_(output|input)\.[a-z]+-[A-Za-z0-9_]+_[A-Z0-9]{8,}' "出现了含序列号的音频设备 ID"

# 音频文件一律不该入库
media=$(printf '%s\n' "$files" | grep -iE '\.(mkv|wav|flac|mp3|m4a|opus|srt|vtt)$' | head -5)
[ -n "$media" ] && hit "有音视频文件被纳入版本控制" "$media"

if [ "$fail" -eq 0 ]; then
    printf '\033[32m✓ 隐私扫描通过（%s 个文件）\033[0m\n' "$(printf '%s\n' "$files" | wc -l)"
fi
exit "$fail"
