#!/usr/bin/env bash
# Start, stop or reset a native `clickhouse server` for the B1 benchmark.
# Usage: native_server.sh start|stop|fresh [--binary P] [--tcp-port N] [--http-port N] [--state DIR]
# `fresh` wipes the state directory, so the compiled-expression cache starts cold.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
CMD="${1:-}"; shift || true
BINARY="${CLICKDOOM_NATIVE_CLICKHOUSE:-$HOME/.clickhouse/26.3.25.2/clickhouse}"
TCP_PORT=9100
HTTP_PORT=8223
STATE="${TMPDIR:-/tmp}/clickdoom-native-ch"
while [ $# -gt 0 ]; do
  case "$1" in
    --binary) BINARY="$2"; shift 2 ;;
    --tcp-port) TCP_PORT="$2"; shift 2 ;;
    --http-port) HTTP_PORT="$2"; shift 2 ;;
    --state) STATE="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done
PIDFILE="$STATE/clickhouse-server.pid"

stop() {
  local pid
  [ -f "$PIDFILE" ] || return 0
  pid=$(cat "$PIDFILE")
  if kill -0 "$pid" 2>/dev/null; then
    kill "$pid"
    for _ in $(seq 1 100); do kill -0 "$pid" 2>/dev/null || break; sleep 0.2; done
    if kill -0 "$pid" 2>/dev/null; then kill -9 "$pid" || true; fi
  fi
  rm -f "$PIDFILE"
}

start() {
  [ -x "$BINARY" ] || { echo "::error::cannot run native clickhouse binary: $BINARY" >&2; exit 1; }
  mkdir -p "$STATE"/{log,data,tmp,user_files,format_schemas,etc}
  sed -e "s#@@PATH@@#$STATE#g" -e "s#@@TCP_PORT@@#$TCP_PORT#g" -e "s#@@HTTP_PORT@@#$HTTP_PORT#g" \
      "$HERE/native-server.xml" > "$STATE/etc/config.xml"
  cp "$HERE/users.xml" "$STATE/etc/users.xml"
  ulimit -n 262144 2>/dev/null || ulimit -n 65536
  nohup "$BINARY" server --config-file="$STATE/etc/config.xml" --pid-file="$PIDFILE" \
      >"$STATE/log/stdout.log" 2>&1 &
  for _ in $(seq 1 100); do
    if "$BINARY" client --host 127.0.0.1 --port "$TCP_PORT" --password clickdoom --query "SELECT 1" >/dev/null 2>&1; then
      echo "# native server up: $("$BINARY" client --host 127.0.0.1 --port "$TCP_PORT" --password clickdoom --query 'SELECT version()') pid=$(cat "$PIDFILE") tcp=$TCP_PORT http=$HTTP_PORT state=$STATE" >&2
      echo "# binary: $BINARY sha256=$(shasum -a 256 "$BINARY" | cut -d' ' -f1)" >&2
      return 0
    fi
    sleep 0.3
  done
  echo "::error::native server did not come up; see $STATE/log/" >&2; tail -20 "$STATE/log/stdout.log" >&2; exit 1
}

case "$CMD" in
  start) start ;;
  stop) stop ;;
  fresh) stop; rm -rf "$STATE"; start ;;
  *) echo "usage: $0 start|stop|fresh [--binary P] [--tcp-port N] [--http-port N] [--state DIR]" >&2; exit 1 ;;
esac
