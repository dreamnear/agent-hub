#!/usr/bin/env bash
# claude-view server 一键安装（只装服务端二进制，不装桌面壳 / tauri 任何产物）
#
# 用法：
#   ./install.sh                      # 从 GitHub Releases 下载并装到 /usr/local/bin（无权限回退 ~/.local/bin）
#   ./install.sh --with-systemd       # 额外安装并 enable --now systemd 单元（Linux only）
#   ./install.sh --prefix ~/.local/bin
#   RELEASE_BASE_URL=<url> ./install.sh   # 覆盖默认下载源（自建分发时用）
#
# 二进制来源：RELEASE_BASE_URL（默认 GitHub Releases latest 直链）下载；置空可回退
# 脚本同目录（或 ./dist/）的同名文件。
# 命名：claude-view-server-{os}-{arch}，os ∈ linux|darwin，arch ∈ x86_64|aarch64。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd || pwd)"
DEST_BIN_NAME="claude-view-server"
# 默认源：GitHub Releases latest 直链；显式 RELEASE_BASE_URL= 置空则回退脚本同目录文件
RELEASE_BASE_URL="${RELEASE_BASE_URL-https://github.com/dreamnear/agent-hub/releases/latest/download}"

WITH_SYSTEMD=0
FORCE_PREFIX=""

die() {
  echo "错误：$*" >&2
  exit 1
}

# 用 while + shift 而非 for arg in "$@"：后者里 shift 不推进迭代列表（--prefix X 空格形式会失效），
# 且 bash 3.2 零参数时仍执行一次循环体。参数值一律 ${} 包裹——bash 3.2 下 $var 紧贴多字节字符会被
# 误解析进变量名。
while [ "$#" -gt 0 ]; do
  case "$1" in
    --with-systemd)
      WITH_SYSTEMD=1
      shift
      ;;
    --prefix=*)
      FORCE_PREFIX="${1#*=}"
      shift
      ;;
    --prefix)
      [ "$#" -ge 2 ] || die "--prefix 需要一个目录参数"
      FORCE_PREFIX="$2"
      shift 2
      ;;
    -h | --help)
      sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "未知参数：${1}（--help 看用法）" >&2
      exit 2
      ;;
  esac
done

# ---------- 探测 OS / 架构 ----------
detect_os() {
  case "$(uname -s)" in
    Linux) echo linux ;;
    Darwin) echo darwin ;;
    *) die "不支持的操作系统 $(uname -s)（本脚本仅支持 linux / darwin）" ;;
  esac
}

detect_arch() {
  case "$(uname -m)" in
    x86_64 | amd64) echo x86_64 ;;
    aarch64 | arm64) echo aarch64 ;;
    *) die "不支持的架构 $(uname -m)（本脚本仅支持 x86_64 / aarch64）" ;;
  esac
}

OS="$(detect_os)"
ARCH="$(detect_arch)"
ASSET="claude-view-server-${OS}-${ARCH}"

# ---------- 取二进制 ----------
# SRC 指向来源文件（同目录模式可能是用户产物本身，绝不能在退出时删——只清 DOWNLOAD_TMP）
SRC=""
DOWNLOAD_TMP=""
cleanup() { [ -n "$DOWNLOAD_TMP" ] && rm -f "$DOWNLOAD_TMP"; }
trap cleanup EXIT

fetch_binary() {
  if [ -n "${RELEASE_BASE_URL:-}" ]; then
    local url="${RELEASE_BASE_URL%/}/${ASSET}"
    echo "→ 从 ${url} 下载"
    DOWNLOAD_TMP="$(mktemp)"
    curl -fL --retry 2 --connect-timeout 15 -o "$DOWNLOAD_TMP" "$url" ||
      die "下载失败：${url}（检查 RELEASE_BASE_URL 是否指向可公开访问的分发目录）"
    chmod +x "$DOWNLOAD_TMP"
    SRC="$DOWNLOAD_TMP"
    return
  fi
  local cand
  for cand in "$SCRIPT_DIR/$ASSET" "$SCRIPT_DIR/dist/$ASSET"; do
    if [ -f "$cand" ]; then
      SRC="$cand"
      return
    fi
  done
  die "本机需要 ${ASSET}，但脚本目录内没有该文件。
  二选一：
  1) 把 dist 整个目录（含 install.sh 与二进制）scp 到本机后在本目录执行
  2) 设 RELEASE_BASE_URL=<分发 URL 前缀> 由脚本下载
  已有产物可用时扩展 x86_64：在对应架构的机器上
  docker run --rm --platform linux/amd64 -v \"\$PWD\":/app -w /app -e CARGO_TARGET_DIR=/tmp/t \\
    rust:1-alpine sh -c 'cargo build -p server --release --locked &&
      cp /tmp/t/x86_64-unknown-linux-musl/release/server dist/claude-view-server-linux-x86_64'"
}

