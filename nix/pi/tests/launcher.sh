# Launcher layering (spec §9, §14): a fake agent dir whose settings configure kept and excluded
# extensions, plus an auto-discovered one. Through controlroom-pi only @controlroom/pi and the
# keep-list load, excluded ones are refused, and the agent dir's config is left as it was.
#
# Inputs (environment): BUNDLE (controlroom-pi built with the stub extension), JQ.
# Runs offline; HOME and the agent dir are scratch directories.

work=$(mktemp -d)
export HOME=$work/home PI_OFFLINE=1 PI_SKIP_VERSION_CHECK=1 PI_TELEMETRY=0
agent=$work/agent
export PI_CODING_AGENT_DIR=$agent
mkdir -p "$HOME" "$agent/extensions" "$agent/npm/node_modules/@scope"

# A package with one extension that registers the command $3.
package() {
  mkdir -p "$1"
  printf '{"name": "%s", "version": "1.0.0", "pi": {"extensions": ["./index.ts"]}}\n' "$2" >"$1/package.json"
  printf 'export default function (pi: any) { pi.registerCommand("%s", { handler: async () => {} }); }\n' "$3" >"$1/index.ts"
}
package "$agent/npm/node_modules/kept-npm" kept-npm kept-npm
package "$agent/npm/node_modules/@scope/kept-scoped" @scope/kept-scoped kept-scoped
package "$agent/npm/node_modules/pi-subagents" pi-subagents excluded-subagents
package "$agent/npm/node_modules/pi-intercom" pi-intercom excluded-intercom
package "$work/store/cccccccccccccccccccccccccccccccc-pi-lens-0.4.0" pi-lens excluded-lens
package "$work/store/aaaa-provider" kept-provider kept-provider
package "$work/store/bbbb-pi-vimmode-0.9.0" unrelated-name vim
printf 'export default function (pi: any) { pi.registerCommand("auto-discovered", { handler: async () => {} }); }\n' \
  >"$agent/extensions/auto.ts"
cat >"$agent/settings.json" <<EOF
{"quietStartup": true, "packages": ["npm:kept-npm@1.0.0", "npm:@scope/kept-scoped@1.0.0",
  "npm:pi-subagents@1.0.0", "npm:pi-intercom@1.0.0", "$work/store/aaaa-provider"]}
EOF
echo '{"providers": {}}' >"$agent/models.json"
echo '{}' >"$agent/auth.json"
config_before=$(cat "$agent/settings.json" "$agent/models.json" "$agent/auth.json")

# The commands pi reports for `controlroom-pi <args>`, sorted, with their sources.
commands() {
  echo '{"id": "1", "type": "get_commands"}' |
    "$BUNDLE/bin/controlroom-pi" "$@" --mode rpc --no-themes --no-session |
    "$JQ" -r 'select(.id == "1") | .data.commands[] | select(.source == "extension") | .name' |
    sort | tr '\n' ' '
}

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# Plain pi would load everything; the launcher loads controlroom's extension and the keep-list.
got=$(commands --keep npm:kept-npm --keep npm:@scope/kept-scoped@1.0.0 --keep "$work/store/aaaa-provider")
[ "$got" = "controlroom kept-npm kept-provider kept-scoped " ] || fail "kept: got '$got'"
got=$(commands)
[ "$got" = "controlroom " ] || fail "nothing kept: got '$got'"

# Excluded extensions are refused by npm name, package.json name or path, and so is a bypass.
refused() {
  if output=$("$BUNDLE/bin/controlroom-pi" "$@" --mode rpc </dev/null 2>&1); then
    fail "accepted: $*"
  fi
  case $output in *controlroom-pi:*) ;; *) fail "no launcher error for $*: $output" ;; esac
}
refused --keep npm:pi-subagents
refused --keep npm:pi-lens@1.0.0
refused --keep npm:@narumitw/pi-goal@0.54.4
refused --keep npm:pi-tmux-session-map
refused --keep "$agent/npm/node_modules/pi-intercom"
refused --keep "$work/store/bbbb-pi-vimmode-0.9.0"
refused --keep "$work/store/cccccccccccccccccccccccccccccccc-pi-lens-0.4.0"
refused --keep npm:not-installed
refused --keep relative/path
refused -e "$agent/npm/node_modules/pi-subagents"
refused --keep npm:kept-npm -- --extension="$agent/npm/node_modules/pi-subagents"

config_after=$(cat "$agent/settings.json" "$agent/models.json" "$agent/auth.json")
[ "$config_before" = "$config_after" ] || fail "the agent dir's config changed"
echo "launcher: ok"
