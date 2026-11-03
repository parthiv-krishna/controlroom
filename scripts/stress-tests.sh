#!/usr/bin/env bash
# Runs integration suites over and over under CPU load to shake out timing-dependent tests.
#
# Run it inside the dev shell (`nix develop -c scripts/stress-tests.sh ...`). It builds the
# test binaries once, starts busy loops to load the CPUs, then runs each suite's binaries
# repeatedly (several copies at a time with -j) and keeps the log of every failed run.
#
# Examples:
#   scripts/stress-tests.sh -n 50 serve
#   scripts/stress-tests.sh -n 30 -j 4 runtime supervisor controller web
#   scripts/stress-tests.sh -n 30 -c 0,1 -l 4 serve    # everything pinned to two CPUs
#   scripts/stress-tests.sh -n 100 serve -- queue::     # extra args go to the test binaries
set -euo pipefail

usage() {
	cat <<'EOF'
usage: stress-tests.sh [options] SUITE... [-- TEST-ARGS...]

Suites: serve, runtime, supervisor, controller, web, all.

Options:
  -n RUNS     runs of each suite (default 30)
  -j JOBS     copies of a suite running at the same time (default 2)
  -l LOAD     busy-loop processes loading the CPUs (default: number of CPUs; 0 for none)
  -c CPUS     pin tests and load to these CPUs (a taskset list such as 0,1)
  -t THREADS  --test-threads for each test binary (default: libtest's default)
  -T SECONDS  kill a test binary that runs longer than this (default 900)
  -o DIR      directory for failure logs (default: a new temporary directory)
EOF
	exit 2
}

runs=30 jobs=2 load=$(nproc) cpus="" threads="" limit=900 logs=""
while getopts "n:j:l:c:t:T:o:h" opt; do
	case $opt in
	n) runs=$OPTARG ;;
	j) jobs=$OPTARG ;;
	l) load=$OPTARG ;;
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

# The cargo test selection of each suite.
cargo_args() {
	case $1 in
	serve) echo "-p controlroomd --test serve" ;;
	runtime) echo "-p controlroom-runtime --lib" ;;
	supervisor) echo "-p controlroom-supervisor --lib -p controlroomd --test supervise_io --test supervise_lifecycle --test supervise_protocol" ;;
	controller) echo "-p controlroom-controller --lib --tests" ;;
	web) echo "-p controlroom-web --lib --test integration" ;;
	*)
		echo "unknown suite: $1" >&2
		exit 2
		;;
	esac
}

cd "$(dirname "$0")/.."
logs=${logs:-$(mktemp -d -t controlroom-stress.XXXXXX)}
mkdir -p "$logs"

# Build everything first, so the load doesn't slow the build and builds don't race the runs.
echo "building test binaries..." >&2
cargo build --quiet -p controlroom-fake-agent
export CONTROLROOM_FAKE_AGENT=$PWD/target/debug/controlroom-fake-agent
declare -A binaries
for suite in "${suites[@]}"; do
	# shellcheck disable=SC2046 # word splitting of the cargo selection is intended
	binaries[$suite]=$(
		cargo test --no-run --message-format=json $(cargo_args "$suite") 2>/dev/null |
			grep '"profile":{[^}]*"test":true}' | grep -o '"executable":"[^"]*"' | cut -d'"' -f4 | sort -u
	)
	[[ -n ${binaries[$suite]} ]] || {
		echo "no test binaries for $suite" >&2
		exit 1
	}
done

pin=()
[[ -n $cpus ]] && pin=(taskset -c "$cpus")

loaders=()
stop_load() { ((${#loaders[@]})) && kill "${loaders[@]}" 2>/dev/null || true; }
trap stop_load EXIT
for ((i = 0; i < load; i++)); do
	"${pin[@]}" sh -c 'while :; do :; done' &
	loaders+=($!)
done

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
	echo "$suite: $((runs - failed))/$runs runs passed in $((SECONDS - start))s (load $load, jobs $jobs${cpus:+, cpus $cpus})"
	((failed == 0)) || status=1
done
echo "failure logs: $logs"
exit $status
