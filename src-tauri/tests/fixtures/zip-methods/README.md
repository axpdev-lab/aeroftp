# ZIP non-Deflate method fixtures

Small, deterministic `.zip` files whose single member `payload.txt` is compressed
with a method the `zip` crate's WRITER cannot emit. They back the open-only
interop tests in `zip_compression_method_tests` (src/lib.rs): direction
native archiver -> AeroFTP. Methods the writer CAN emit (BZip2 / Zstd / Xz) are
tested in-memory and need no committed fixture.

Each member's exact bytes are `"<payload line>\n"` repeated 64 times; the tests
assert both the declared compression method and the decoded bytes.

| Fixture | Method (id) | Producer | Regenerate |
|---------|-------------|----------|------------|
| `lzma.zip` | LZMA (14) | Python `zipfile.ZIP_LZMA` | see below |
| `deflate64.zip` | Deflate64 (9) | 7-Zip 23.01 | see below |

Regenerate `deflate64.zip` (needs `p7zip-full`):

    printf 'AeroFTP zip-method fixture: deflate64 read-path.\n%.0s' {1..64} > payload.txt
    7z a -tzip -mm=Deflate64 deflate64.zip payload.txt

Regenerate `lzma.zip`:

    python3 - <<'PY'
    import zipfile
    payload = b"AeroFTP zip-method fixture: lzma read-path.\n" * 64
    zi = zipfile.ZipInfo("payload.txt", date_time=(1980, 1, 1, 0, 0, 0))
    zi.compress_type = zipfile.ZIP_LZMA
    zi.external_attr = 0o644 << 16
    with zipfile.ZipFile("lzma.zip", "w") as zf:
        zf.writestr(zi, payload)
    PY
