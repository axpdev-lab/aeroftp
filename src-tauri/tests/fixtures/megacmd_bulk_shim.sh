#!/bin/sh
# Stand-in for MEGAcmd's `mega-ls`, `mega-put`, `mega-get` and
# `mega-transfers` for the bulk tests in src/providers/mega/bulk.rs, which
# link the names to this one file (a checked-in executable: see
# megacmd_shim.sh). Everything lives in files next to the link:
#
# - listing.txt: the lines `mega-ls -l` prints for the one folder of a test.
# - every mega-put and mega-get appends its arguments to put.log / get.log.
# - mega-put prints `Upload started: Tag = N. Source = <path>` per source, as
#   MEGAcmd 2.6 does with --print-tag-at-start, records `N|STATE` in
#   up-rows.txt and lists a completed file in listing.txt. put-skip-<name>:
#   no tag and no file; put-fail-<name>: FAILED, no file; put-size-<name>:
#   lists that size instead; put-hang: rows stay ACTIVE and the put waits.
# - mega-get <folder> <staging> writes every file of listing.txt under
#   <staging>/<folder> and records `N|STATE|<destination>` in down-rows.txt,
#   with a row for the folder itself first. get-fail-<name>: FAILED, no
#   file; get-cut-<name>: the row's path is cut, as a narrow display would;
#   get-dup: the first file has two rows; get-hang: rows stay ACTIVE and
#   the get waits.
# - mega-transfers -c TAG appends TAG to cancel.log (-a appends "-a"); any
#   other call prints the table of the direction asked for.
# - replay (a file holding the path of tests/fixtures/megacmd_real): the
#   commands print what MEGAcmd 2.6.0.2 printed on a real account, with the
#   placeholders replaced by the paths of this call. mega-put prints
#   put.stdout (<STAGE>/up becomes the folder of its first source) and lists
#   its sources; mega-transfers prints transfers-uploads.txt, or the download
#   table mega-get wrote from transfers-downloads.txt (<STAGE>/stage becomes
#   its staging folder); mega-get writes the files of listing.txt.
here=$(dirname "$0")
real=""
[ -f "$here/replay" ] && real=$(cat "$here/replay")
next_tag() {
    tag=$(( $(cat "$here/tagseq" 2>/dev/null || echo 100) + 1 ))
    echo "$tag" > "$here/tagseq"
}
case "$(basename "$0")" in
    mega-ls)
        echo "$*" >> "$here/ls.log"
        if [ ! -f "$here/listing.txt" ]; then
            echo "Couldn't find $2" >&2
            exit 53
        fi
        echo 'FLAGS VERS SIZE DATE TIME NAME'
        cat "$here/listing.txt"
        ;;
    mega-put)
        echo "$*" >> "$here/put.log"
        shift
        if [ -n "$real" ]; then
            srcdir=$(dirname "$1")
            sed "s#<STAGE>/up#$srcdir#g" "$real/put.stdout"
            while [ $# -gt 1 ]; do
                size=$(wc -c < "$1" | tr -d ' ')
                echo "----  1  $size  15Jan2026  14:30  $(basename "$1")" >> "$here/listing.txt"
                shift
            done
            exit 0
        fi
        state=COMPLETED
        [ -f "$here/put-hang" ] && state=ACTIVE
        while [ $# -gt 1 ]; do
            src=$1
            name=$(basename "$src")
            shift
            [ -f "$here/put-skip-$name" ] && continue
            next_tag
            printf 'Upload started: Tag = %s. Source = %s\n' "$tag" "$src"
            if [ -f "$here/put-fail-$name" ]; then
                echo "$tag|FAILED" >> "$here/up-rows.txt"
                continue
            fi
            echo "$tag|$state" >> "$here/up-rows.txt"
            [ "$state" = COMPLETED ] || continue
            size=$(wc -c < "$src" | tr -d ' ')
            [ -f "$here/put-size-$name" ] && size=$(cat "$here/put-size-$name")
            echo "----  1  $size  15Jan2026  14:30  $name" >> "$here/listing.txt"
        done
        [ -f "$here/put-hang" ] && exec sleep 30
        exit 0
        ;;
    mega-get)
        echo "$*" >> "$here/get.log"
        folder=$(basename "$1")
        dest="$2/$folder"
        mkdir -p "$dest"
        if [ -n "$real" ]; then
            while read -r flags vers size date time name; do
                head -c "$size" /dev/zero > "$dest/$name"
            done < "$here/listing.txt"
            sed "s#<STAGE>/stage#$2#g" "$real/transfers-downloads.txt" > "$here/replay-down.txt"
            sed "s#<STAGE>/stage#$2#g" "$real/get.stdout"
            exit 0
        fi
        state=COMPLETED
        [ -f "$here/get-hang" ] && state=ACTIVE
        next_tag
        echo "$tag|$state|$dest" >> "$here/down-rows.txt"
        first=1
        while read -r flags vers size date time name; do
            next_tag
            if [ -f "$here/get-fail-$name" ]; then
                echo "$tag|FAILED|$dest/$name" >> "$here/down-rows.txt"
                continue
            fi
            head -c "$size" /dev/zero > "$dest/$name"
            row="$dest/$name"
            [ -f "$here/get-cut-$name" ] && row="...${row#????????????}"
            echo "$tag|$state|$row" >> "$here/down-rows.txt"
            if [ "$first" = 1 ] && [ -f "$here/get-dup" ]; then
                next_tag
                echo "$tag|$state|$row" >> "$here/down-rows.txt"
            fi
            first=0
        done < "$here/listing.txt"
        [ -f "$here/get-hang" ] && exec sleep 30
        exit 0
        ;;
    mega-transfers)
        if [ "$1" = "-c" ]; then
            echo "$2" >> "$here/cancel.log"
            exit 0
        fi
        for arg in "$@"; do
            [ "$arg" = "-a" ] && echo "-a" >> "$here/cancel.log"
        done
        if [ -n "$real" ]; then
            case " $* " in
                *" --only-uploads "*) cat "$real/transfers-uploads.txt" ;;
                *" --only-downloads "*) cat "$here/replay-down.txt" 2>/dev/null ;;
            esac
            exit 0
        fi
        echo ' ...  Showing first 1000 transfers ...'
        case " $* " in
            *" --only-uploads "*)
                echo 'TAG|STATE'
                [ -f "$here/up-rows.txt" ] && cat "$here/up-rows.txt"
                ;;
            *" --only-downloads "*)
                echo 'TAG|STATE|DESTINYPATH'
                [ -f "$here/down-rows.txt" ] && cat "$here/down-rows.txt"
                ;;
        esac
        exit 0
        ;;
    *)
        echo "unhandled $0 $*" >&2
        exit 1
        ;;
esac
