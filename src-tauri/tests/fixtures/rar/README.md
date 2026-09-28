# RAR fixtures

Both archives are copied unchanged from the test data of the `unrar` crate
0.5.8 (`data/`, <https://github.com/muja/unrar.rs>), which AeroFTP already
depends on, licensed MIT OR Apache-2.0; the crate's own tests (`tests/crypted.rs`,
`tests/multipart.rs`) document them. Writing RAR takes the proprietary `rar`
tool, which was not available where the fixtures were added, so existing
archives are reused rather than new ones made.

## crypted.rar

109 bytes, a RAR 4 archive (RAR 2.9 method, AES-128 encryption of the file
data, header in the clear) with a single member, `.gitignore`, whose 18 bytes
are `target\nCargo.lock\n`. The password is `unrar`.

A RAR 4 archive carries no password check value, so a wrong password decrypts
the data to noise and UnRAR reports the entry as damaged (a CRC error) only
after it has opened, and so truncated, the output file, which it then deletes.
It backs `a_failed_rar_entry_keeps_the_existing_file` in `src/lib.rs`, which
checks that a file already at the entry's path survives such a failure on both
the whole-archive and the single-entry extraction paths.

sha256: `3215288892bfae2e641786952ab2547ea72cdd58c9c544604cd63e242fa07318`

## archive.part1.rar

10000 bytes, the first volume of a multi-volume RAR 4 archive. It holds six
complete members, in nested folders (`build.rs`, `Cargo.toml`,
`examples/lister.rs`, `src/lib.rs`, `vendor/unrar/acknow.txt`,
`vendor/unrar/arccmt.cpp`), and the start of `vendor/unrar/archive.cpp`, which
continues in a second volume that is not included. It backs
`a_rar_extraction_that_stops_part_way_keeps_what_it_extracted`: the extraction
fails on the missing volume, and the members extracted before it must still
land in place.

sha256: `b64e082db122fac3616b8309afaaff78f939424767fbf0f097fa178ac9365548`
