# The models helper (Settings › Models) against pi's own code: the model list of a config (with
# a provider's model definitions, which define an added account's provider), and sign-in and
# sign-out through pi's Radius OAuth flow pointed at a mock provider (never a real provider or
# credential): device code, browser with the redirect URL pasted, a bad code, a cancel, and two
# accounts signed in independently, each in its own directory, refreshed there only.
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
trap 'kill "$MOCK_SERVER_PID" 2>/dev/null || true; [ -n "${login_pid:-}" ] && kill "$login_pid" 2>/dev/null || true' EXIT
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
[ "$("$JQ" '.definitions | length' <<<"$list")" = 0 ] || fail "definitions unasked: $list"

# Model definitions: every model of a provider, as models.json entries pi takes for a provider
# of its own (an added account).
list=$(helper list --definitions anthropic --definitions gw) || fail "list --definitions failed"
[ "$("$JQ" -r '.definitions.gw[0].id' <<<"$list")" = gw-model ] || fail "gw definitions: $list"
"$JQ" -e '.definitions.anthropic | length > 0 and all(.api and .baseUrl and .id)' <<<"$list" >/dev/null ||
  fail "anthropic definitions: $list"
account=$work/account-check
mkdir -p "$account"
"$JQ" '{providers: {"anthropic-work": {name: "Anthropic (work)", models: .definitions.anthropic}}}' <<<"$list" >"$account/models.json"
echo '{"anthropic-work": {"type": "api_key", "key": "controlroom-placeholder"}}' >"$account/auth.json"
listed=$("$BUNDLE/bin/controlroom-pi-models" list --agent-dir "$account") || fail "account list failed"
[ "$("$JQ" '[.models[] | select(.provider == "anthropic-work")] | length' <<<"$listed")" = \
  "$("$JQ" '.definitions.anthropic | length' <<<"$list")" ] || fail "account models: $listed"

# One sign-in on a coprocess (in DIR, the agent dir by default): `send` writes a line, `expect
# KIND` reads steps until one of KIND.
start_login() {
  local dir=${1:-$agent}
  coproc LOGIN { exec "$BUNDLE/bin/controlroom-pi-models" login --agent-dir "$dir" --provider mock-sub; }
  # Kept apart from bash's LOGIN_PID, which bash unsets as soon as the coprocess exits (a
  # sign-in that finishes quickly would leave `finish` waiting on an unset variable).
  login_pid=$!
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
  wait "$login_pid" || status=$?
  login_pid=
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

# Two accounts sign in independently, each with pi's login in its own directory (as Settings ›
# Models does for an added account); signing one out or refreshing it leaves the other alone.
for name in personal work; do
  mkdir -p "$work/accounts/$name"
  cp "$agent/models.json" "$work/accounts/$name/"
  start_login "$work/accounts/$name"
  expect choose
  send '{"input": "device-code"}'
  expect device_code
  expect 'done'
  finish 0
done
personal_access=$("$JQ" -r '."mock-sub".access' "$work/accounts/personal/auth.json")
work_access=$("$JQ" -r '."mock-sub".access' "$work/accounts/work/auth.json")
[ -n "$personal_access" ] && [ "$personal_access" != "$work_access" ] || fail "accounts share a token"
signed_in && fail "an account's sign-in reached the agent dir"
work_before=$(cat "$work/accounts/work/auth.json")
# The OAuth helper refreshes the personal account (expired here) in its own directory only.
"$JQ" '."mock-sub".expires = 1' "$work/accounts/personal/auth.json" >"$work/expired.json"
mv "$work/expired.json" "$work/accounts/personal/auth.json"
refreshed=$("$BUNDLE/bin/controlroom-pi-oauth" --agent-dir "$work/accounts/personal" --provider mock-sub \
  --valid-for-secs 600) || fail "refreshing the personal account failed"
[ "$refreshed" != "$personal_access" ] || fail "not refreshed"
[ "$(cat "$work/accounts/work/auth.json")" = "$work_before" ] || fail "refreshing one account touched the other"
"$BUNDLE/bin/controlroom-pi-models" logout --agent-dir "$work/accounts/personal" --provider mock-sub ||
  fail "signing the personal account out failed"
[ "$("$JQ" -r '."mock-sub".type' "$work/accounts/work/auth.json")" = oauth ] || fail "the work account was signed out"

# Usage errors.
status=0
helper login 2>/dev/null || status=$?
[ "$status" = 2 ] || fail "usage error exited $status"
echo "models helper: ok"
