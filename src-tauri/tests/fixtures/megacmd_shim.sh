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
# the one below it (a restore keeps the old time), `/x.txt` two versions of
# one time, and `/y.txt` gains a new current version after its first listing
# (a file put at the path by another client). Every
# mega-get source is appended to get.log, its local target to
# get-targets.log, and every mega-put target to put.log (its size to
# put-size-<name>: /v.txt and /w.txt then list a new current version,
# H:vAAAAAA4 or H:wAAAAAA4, of that size; /v.txt lists H:vAAAAAA5 (21 bytes)
# below it when a file put-raced says another client put the file just
# before); a file put-stdout-fails makes mega-put print its failure on stdout
# and exit 1 with nothing put, put-same-bytes makes it put nothing and exit 0
# (MEGA's answer to the current version's own bytes, measured 2026-11-18),
# and with put-then-foreign another client then writes /v.txt (H:vAAAAAA5 on
# top) after either; with a file versioning-off a put to /v.txt leaves
# its new version listed; a file get-writes-then-fails makes mega-get write
# 500 bytes and then refuse; get-short makes it write 3 bytes and exit 0;
# get-stdout-fails makes it print its failure on stdout and exit 1. A version
# handle is fetched filled with its last character and a path with its
# current contents (what the last put wrote, or another client's write), so a
# restore can be checked by its bytes; put-ignored makes mega-put exit 0 with
# nothing changed, put-try-again fails on stdout with MEGAcmd's transient
# text (every attempt logged), and get-partial-try-again makes the first
# mega-get leave 500 bytes and fail so. mega-get never writes over a file at
# its target: it writes a " (1)" sibling, as MEGAcmd does. With versioning-off the
# listing after a put is the one file row MEGAcmd prints for a single version. `mega-thumbnail /photo.jpg <out>` writes the first bytes of a JPEG
# to <out>; for `/plain.txt`, a file with no thumbnail, it writes nothing and
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
            exit 0
        fi
        if [ "$1" = "-l" ] && [ "$2" = "--versions" ] && [ "$3" = "--show-handles" ] && [ "$4" = "/v.txt" ]; then
            echo 'FLAGS VERS      SIZE            DATE          HANDLE NAME'
            if [ -f "$here/put-size-v.txt" ] && [ -f "$here/versioning-off" ]; then
                # Versioning turned off: the upload replaced the file, which
                # has one version, and MEGAcmd prints no "Versions of" part.
                echo "----    1 $(cat "$here/put-size-v.txt") 16Nov2026 15:54:00 H:vAAAAAA4 v.txt"
                exit 0
            fi
            echo '----    3           13 16Nov2026 15:53:00 H:vAAAAAA3 v.txt'
            echo ''
            echo 'Versions of v.txt:'
            if [ -f "$here/put-size-v.txt" ]; then
                echo "----    5 $(cat "$here/put-size-v.txt") 16Nov2026 15:54:00 H:vAAAAAA4 v.txt#1794840840"
            fi
            # Another client's version: put just before ours (put-raced, shown
            # once ours is there) or just after a put that failed.
            if { [ -f "$here/put-raced" ] && [ -f "$here/put-size-v.txt" ]; } || [ -f "$here/foreign-v.txt" ]; then
                echo "----    4 $(cat "$here/foreign-size" 2>/dev/null || echo 21) 16Nov2026 15:53:30 H:vAAAAAA5 v.txt#1794840810"
            fi
            echo '----    3           13 16Nov2026 15:53:00 H:vAAAAAA3 v.txt#1794840780'
            echo '----    2           12 16Nov2026 15:52:57 H:vAAAAAA2 v.txt#1794840777'
            echo '----    1            8 16Nov2026 15:52:55 H:vAAAAAA1 v.txt#1794840775'
            exit 0
        fi
        if [ "$1" = "-l" ] && [ "$2" = "--versions" ] && [ "$3" = "--show-handles" ] && [ "$4" = "/w.txt" ]; then
            echo 'Versions of w.txt:'
            if [ -f "$here/put-size-w.txt" ]; then
                echo "----    4 $(cat "$here/put-size-w.txt") 16Nov2026 15:54:00 H:wAAAAAA4 w.txt#1794840840"
            fi
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
        if [ "$1" = "-l" ] && [ "$2" = "--versions" ] && [ "$3" = "--show-handles" ] && [ "$4" = "/y.txt" ]; then
            echo 'Versions of y.txt:'
            if [ -f "$here/y-listed" ]; then
                echo '----    4            9 16Nov2026 15:54:00 H:yAAAAAA9 y.txt#1794840840'
            fi
            touch "$here/y-listed"
            echo '----    2            7 16Nov2026 15:53:00 H:yAAAAAA2 y.txt#1794840780'
            echo '----    1            5 16Nov2026 15:52:00 H:yAAAAAA1 y.txt#1794840720'
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
    mega-userattr)
        # The DISABLE_VERSIONS attribute as MEGAcmd 2.6 prints it (measured
        # 2026-11-18): 1 with versioning-off, else 0; versioning-unreadable
        # answers neither.
        if [ -f "$here/versioning-unreadable" ]; then
            echo "Unexpected answer"
        elif [ -f "$here/versioning-off" ]; then
            printf '\tDISABLE_VERSIONS (^!dv) = 1\n'
        else
            printf '\tDISABLE_VERSIONS (^!dv) = 0\n'
        fi
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
        # With put-then-foreign another client writes the file right after a
        # put that wrote nothing (put-stdout-fails, put-same-bytes).
        if [ -f "$here/put-then-foreign" ]; then
            if [ -f "$here/put-stdout-fails" ] || [ -f "$here/put-same-bytes" ]; then
                touch "$here/foreign-$(basename "$2")"
            fi
        fi
        if [ -f "$here/put-stdout-fails" ]; then
            # The failure on stdout and a non-zero exit, nothing put.
            echo "Upload failed: Access denied"
            exit 1
        fi
        if [ -f "$here/put-try-again" ]; then
            # A transient failure on stdout, nothing put; every attempt logged.
            echo "$2" >> "$here/put.log"
            echo "Upload failed: Temporarily unavailable, try again"
            exit 1
        fi
        if [ -f "$here/put-same-bytes" ]; then
            # The bytes the current version already holds: MEGA adds no
            # version and says it is done (measured 2026-11-18). Nothing
            # changes: the current bytes are what they were.
            echo "$2" >> "$here/put.log"
            printf '\nUpload finished: %s\n' "$2"
            exit 0
        fi
        if [ -f "$here/put-ignored" ]; then
            # Done by its exit and its words, with nothing changed. With
            # foreign-writes-put-bytes another client then writes the same
            # bytes at the path, with no version in the listing yet.
            echo "$2" >> "$here/put.log"
            if [ -f "$here/foreign-writes-put-bytes" ]; then
                cp "$1" "$here/put-bytes-$(basename "$2")"
            fi
            printf '\nUpload finished: %s\n' "$2"
            exit 0
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
        echo "$2" >> "$here/put.log"
        cp "$1" "$here/put-bytes-$(basename "$2")"
        wc -c < "$1" | tr -d ' ' > "$here/put-size-$(basename "$2")"
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
            echo "Download failed: Temporarily unavailable, try again"
            exit 1
        fi
        # MEGAcmd does not write over a file at the target: it writes a
        # " (1)" sibling (measured 2026-11-18).
        out="$2"
        [ -f "$2" ] && out="$2 (1)"
        # A version handle gets the size its listing line above says, filled
        # with its last character, so two versions differ in their bytes. A
        # path gets its current contents: another client's write on top
        # (foreign-<name>), else what the last put left (put-bytes-<name>).
        n=1000
        fill=''
        case "$1" in
            H:vAAAAAA4|H:wAAAAAA4)
                # The version a put added holds what was put.
                name=$(printf '%s' "$1" | cut -c3).txt
                cp "$here/put-bytes-$name" "$out"
                printf '\nDownload finished: %s\n' "$out"
                exit 0
                ;;
            H:vAAAAAA5)
                # Another client's version: its size and bytes as a path get
                # of foreign-<name> gives them.
                n=$(cat "$here/foreign-size" 2>/dev/null || echo 21)
                fill=5
                ;;
            H:*)
                n=$(awk -v h="$1" 'index($0, " " h " ") { print $4; exit }' "$0")
                fill=$(printf '%s' "$1" | tail -c 1)
                # w-current-holds-w1: the current w.txt has H:wAAAAAA1's bytes.
                if [ "$1" = H:wAAAAAA3 ] && [ -f "$here/w-current-holds-w1" ]; then
                    fill=1
                fi
                ;;
            /*)
                name=$(basename "$1")
                if [ -f "$here/foreign-$name" ]; then
                    n=$(cat "$here/foreign-size" 2>/dev/null || echo 21)
                    fill=5
                elif [ -f "$here/put-bytes-$name" ]; then
                    cp "$here/put-bytes-$name" "$out"
                    printf '\nDownload finished: %s\n' "$out"
                    exit 0
                else
                    # The current version each listing above starts with.
                    case "$name" in
                        v.txt) head=H:vAAAAAA3 ;;
                        w.txt) head=H:wAAAAAA3 ;;
                        *) head='' ;;
                    esac
                    if [ -n "$head" ]; then
                        n=$(awk -v h="$head" 'index($0, " " h " ") { print $4; exit }' "$0")
                        fill=$(printf '%s' "$head" | tail -c 1)
                    fi
                fi
                ;;
        esac
        if [ -f "$here/get-short" ]; then
            # Cut midway with nothing said and a zero exit.
            dd if=/dev/zero of="$2" bs=3 count=1 2>/dev/null
            exit 0
        fi
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
