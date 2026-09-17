#!/usr/bin/env bash
# Checks on a built akramium-home before it is released: the behaviours people rely on, and
# the mistakes fixed once that must not come back. Prints one line per check; exits non-zero
# when any line says FAIL. A line that cannot run here says "skip" with the reason, never "ok".
#
# Usage: check-home.sh <binary> <version>
# Env:   IDLE_SECONDS  how long to watch for outbound connections (default 20; releases use 600)
#        PORT          the port the checked instance listens on (default 11790)
set -uo pipefail
BIN=${1:?binary}; VERSION=${2:?version}
IDLE_SECONDS=${IDLE_SECONDS:-20}; PORT=${PORT:-11790}
B=http://127.0.0.1:$PORT
fails=0
check() {  # $1 what, $2 0 = ok, $3 detail shown on failure
  if [ "$2" = 0 ]; then echo "  ok    $1"; else echo "  FAIL  $1${3:+   ($3)}"; fails=$((fails + 1)); fi
}
skip() { echo "  skip  $1   ($2)"; }
code() { curl -s -m 10 -o /dev/null -w '%{http_code}' "$@"; }

T=$(mktemp -d)
# A name no cache has seen: a stale announcement from an earlier run cannot answer for this one.
NAME="akramium-check-$$-$RANDOM"
PID=
cleanup() { [ -n "$PID" ] && kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; rm -rf "$T"; }
trap cleanup EXIT

# --- the binary itself -------------------------------------------------------------------------
said=$("$BIN" --version 2>/dev/null)
[ "$said" = "Akramium Home $VERSION" ]; check "--version says Akramium Home $VERSION" $? "it says '$said'"
strings -a "$BIN" 2>/dev/null > "$T/strings.txt"
[ -s "$T/strings.txt" ] && ! grep -qiE 'claude|anthropic' "$T/strings.txt"; check "the binary names no assistant or its maker" $? "$(grep -ciE 'claude|anthropic' "$T/strings.txt") lines"

# --- start an instance on the LAN side, with its own name so it cannot collide with a real one --
cat > "$T/home.toml" <<EOF
listen = "0.0.0.0:$PORT"
data_dir = "$T/data"
host_names = ["$NAME.local"]
[mdns]
name = "$NAME"
EOF
"$BIN" --config "$T/home.toml" > "$T/log" 2>&1 &
PID=$!
for _ in $(seq 1 50); do [ "$(code $B/login)" = 200 ] && break; sleep 0.2; done
curl -s -m 10 $B/login | grep -q '<title>Sign in to Akramium Home</title>'; check "the daemon starts and serves the sign-in page" $? "log: $(tail -1 "$T/log" 2>/dev/null)"

# --- first run ------------------------------------------------------------------------------------
LINK=$(cat "$T/data/setup-link" 2>/dev/null); TOKEN=${LINK##*token=}
[ -n "$TOKEN" ] && grep -q "setup?token=$TOKEN" "$T/log"; check "a first run prints its one-time setup link and writes it to setup-link" $?
J="$T/jar"
# JSON bodies live in variables: bash 3.2 (macOS) mis-parses escaped quotes inside "$( )".
JSON='content-type: application/json'
SETUP_BODY=$(printf '{"token":"%s","name":"alice","password":"correct horse"}' "$TOKEN")
got=$(curl -s -m 10 -c "$J" -H "$JSON" -d "$SETUP_BODY" $B/api/setup)
echo "$got" | grep -q '"name":"alice"' && echo "$got" | grep -q '"is_admin":true'; check "the setup link creates the admin" $? "got $got"
[ "$(code "$B/setup?token=$TOKEN")" = 410 ] && [ ! -e "$T/data/setup-link" ]; check "the setup link works once (410 after, file gone)" $?
grep -q 'HttpOnly' "$J" 2>/dev/null || grep -q '#HttpOnly_' "$J"; check "the session cookie is HttpOnly" $?

# --- the drive --------------------------------------------------------------------------------------
printf '<script>alert(1)</script>' > "$T/x.html"
head -c 3000000 /dev/urandom > "$T/blob.bin"
HTML_ID=$(curl -s -b "$J" -X PUT --data-binary @"$T/x.html" "$B/api/drive/files?name=x.html" | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])' 2>/dev/null)
BLOB_ID=$(curl -s -b "$J" -X PUT --data-binary @"$T/blob.bin" "$B/api/drive/files?name=blob.bin" | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])' 2>/dev/null)
curl -s -b "$J" -D "$T/h" -o /dev/null "$B/api/drive/files/$HTML_ID/content"
grep -qi '^content-disposition: attachment' "$T/h" && grep -qi "^content-security-policy: sandbox" "$T/h" && grep -qi '^x-content-type-options: nosniff' "$T/h"
check "uploaded HTML downloads instead of rendering (attachment, sandbox policy, nosniff)" $?
curl -s -b "$J" -H 'Range: bytes=1000000-1000999' -D "$T/h" -o "$T/part" "$B/api/drive/files/$BLOB_ID/content"
grep -q '^HTTP/1.1 206' "$T/h" && cmp -s "$T/part" <(dd if="$T/blob.bin" bs=1 skip=1000000 count=1000 2>/dev/null)
check "a Range request returns 206 with exactly those bytes" $?
[ "$(code -b "$J" -X PUT --data-binary x "$B/api/drive/files?name=..%2Fescape")" = 400 ] && [ "$(code -b "$J" -H 'content-type: application/json' -d '{"name":".."}' $B/api/drive/folders)" = 400 ]
check "a name with .. or a separator is refused (400)" $?
[ -f "$T/data/users/1/files/blob.bin" ] && [ -z "$(find "$T" -name 'escape*' 2>/dev/null)" ]; check "files land in the user's folder and nothing was written outside it" $?

