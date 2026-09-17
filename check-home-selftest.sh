#!/usr/bin/env bash
# Keeps check-home.sh honest: run it against a fake daemon that answers 200 to everything and
# protects nothing. Every behaviour line must say FAIL. A line that says "ok" here would say
# "ok" for a broken build too, so it is listed and this script fails.
# Usage: check-home-selftest.sh
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT

cat > "$T/yes.py" <<'PY'
import http.server, re, sys
port = int(re.search(r'listen = "0\.0\.0\.0:(\d+)"', open(sys.argv[1]).read()).group(1))
class Yes(http.server.BaseHTTPRequestHandler):
    def answer(self):
        n = int(self.headers.get('content-length') or 0)
        if n: self.rfile.read(n)
        body = b'{"id": 1, "token": "t", "entries": []}'
        self.send_response(200); self.send_header('content-type', 'application/json')
        self.send_header('content-length', str(len(body))); self.end_headers(); self.wfile.write(body)
    do_GET = do_POST = do_PUT = do_DELETE = do_PROPFIND = do_OPTIONS = answer
    def log_message(self, *a): pass
http.server.ThreadingHTTPServer(('0.0.0.0', port), Yes).serve_forever()
PY
cat > "$T/fake" <<FAKE
#!/usr/bin/env bash
if [ "\${1:-}" = "--version" ]; then echo "Akramium Home 9.9.9"; exit 0; fi
exec python3 "$T/yes.py" "\$2"
FAKE
chmod +x "$T/fake"

IDLE_SECONDS=1 PORT=${PORT:-11791} "$HERE/check-home.sh" "$T/fake" 9.9.9 > "$T/out" 2>&1
cat "$T/out"
# Allowed to pass here: the two lines about the binary file itself (true of any file that
# prints the version), and the idle line (a daemon that does nothing opens no connection;
# that line carries its own control, that lsof can see the process at all).
passed=$(grep -E '^  ok ' "$T/out" | grep -vE -- '--version says|names no assistant|no outbound connection')
echo
if [ -z "$passed" ] && grep -q '^  FAIL' "$T/out"; then
  echo "selftest: every behaviour check can fail ($(grep -c '^  FAIL' "$T/out") FAIL lines against the fake daemon)"
else
  echo "selftest: these checks pass against a daemon that protects nothing:"; echo "$passed"; exit 1
fi
