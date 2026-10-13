#!/usr/bin/env bash
# Tiered verification entry point for the Glyphst applications.
#
#   script/check.sh quick   every edit loop: fmt, clippy, tests
#   script/check.sh gate    before a PR: quick + doctests, audit, and a release
#                           Reader build
#   script/check.sh corpus  GPU page rendering against MuPDF on sample PDFs
#                           (GLYPHST_PDF_CORPUS, default: ../bak pdf.js corpus;
#                           GLYPHST_PDF_CORPUS_LIMIT files, default 100), and
#                           20 of them turned 90 degrees
#
# Options:
#   --keep-going  run every step and report all failures
#
# Each run writes step logs and summary.json to target/check-reports/<tier>-<time>/.

set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

if [[ -z ${IN_NIX_SHELL:-} ]] && command -v nix > /dev/null; then
    exec nix develop "${repo}" --command "$0" "$@"
fi

tier=${1:-}
shift || true
keep_going=0
for argument in "$@"; do
    case ${argument} in
        --keep-going) keep_going=1 ;;
        *) printf 'unknown option: %s\n' "${argument}" >&2; exit 2 ;;
    esac
done
case ${tier} in
    quick | gate | corpus) ;;
    *) sed -n '2,13p' "$0" >&2; exit 2 ;;
esac

cd "${repo}"
# App tests are quick, so the unoptimized dev profile (without debug info)
# minimizes compile time.
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0

run_dir="${repo}/target/check-reports/${tier}-$(date +%Y%m%dT%H%M%S)"
mkdir -p "${run_dir}"
started_at=$(date +%s)
failures=()
steps_json=()

write_summary() {
    local dirty
    dirty=$([[ -n $(git status --porcelain) ]] && echo true || echo false)
    {
        printf '{"tier":"%s","status":"%s","revision":"%s","dirty":%s,' \
            "${tier}" "$1" "$(git rev-parse HEAD)" "${dirty}"
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

printf 'check %s at %s\n' "${tier}" "$(git rev-parse --short HEAD)"
if [[ ${tier} == corpus ]]; then
    corpus=${GLYPHST_PDF_CORPUS:-${repo}/../bak/glyphst-pdf/references/pdfjs/test/pdfs}
    # Release build: rendering and MuPDF rasterization are too slow unoptimized.
    step corpus-build cargo build --release --locked \
        -p glyphst-pdf-gpu --example corpus
    step corpus "${repo}/target/release/examples/corpus" "${corpus}" \
        --limit "${GLYPHST_PDF_CORPUS_LIMIT:-100}" --out "${run_dir}/corpus"
    step corpus-rotated "${repo}/target/release/examples/corpus" "${corpus}" \
        --limit 20 --rotate 90 --out "${run_dir}/corpus-rotated"
    finish
fi
step fmt cargo fmt --all -- --check
# External Cargo commands only receive configuration placed after the subcommand.
step clippy cargo clippy --workspace --all-targets --all-features \
    --locked -- -D warnings
step tests cargo nextest run --workspace --all-targets --all-features \
    --locked --no-fail-fast

if [[ ${tier} == gate ]]; then
    step doctests cargo test --doc --workspace --all-features \
        --locked
    step audit cargo audit
    step reader-build cargo build --release --locked -p glyphst-reader
fi

finish