# resumable upload: a wrong offset is refused and answered cleanly
UP=$(curl -s -b "$J" -H 'content-type: application/json' -d '{"name":"chunks.bin","size":3000000}' $B/api/drive/uploads | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])' 2>/dev/null)
head -c 1500000 "$T/blob.bin" > "$T/c1"; tail -c 1500000 "$T/blob.bin" > "$T/c2"
a=$(code -b "$J" -X PUT --data-binary @"$T/c1" "$B/api/drive/uploads/$UP/0"); b=$(code -b "$J" -X PUT --data-binary @"$T/c1" "$B/api/drive/uploads/$UP/0")
c=$(code -b "$J" -X PUT --data-binary @"$T/c2" "$B/api/drive/uploads/$UP/1500000"); d=$(code -b "$J" -X POST "$B/api/drive/uploads/$UP/finish")
[ "$a$b$c$d" = "200409200201" ]; check "chunks upload in order, a repeated chunk gets 409, finish creates the file" $? "got $a $b $c $d"
CH_ID=$(curl -s -b "$J" $B/api/drive/files | python3 -c 'import sys,json;print([e["id"] for e in json.load(sys.stdin)["entries"] if e["name"]=="chunks.bin"][0])' 2>/dev/null)
curl -s -b "$J" "$B/api/drive/files/$CH_ID/content" | cmp -s - "$T/blob.bin"; check "the chunked file is byte-identical to what was sent" $?

# --- share links ------------------------------------------------------------------------------------
SHARE=$(curl -s -b "$J" -H 'content-type: application/json' -d '{"expires_in":2}' "$B/api/drive/files/$BLOB_ID/shares" | python3 -c 'import sys,json;print(json.load(sys.stdin)["token"])' 2>/dev/null)
[ "$(code "$B/s/$SHARE")" = 200 ] && curl -s -m 20 "$B/s/$SHARE/content/$BLOB_ID" | cmp -s - "$T/blob.bin"; check "a share link opens without an account, with the file's exact bytes" $?
[ "$(code "$B/s/$SHARE/content/$HTML_ID")" = 404 ]; check "a share link opens nothing but what was shared (404)" $?
curl -s -D "$T/h" -o /dev/null "$B/s/$SHARE"; grep -qi '^x-robots-tag: noindex' "$T/h" && grep -qi '^referrer-policy: no-referrer' "$T/h"
check "a share page is not indexed and sends no referrer" $?
sleep 3
[ "$(code "$B/s/$SHARE")" = 410 ] && [ "$(code "$B/s/$SHARE/content/$BLOB_ID")" = 410 ]; check "an expired share link answers 410, page and bytes" $?

# --- the two doors ------------------------------------------------------------------------------------
[ "$(code -u 'alice:correct horse' -X PROPFIND -H 'Depth: 1' $B/dav/)" = 207 ] && curl -s -u 'alice:correct horse' -X PROPFIND -H 'Depth: 1' $B/dav/ | grep -q '/dav/blob.bin'
check "WebDAV lists the drive with a name and password" $?
[ "$(code -b "$J" -X PROPFIND $B/dav/)" = 401 ]; check "WebDAV refuses the session cookie (401)" $?
[ "$(code -u 'alice:correct horse' $B/api/drive/files)" = 401 ]; check "the pages' API refuses a name and password (401)" $?
[ "$(code $B/api/drive/files)" = 401 ]; check "the API refuses a request with no session (401)" $?
echo dav > "$T/d.txt"; curl -s -u 'alice:correct horse' -T "$T/d.txt" -o /dev/null "$B/dav/from-dav.txt"
curl -s -b "$J" $B/api/drive/files | grep -q '"from-dav.txt"'; check "a file written through WebDAV shows in the pages" $?
[ "$(code -u 'alice:correct horse' -X DELETE $B/dav/from-dav.txt)" = 204 ] && curl -s -b "$J" $B/api/drive/trash | grep -q '"from-dav.txt"'
check "a WebDAV delete lands in the trash" $?

