#!/bin/sh
set -e
# 以 root 启动时先修正挂载卷属主(无缝升级 legacy root-created ./data),再降权到 appuser。
if [ "$(id -u)" = "0" ]; then
    chown -R appuser:appuser /app/data 2>/dev/null || true
    exec gosu appuser "$@"
fi
exec "$@"
