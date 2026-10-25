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
#   script/check.sh screens [NAME...]
#                           screenshots of the frames of design/main.pen from
#                           Reader runs in a private headless sway, so no
#                           window reaches the desktop; NAMEs pick states by
#                           name or prefix (02d, 03-search, 07a), all by
#                           default; GLYPHST_SCREENS_DOC and
#                           GLYPHST_SCREENS_DOC2 are the PDFs shown (default:
#                           the first two of ../pdf-samples); each shot reads
#                           its own copy of the library as it was when the
#                           run started, so shots agree and never touch it;
#                           the CJK faces come from GLYPHST_FONTS_DIR
#                           (default: ../fonts, the glyphst-fonts checkout)
#
# Options:
#   --keep-going      run every step and report all failures
#   --theme T         screens: dark or light only (default: both)
#   --jobs N          screens: Readers running at once (default: 4 headless,
#                     1 on the desktop)
#   --desktop         screens: run on the desktop session instead
#
# Each run writes step logs and summary.json to target/check-reports/<tier>-<time>/;
# the newest ten runs of each tier are kept.

set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

if [[ -z ${IN_NIX_SHELL:-} ]] && command -v nix > /dev/null; then
    exec nix develop "${repo}" --command "$0" "$@"
fi

arguments=("$@")
tier=${1:-}
shift || true
keep_going=0
desktop=0
parallel=
themes=(dark light)
names=()
while (($# > 0)); do
    case $1 in
        --keep-going) keep_going=1 ;;
        --desktop) desktop=1 ;;
        --theme)
            [[ ${2:-} == dark || ${2:-} == light ]] || {
                printf -- '--theme takes dark or light\n' >&2
                exit 2
            }
            themes=("$2")
            shift
            ;;
        --jobs)
            [[ ${2:-} =~ ^[1-9][0-9]*$ ]] || {
                printf -- '--jobs takes a positive number\n' >&2
                exit 2
            }
            parallel=$2
            shift
            ;;
        -*) printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
        *) names+=("$1") ;;
    esac
    shift
done
case ${tier} in
    quick | gate | corpus | screens) ;;
    *) sed -n '2,28p' "$0" >&2; exit 2 ;;