fetch_binary

# ---------- 选安装目录 ----------
if [ -n "$FORCE_PREFIX" ]; then
  PREFIX="$FORCE_PREFIX"
elif [ -w /usr/local/bin ]; then
  PREFIX="/usr/local/bin"
elif command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
  PREFIX="/usr/local/bin"
else
  PREFIX="$HOME/.local/bin"
  echo "→ /usr/local/bin 不可写且无免密 sudo，回退 ${PREFIX}"
fi
mkdir -p "$PREFIX"
DEST="$PREFIX/$DEST_BIN_NAME"

install_binary() {
  local src="$SRC"
  if [ -w "$PREFIX" ]; then
    install -m 755 "$src" "$DEST"
  else
    sudo install -m 755 "$src" "$DEST"
  fi
  echo "✓ 已安装 $DEST"
}

install_binary

# ---------- 数据目录（不覆盖已有 token）----------
mkdir -p "$HOME/.claude-view"
if [ -f "$HOME/.claude-view/token" ]; then
  echo "✓ 已有 token 保留未动：$HOME/.claude-view/token"
fi

# ---------- systemd（Linux，可选）----------
if [ "$WITH_SYSTEMD" = 1 ]; then
  if [ "$OS" != linux ]; then
    echo "⚠ --with-systemd 仅 Linux 生效，${OS} 请改用 launchd（见 docs/deploy-remote-instance.md）"
  else
    UNIT_SRC=""
    for cand in "$SCRIPT_DIR/claude-view-server.service" "$SCRIPT_DIR/../deploy/claude-view-server.service"; do
      [ -f "$cand" ] && {
        UNIT_SRC="$cand"
        break
      }
    done
    [ -n "$UNIT_SRC" ] || die "找不到 claude-view-server.service（与 install.sh 同目录）"

    # token：安装环境里给了就写死；没给则注释掉该行，让 server 首启自生成
    # （不预置弱口令 token 是安全默认）
    if [ -n "${AGENT_HUB_TOKEN:-}" ]; then
      TOKEN_LINE="Environment=AGENT_HUB_TOKEN=${AGENT_HUB_TOKEN}"
    else
      TOKEN_LINE="# Environment=AGENT_HUB_TOKEN=CHANGE_ME  # 未预置：server 首启自生成到 ~/.claude-view/token"
    fi

    UNIT_TMP="$(mktemp)"
    sed -e "s|^User=.*|User=$(id -un)|" \
      -e "s|^ExecStart=.*|ExecStart=${DEST}|" \
      -e "s|^Environment=AGENT_HUB_TOKEN=.*|${TOKEN_LINE}|" \
      "$UNIT_SRC" >"$UNIT_TMP"

    if [ -w /etc/systemd/system ]; then
      install -m 644 "$UNIT_TMP" /etc/systemd/system/claude-view-server.service
    else
      sudo install -m 644 "$UNIT_TMP" /etc/systemd/system/claude-view-server.service
    fi
    rm -f "$UNIT_TMP"
    echo "✓ 已安装 systemd 单元 /etc/systemd/system/claude-view-server.service"

    if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
      sudo systemctl daemon-reload
      sudo systemctl enable --now claude-view-server
      echo "✓ 已 enable --now，查看状态：systemctl status claude-view-server"
    else
      echo "⚠ 未检测到运行中的 systemd，未启动。装完单元后手动："
      echo "  sudo systemctl daemon-reload && sudo systemctl enable --now claude-view-server"
    fi
  fi
fi

# ---------- 后续指引 ----------
cat <<EOF

下一步：
  1) 装 claude CLI 并以 $(id -un) 登录一次（server 依赖它 spawn 会话）：
       npm install -g @anthropic-ai/claude-code && claude
  2) token：显式设 AGENT_HUB_TOKEN=<强随机串>（openssl rand -hex 32）；
     未设时 server 首启自动生成并持久化到 ~/.claude-view/token
  3) 启动（默认只听 127.0.0.1 安全回环；远程接入才需要放开）：
       ${DEST}                                   # loopback，免认证
       AGENT_HUB_ALLOW_LAN=1 AGENT_HUB_TOKEN=<强随机串> ${DEST}   # 监听 0.0.0.0 且强制 token 认证
EOF
