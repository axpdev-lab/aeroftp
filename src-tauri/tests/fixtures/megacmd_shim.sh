#!/bin/sh
# Stand-in for MEGAcmd's `mega-ls`, `mega-mv`, `mega-rm`, `mega-put` and
# `mega-get`, used by the tests in src/providers/mega.rs, which link the names
# to this one file. It is a
# checked-in executable on purpose: a test that writes a script and then runs
# it can fail with ETXTBSY (see `link_shim` in src/providers/proton.rs).
#
# The root holds the files a.txt and b.txt and the folder d. Every mega-mv is
# appended to mv.log next to the link it was invoked through (not resolved).
# The rubbish bin (`mega-ls -l //bin`) lists the lines of bin.txt next to the
# link, and every mega-rm is appended to rm.log. With --show-handles the bin
# lists bin-handles.txt and any other path lists folder-handles.txt, the path
# asked for is appended to ls.log, and a file ls-fails makes it fail. A file
# bin-fails makes the bin listing fail, and a file rm-fails makes mega-rm fail.
# `mega-ls -l /f.dat` lists that one 1000-byte file, as MEGAcmd 2.6 lists a
# file path; `mega-ls -l /same` lists a folder holding a file of its own name,
# opened by the `path:` line MEGAcmd 2.6 writes before a folder's listing.
here=$(dirname "$0")
case "$(basename "$0")" in
    mega-ls)
        if [ "$1" = "-l" ] && [ "$2" = "--show-handles" ]; then
            if [ "$3" = "//bin" ]; then
                if [ -f "$here/bin-fails" ]; then
                    echo "Failed to list //bin" >&2
                    exit 2
                fi
                listing=bin-handles.txt
            else
                echo "$3" >> "$here/ls.log"
                if [ -f "$here/ls-fails" ]; then
                    echo "Invalid argument --show-handles" >&2
                    exit 2
                fi
                listing=folder-handles.txt
            fi
            echo "$3: "
            echo 'FLAGS VERS SIZE DATE HANDLE NAME'
            [ -f "$here/$listing" ] && cat "$here/$listing"
            exit 0
        fi
        if [ "$1" = "-l" ] && [ "$2" = "/" ]; then
            echo 'FLAGS VERS SIZE DATE TIME NAME'
            echo '----  1  3  15Jan2026  14:30  a.txt'
            echo '----  1  3  15Jan2026  14:30  b.txt'
            echo 'd---  -  -  15Jan2026  14:30  d'
            exit 0
        fi
        if [ "$1" = "-l" ] && [ "$2" = "/f.dat" ]; then
            echo 'FLAGS VERS      SIZE            DATE       NAME'
            echo '----    1         1000 10Oct2026 17:12:39 f.dat'
            exit 0
        fi
        if [ "$1" = "-l" ] && [ "$2" = "/same" ]; then
            echo '/same: '
            echo 'FLAGS VERS      SIZE            DATE       NAME'
            echo '----    1           7 10Oct2026 17:12:39 same'
            exit 0
        fi
        if [ "$1" = "-l" ] && [ "$2" = "//bin" ]; then
            echo 'FLAGS VERS SIZE DATE TIME NAME'
            [ -f "$here/bin.txt" ] && cat "$here/bin.txt"
            exit 0
        fi
        echo "Couldn't find $2" >&2
        exit 53
        ;;
    mega-rm)
        if [ -f "$here/rm-fails" ]; then
            echo "Failed to remove $*" >&2
            exit 2
        fi
        echo "$*" >> "$here/rm.log"
        ;;
    mega-mv)
        echo "$1 $2" >> "$here/mv.log"
        ;;
    mega-put)
        # Progress lines as MEGAcmd 2.6 writes them through a pipe: on
        # stderr, each ended by a NUL and a carriage return, at 10, 50 and 90
        # percent; then "Upload finished" on stdout and the last line. A file put-fails makes it refuse after two progress lines; a file put-slow spaces
        # five lines 0.4 s apart (2 s in all); a file put-stalls prints one
        # line and then says nothing for 3 s.
        if [ -f "$here/put-fails" ]; then
            printf 'TRANSFERRING ||####..........||(3/30 MB:  10.00 %%) \000\r' >&2
            printf 'TRANSFERRING ||####..........||(6/30 MB:  20.00 %%) \000\r' >&2
            echo "Upload failed: Access denied" >&2
            exit 2
        fi
        if [ -f "$here/put-stalls" ]; then
            printf 'TRANSFERRING ||####..........||(3/30 MB:  10.00 %%) \000\r' >&2
            sleep 3
            exit 0
        fi
        if [ -f "$here/put-slow" ]; then
            for p in 10.00 30.00 50.00 70.00 90.00; do
                printf 'TRANSFERRING ||####..........||(9/30 MB:  %s %%) \000\r' "$p" >&2
                sleep 0.4
            done
        else
            for p in 10.00 50.00 90.00; do
                printf 'TRANSFERRING ||####..........||(9/30 MB:  %s %%) \000\r' "$p" >&2
            done
        fi
        printf '\nUpload finished: %s\n' "$2"
        printf 'TRANSFERRING ||##############||(30/30 MB: 100.00 %%) \000\n' >&2
        ;;
    mega-get)
        # mega-put's progress lines on the download side, as MEGAcmd 2.6
        # writes them for mega-get (measured 2026-10-10): 10, 50 and 90
        # percent on stderr, then 1000 bytes at the local path, "Download
        # finished" on stdout and the last line. A file get-fails makes it
        # refuse after two progress lines, with nothing written.
        if [ -f "$here/get-fails" ]; then
            printf 'TRANSFERRING ||####..........||(3/30 MB:  10.00 %%) \000\r' >&2
            printf 'TRANSFERRING ||####..........||(6/30 MB:  20.00 %%) \000\r' >&2
            echo "Download failed: Access denied" >&2
            exit 2
        fi
        for p in 10.00 50.00 90.00; do
            printf 'TRANSFERRING ||####..........||(9/30 MB:  %s %%) \000\r' "$p" >&2
        done
        dd if=/dev/zero of="$2" bs=1000 count=1 2>/dev/null
        printf '\nDownload finished: %s\n' "$2"
        printf 'TRANSFERRING ||##############||(30/30 MB: 100.00 %%) \000\n' >&2
        ;;
    *)
        echo "unhandled $0 $*" >&2
        exit 1
        ;;
esac
