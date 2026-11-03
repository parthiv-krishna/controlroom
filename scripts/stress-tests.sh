#!/usr/bin/env bash
# Runs integration suites over and over under load to shake out timing-dependent tests.
#
# Run it inside the dev shell (`nix develop -c scripts/stress-tests.sh ...`). It builds the
# test binaries once, starts busy loops to load the CPUs (or stress-ng CPU, I/O and memory
# workers with -m), then runs each suite's binaries repeatedly (several copies at a time with
# -j) and keeps the log of every failed run.
#
# Examples:
#   scripts/stress-tests.sh -n 50 serve
#   scripts/stress-tests.sh -n 30 -j 4 runtime supervisor controller web
#   scripts/stress-tests.sh -n 50 -j 8 -l 20 -m serve  # mixed CPU, I/O and memory load
#   scripts/stress-tests.sh -n 30 -c 0,1 -l 4 serve    # everything pinned to two CPUs
#   scripts/stress-tests.sh -n 100 serve -- queue::     # extra args go to the test binaries
set -euo pipefail

usage() {
	cat <<'EOF'
usage: stress-tests.sh [options] SUITE... [-- TEST-ARGS...]

Suites: serve, runtime, supervisor, controller, web, all (those five), other (the rest of
the workspace).

Options:
  -n RUNS     runs of each suite (default 30)
  -j JOBS     copies of a suite running at the same time (default 2)
  -l LOAD     busy-loop processes loading the CPUs (default: number of CPUs; 0 for none)
  -m          mixed load: stress-ng with LOAD CPU workers plus I/O, disk and memory workers
              (stress-ng from PATH, else from this flake's nixpkgs)
  -c CPUS     pin tests and load to these CPUs (a taskset list such as 0,1)
  -t THREADS  --test-threads for each test binary (default: libtest's default)
  -T SECONDS  kill a test binary that runs longer than this (default 900)
  -o DIR      directory for failure logs (default: a new temporary directory)
EOF
	exit 2
}

runs=30 jobs=2 load=$(nproc) mixed="" cpus="" threads="" limit=900 logs=""
while getopts "n:j:l:mc:t:T:o:h" opt; do
	case $opt in
	n) runs=$OPTARG ;;
	j) jobs=$OPTARG ;;
	l) load=$OPTARG ;;
	m) mixed=1 ;;
	c) cpus=$OPTARG ;;
	t) threads=$OPTARG ;;
	T) limit=$OPTARG ;;
	o) logs=$OPTARG ;;
	*) usage ;;
	esac
done
shift $((OPTIND - 1))

suites=()
while (($#)) && [[ $1 != -- ]]; do
	if [[ $1 == all ]]; then
		suites+=(serve runtime supervisor controller web)
	else
		suites+=("$1")
	fi
	shift
done
(($#)) && shift
test_args=("$@")
((${#suites[@]})) || usage
[[ -n $threads ]] && test_args+=("--test-threads=$threads")

# The cargo test selections of each suite, one per line.
cargo_args() {
	case $1 in
	serve) echo "-p controlroomd --test serve" ;;
	runtime) echo "-p controlroom-runtime --lib" ;;
	supervisor)
		echo "-p controlroom-supervisor --lib"
		echo "-p controlroomd --test supervise_io --test supervise_lifecycle --test supervise_protocol"
		;;
	controller) echo "-p controlroom-controller --lib --tests" ;;
	web) echo "-p controlroom-web --lib --test integration" ;;
	# Every other test of the workspace.
	other)
		echo "-p controlroom-client-core -p controlroom-drivers -p controlroom-fake-agent -p controlroom-fixture-tool -p controlroom-protocol -p controlroom-sandbox -p controlroom-transcript"
		echo "-p controlroomd --lib --bins --test acceptance_agent --test acceptance_pi_nono --test acceptance_serve --test cli --test connect --test credential --test ctl_render"
		;;
	*) return 1 ;;
	esac
}
for suite in "${suites[@]}"; do
	cargo_args "$suite" >/dev/null || {
		echo "unknown suite: $suite" >&2
		exit 2
	}
