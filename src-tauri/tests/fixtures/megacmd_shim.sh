#!/bin/sh
# Stand-in for MEGAcmd's `mega-ls`, `mega-mv`, `mega-rm`, `mega-put`, `mega-get` and
# `mega-thumbnail`, used by the tests in src/providers/mega.rs, which link the names
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
# `mega-ls -l --versions --show-handles /v.txt` lists v.txt and its chain of
# three, the current one first, each with its handle, as MEGAcmd 2.6.0 lists
# it (measured 2026-11-17); `/w.txt` has a current version older in time than
# the one below it, `/x.txt` two versions of one time, `/z.txt` an empty past
# version and `/docs` a folder with a versions part per child. A file
# versions-fail makes the /v.txt listing fail after its rows. Every mega-get
# source is appended to get.log and its local target to get-targets.log, and
# every mega-put target to put.log. A version handle is fetched filled with
# its last character, at the size its listing line says, and a path with the
# contents of its current version. A file put-fails makes mega-put refuse,
# put-stdout-fails makes it print its failure on stdout and exit 1 with
# nothing put, put-slow and put-stalls pace or stop its progress. A file
# get-fails makes mega-get refuse, get-writes-then-fails makes it write 500
# bytes and then refuse, get-stdout-fails makes it print its failure on
# stdout and exit 1, get-always-try-again and get-partial-try-again (once)
# leave 500 bytes and fail with MEGAcmd's transient text, and z-get-three
# makes the empty version arrive as 3 bytes. mega-get never writes over a
# file at its target: it writes a " (1)" sibling, as MEGAcmd does.
# `mega-thumbnail /photo.jpg <out>` writes the first bytes of a JPEG to
# <out>; for `/plain.txt`, a file with no thumbnail, it writes nothing and
# exits 9 with MEGAcmd's "Not found", as measured the same day.
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
            # The root's own rows for the nodes the download tests use: a
            # node's type is read from its row here, never from a listing of
            # the node itself.
            if [ "$3" = / ]; then
                echo 'd---    -           - 10Oct2026 17:12:39 H:sAAAAAA1 same'
                echo 'd---    -           - 10Oct2026 17:12:39 H:pAAAAAA1 plain'
                echo '----    1        1000 10Oct2026 17:12:39 H:fAAAAAA1 f.dat'
            fi
            exit 0
        fi
        if [ "$1" = "-l" ] && [ "$2" = "--versions" ] && [ "$3" = "--show-handles" ] && [ "$4" = "/v.txt" ]; then
            echo 'FLAGS VERS      SIZE            DATE          HANDLE NAME'
            echo '----    3           13 16Nov2026 15:53:00 H:vAAAAAA3 v.txt'
            echo ''
            echo 'Versions of v.txt:'
            echo '----    3           13 16Nov2026 15:53:00 H:vAAAAAA3 v.txt#1794840780'
            echo '----    2           12 16Nov2026 15:52:57 H:vAAAAAA2 v.txt#1794840777'
            echo '----    1            8 16Nov2026 15:52:55 H:vAAAAAA1 v.txt#1794840775'
            # A listing that printed its rows and then failed.
            [ -f "$here/versions-fail" ] && exit 1
            exit 0
        fi
        if [ "$1" = "-l" ] && [ "$2" = "--versions" ] && [ "$3" = "--show-handles" ] && [ "$4" = "/w.txt" ]; then
            echo 'Versions of w.txt:'
            echo '----    3            8 16Nov2026 15:51:40 H:wAAAAAA3 w.txt#1794840700'
            echo '----    2           13 16Nov2026 15:53:10 H:wAAAAAA2 w.txt#1794840790'
            echo '----    1            8 16Nov2026 15:53:00 H:wAAAAAA1 w.txt#1794840780'
            exit 0
        fi
        if [ "$1" = "-l" ] && [ "$2" = "--versions" ] && [ "$3" = "--show-handles" ] && [ "$4" = "/x.txt" ]; then
            echo 'Versions of x.txt:'
            echo '----    3           13 16Nov2026 15:53:10 H:xAAAAAA3 x.txt#1794840790'
            echo '----    2           12 16Nov2026 15:52:57 H:xAAAAAA2 x.txt#1794840777'
            echo '----    1            8 16Nov2026 15:52:57 H:xAAAAAA1 x.txt#1794840777'
            exit 0
        fi
        if [ "$1" = "-l" ] && [ "$2" = "--versions" ] && [ "$3" = "--show-handles" ] && [ "$4" = "/docs" ]; then
            # A folder: MEGAcmd 2.6 lists its children and a versions part
            # for each versioned child, named by the child's path.
            echo 'FLAGS VERS      SIZE            DATE          HANDLE NAME'
            echo '----    2            6 16Nov2026 15:53:00 H:dAAAAAA2 a.txt'
            echo ''
            echo 'Versions of docs/a.txt:'
            echo '----    2            6 16Nov2026 15:53:00 H:dAAAAAA2 a.txt#1794840780'
            echo '----    1            4 16Nov2026 15:52:00 H:dAAAAAA1 a.txt#1794840720'
            echo ''
            echo 'Versions of docs/b.txt:'
            echo '----    2            7 16Nov2026 15:53:00 H:dBBBBBB2 b.txt#1794840780'
            echo '----    1            5 16Nov2026 15:52:00 H:dBBBBBB1 b.txt#1794840720'
            exit 0
        fi
        if [ "$1" = "-l" ] && [ "$2" = "--versions" ] && [ "$3" = "--show-handles" ] && [ "$4" = "/z.txt" ]; then
            # An empty past version (0 bytes).
            echo 'FLAGS VERS      SIZE            DATE          HANDLE NAME'
            echo '----    2            4 16Nov2026 15:53:00 H:zAAAAAA2 z.txt'
            echo ''
            echo 'Versions of z.txt:'
            echo '----    2            4 16Nov2026 15:53:00 H:zAAAAAA2 z.txt#1794840780'
            echo '----    1            0 16Nov2026 15:52:00 H:zAAAAAA1 z.txt#1794840720'
            exit 0
        fi
        if [ "$1" = "-l" ] && [ "$2" = "/" ]; then
            echo 'FLAGS VERS SIZE DATE TIME NAME'
            echo '----  1  3  15Jan2026  14:30 a.txt'
            echo '----  1  3  15Jan2026  14:30 b.txt'
            echo 'd---  -  -  15Jan2026  14:30 d'
            exit 0
        fi
        if [ "$1" = "-l" ] && [ "$2" = "/f.dat" ]; then
            echo 'FLAGS VERS      SIZE            DATE       NAME'
            echo '----    1         1000 10Oct2026 17:12:39 f.dat'
            exit 0
        fi
        # An ordinary folder as MEGAcmd 2.6 lists it without -R: its
        # children, and no header line naming the folder.
        if [ "$1" = "-l" ] && [ "$2" = "/plain" ]; then
            echo 'FLAGS VERS      SIZE            DATE       NAME'
            echo '----    1           5 10Oct2026 17:12:39 inner.txt'
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
        # line and then says nothing for 30 s, far past any silence limit a
        # test sets (exec, so the kill of a cut transfer reaches the sleep).
        if [ -f "$here/put-fails" ]; then
            printf 'TRANSFERRING ||####..........||(3/30 MB:  10.00 %%) \000\r' >&2
            printf 'TRANSFERRING ||####..........||(6/30 MB:  20.00 %%) \000\r' >&2
            echo "Upload failed: Access denied" >&2
            exit 2
        fi
        if [ -f "$here/put-stdout-fails" ]; then
            # The failure on stdout and a non-zero exit, nothing put.
            echo "Upload failed: Access denied"
            exit 1
        fi
        if [ -f "$here/put-stalls" ]; then
            printf 'TRANSFERRING ||####..........||(3/30 MB:  10.00 %%) \000\r' >&2
            exec sleep 30
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
        echo "$2" >> "$here/put.log"
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
        echo "$1" >> "$here/get.log"
        echo "$2" >> "$here/get-targets.log"
        if [ -f "$here/get-writes-then-fails" ]; then
            dd if=/dev/zero of="$2" bs=500 count=1 2>/dev/null
            # Not a transient text: the provider would retry a reset.
            echo "Download failed: Write error" >&2
            exit 2
        fi
        if [ -f "$here/get-always-try-again" ]; then
            # Every attempt: 500 bytes at the target, then a transient failure.
            dd if=/dev/zero of="$2" bs=500 count=1 2>/dev/null
            echo "Download failed: Temporarily unavailable, try again"
            exit 1
        fi
        if [ -f "$here/get-partial-try-again" ] && [ ! -f "$here/get-retried" ]; then
            # Once: 500 bytes at the target, then a transient failure on
            # stdout; every attempt is in get.log above.
            touch "$here/get-retried"
            dd if=/dev/zero of="$2" bs=500 count=1 2>/dev/null
            # get-pins-partial: the partial file cannot be removed (its
            # folder is made read-only).
            if [ -f "$here/get-pins-partial" ]; then
                chmod a-w "$(dirname "$2")"
            fi
            echo "Download failed: Temporarily unavailable, try again"
            exit 1
        fi
        # A folder got into an existing folder: MEGAcmd writes the folder of
        # its name inside it.
        if [ "$1" = /plain ] && [ -d "$2" ]; then
            mkdir -p "$2/plain" && printf 'five!' > "$2/plain/inner.txt"
            printf '\nDownload finished: %s\n' "$2/plain"
            exit 0
        fi
        if [ "$1" = /same ] && [ -d "$2" ]; then
            # folder-get-slow: progress for three seconds before it ends.
            if [ -f "$here/folder-get-slow" ]; then
                for p in 10 30 50 70 90 95; do
                    printf 'TRANSFERRING ||####..........||(%s/100 MB: %s.00 %%) \000\r' "$p" "$p" >&2
                    sleep 0.5
                done
            fi
            mkdir -p "$2/same" && printf 'seven!!' > "$2/same/same"
            printf '\nDownload finished: %s\n' "$2/same"
            exit 0
        fi
        # MEGAcmd does not write over a file at the target: it writes a
        # " (1)" sibling (measured 2026-11-18).
        out="$2"
        [ -f "$2" ] && out="$2 (1)"
        # A version handle gets the size its listing line above says, filled
        # with its last character, so two versions differ in their bytes. A
        # path gets the contents of the current version its listing starts
        # with.
        n=1000
        fill=''
        case "$1" in
            H:*)
                n=$(awk -v h="$1" '$1 == "echo" && $4 ~ /^[0-9]+$/ && index($0, " " h " ") { print $4; exit }' "$0")
                fill=$(printf '%s' "$1" | tail -c 1)
                ;;
            /*)
                case "$(basename "$1")" in
                    v.txt) head=H:vAAAAAA3 ;;
                    w.txt) head=H:wAAAAAA3 ;;
                    *) head='' ;;
                esac
                if [ -n "$head" ]; then
                    n=$(awk -v h="$head" '$1 == "echo" && $4 ~ /^[0-9]+$/ && index($0, " " h " ") { print $4; exit }' "$0")
                    fill=$(printf '%s' "$head" | tail -c 1)
                fi
                ;;
        esac
        if [ -f "$here/get-stdout-fails" ]; then
            # The failure on stdout and a non-zero exit, nothing written.
            echo "Download failed: Access denied"
            exit 1
        fi
        for p in 10.00 50.00 90.00; do
            printf 'TRANSFERRING ||####..........||(9/30 MB:  %s %%) \000\r' "$p" >&2
        done
        # z-get-three: the empty version arrives as 3 bytes.
        if [ "$1" = H:zAAAAAA1 ] && [ -f "$here/z-get-three" ]; then
            n=3
        fi
        if [ -n "$fill" ]; then
            head -c "$n" /dev/zero | tr '\0' "$fill" > "$out"
        else
            dd if=/dev/zero of="$out" bs="$n" count=1 2>/dev/null
        fi
        printf '\nDownload finished: %s\n' "$out"
        printf 'TRANSFERRING ||##############||(30/30 MB: 100.00 %%) \000\n' >&2
        ;;
    mega-thumbnail)
        case "$1" in
            /photo.jpg) printf '\377\330\377\340thumb' > "$2" ;;
            /plain.txt) echo "Failed to get thumbnail $1 to $2: Not found" >&2; exit 9 ;;
            *) echo "Couldn't find $1" >&2; exit 53 ;;
        esac
        ;;
    *)
        echo "unhandled $0 $*" >&2
        exit 1
        ;;
esac
