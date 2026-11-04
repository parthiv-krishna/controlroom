# controlroom-pi: the pinned pi with controlroom's extension layering (spec §9, §14; research
# §4 "mechanism B").
#
#   controlroom-pi [--keep SOURCE]... [--] [pi arguments...]
#
# Runs `pi --no-extensions -e <@controlroom/pi> -e <kept extension>... <pi arguments>`: no
# configured, discovered or built-in extension loads, only controlroom's and the keep-list.
# Auth, models, settings and sessions still come from the agent dir ($PI_CODING_AGENT_DIR, or
# ~/.pi/agent), which this launcher never writes.
#
# A keep-list SOURCE is an absolute path (an extension file or package directory, e.g. a Nix
# store path), `npm:<name>[@version]` (the package as the user's pi installed it, under
# <agent dir>/npm/node_modules) or `builtin:<name>`. Extensions that overlap controlroom's own
# tools (subagents, intercom, background tasks, goals, threadroom, tmux and vim integrations)
# are refused, and so is pi-lens: pi runs on the controlroom server, where the file tools'
# paths are `machine:path` on other machines, which its formatter, LSP and read guard would
# treat as local files.
#
# Only shell builtins run before the final exec: a sandboxed session (nono) may only execute
# programs from PATH directories, and this script's exec target.

die() {
  printf 'controlroom-pi: %s\n' "$*" >&2
  exit 2
}

# The unscoped package name (`@scope/name` -> `name`) must not be an excluded extension.
check_not_excluded() {
  local source=$1 name=${2##*/}
  case $name in
    pi-subagents | pi-intercom | pi-background-tasks | pi-goal | threadroom-pi | *tmux* | *vim*)
      die "refusing to keep $source: $2 overlaps controlroom's own tools"
      ;;
    pi-lens | pi-lens-[0-9]*)
      die "refusing to keep $source: $2 acts on local files, but controlroom's file tools work on remote machines"
      ;;
  esac
}

# The `name` of a package directory's package.json (the first "name" key), if any.
package_name() {
  local line pattern='^[[:space:]]*"name"[[:space:]]*:[[:space:]]*"([^"]+)"'
  [[ -f $1/package.json ]] || return 0
  while IFS= read -r line; do
    if [[ $line =~ $pattern ]]; then
      printf '%s' "${BASH_REMATCH[1]}"
      return 0
    fi
  done <"$1/package.json"
}

keep=()
while (($# > 0)); do
  case $1 in
    --keep)
      (($# >= 2)) || die "--keep needs a source"
      keep+=("$2")
      shift 2
      ;;
    --keep=*)
      keep+=("${1#--keep=}")
      shift
      ;;
    --)
      shift
      break
      ;;
    *) break ;;
  esac
done

for arg in "$@"; do
  case $arg in
    -e | --extension | --extension=* | -ne | --no-extensions)
      die "extensions come from --keep only (got $arg)"
      ;;
  esac
done

# @controlroom/pi in the bundle; empty in a bundle built without it.
controlroom_extension="@extension@"
agent_dir=${PI_CODING_AGENT_DIR:-${HOME:?HOME is not set}/.pi/agent}
extensions=()
if [[ -n $controlroom_extension ]]; then
  extensions+=(-e "$controlroom_extension")
fi
for source in "${keep[@]}"; do
  case $source in
    npm:*)
      spec=${source#npm:}
      if [[ $spec == @*/* ]]; then
        rest=${spec#*/}
        name=${spec%%/*}/${rest%%@*}
      else
        name=${spec%%@*}
      fi
      [[ -n $name ]] || die "invalid keep-list source $source"
      check_not_excluded "$source" "$name"
      path=$agent_dir/npm/node_modules/$name
      [[ -e $path ]] || die "kept extension $source is not installed (no $path); install it with pi first"
      extensions+=(-e "$path")
      ;;
    builtin:*)
      check_not_excluded "$source" "${source#builtin:}"
      extensions+=(-e "$source")
      ;;
    /*)
      [[ -e $source ]] || die "kept extension $source does not exist"
      check_not_excluded "$source" "$(package_name "$source")"
      base=${source##*/}
      # Without a Nix store hash, which could spell anything.
      if [[ $base =~ ^[0-9a-z]{32}-(.*)$ ]]; then
        base=${BASH_REMATCH[1]}
      fi
      check_not_excluded "$source" "$base"
      extensions+=(-e "$source")
      ;;
    *) die "invalid keep-list source $source (expected an absolute path, npm:<name> or builtin:<name>)" ;;
  esac
done

exec "@pi@" --no-extensions "${extensions[@]}" "$@"