done

cd "$(dirname "$0")/.."
logs=${logs:-$(mktemp -d -t controlroom-stress.XXXXXX)}
mkdir -p "$logs"

# Build everything first, so the load doesn't slow the build and builds don't race the runs.
echo "building test binaries..." >&2
cargo build --quiet -p controlroom-fake-agent
export CONTROLROOM_FAKE_AGENT=$PWD/target/debug/controlroom-fake-agent
declare -A binaries
for suite in "${suites[@]}"; do
	binaries[$suite]=$(
		cargo_args "$suite" | while read -r selection; do
			# shellcheck disable=SC2086 # word splitting of the selection is intended
			cargo test --no-run --message-format=json $selection 2>/dev/null |
				grep '"profile":{[^}]*"test":true}' | grep -o '"executable":"[^"]*"' | cut -d'"' -f4
		done | sort -u
	)
	[[ -n ${binaries[$suite]} ]] || {
		echo "no test binaries for $suite" >&2
		exit 1
	}
done

pin=()
[[ -n $cpus ]] && pin=(taskset -c "$cpus")

loaders=()
# shellcheck disable=SC2329 # run by the trap
stop_load() { ((${#loaders[@]})) && kill "${loaders[@]}" 2>/dev/null || true; }
trap stop_load EXIT
if [[ -n $mixed ]]; then
	stress_ng=$(command -v stress-ng ||
		echo "$(nix build --inputs-from . nixpkgs#stress-ng --no-link --print-out-paths)/bin/stress-ng")
	mkdir -p "$logs/stress-ng"
	workers=(--iomix 2 --hdd 2 --hdd-bytes 128m --vm 2 --vm-bytes 256m)
	# `--cpu 0` would mean one worker per CPU.
	((load > 0)) && workers+=(--cpu "$load")
	"${pin[@]}" "$stress_ng" --quiet --temp-path "$logs/stress-ng" "${workers[@]}" &
	loaders+=($!)
else
	for ((i = 0; i < load; i++)); do
		"${pin[@]}" sh -c 'while :; do :; done' &
		loaders+=($!)
	done
fi

# Runs `suite` once (all its binaries); keeps the log on failure.
run_once() {
	local suite=$1 run=$2 log
	log=$logs/$suite-$run.log
	for binary in ${binaries[$suite]}; do
		echo "=== $binary" >>"$log"
		if ! timeout --signal=KILL "$limit" "${pin[@]}" "$binary" "${test_args[@]}" >>"$log" 2>&1; then
			echo "FAIL $suite run $run ($(basename "$binary")): $log" >&2
			grep -E '^test .* FAILED$|^    [a-z_:0-9]+$|timed out' "$log" | sed 's/^/    /' >&2 || true
			return 1
		fi
	done
	rm -f "$log"
}

# Worker `w` of `jobs` runs every jobs-th run; prints its failure count.
worker() {
	local suite=$1 w=$2 failed=0
	for ((run = w; run < runs; run += jobs)); do
		run_once "$suite" "$run" || failed=$((failed + 1))
	done
	echo "$failed"
}

status=0
for suite in "${suites[@]}"; do
	start=$SECONDS
	results=$(mktemp)
	workers=()
	for ((w = 0; w < jobs; w++)); do
		worker "$suite" "$w" >>"$results" &
		workers+=($!)
	done
	wait "${workers[@]}"
	failed=$(awk '{ s += $1 } END { print s + 0 }' "$results")
	rm -f "$results"
	echo "$suite: $((runs - failed))/$runs runs passed in $((SECONDS - start))s (load $load${mixed:+ mixed}, jobs $jobs${cpus:+, cpus $cpus})"
	((failed == 0)) || status=1
done
echo "failure logs: $logs"
exit $status
