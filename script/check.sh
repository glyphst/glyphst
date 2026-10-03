#!/usr/bin/env bash
# Tiered verification entry point for the Glyphst applications.
#
#   script/check.sh quick   every edit loop: fmt, clippy, tests
#   script/check.sh gate    before a PR or pin bump: quick + all-target tests,
#                           doctests, audit, release Reader, and the engine's
#                           Reader interaction profile in a headless compositor
#
# Options:
#   --local       build against sibling checkouts via .cargo/config.local.toml
#   --keep-going  run every step and report all failures
# Environment: GLYPHST_PDF_REPO (default ../glyphst-pdf),
#              GLYPHST_PDF_SUITE_ROOT (default ../pdf-test-suite)
#
# Each run writes step logs and summary.json to <suite>/reports/checks/reader-<tier>-<time>/.

set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

if [[ -z ${IN_NIX_SHELL:-} ]] && command -v nix > /dev/null; then
    exec nix develop "${repo}" --command "$0" "$@"
fi

tier=${1:-}
shift || true
keep_going=0
cargo_config=()
locked=(--locked)
for argument in "$@"; do
    case ${argument} in
        --keep-going) keep_going=1 ;;
        --local)
            # Local overrides rewrite Cargo.lock; never commit that change.
            cargo_config=(--config "${repo}/.cargo/config.local.toml")
            locked=()
            ;;
        *) printf 'unknown option: %s\n' "${argument}" >&2; exit 2 ;;
    esac
done
case ${tier} in
    quick | gate) ;;
    *) sed -n '2,15p' "$0" >&2; exit 2 ;;
esac

cd "${repo}"
engine=$(cd "${GLYPHST_PDF_REPO:-${repo}/../glyphst-pdf}" && pwd)
suite_root=$(cd "${GLYPHST_PDF_SUITE_ROOT:-${repo}/../pdf-test-suite}" && pwd)
# App tests are quick, so the unoptimized dev profile (without debug info)
# minimizes compile time; the engine's checks use its optimized `fast` profile.
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0

run_dir="${suite_root}/reports/checks/reader-${tier}-$(date +%Y%m%dT%H%M%S)"
mkdir -p "${run_dir}"
started_at=$(date +%s)
failures=()
steps_json=()

write_summary() {
    local dirty
    dirty=$([[ -n $(git status --porcelain) ]] && echo true || echo false)
    {
        printf '{"tier":"reader-%s","status":"%s","revision":"%s","dirty":%s,"local_deps":%s,' \
            "${tier}" "$1" "$(git rev-parse HEAD)" "${dirty}" \
            "$( ((${#cargo_config[@]})) && echo true || echo false)"
        printf '"seconds":%d,"steps":[' "$(($(date +%s) - started_at))"
        local IFS=,
        printf '%s' "${steps_json[*]}"
        printf ']}\n'
    } > "${run_dir}/summary.json"
}

finish() {
    if ((${#failures[@]} == 0)); then
        write_summary pass
        printf '\ncheck %s passed in %ds (%s)\n' "${tier}" "$(($(date +%s) - started_at))" "${run_dir}"
        exit 0
    fi
    write_summary fail
    printf '\ncheck %s FAILED: %s\nlogs: %s\n' "${tier}" "${failures[*]}" "${run_dir}" >&2
    exit 1
}

step() {
    local name=$1
    shift
    local log="${run_dir}/${name}.log" begin status=0
    begin=$(date +%s)
    printf '%-22s ' "${name}"
    "$@" > "${log}" 2>&1 || status=$?
    local seconds=$(($(date +%s) - begin))
    steps_json+=("{\"name\":\"${name}\",\"exit\":${status},\"seconds\":${seconds}}")
    if ((status == 0)); then
        printf 'ok    %5ds\n' "${seconds}"
        return 0
    fi
    printf 'FAIL  %5ds  (%s)\n' "${seconds}" "${log}"
    tail -n 25 "${log}" | sed 's/^/    /'
    failures+=("${name}")
    if ((keep_going == 0)); then
        finish
    fi
}

# Timing must not overlap builds or other GPU measurements.
with_perf_lock() {
    local waited=0
    while pgrep -u "$(id -u)" -x 'cargo|rustc' > /dev/null; do
        if ((waited >= 1800)); then
            printf 'builds still running after 30 minutes; refusing to measure\n'
            return 1
        fi
        sleep 10
        waited=$((waited + 10))
    done
    flock --timeout 7200 /tmp/glyphst-perf.lock "$@"
}

printf 'check %s at %s%s\n' "${tier}" "$(git rev-parse --short HEAD)" \
    "$( ((${#cargo_config[@]})) && echo ' (local sibling dependencies)')"
step fmt cargo fmt --all -- --check
step clippy cargo "${cargo_config[@]}" clippy --workspace --all-targets --all-features \
    "${locked[@]}" -- -D warnings
step tests cargo "${cargo_config[@]}" nextest run --workspace --all-targets --all-features \
    "${locked[@]}" --no-fail-fast

if [[ ${tier} == gate ]]; then
    reader="${repo}/target/release/glyphst-reader"
    step doctests cargo "${cargo_config[@]}" test --doc --workspace --all-features \
        "${locked[@]}"
    step audit cargo audit
    step disk bash "${engine}/scripts/disk-check.sh"
    step reader-build cargo "${cargo_config[@]}" build --release "${locked[@]}" -p glyphst-reader
    # The engine keeps its own build flags; this shell's mold RUSTFLAGS would
    # invalidate its cache.
    step engine-tools-build env -u RUSTFLAGS -u IN_NIX_SHELL bash -c "cd '${engine}' && \
        nix develop -c cargo build --release --locked -p glyphst-pdf-tools"
    step reader-interaction with_perf_lock "${engine}/scripts/headless-session.sh" -- \
        "${engine}/target/release/glyphst-pdf-tools" interaction suite --reader "${reader}" \
        --suite-root "${suite_root}" --profile common --repetitions 3
fi

finish