# --- guards -----------------------------------------------------------------------------------------------
[ "$(code -H 'Host: evil.example' $B/login)" = 421 ] && [ "$(code -H "Host: 127.0.0.1.evil.example" $B/login)" = 421 ]; check "an unknown Host is refused (421)" $?
curl -s -m 10 -H "Host: $NAME.local:$PORT" $B/login | grep -q '<title>Sign in to Akramium Home</title>'; check "a configured host name is answered" $?
[ "$(code -b "$J" -H 'Origin: http://evil.example' -H 'content-type: application/json' -d '{"name":"x"}' $B/api/drive/folders)" = 403 ] \
  && [ "$(code -b "$J" -H 'Sec-Fetch-Site: cross-site' -H 'content-type: application/json' -d '{"name":"x"}' $B/api/drive/folders)" = 403 ]
check "a write from another site is refused (403)" $?
# control first: a same-origin write is visible in the listing, so the absence below means something
code -b "$J" -H "Origin: $B" -H "$JSON" -d '{"name":"control-folder"}' $B/api/drive/folders > /dev/null
listing=$(curl -s -b "$J" $B/api/drive/files)
echo "$listing" | grep -q '"name":"control-folder"' && ! echo "$listing" | grep -q '"name":"x"'; check "the refused write changed nothing (and an allowed one did)" $?
for _ in 1 2 3 4 5; do code -H 'content-type: application/json' -d '{"name":"alice","password":"wrong wrong"}' $B/api/login > /dev/null; done
curl -s -D "$T/h" -o /dev/null -H 'content-type: application/json' -d '{"name":"alice","password":"correct horse"}' $B/api/login
grep -q '^HTTP/1.1 429' "$T/h" && grep -qi '^retry-after: [1-9]' "$T/h"; check "five wrong passwords make the address wait (429 with Retry-After)" $?
curl -s -m 10 -b "$J" $B/api/me | grep -q '"name":"alice"'; check "a lockout does not sign out people already in" $?

# --- on the network -----------------------------------------------------------------------------------------
if command -v dns-sd > /dev/null; then
  (dns-sd -G v4 "$NAME.local" > "$T/mdns" 2>&1 & P=$!; sleep 4; kill $P 2>/dev/null)
  grep -qE "$NAME\.local\.[[:space:]]+[0-9]+\.[0-9]+\." "$T/mdns"; check "the daemon's name resolves on the network (mDNS)" $?
elif command -v avahi-resolve > /dev/null; then
  avahi-resolve -4 -n "$NAME.local" 2>/dev/null | grep -qE '[0-9]+\.[0-9]+\.'; check "the daemon's name resolves on the network (mDNS)" $?
elif getent hosts localhost > /dev/null 2>&1 && grep -qE '^hosts:.*mdns' /etc/nsswitch.conf 2>/dev/null; then
  sleep 2; getent hosts "$NAME.local" | grep -qE '^[0-9a-f]'; check "the daemon's name resolves on the network (mDNS)" $?
else
  skip "the daemon's name resolves on the network (mDNS)" "no dns-sd, avahi-resolve or mdns in nsswitch here"
fi

# Idle: nothing but our own listener and mDNS. Any TCP connection the daemon opened to
# somewhere else is a leak.
sleep "$IDLE_SECONDS"
if command -v lsof > /dev/null; then
  # control: lsof must see this process listening, or an empty list below proves nothing
  seen=$(lsof -nP -a -p "$PID" -iTCP -sTCP:LISTEN 2>/dev/null | grep -c ":$PORT")
  out=$(lsof -nP -a -p "$PID" -iTCP -sTCP:ESTABLISHED,SYN_SENT 2>/dev/null | awk 'NR>1 {print $9}' | grep -v -- "->127.0.0.1:" | grep -v -- "->\[::1\]:" || true)
  kill -0 "$PID" 2>/dev/null && [ "$seen" -ge 1 ] && [ -z "$out" ]; check "no outbound connection in $IDLE_SECONDS idle seconds" $? "listening seen: $seen; outbound: $out"
else
  skip "no outbound connection while idle" "no lsof here"
fi

# --- dependencies -----------------------------------------------------------------------------------------------
if [ ! -f "$(dirname "$0")/Cargo.lock" ]; then
  skip "cargo audit finds no known vulnerability" "no Cargo.lock beside this script: run it from the source tree"
elif cargo audit --version > /dev/null 2>&1; then
  (cd "$(dirname "$0")" && cargo audit -q > "$T/audit" 2>&1); check "cargo audit finds no known vulnerability" $? "$(grep -c '^ID:' "$T/audit") advisories"
else
  skip "cargo audit finds no known vulnerability" "cargo-audit is not installed: cargo install cargo-audit"
fi

echo
if [ "$fails" = 0 ]; then echo "check-home: all checks passed"; else echo "check-home: $fails FAILED"; fi
exit "$fails"
