#!/bin/sh
# Stand-in for MEGAcmd's `mega-ls` and `mega-mv`, used by the rename tests in
# src/providers/mega.rs, which link both names to this one file. It is a
# checked-in executable on purpose: a test that writes a script and then runs
# it can fail with ETXTBSY (see `link_shim` in src/providers/proton.rs).
#
# The root holds the files a.txt and b.txt and the folder d. Every mega-mv is
# appended to mv.log next to the link it was invoked through (not resolved).
here=$(dirname "$0")
case "$(basename "$0")" in
    mega-ls)
        if [ "$1" = "-l" ] && [ "$2" = "/" ]; then
            echo 'FLAGS VERS SIZE DATE TIME NAME'
            echo '----  1  3  15Jan2026  14:30  a.txt'
            echo '----  1  3  15Jan2026  14:30  b.txt'
            echo 'd---  -  -  15Jan2026  14:30  d'
            exit 0
        fi
        echo "Couldn't find $2" >&2
        exit 53
        ;;
    mega-mv)
        echo "$1 $2" >> "$here/mv.log"
        ;;
    *)
        echo "unhandled $0 $*" >&2
        exit 1
        ;;
esac
