#!/usr/bin/env bash
# 统一设置项目版本号,一处修改、多处同步:
#   - VERSION
#   - Cargo.toml 的 [package] version
#   - 根 README 与全部多语言文档(docs/*/README.md)顶部的 version 徽章
#
# 用法: scripts/set-version.sh 0.1.1
#
# 发版流程建议:先跑本脚本同步版本 → 更新 CHANGELOG → 提交 → 打 tag vX.Y.Z → 推送
# (推 tag 触发 docker-publish 自动发 GHCR;README 徽章即随该 tag 的提交一并更新)。
set -euo pipefail

V="${1:-}"
if [[ -z "$V" ]]; then
  echo "用法: scripts/set-version.sh <version,例如 0.1.1>" >&2
  exit 1
fi
V="${V#v}" # 容忍带前导 v 的输入
if [[ ! "$V" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
  echo "版本号格式不对: $V(应形如 0.1.1)" >&2
  exit 1
fi

cd "$(dirname "$0")/.."

printf '%s\n' "$V" >VERSION
# Cargo.toml:只改 [package] 段的第一处顶格 version = "..."(依赖项不顶格,不受影响)。
sed -i -E '0,/^version = "[^"]*"/s//version = "'"$V"'"/' Cargo.toml
# 所有 README 的 version 徽章(shields.io badge/version-vX-success)。
for f in README.md docs/*/README.md; do
  [[ -f "$f" ]] && sed -i -E 's#badge/version-v[0-9A-Za-z.+-]+-success#badge/version-v'"$V"'-success#g' "$f"
done

echo "已同步版本 → v$V:"
echo "  VERSION    = $(cat VERSION)"
echo "  Cargo.toml = $(grep -m1 -E '^version = ' Cargo.toml)"
echo "  README 徽章:"
grep -h -oE 'badge/version-v[0-9A-Za-z.+-]+-success' README.md docs/*/README.md | sort -u | sed 's/^/    /'
