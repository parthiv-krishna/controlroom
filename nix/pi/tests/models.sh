# The models helper (Settings › Models) against pi's own code: the model list of a config, and
# sign-in and sign-out through pi's Radius OAuth flow pointed at a mock provider (never a real
# provider or credential): device code, browser with the redirect URL pasted, a bad code, and a
# cancel.
#
# Inputs (environment): BUNDLE (controlroom-pi), NODE, JQ, MOCK (mock-oauth.mjs).

work=$(mktemp -d)
export HOME=$work/home
agent=$work/agent
mkdir -p "$HOME" "$agent"
log=$work/token-requests
: >"$log"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

coproc MOCK_SERVER { exec "$NODE" "$MOCK" "$log"; }
trap 'kill "$MOCK_SERVER_PID" 2>/dev/null || true; [ -n "${LOGIN_PID:-}" ] && kill "$LOGIN_PID" 2>/dev/null || true' EXIT
read -r port <&"${MOCK_SERVER[0]}" || fail "the mock didn't start"

cat >"$agent/models.json" <<EOF
{"providers": {
  "mock-sub": {"name": "Mock", "oauth": "radius", "baseUrl": "http://127.0.0.1:$port/v1"},
  "gw": {"baseUrl": "https://gw.example.com/v1", "api": "openai-completions", "apiKey": "controlroom-placeholder",
         "models": [{"id": "gw-model"}]}
}}
EOF
echo '{}' >"$agent/auth.json"
helper() {
  "$BUNDLE/bin/controlroom-pi-models" "$1" --agent-dir "$agent" "${@:2}"
}

# The list: every provider pi knows (sign-in capable or not), models of configured ones only.
list=$(helper list) || fail "list failed"
[ "$("$JQ" -r '.providers[] | select(.id == "mock-sub") | .oauth' <<<"$list")" = true ] || fail "list: $list"
[ "$("$JQ" -r '.providers[] | select(.id == "anthropic") | .oauth' <<<"$list")" = true ] || fail "no anthropic sign-in"
[ "$("$JQ" -r '.providers[] | select(.id == "google") | .oauth' <<<"$list")" = false ] || fail "google sign-in"
[ "$("$JQ" -r '[.models[] | select(.provider == "gw") | .id] | join(",")' <<<"$list")" = gw-model ] || fail "models: $list"
[ "$("$JQ" '[.models[] | select(.provider == "anthropic")] | length' <<<"$list")" = 0 ] || fail "unconfigured models listed"

# One sign-in on a coprocess: `send` writes a line, `expect KIND` reads steps until one of KIND.
start_login() {
  coproc LOGIN { exec "$BUNDLE/bin/controlroom-pi-models" login --agent-dir "$agent" --provider mock-sub; }
  LOGIN_PID=$!
}
send() {
  printf '%s\n' "$1" >&"${LOGIN[1]}"
}
expect() {
  local line kind
  while read -t 30 -r line <&"${LOGIN[0]}"; do
    kind=$("$JQ" -r '.step.kind' <<<"$line")
    if [ "$kind" = "$1" ]; then
      step=$line
      return 0
    fi
    [ "$kind" = progress ] || fail "expected $1, got $line"
  done
  fail "no $1 step"
}
finish() {
  local status=0
  wait "$LOGIN_PID" || status=$?
  LOGIN_PID=
  [ "$status" = "$1" ] || fail "the sign-in exited $status, not $1"
}
signed_in() {
  [ "$("$JQ" -r '."mock-sub".type' "$agent/auth.json")" = oauth ]
}

# Device code: the code is shown, and pi's polling finishes by itself.
start_login
expect choose
"$JQ" -e '.step.options | map(.id) | index("device-code")' <<<"$step" >/dev/null || fail "options: $step"
send '{"input": "device-code"}'
expect device_code
[ "$("$JQ" -r '.step.user_code' <<<"$step")" = ABCD-1234 ] || fail "device code: $step"
expect 'done'
finish 0
signed_in || fail "device code: not signed in"
grep -q 'device_code dc-1' "$log" || fail "requests: $(cat "$log")"

# Sign out, through pi.
helper logout --provider mock-sub || fail "logout failed"
signed_in && fail "still signed in"
if helper logout --provider mock-sub 2>/dev/null; then fail "signed out twice"; fi

# Browser: open the sign-in page, paste the URL the browser ends on (pi's loopback callback on
# this machine, which the helper delivers).
start_login
expect choose
send '{"input": "browser"}'
expect open_url
url=$("$JQ" -r '.step.url' <<<"$step")
state=$(sed -n 's/.*[?&]state=\([^&]*\).*/\1/p' <<<"$url")
redirect=$(sed -n 's/.*[?&]redirect_uri=\([^&]*\).*/\1/p' <<<"$url" | sed 's/%3A/:/g; s#%2F#/#g')
[ -n "$state" ] && [ -n "$redirect" ] || fail "sign-in page: $url"
send "{\"input\": \"$redirect?code=good&state=$state\"}"
expect 'done'
finish 0
signed_in || fail "browser: not signed in"
grep -q 'authorization_code good' "$log" || fail "requests: $(cat "$log")"
helper logout --provider mock-sub || fail "logout failed"

# A bad code fails with the provider's error, quoting no secret.
start_login
expect choose
send '{"input": "browser"}'
expect open_url
url=$("$JQ" -r '.step.url' <<<"$step")
state=$(sed -n 's/.*[?&]state=\([^&]*\).*/\1/p' <<<"$url")
send "{\"input\": \"http://127.0.0.1:1456/oauth/callback?code=bad&state=$state\"}"
expect failed
finish 1
signed_in && fail "signed in with a bad code"

# Cancel.
start_login
expect choose
send '{"cancel": true}'
expect failed
[ "$("$JQ" -r '.step.message' <<<"$step")" = cancelled ] || fail "cancel: $step"
finish 1

# Usage errors.
status=0
helper login 2>/dev/null || status=$?
[ "$status" = 2 ] || fail "usage error exited $status"
echo "models helper: ok"