esac
if [[ ${tier} != screens ]] && ((${#names[@]} > 0)); then
    printf 'only screens takes names: %s\n' "${names[*]}" >&2
    exit 2
fi
# Screens run in a private compositor so the Readers never take the desktop.
if [[ ${tier} == screens && ${desktop} == 0 && -z ${GLYPHST_HEADLESS:-} ]]; then
    exec "${repo}/script/headless-session.sh" "$0" "${arguments[@]}"
fi

cd "${repo}"
# App tests are quick, so the unoptimized dev profile (without debug info)
# minimizes compile time.
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0

run_dir="${repo}/target/check-reports/${tier}-$(date +%Y%m%dT%H%M%S)"
mkdir -p "${run_dir}"
find "${repo}/target/check-reports" -mindepth 1 -maxdepth 1 -name "${tier}-*" \
    | sort | head -n -10 | xargs -r rm -rf --
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
    local running
    running=$(jobs -pr)
    if [[ -n ${running} ]]; then
        # shellcheck disable=SC2086
        kill ${running} 2> /dev/null || true
        wait 2> /dev/null || true
    fi
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
    local begin status=0
    begin=$(date +%s)
    printf '%-22s ' "${name}"
    "$@" > "${run_dir}/${name}.log" 2>&1 || status=$?
    record "${name}" "${status}" "$(($(date +%s) - begin))"
}

# Records a finished step (name, exit status, seconds) after its name was printed.
record() {
    local name=$1 status=$2 seconds=$3 log="${run_dir}/${1}.log"
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
    # The glyphst-fonts checkout gives every shot the same CJK faces
    # instead of whatever fontconfig has installed.
    export GLYPHST_FONTS_DIR=${GLYPHST_FONTS_DIR:-${repo}/../fonts}
    if [[ ! -f ${GLYPHST_FONTS_DIR}/fonts/manifest.txt ]]; then
        printf 'error: no glyphst-fonts checkout at %s (set GLYPHST_FONTS_DIR)\n' "${GLYPHST_FONTS_DIR}" >&2
        exit 1
    fi
    shots="${run_dir}/screens"
    mkdir -p "${shots}"
    reader="${repo}/target/debug/glyphst-reader"
    dense=${GLYPHST_SCREENS_DENSE:-${repo}/../pdf-samples/Murietta_Level1.pdf}
    # A long document, for the translation's page map: 240 pages of text,
    # written here so no sample needs to be a book.
    long=${GLYPHST_SCREENS_LONG:-${run_dir}/long.pdf}
    if [[ ! -e ${long} ]]; then
        LC_ALL=C awk 'function emit(text) { printf "%s", text; at += length(text) }
            function object(number, body) {
                offset[number] = at
                emit(number " 0 obj\n" body "\nendobj\n")
            }
            BEGIN {
                pages = 240
                emit("%PDF-1.4\n")
                object(1, "<< /Type /Catalog /Pages 2 0 R >>")
                for (i = 0; i < pages; i++) kids = kids (4 + 2 * i) " 0 R "
                object(2, "<< /Type /Pages /Kids [" kids "] /Count " pages " >>")
                object(3, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
                for (i = 0; i < pages; i++) {
                    object(4 + 2 * i, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842]" \
                        " /Resources << /Font << /F1 3 0 R >> >> /Contents " (5 + 2 * i) " 0 R >>")
                    text = "BT /F1 20 Tf 72 760 Td (Chapter " (i + 1) ") Tj /F1 11 Tf 0 -30 Td" \
                        " (This page of the long sample says something worth translating.) Tj" \
                        " 0 -16 Td (Its second line carries on, so the paragraph has length.) Tj ET"
                    object(5 + 2 * i, "<< /Length " length(text) " >>\nstream\n" text "\nendstream")
                }
                size = 4 + 2 * pages
                start = at
                emit("xref\n0 " size "\n0000000000 65535 f \n")
                for (k = 1; k < size; k++) emit(sprintf("%010d 00000 n \n", offset[k]))
                emit("trailer\n<< /Size " size " /Root 1 0 R >>\nstartxref\n" start "\n%%EOF\n")
            }' > "${long}"
    fi
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
    # Hard-edged stripes and discs, where a blur that shows its texels
    # would show blocks.
    detail=${run_dir}/wallpaper-detail.ppm
    if [[ ! -e ${detail} ]]; then
        LC_ALL=C awk 'BEGIN {
            w = 960; h = 600; printf "P6\n%d %d\n255\n", w, h
            for (y = 0; y < h; y++) {
                for (x = 0; x < w; x++) {
                    stripe = int((x + y) / 24) % 2
                    r = 20 + 30 * stripe; g = 24 + 30 * stripe; b = 40 + 40 * stripe
                    dx = x - 300; dy = y - 260; ex = x - 690; ey = y - 360
                    if (dx * dx + dy * dy < 140 * 140) { r = 230; g = 70; b = 60 }
                    if (ex * ex + ey * ey < 110 * 110) { r = 60; g = 200; b = 190 }
                    printf "%c%c%c", r, g, b
                }
            }
        }' > "${detail}"
    fi
    # name, then the Reader's arguments (`@` stands for the document, `#`
    # for the second one, `%` for a dense drawing, `+` for a long text, `^`
    # for the wallpaper, `~` for the detailed one and `@@` for a plain `@`;
    # a `--screenshot-*` argument picks the kind of shot instead of a plain
    # one).
    states=(
        "01-gallery --gallery"
        "02a-reader @ --sidebar outline --assistant-demo done"
        "02b-generating @ --screenshot-generating"
        "02m-translating @ --sidebar translation --translate-fake --translate-follow --screenshot-translating"
        "02o-reveal-film @ --translate-fake --translate-follow --screenshot-translating-film"
        "02o-reveal-check @ --translate-fake --translate-follow --screenshot-reveal-check"
        "02o-reveal-reduced @ --translate-fake --translate-follow --reduce-motion --screenshot-translating-film"
        "02n-side-by-side @ --translate-fake --command translation.side_by_side"
        "02n-partner-hover @ --translate-fake --command translation.side_by_side --pointer-demo move:972,504;wait*3"
        "02d-find @ --find the"
        "02d-find-empty @ --command text.find"
        "02e-overview @ --command view.page_overview"
        "02e-overview-find @ --sidebar search --find the --command view.page_overview"
        "02e-overview-wallpaper @ --background image --wallpaper ^ --colours background --command view.page_overview"
        "02f-split @ # --split right"
        "02g-present @ --command view.present"
        "02h-present-tools @ --command view.present --screenshot-present-tools"
        "02i-present-palette @ --command view.present --command app.command_palette"
        "02k-switch-slide-over @ # --transition slide-over --screenshot-switch"
        "02k-switch-wipe @ # --transition wipe --screenshot-switch"
        "02k-switch-disc @ # --transition disc --screenshot-switch"
        "02k-switch-stripes @ # --transition stripes --screenshot-switch"
        "02k-switch-zoom @ # --transition zoom --screenshot-switch"
        "02k-switch-honeycomb @ # --transition honeycomb --screenshot-switch"
        "02k-switch-pixelate @ # --transition pixelate --screenshot-switch"
        "02k-switch-ripple @ # --transition ripple --screenshot-switch"
        "02k-switch-burn @ # --transition burn --screenshot-switch"
        "02k-switch-glitch @ # --transition glitch --screenshot-switch"
        "02k-switch-random @ # --transition random --screenshot-switch"
        "02k-switch-reduced @ # --transition disc --reduce-motion --screenshot-switch"
        "02k-close-disc @ # --transition disc --screenshot-close"
        "02k-open-disc @ # --transition disc --screenshot-open"
        "02k-sweep-disc @ # --background image --wallpaper ^ --book-background 2:aurora --transition disc --screenshot-switch"
        "10a-switcher @ # % --command app.book_switcher"
        "10b-switcher-peek @ # % --transition disc --screenshot-peek"
        "10c-switcher-gamepad @ # % --gamepad-mode --command app.book_switcher"
        "10d-switcher-carousel @ # % --switcher-layout carousel --command app.book_switcher"
        "10e-switcher-grid @ # % --switcher-layout grid --command app.book_switcher"
        "10f-switcher-slices @ # % --switcher-layout slices --command app.book_switcher"
        "03-item @ --sidebar item --library-demo item"
        "03-item-new @ --sidebar item"
        "03-outline @ --sidebar outline"
        "03-pages @ --sidebar pages"
        "03-pages-one @ --sidebar pages --command view.page_columns"
        "03-pages-dense % --sidebar pages"
        "03-bookmarks @ --sidebar bookmarks"
        "03-notes @ --sidebar notes"
        "03-search @ --sidebar search --find the"
        "03-translation @ --sidebar translation --translate-fake"
        "03-translation-following @ --sidebar translation --translate-fake --translate-follow"
        "03-translation-done @ --sidebar translation --translate-fake --translate-document"
        "03-translation-clear-confirm @ --sidebar translation --translate-fake --translate-document --command translation.clear"
        "03-translation-book + --sidebar translation --translate-fake --translate-follow"
        "03-translation-kept @ --sidebar translation --translate-fake --translate-follow --translate-kept 2"
        "03-translation-failed @ --sidebar translation --translate-fake --translate-document --translate-failures network,network"
        "03-translation-failed-install @ --sidebar translation --translate-fake --translate-follow --translate-failures local-ocr-missing"
        "03-translation-ocr-menu @ --sidebar translation --pointer-demo click:189,211;wait*3"
        "03-effects @ --sidebar effects"
        "04-home"
        "04-home-pad --pad-demo tap:right;wait;tap:down"
        "04b-new-tab --command app.new_tab"
        "04c-activity --command app.reading_activity"
        "04d-library --library-demo all"
        "04d-library-history --command app.library"
        "04f-library-item --library-demo item"
        "04l-library-duplicate --library-demo duplicate"
        "04m-library-merged --library-demo merged"
        "04n-library-search --library-demo search"
        "04o-meaning-offer --library-demo search-meaning"
        "04o-meaning-downloading --library-demo search-downloading"
        "04o-meaning-failed --library-demo search-failed"
        "04p-library-pages --library-demo pages"
        "04q-library-page --library-demo page"
        "04s-library-write-page --ai-fake --library-demo page-cost"
        "04v-library-history --library-demo history"
        "02l-tab-menu @ # --pointer-demo right:tab:1;wait*3"
        "02l-tab-drag @ # % --pointer-demo drag:tab:0>tab:2;wait*3"
        "02l-tab-pinned @ # % --command app.pin_tab"
        "02l-page-menu @ --pointer-demo right:page;wait*3"
        "02h-outside-library @ --library-demo outside"
        "02h-adding-to-library @ --library-demo outside-adding"
        "02h-in-library @ --library-demo added"
        "04w-library-processing --library-demo processing"
        "04h-library-backfill --ai-fake --library-demo backfill"
        "04k-library-folder --library-demo folder"
        "04t-library-join --library-demo join"
        "04j-zotero --library-demo zotero"
        "04j-zotero-done --library-demo zotero-done"
        "06d-add-identifier --library-demo identifier"
        "04e-home-narrow --window 960x900"
        "05-empty @ --assistant-demo empty"
        "05-menu @ --assistant-demo menu"
        "05-running @ --assistant-demo running"
        "05-done @ --assistant-demo done"
        "05-markdown @ --assistant-demo markdown"
        "05-error @ --assistant-demo error"
        "05-history @ --assistant-demo history"
        "05-edit @ --assistant-demo edit"
        "05-library-ask --assistant-demo library --library-demo all"
        "06a-palette @ --palette tra"
        "06a-palette-sections @ --palette @@"
        "06b-path-prompt @ --command app.open"
        "07a-appearance @ --settings appearance"
        "07a-background @ --settings appearance:background --background image --wallpaper ^ --colours background"
        "07a-colours @ --settings appearance:colours --background image --wallpaper ^ --colours background"
        "02a-wallpaper-theme @ --background image --wallpaper ^ --colours background"
        "02a-wallpaper-blur @ --background image --wallpaper ~ --background-params 0,6,0,0"
        "07a-tint @ --settings appearance:tint --overlay tint"
        "02a-theme-tint @ --overlay theme-tint --colours custom"
        "07-reading @ --settings reading"
        "07-presenting @ --settings presenting"
        "07-library @ --settings library"
        "07-library-search @ --settings library:search --library-demo search"
        "07-library-ai @ --settings library:ai"
        "07s-library-sync @ --settings library:sync --library-demo sync"
        "07s-library-sync-conflicts @ --settings library:sync --library-demo sync-conflicts"
        "07s-library-sync-s3 @ --settings library:sync --library-demo sync-s3"
        "07s-library-sync-webdav @ --settings library:sync --library-demo sync-webdav"
        "07-effects @ --settings effects"
        "07-translation @ --settings translation"
        "07-ocr @ --settings ocr"
        "07c-models @ --settings models"
        "07c-models-downloading @ --settings models:downloads"
        "07-ai @ --settings ai"
        "07-shortcuts --settings shortcuts"
        "07-shortcuts-glass --settings shortcuts --opacity 0.5"
        "07-shortcuts-gamepad --settings shortcuts:gamepad"
        "09a-pad-connected @ --pad-demo connect:8BitDo_Pro_2"
        "09a-pad-connected-ps @ --pad-family ps --pad-demo connect:DualSense"
        "09b-pad-ring @ --pad-demo stick:R:0.65,-0.65"
        "09c-gamepad-home --gamepad-mode --fixed-clock --pad-demo connect"
        "09c-gamepad-home-move --gamepad-mode --fixed-clock --pad-demo connect;stick:L:1,0;wait;stick:L:0,0"
        "09c-gamepad-leave --gamepad-mode --fixed-clock --pad-demo connect;tap:east"
        "09d-gamepad-reader @ --gamepad-mode --pad-demo connect;tap:rs"
        "09d-gamepad-reader-quiet @ --gamepad-mode --pad-demo connect;tap:rs*2;tap:rb"
        "09e-gamepad-ring @ --gamepad-mode --pad-demo stick:R:0.65,-0.65"
        "09f-gamepad-contents @ --gamepad-mode --command view.outline"
        "09g-gamepad-overview @ --gamepad-mode --command view.page_overview"
        "09h-gamepad-go-to @ --gamepad-mode --command view.go_to_page --pad-demo tap:right*5"
        "09h-gamepad-go-to-back @ --gamepad-mode --command view.go_to_page --pad-demo tap:rb;tap:lb*2"
        "09i-gamepad-menu @ --gamepad-mode --command app.gamepad_menu"
        "09j-gamepad-disconnected --gamepad-mode --fixed-clock --pad-demo connect;disconnect"
    )
    if ((${#names[@]} > 0)); then
        chosen=()
        for wanted in "${names[@]}"; do
            found=0
            for state in "${states[@]}"; do
                if [[ ${state%% *} == "${wanted}"* ]]; then
                    found=1
                    # Named twice (07c and 07c-models), a state runs once:
                    # two runs would share its data folder.
                    for taken in "${chosen[@]}"; do
                        [[ ${taken} == "${state}" ]] && continue 2
                    done
                    chosen+=("${state}")
                fi
            done
            if ((found == 0)); then
                printf 'no screens state starts with %s; states:\n' "${wanted}" >&2
                printf '  %s\n' "${states[@]%% *}" >&2
                exit 2
            fi
        done
        states=("${chosen[@]}")
    fi
    parallel=${parallel:-$([[ -n ${GLYPHST_HEADLESS:-} ]] && echo 4 || echo 1)}
    step reader-build cargo build --locked -p glyphst-reader

    # The library (positions, history, bookmarks) as the run starts. Each
    # shot gets a copy: Readers sharing one would race for its lock, and
    # only the winner would open documents where they were left.
    seed="${run_dir}/data-seed"
    mkdir -p "${seed}/glyphst-reader"
    data_home=${XDG_DATA_HOME:-${HOME}/.local/share}
    if [[ -d ${data_home}/glyphst-reader ]]; then
        cp -a "${data_home}/glyphst-reader/." "${seed}/glyphst-reader/"
    fi
    # User fonts stay where fontconfig finds them.
    if [[ -d ${data_home}/fonts ]]; then
        ln -s "${data_home}/fonts" "${seed}/fonts"
    fi

    # A shot still running after this long is stuck (the slowest take
    # about 80 seconds with every state running).
    shot_seconds=300
    # Runs one shot (name, image, then the Reader's command) in the
    # background, leaving its exit status and seconds in name.status.
    shoot() {
        local name=$1 image=$2 begin status=0 data="${run_dir}/data/$1"
        shift 2
        begin=$(date +%s)
        mkdir -p "${data}"
        # Once more on a failed copy; then the shot fails rather than ending
        # without a status.
        if ! cp -a "${seed}/." "${data}/" 2> "${run_dir}/${name}.log"; then
            rm -rf "${data}"
            mkdir -p "${data}"
            if ! cp -a "${seed}/." "${data}/" 2>> "${run_dir}/${name}.log"; then
                printf '1 %d\n' "$(($(date +%s) - begin))" > "${run_dir}/${name}.status"
                return
            fi
        fi
        XDG_DATA_HOME="${data}" timeout --kill-after=10 "${shot_seconds}" "$@" \
            > "${run_dir}/${name}.log" 2>&1 || status=$?
        rm -rf "${data}"
        if ((status == 124 || status == 137)); then
            printf 'stopped after %s seconds\n' "${shot_seconds}" >> "${run_dir}/${name}.log"
        fi
        # The Reader exits cleanly even when it could not save the image.
        if ((status == 0)) && [[ ! -s ${image} ]]; then
            printf 'no image at %s\n' "${image}" >> "${run_dir}/${name}.log"
            status=1
        fi
        printf '%d %d\n' "${status}" "$(($(date +%s) - begin))" > "${run_dir}/${name}.status"
    }
    # Reports finished shots in order, up to the first one still running.
    shot_names=()
    reported=0
    report() {
        local status seconds
        while ((reported < ${#shot_names[@]})) \
            && [[ -e ${run_dir}/${shot_names[reported]}.status ]]; do
            read -r status seconds < "${run_dir}/${shot_names[reported]}.status"
            printf '%-22s ' "${shot_names[reported]}"
            reported=$((reported + 1))
            record "${shot_names[reported - 1]}" "${status}" "${seconds}"
        done
    }
    for theme in "${themes[@]}"; do
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
                    +) args+=("${long}") ;;
                    ^) args+=("${wallpaper}") ;;
                    \~) args+=("${detail}") ;;
                    --screenshot-*) shot=${arg} ;;
                    *) args+=("${arg}") ;;
                esac
            done
            while (($(jobs -pr | wc -l) >= parallel)); do
                wait -n || true
                report
            done
            image="${shots}/${theme}-${name}.png"
            shot_names+=("${theme}-${name}")
            shoot "${theme}-${name}" "${image}" "${reader}" "${args[@]}" \
                --theme "${theme}" "${shot}" "${image}" &
            report
        done
    done
    wait
    report
    # A shot that left no status failed.
    while ((reported < ${#shot_names[@]})); do
        printf '%-22s ' "${shot_names[reported]}"
        printf 'no status\n' >> "${run_dir}/${shot_names[reported]}.log"
        record "${shot_names[reported]}" 1 0
        reported=$((reported + 1))
        report
    done
    rm -rf "${seed}" "${run_dir}/data"
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
