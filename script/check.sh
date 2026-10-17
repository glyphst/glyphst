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
#   script/check.sh screens screenshots of every frame of design/main.pen in
#                           both themes, from a running Reader (needs the
#                           desktop session); GLYPHST_SCREENS_DOC and
#                           GLYPHST_SCREENS_DOC2 are the PDFs shown (default:
#                           the first two of ../pdf-samples)
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
    quick | gate | corpus | screens) ;;
    *) sed -n '2,18p' "$0" >&2; exit 2 ;;
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
if [[ ${tier} == screens ]]; then
    document=${GLYPHST_SCREENS_DOC:-$(find "${repo}/../pdf-samples" -name '*.pdf' | sort | sed -n 1p)}
    second=${GLYPHST_SCREENS_DOC2:-$(find "${repo}/../pdf-samples" -name '*.pdf' | sort | sed -n 2p)}
    shots="${run_dir}/screens"
    mkdir -p "${shots}"
    step reader-build cargo build --locked -p glyphst-reader
    reader="${repo}/target/debug/glyphst-reader"
    dense=${GLYPHST_SCREENS_DENSE:-${repo}/../pdf-samples/Murietta_Level1.pdf}
    # A dusk gradient (navy to orange, with a teal band) as the wallpaper,
    # written as a binary PPM so no image tool is needed.
    wallpaper=${GLYPHST_SCREENS_WALLPAPER:-${run_dir}/wallpaper.ppm}
    if [[ ! -e ${wallpaper} ]]; then
        LC_ALL=C awk 'BEGIN {
            w = 480; h = 300; printf "P6\n%d %d\n255\n", w, h
            for (y = 0; y < h; y++) {
                t = y / (h - 1)
                for (x = 0; x < w; x++) {
                    band = exp(-((t - 0.55) * 8) ^ 2) * (0.6 + 0.4 * x / w)
                    r = 24 + 200 * t ^ 1.6; g = 36 + 70 * t + 90 * band; b = 80 - 40 * t + 70 * band
                    printf "%c%c%c", r, g, b
                }
            }
        }' > "${wallpaper}"
    fi
    # name, then the Reader's arguments (`@` stands for the document, `#`
    # for the second one, `%` for a dense drawing, `^` for the wallpaper
    # and `@@` for a plain `@`;
    # a `--screenshot-*` argument picks the kind of shot instead of a plain
    # one).
    states=(
        "01-gallery --gallery"
        "02a-reader @ --sidebar outline --assistant-demo done"
        "02b-generating @ --screenshot-generating"
        "02d-find @ --find the"
        "02e-overview @ --command view.page_overview"
        "02e-overview-find @ --sidebar search --find the --command view.page_overview"
        "02f-split @ # --split right"
        "02g-present @ --command view.present"
        "02h-present-tools @ --command view.present --screenshot-present-tools"
        "02i-present-palette @ --command view.present --command app.command_palette"
        "03-outline @ --sidebar outline"
        "03-pages @ --sidebar pages"
        "03-pages-one @ --sidebar pages --command view.page_columns"
        "03-pages-dense % --sidebar pages"
        "03-bookmarks @ --sidebar bookmarks"
        "03-notes @ --sidebar notes"
        "03-search @ --sidebar search --find the"
        "03-translation @ --sidebar translation --translate-fake"
        "03-effects @ --sidebar effects"
        "04-home"
        "04b-new-tab --command app.new_tab"
        "04c-activity --command app.reading_activity"
        "04d-library --command app.library"
        "04e-home-narrow --window 960x900"
        "05-empty @ --assistant-demo empty"
        "05-menu @ --assistant-demo menu"
        "05-running @ --assistant-demo running"
        "05-error @ --assistant-demo error"
        "05-history @ --assistant-demo history"
        "05-edit @ --assistant-demo edit"
        "06a-palette @ --palette tra"
        "06a-palette-sections @ --palette @@"
        "06b-path-prompt @ --command app.open"
        "07a-appearance @ --settings appearance"
        "07a-background @ --settings appearance:background --background image --wallpaper ^ --colours background"
        "07a-colours @ --settings appearance:colours --background image --wallpaper ^ --colours background"
        "02a-wallpaper-theme @ --background image --wallpaper ^ --colours background"
        "07-reading @ --settings reading"
        "07-presenting @ --settings presenting"
        "07-effects @ --settings effects"
        "07-translation @ --settings translation"
        "07-ai @ --settings ai"
        "07-shortcuts --settings shortcuts"
        "07-shortcuts-glass --settings shortcuts --opacity 0.5"
    )
    for theme in dark light; do
        for state in "${states[@]}"; do
            read -r name rest <<< "${state}"
            args=()
            shot=--screenshot
            for arg in ${rest}; do
                case ${arg} in
                    @) args+=("${document}") ;;
                    @@) args+=("@") ;;
                    \#) args+=("${second}") ;;
                    %) args+=("${dense}") ;;
                    ^) args+=("${wallpaper}") ;;
                    --screenshot-*) shot=${arg} ;;
                    *) args+=("${arg}") ;;
                esac
            done
            step "${theme}-${name}" "${reader}" "${args[@]}" --theme "${theme}" \
                "${shot}" "${shots}/${theme}-${name}.png"
        done
    done
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
