# The OAuth helper (spec §11.3) against a mock token endpoint and a fake auth.json; never a
# real provider or credential. The provider is pi's built-in Radius OAuth flow pointed at the
# mock through models.json, so the refresh runs through pi's own code end to end.
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

# The mock prints its port once it listens: reading it is the readiness gate.
coproc MOCK_SERVER { exec "$NODE" "$MOCK" "$log"; }
trap 'kill "$MOCK_SERVER_PID" 2>/dev/null || true' EXIT
read -r port <&"${MOCK_SERVER[0]}" || fail "the mock didn't start"

cat >"$agent/models.json" <<EOF
{"providers": {"mock-sub": {"name": "Mock", "oauth": "radius", "baseUrl": "http://127.0.0.1:$port/v1"}}}
EOF
# A credential in the shape of auth.json; `expires` in the past forces a refresh.
credential() {
  printf '{"mock-sub": {"type": "oauth", "access": "%s", "refresh": "%s", "expires": %s},' "$1" "$2" "$3"
  printf ' "gateway": {"type": "api_key", "key": "sk-test-key"}}\n'
}
helper() {
  "$BUNDLE/bin/controlroom-pi-oauth" --agent-dir "$agent" "$@"
}

# An expired token is refreshed once, through pi, and the rotated tokens are persisted.
credential access-0 refresh-0 1000 >"$agent/auth.json"
token=$(helper --provider mock-sub --valid-for-secs 600) || fail "refresh failed"
[ "$token" = access-1 ] || fail "token: $token"
[ "$("$JQ" -r '."mock-sub".refresh' "$agent/auth.json")" = refresh-1 ] || fail "refresh token not rotated"
[ "$("$JQ" -r '.gateway.key' "$agent/auth.json")" = sk-test-key ] || fail "other credentials changed"
[ "$(cat "$log")" = "refresh_token refresh-0" ] || fail "requests: $(cat "$log")"

# A token that is still valid long enough is served as is.
[ "$(helper --provider mock-sub --valid-for-secs 600)" = access-1 ] || fail "valid token not reused"
[ "$(wc -l <"$log")" -eq 1 ] || fail "refreshed a valid token: $(cat "$log")"
# One that expires within the requested validity is refreshed before it is served.
credential access-soon refresh-soon "$((($(date +%s) + 120) * 1000))" >"$agent/auth.json"
[ "$(helper --provider mock-sub --valid-for-secs 600)" = access-2 ] || fail "expiring token served"
[ "$(tail -n 1 "$log")" = "refresh_token refresh-soon" ] || fail "requests: $(cat "$log")"

# Concurrent helpers (as several sessions' nono proxies) refresh once: pi's auth.json lock.
credential access-x refresh-x 1000 >"$agent/auth.json"
: >"$log"
pids=()
for i in 1 2 3 4 5; do
  helper --provider mock-sub >"$work/concurrent-$i" &
  pids+=("$!")
done
wait "${pids[@]}"
[ "$(cat "$log")" = "refresh_token refresh-x" ] || fail "concurrent refreshes: $(cat "$log")"
for i in 1 2 3 4 5; do
  [ "$(cat "$work/concurrent-$i")" = access-3 ] || fail "concurrent helper $i: $(cat "$work/concurrent-$i")"
done

# Failures exit 1 with a message that quotes no token.
credential access-old rt-revoked 1000 >"$agent/auth.json"
if helper --provider mock-sub >"$work/out" 2>"$work/err"; then fail "a revoked refresh token succeeded"; fi
[ ! -s "$work/out" ] || fail "printed a token on failure"
grep -q 'refreshing mock-sub failed' "$work/err" || fail "stderr: $(cat "$work/err")"
if grep -q "access-old\\|rt-revoked" "$work/err"; then fail "stderr quotes a token: $(cat "$work/err")"; fi
[ "$("$JQ" -r '."mock-sub".refresh' "$agent/auth.json")" = rt-revoked ] || fail "auth.json changed on failure"

if helper --provider gateway 2>"$work/err"; then fail "an API-key provider was served"; fi
grep -q 'no OAuth credential' "$work/err" || fail "stderr: $(cat "$work/err")"
if helper --provider missing 2>/dev/null; then fail "a missing provider was served"; fi
status=0
helper 2>/dev/null || status=$?
[ "$status" = 2 ] || fail "usage error exited $status"

# Nothing but auth.json (and pi's transient lock) is ever written into the agent dir.
[ "$(cd "$agent" && echo *)" = "auth.json models.json" ] || fail "agent dir: $(ls -A "$agent")"
echo "oauth helper: ok"
