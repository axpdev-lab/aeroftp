#!/bin/sh
# Stand-in for MEGAcmd's `mega-ls` and `mega-mv`, used by the rename tests in
# src/providers/mega.rs, which link both names to this one file. It is a
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
    *)
        echo "unhandled $0 $*" >&2
        exit 1
        ;;
esac
