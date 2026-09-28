# 7z early-end fixture

`early-end.7z` (226 bytes) is a 7z archive made by 7-Zip 23.01 with content-only
AES encryption (header in the clear), one LZMA2 block and a single member,
`payload.txt`: the line `AeroFTP 7z underrun fixture: a wrong password must not extract.`
repeated 8 times (512 bytes). The right password is `right-password`.

The wrong password `wrong-388` was picked because, with this archive's random
IV, its key decrypts the first byte of the packed stream to `0x00`, the LZMA2
end-of-stream marker (the right key decrypts it to `0xE0`, an LZMA chunk). The
entry therefore decodes to 0 of its 512 declared bytes with no decoder error,
and sevenz-rust2 verifies an entry's CRC only after the full declared size has
been read, so nothing below AeroFTP notices. 7-Zip itself rejects it
(`Data Error in encrypted file. Wrong password?`).

It backs `wrong_7z_password_that_decodes_to_an_early_end_is_refused` in
`src/lib.rs`, which first re-checks that property on sevenz-rust2 itself, so a
decoder change cannot make the test quietly stop exercising the early end.

sha256: `212c5d4b316dce2f01c9a0251e366f80d242e4473139076e54292bb839b7eca4`

## Regenerate

A new archive gets a new random IV, so the wrong password has to be found
again:

    printf 'AeroFTP 7z underrun fixture: a wrong password must not extract.\n%.0s' {1..8} > payload.txt
    touch -d '2026-01-01 00:00:00 UTC' payload.txt
    7z a -t7z -m0=lzma2 -mx=5 -mhe=off -ms=off -mtc=off -mta=off -p'right-password' early-end.7z payload.txt

Then try `wrong-0`, `wrong-1`, and so on, until one opens with sevenz-rust2 and
yields an empty `payload.txt` with no error (about one candidate in 256), and
set `EARLY_END_WRONG` in the test to it.
